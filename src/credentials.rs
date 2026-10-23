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

use miette::{Context, IntoDiagnostic};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    io::{ErrorKind, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};
use tokio::fs;

use crate::{
    errors::{DeserializationError, FileExistsError, ReadError, SerializationError, WriteError},
    registry::RegistryUri,
    ManagedFile,
};

/// Filename of the credential store
pub const CREDENTIALS_FILE: &str = "credentials.toml";

/// Credential store for storing authentication data
///
/// This type represents a snapshot of the read credential store.
#[derive(Debug, Default, Clone)]
pub struct Credentials {
    /// A mapping from registry URIs to their corresponding tokens
    pub registry_tokens: HashMap<RegistryUri, String>,
}

impl Credentials {
    fn location() -> miette::Result<PathBuf> {
        Ok(crate::home().into_diagnostic()?.join(CREDENTIALS_FILE))
    }

    /// Checks if the credentials exists
    pub async fn exists() -> miette::Result<bool> {
        fs::try_exists(Self::location()?)
            .await
            .into_diagnostic()
            .wrap_err(FileExistsError(CREDENTIALS_FILE.to_string()))
    }

    /// Reads the credentials from the file system
    pub async fn read() -> miette::Result<Option<Self>> {
        // if the file does not exist, we don't need to treat it as an error.
        match fs::read_to_string(Self::location()?).await {
            Ok(contents) => {
                let raw: RawCredentialCollection = toml::from_str(&contents)
                    .into_diagnostic()
                    .wrap_err(DeserializationError(ManagedFile::Credentials))?;
                Ok(Some(raw.into()))
            }
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error)
                .into_diagnostic()
                .wrap_err(ReadError(CREDENTIALS_FILE)),
        }
    }

    /// Writes the credentials to the file system
    pub async fn write(&self) -> miette::Result<()> {
        let location = Self::location()?;

        if let Some(parent) = location.parent() {
            // if directory already exists, error is returned but that is fine
            fs::create_dir(parent).await.ok();
        }

        let data: RawCredentialCollection = self.clone().into();

        let contents = toml::to_string(&data)
            .into_diagnostic()
            .wrap_err(SerializationError(ManagedFile::Credentials))?;

        replace_file(&location, contents.as_bytes())
            .await
            .into_diagnostic()
            .wrap_err(WriteError(CREDENTIALS_FILE))
    }

    /// Loads the credentials from the file system, returning default credentials if
    /// they do not exist.
    ///
    /// Note, this should not create files in the user's home directory, as we should
    /// not be performing global stateful operations in absence of a user instruction.
    pub async fn load() -> miette::Result<Self> {
        Ok(Self::read().await?.unwrap_or_else(Credentials::default))
    }
}

/// Replaces `path` with `contents` so that concurrent readers see either the
/// previous file or the complete new one, never a truncated file.
///
/// Parallel build scripts log in and read credentials at the same time, so a
/// plain truncate-and-write lets a reader parse an empty store and fail with
/// "unauthorized".
///
/// Each call writes its own temporary file, created owner-only on Unix because
/// the rename gives `path` the temporary file's permissions.
async fn replace_file(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

    let mut temp_name = path.file_name().unwrap_or_default().to_os_string();
    temp_name.push(format!(
        ".{}.{}.tmp",
        std::process::id(),
        NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed)
    ));
    let temp_path = path.with_file_name(temp_name);

    if let Err(error) = write_private_file(&temp_path, contents).await {
        fs::remove_file(&temp_path).await.ok();
        return Err(error);
    }
    if let Err(error) = fs::rename(&temp_path, path).await {
        fs::remove_file(&temp_path).await.ok();
        return Err(error);
    }
    Ok(())
}

/// Creates `path`, which must not exist, readable and writable by the owner
/// only on Unix, and writes `contents` to it.
async fn write_private_file(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    let path = path.to_owned();
    let contents = contents.to_owned();

    tokio::task::spawn_blocking(move || {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);

        let mut file = options.open(&path)?;
        file.write_all(&contents)?;
        file.sync_all()
    })
    .await
    .map_err(std::io::Error::other)?
}

/// Credential store for storing authentication data. Serialization type.
#[derive(Serialize, Deserialize)]
struct RawCredentialCollection {
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    credentials: Vec<RawRegistryCredentials>,
}

/// Credentials for a single registry. Serialization type.
#[derive(Serialize, Deserialize)]
struct RawRegistryCredentials {
    uri: RegistryUri,
    token: String,
}

impl From<RawCredentialCollection> for Credentials {
    fn from(value: RawCredentialCollection) -> Self {
        let credentials = value
            .credentials
            .into_iter()
            .map(|it| (it.uri, it.token))
            .collect();

        Self {
            registry_tokens: credentials,
        }
    }
}

impl From<Credentials> for RawCredentialCollection {
    fn from(value: Credentials) -> Self {
        let credentials = value
            .registry_tokens
            .into_iter()
            .map(|(uri, token)| RawRegistryCredentials { uri, token })
            .collect();

        Self { credentials }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_readers_never_see_a_partial_credentials_file() {
        let dir = assert_fs::TempDir::new().unwrap();
        let path = dir.path().join(CREDENTIALS_FILE);
        let contents = "x".repeat(1 << 20);
        replace_file(&path, contents.as_bytes()).await.unwrap();

        let writer = {
            let path = path.clone();
            let contents = contents.clone();
            tokio::spawn(async move {
                for _ in 0..200 {
                    replace_file(&path, contents.as_bytes()).await.unwrap();
                }
            })
        };

        while !writer.is_finished() {
            // On Windows a read can fail while the file is being replaced.
            if let Ok(read) = fs::read_to_string(&path).await {
                assert_eq!(read.len(), contents.len());
            }
        }
        writer.await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn overlapping_writers_in_one_process_each_replace_the_whole_file() {
        let dir = assert_fs::TempDir::new().unwrap();
        let path = dir.path().join(CREDENTIALS_FILE);
        let contents = "x".repeat(1 << 20);

        let writers: Vec<_> = (0..4)
            .map(|_| {
                let path = path.clone();
                let contents = contents.clone();
                tokio::spawn(async move {
                    for _ in 0..50 {
                        replace_file(&path, contents.as_bytes()).await.unwrap();
                    }
                })
            })
            .collect();
        for writer in writers {
            writer.await.unwrap();
        }

        assert_eq!(fs::read_to_string(&path).await.unwrap(), contents);
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .filter(|name| name != CREDENTIALS_FILE)
            .collect();
        assert!(leftovers.is_empty(), "temporary files left: {leftovers:?}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn replacing_a_private_credentials_file_keeps_it_private() {
        use std::os::unix::fs::PermissionsExt;

        let dir = assert_fs::TempDir::new().unwrap();
        let path = dir.path().join(CREDENTIALS_FILE);
        std::fs::write(&path, "old").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();

        replace_file(&path, b"new").await.unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}
