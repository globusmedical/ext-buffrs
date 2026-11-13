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

use std::{
    io::ErrorKind,
    path::{Path, PathBuf},
    str::FromStr,
};

use bytes::Bytes;
use miette::{miette, Context, IntoDiagnostic};
use walkdir::WalkDir;

use crate::{
    lock::{Digest, DigestAlgorithm, FileRequirement},
    package::{Package, PackageName},
};

/// The environment variable that overrides the default cache location
const CACHE_ENV_VAR: &str = "BUFFRS_CACHE";
/// The default cache directory name
const CACHE_DIRECTORY: &str = "cache";

/// A instance of a cache
pub struct Cache(PathBuf);

impl Cache {
    /// Open the cache
    pub async fn open() -> miette::Result<Self> {
        if let Ok(cache) = std::env::var(CACHE_ENV_VAR).map(PathBuf::from) {
            let res = tokio::fs::create_dir_all(&cache).await;

            match res {
                Ok(()) => (),
                // If the filesystem entry already exists, check if it's a directory,
                // this allow us to give a nicer error message
                Err(err) if err.kind() == ErrorKind::AlreadyExists => {
                    let is_dir = tokio::fs::metadata(&cache)
                        .await
                        .into_diagnostic()
                        .wrap_err_with(|| miette!(
                            "internal: failed to get metadata for cache dir set by {CACHE_ENV_VAR} ({})",
                            cache.display()
                        ))?
                        .is_dir();

                    if !is_dir {
                        return Err(miette!(
                            "internal: failed to initialize cache dir set by {CACHE_ENV_VAR}: '{}' exists but is not directory",
                            cache.display()
                        ));
                    }
                }
                other => other.into_diagnostic().wrap_err_with(|| {
                    miette!(
                        "internal: failed to initialize cache dir set by {CACHE_ENV_VAR} ({})",
                        cache.display()
                    )
                })?,
            }

            let path = tokio::fs::canonicalize(cache)
                .await
                .into_diagnostic()
                .wrap_err("failed to canonicalize cache directory")?;

            let cache = Self::new(path).await?;

            return Ok(cache);
        }

        let path = crate::home()?.join(CACHE_DIRECTORY);

        let cache = Self::new(path).await?;

        Ok(cache)
    }

    /// Create a new buffrs cache at a given location
    ///
    /// This function is idempotent so multiple invocations of the same path
    /// will not modify the filesystem contents.
    pub async fn new(path: PathBuf) -> miette::Result<Self> {
        let exists = tokio::fs::try_exists(&path).await.into_diagnostic()?;

        if !exists {
            tokio::fs::create_dir_all(&path).await.ok();
        }

        let cache = Self(path);

        cache.homogenize().await?;

        Ok(cache)
    }

    /// Homogenize the cache contents to adhere to the cache specification of buffrs
    ///
    /// Note: This function removes all malformed contents of the cache in an idempotent manner.
    /// Please be cautions when calling this function on arbitrary directories as subcontents may
    /// be removed.
    pub async fn homogenize(&self) -> miette::Result<()> {
        let dir = WalkDir::new(self.path())
            .max_depth(1)
            .into_iter()
            .filter_map(std::result::Result::ok);

        let (dirs, files): (Vec<_>, Vec<_>) = dir.partition(|e| e.path().is_dir());

        let invalid_dirs = dirs.into_iter().filter(|d| d.path() != self.path());

        for dir in invalid_dirs {
            tracing::debug!("removing invalid cache entry: {}", dir.path().display());

            tokio::fs::remove_dir_all(dir.path())
                .await
                .into_diagnostic()
                .wrap_err_with(|| miette!(
                    "cache contained an unexpected subdirectory ({}) and buffrs was unable to clean it up",
                    dir.path().display()
                ))?;
        }

        let invalid_files = files.into_iter().filter(|f| {
            let filename = f.path().file_name().unwrap_or_default().to_string_lossy();

            let parts: Vec<_> = filename.split('.').collect();

            // invalid – we should have: {name}.{type}.{digest}.{ext}
            let &[name, r#type, digest, ext] = parts.as_slice() else {
                return true;
            };

            // package name part is invalid
            if PackageName::new(name).is_err() {
                return true;
            }

            // unknown / unsupported digest algorithm
            let Ok(algorithm) = DigestAlgorithm::from_str(r#type) else {
                return true;
            };

            // invalid digest
            if Digest::from_parts(algorithm, digest).is_err() {
                return true;
            }

            // invalid extension
            if ext != "tgz" {
                return true;
            }

            false
        });

        for file in invalid_files {
            tracing::debug!("removing invalid cache entry: {}", file.path().display());

            tokio::fs::remove_file(file.path())
                .await
                .into_diagnostic()
                .wrap_err_with(|| {
                    miette!(
                        "cache contained an unexpected file ({}) and buffrs was unable to clean it up",
                        file.path().display()
                    )
                })?;
        }

        Ok(())
    }

    /// Resolve a file requirement from the cache
    pub async fn get(&self, file: FileRequirement) -> miette::Result<Option<Package>> {
        let entry: Entry = file.into();

        let file = self.path().join(entry.filename());

        let tgz = tokio::fs::read(&file)
            .await
            .into_diagnostic()
            .map(bytes::Bytes::from);

        if let Ok(tgz) = tgz {
            let pkg = Package::parse(tgz)?;

            return Ok(Some(pkg));
        }

        Ok(None)
    }

