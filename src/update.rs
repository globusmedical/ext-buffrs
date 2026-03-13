// Copyright 2023 Helsing GmbH
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Self-update command — downloads the latest buffrs binary from GitHub Releases.

use miette::{bail, miette, Context, IntoDiagnostic};
use std::env;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const GITHUB_REPO: &str = "globusmedical/ext-buffrs";
const RELEASE_TAG_PREFIX: &str = "gm/v";

#[cfg(target_os = "windows")]
const GH_BINARY_NAME: &str = "gh.exe";

#[cfg(not(target_os = "windows"))]
const GH_BINARY_NAME: &str = "gh";

// Target triples used in release asset names
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const TARGET_TRIPLE: &str = "x86_64-unknown-linux-gnu";

#[cfg(all(target_os = "linux", target_arch = "arm"))]
const TARGET_TRIPLE: &str = "arm-unknown-linux-gnueabihf";

#[cfg(all(target_os = "macos", target_arch = "x86_64"))]
const TARGET_TRIPLE: &str = "x86_64-apple-darwin";

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const TARGET_TRIPLE: &str = "aarch64-apple-darwin";

#[cfg(all(target_os = "windows", target_arch = "x86_64"))]
const TARGET_TRIPLE: &str = "x86_64-pc-windows-msvc";

#[cfg(all(target_os = "windows", target_arch = "x86"))]
const TARGET_TRIPLE: &str = "i686-pc-windows-msvc";

#[cfg(target_os = "windows")]
const BINARY_NAME: &str = "buffrs.exe";

#[cfg(not(target_os = "windows"))]
const BINARY_NAME: &str = "buffrs";

/// Check for available updates without installing.
pub async fn check() -> miette::Result<()> {
    let current_version = env!("CARGO_PKG_VERSION");

    println!("Current version: {current_version}");
    println!("Checking for updates...");

    let latest_version = get_latest_release().await?;

    if latest_version == current_version {
        println!("✓ You are running the latest version");
    } else {
        println!("⚠ Update available: {latest_version}");
        println!("Run 'buffrs self-update' to install the latest version");
    }

    Ok(())
}

/// Update to the latest version.
pub async fn update(force: bool) -> miette::Result<()> {
    let current_version = env!("CARGO_PKG_VERSION");

    println!("Current version: {current_version}");
    println!("Checking for updates...");

    let latest_version = get_latest_release().await?;

    if latest_version == current_version && !force {
        println!("✓ You are already running the latest version");
        return Ok(());
    }

    if force {
        println!("Forcing update to version {latest_version}...");
    } else {
        println!("Updating to version {latest_version}...");
    }

    let bytes = download_archive(&latest_version).await?;
    println!("Downloaded {} bytes", bytes.len());

    let current_exe = env::current_exe()
        .into_diagnostic()
        .wrap_err("failed to get current executable path")?;
    let temp_path = get_temp_path(&current_exe);
    let backup_path = get_backup_path(&current_exe);

    fs::write(&temp_path, &bytes)
        .into_diagnostic()
        .wrap_err("failed to write temporary file")?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&temp_path).into_diagnostic()?.permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&temp_path, perms).into_diagnostic()?;
    }

    fs::rename(&current_exe, &backup_path)
        .into_diagnostic()
        .wrap_err("failed to backup current binary")?;

    if let Err(e) = fs::rename(&temp_path, &current_exe) {
        // Rollback on failure
        let _ = fs::rename(&backup_path, &current_exe);
        bail!("failed to install update: {e}");
    }

    let _ = fs::remove_file(&backup_path);

    println!("✓ Successfully updated to version {latest_version}");
    println!("Restart buffrs to use the new version");

    Ok(())
}

// ---------------------------------------------------------------------------
//  Internal helpers
// ---------------------------------------------------------------------------

/// Locate the `gh` CLI executable (next to our binary, then in `PATH`).
fn find_gh_cli() -> Option<PathBuf> {
    if let Ok(current_exe) = env::current_exe() {
        if let Some(exe_dir) = current_exe.parent() {
            let gh_path = exe_dir.join(GH_BINARY_NAME);
            if gh_path.exists() {
                return Some(gh_path);
            }
        }
    }

    if Command::new("gh")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
    {
        return Some(PathBuf::from("gh"));
    }

    None
}

/// Run a `gh` CLI command with the given arguments.
fn run_gh_command<I, S>(args: I, working_dir: &Path) -> Option<std::process::Output>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let gh_path = find_gh_cli()?;
    Command::new(gh_path)
        .args(args)
        .current_dir(working_dir)
        .output()
        .ok()
}

/// Download bytes via HTTP (fallback when `gh` is unavailable).
async fn download_via_http(url: &str) -> miette::Result<Vec<u8>> {
    let client = reqwest::Client::builder()
        .user_agent("buffrs-cli")
        .build()
        .into_diagnostic()
        .wrap_err("failed to create HTTP client")?;

    let response = client
        .get(url)
        .send()
        .await
        .into_diagnostic()
        .wrap_err("failed to download update")?;

    if !response.status().is_success() {
        bail!("failed to download update: HTTP {}", response.status());
    }

    let bytes = response
        .bytes()
        .await
        .into_diagnostic()
        .wrap_err("failed to read response body")?;

    Ok(bytes.to_vec())
}

/// Archive asset name for this platform (e.g. `v1.2.5-x86_64-unknown-linux-gnu.tar.gz`).
fn archive_asset_name(version: &str) -> String {
    #[cfg(target_os = "windows")]
    {
        format!("v{version}-{TARGET_TRIPLE}.zip")
    }
    #[cfg(not(target_os = "windows"))]
    {
        format!("v{version}-{TARGET_TRIPLE}.tar.gz")
    }
}