    /// Put a locked package in the cache
    ///
    /// The package is written to a temporary file in the cache directory and renamed into place,
    /// so a concurrent reader finds either no entry or a complete one.
    pub async fn put(&self, entry: Entry, bytes: Bytes) -> miette::Result<()> {
        let file = self.path().join(entry.filename());
        let temp = self.path().join(entry.temp_filename());

        let stored = match tokio::fs::write(&temp, bytes.as_ref()).await {
            Ok(()) => tokio::fs::rename(&temp, &file).await,
            Err(error) => Err(error),
        };

        if let Err(error) = stored {
            tokio::fs::remove_file(&temp).await.ok();

            // An entry is named after the digest of its content, so an entry that exists holds
            // this package, whoever stored it.
            if !tokio::fs::try_exists(&file).await.unwrap_or(false) {
                return Err(error).into_diagnostic().wrap_err(miette!(
                    "failed to put package {} in the cache",
                    entry.filename().display()
                ));
            }
        }

        Ok(())
    }

    /// The directory in the filesystem used by this cache
    #[must_use]
    pub fn path(&self) -> &Path {
        self.0.as_path()
    }
}

/// A cache locator to store or retrieve a package
///
/// This follows the naming scheme of {package-name}-{digest-type}-{digest}.tgz
pub struct Entry(PathBuf);

impl Entry {
    /// The filename of the cache entry
    #[must_use]
    pub fn filename(&self) -> &Path {
        self.0.as_path()
    }

    /// A new name for the file that holds this entry while it is written.
    ///
    /// The name is that of an entry of another package, `{package-name}-tmp-{nonce}`, with the
    /// same digest. [`Cache::homogenize`] in a concurrent process therefore keeps the file, and no
    /// lookup resolves to it, because the digest belongs to a package with a different name.
    fn temp_filename(&self) -> PathBuf {
        use std::hash::{BuildHasher, Hasher};

        let nonce = std::collections::hash_map::RandomState::new()
            .build_hasher()
            .finish();
        let filename = self.0.to_string_lossy();
        let (name, rest) = filename.split_once('.').unwrap_or((&filename, ""));

        format!("{name}-tmp-{nonce:016x}.{rest}").into()
    }
}

impl From<Package> for Entry {
    fn from(value: Package) -> Self {
        Self::from(&value)
    }
}

impl From<&Package> for Entry {
    fn from(value: &Package) -> Self {
        let digest = value.digest(DigestAlgorithm::SHA256);
        Self(
            format!(
                "{}.{}.{}.tgz",
                value.name(),
                digest.algorithm(),
                hex::encode(digest.as_bytes())
            )
            .into(),
        )
    }
}

impl From<FileRequirement> for Entry {
    fn from(req: FileRequirement) -> Entry {
        Self::from(&req)
    }
}

impl From<&FileRequirement> for Entry {
    fn from(req: &FileRequirement) -> Entry {
        Self(
            format!(
                "{}.{}.{}.tgz",
                req.package,
                req.digest.algorithm(),
                hex::encode(req.digest.as_bytes())
            )
            .into(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An entry as [`Entry::from`] names it, for a package of the given name and content.
    fn entry(name: &str, content: &[u8]) -> Entry {
        let digest = DigestAlgorithm::SHA256.digest(content);

        Entry(
            format!(
                "{name}.{}.{}.tgz",
                digest.algorithm(),
                hex::encode(digest.as_bytes())
            )
            .into(),
        )
    }

    fn file_names(cache: &Cache) -> Vec<String> {
        let mut names: Vec<_> = std::fs::read_dir(cache.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_readers_never_see_a_partial_entry() {
        let dir = assert_fs::TempDir::new().unwrap();
        let cache = std::sync::Arc::new(Cache::new(dir.path().to_owned()).await.unwrap());
        let content = Bytes::from(vec![7_u8; 1 << 20]);
        let file = cache.path().join(entry("some-lib", &content).filename());

        let writers: Vec<_> = (0..2)
            .map(|_| {
                let cache = std::sync::Arc::clone(&cache);
                let content = content.clone();
                tokio::spawn(async move {
                    for _ in 0..100 {
                        cache
                            .put(entry("some-lib", &content), content.clone())
                            .await
                            .unwrap();
                    }
                })
            })
            .collect();

        while !writers.iter().all(tokio::task::JoinHandle::is_finished) {
            // A read fails until the first writer has stored the entry, and on Windows it can
            // also fail while the entry is being replaced.
            if let Ok(read) = tokio::fs::read(&file).await {
                assert_eq!(read.len(), content.len());
            }
        }
        for writer in writers {
            writer.await.unwrap();
        }

        assert_eq!(tokio::fs::read(&file).await.unwrap(), content);
        assert_eq!(
            file_names(&cache),
            [file.file_name().unwrap().to_string_lossy()]
        );
    }

    #[tokio::test]
    async fn homogenize_keeps_an_entry_that_is_being_written() {
        let dir = assert_fs::TempDir::new().unwrap();
        let cache = Cache::new(dir.path().to_owned()).await.unwrap();
        let entry = entry("some-lib", b"content");
        let temp = entry.temp_filename();

        assert_ne!(temp, entry.filename());
        assert_ne!(temp, entry.temp_filename());

        std::fs::write(cache.path().join(&temp), b"cont").unwrap();
        std::fs::write(cache.path().join("stray.tmp"), b"stray").unwrap();
        cache.homogenize().await.unwrap();

        assert_eq!(file_names(&cache), [temp.to_string_lossy()]);
    }

    #[tokio::test]
    async fn put_replaces_a_truncated_entry() {
        let dir = assert_fs::TempDir::new().unwrap();
        let cache = Cache::new(dir.path().to_owned()).await.unwrap();
        let content = Bytes::from_static(b"the whole package");
        let file = cache.path().join(entry("some-lib", &content).filename());
        std::fs::write(&file, &content[..4]).unwrap();

        cache
            .put(entry("some-lib", &content), content.clone())
            .await
            .unwrap();

        assert_eq!(std::fs::read(&file).unwrap(), content);
    }
}