/// Download and extract the buffrs binary from a release archive.
async fn download_archive(version: &str) -> miette::Result<Vec<u8>> {
    let asset_name = archive_asset_name(version);
    let temp_dir = std::env::temp_dir();
    let temp_download = temp_dir.join(&asset_name);
    let _ = fs::remove_file(&temp_download);

    let gh_download = run_gh_command(
        [
            "release",
            "download",
            &format!("{RELEASE_TAG_PREFIX}{version}"),
            "--pattern",
            &asset_name,
            "--repo",
            GITHUB_REPO,
            "--dir",
            ".",
            "--clobber",
        ],
        &temp_dir,
    );

    let archive_bytes = if let Some(output) = gh_download {
        if output.status.success() {
            fs::read(&temp_download)
                .into_diagnostic()
                .wrap_err("failed to read downloaded archive")?
        } else {
            let download_url = format!(
                "https://github.com/{GITHUB_REPO}/releases/download/{RELEASE_TAG_PREFIX}{version}/{asset_name}"
            );
            download_via_http(&download_url).await?
        }
    } else {
        let download_url = format!(
            "https://github.com/{GITHUB_REPO}/releases/download/{RELEASE_TAG_PREFIX}{version}/{asset_name}"
        );
        download_via_http(&download_url).await?
    };

    extract_binary_from_archive(&archive_bytes)
}

/// Extract the `buffrs` binary from the release archive.
fn extract_binary_from_archive(archive_bytes: &[u8]) -> miette::Result<Vec<u8>> {
    #[cfg(target_os = "windows")]
    {
        use std::io::{Cursor, Read};
        let cursor = Cursor::new(archive_bytes);
        let mut zip = zip::ZipArchive::new(cursor)
            .into_diagnostic()
            .wrap_err("failed to open ZIP archive")?;

        for i in 0..zip.len() {
            let mut file = zip
                .by_index(i)
                .into_diagnostic()
                .wrap_err("failed to read ZIP entry")?;
            if file.name().ends_with(BINARY_NAME) {
                let mut contents = Vec::new();
                file.read_to_end(&mut contents)
                    .into_diagnostic()
                    .wrap_err("failed to read binary from ZIP")?;
                return Ok(contents);
            }
        }
        bail!("binary '{BINARY_NAME}' not found in ZIP archive");
    }

    #[cfg(not(target_os = "windows"))]
    {
        use flate2::read::GzDecoder;
        use std::io::Read;

        let decompressor = GzDecoder::new(archive_bytes);
        let mut archive = tar::Archive::new(decompressor);

        for entry_result in archive
            .entries()
            .into_diagnostic()
            .wrap_err("failed to read tar entries")?
        {
            let mut entry = entry_result
                .into_diagnostic()
                .wrap_err("failed to read tar entry")?;
            let path = entry
                .path()
                .into_diagnostic()
                .wrap_err("failed to get entry path")?;

            if path.to_string_lossy().ends_with(BINARY_NAME) {
                let mut contents = Vec::new();
                entry
                    .read_to_end(&mut contents)
                    .into_diagnostic()
                    .wrap_err("failed to read binary from tar")?;
                return Ok(contents);
            }
        }
        bail!("binary '{BINARY_NAME}' not found in tar.gz archive");
    }
}

/// Retrieve the latest release version string (without prefix) from GitHub.
async fn get_latest_release() -> miette::Result<String> {
    // Try gh CLI first (handles private repo auth automatically)
    if let Some(gh_path) = find_gh_cli() {
        if let Ok(output) = Command::new(gh_path)
            .args(["api", &format!("repos/{GITHUB_REPO}/releases/latest")])
            .output()
        {
            if output.status.success() {
                let release: serde_json::Value = serde_json::from_slice(&output.stdout)
                    .into_diagnostic()
                    .wrap_err("failed to parse release JSON from gh CLI")?;

                let tag_name = release["tag_name"]
                    .as_str()
                    .ok_or_else(|| miette!("missing tag_name in release"))?;

                let version = tag_name
                    .strip_prefix(RELEASE_TAG_PREFIX)
                    .ok_or_else(|| miette!("unexpected tag format: {tag_name}"))?;

                return Ok(version.to_string());
            }
        }
    }

    // Fallback to direct GitHub API call
    let url = format!("https://api.github.com/repos/{GITHUB_REPO}/releases/latest");

    let client = reqwest::Client::builder()
        .user_agent("buffrs-cli")
        .build()
        .into_diagnostic()
        .wrap_err("failed to create HTTP client")?;

    let response = client
        .get(&url)
        .send()
        .await
        .into_diagnostic()
        .wrap_err("failed to fetch latest release")?;

    if !response.status().is_success() {
        bail!("failed to fetch latest release: HTTP {}", response.status());
    }

    let release: serde_json::Value = response
        .json()
        .await
        .into_diagnostic()
        .wrap_err("failed to parse release JSON")?;

    let tag_name = release["tag_name"]
        .as_str()
        .ok_or_else(|| miette!("missing tag_name in release"))?;

    let version = tag_name
        .strip_prefix(RELEASE_TAG_PREFIX)
        .ok_or_else(|| miette!("unexpected tag format: {tag_name}"))?;

    Ok(version.to_string())
}

fn get_temp_path(current_exe: &Path) -> PathBuf {
    let mut temp = current_exe.to_path_buf();
    temp.set_extension("tmp");
    temp
}

fn get_backup_path(current_exe: &Path) -> PathBuf {
    let mut backup = current_exe.to_path_buf();
    backup.set_extension("bak");
    backup
}
