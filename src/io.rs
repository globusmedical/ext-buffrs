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

//! IO traits for standardizing file operations.
//!
//! This module provides the [`File`] trait for abstracting filesystem operations,
//! improving testability and enabling future cross-cutting features like workspace
//! support and packaging metadata.

use miette::{Context, IntoDiagnostic};
use std::path::Path;
use tokio::fs;

use crate::errors::FileExistsError;

/// The `File` trait standardizes the process of reading and writing files.
///
/// This trait provides a common interface for file-backed types like manifests,
/// lockfiles, and credentials, enabling consistent filesystem operations and
/// improved testability.
#[async_trait::async_trait]
pub trait File: Sized + Send + Sync + 'static {
    /// The default location of this file
    const DEFAULT_PATH: &'static str;

    /// Checks if the file currently exists in the filesystem at its default path
    async fn exists() -> miette::Result<bool> {
        Self::exists_at(Self::DEFAULT_PATH).await
    }

    /// Checks if the file currently exists in the filesystem at a given path
    async fn exists_at<P>(path: P) -> miette::Result<bool>
    where
        P: AsRef<Path> + Send + Sync,
    {
        let path_ref = path.as_ref();
        fs::try_exists(path_ref)
            .await
            .into_diagnostic()
            .wrap_err(FileExistsError(path_ref.to_string_lossy().into_owned()))
    }

    /// Loads the file from the current directory
    async fn load() -> miette::Result<Self> {
        Self::load_from(Self::DEFAULT_PATH).await
    }

    /// Loads the file from a specific path.
    async fn load_from<P>(path: P) -> miette::Result<Self>
    where
        P: AsRef<Path> + Send + Sync;

    /// Loads the file from the current directory, if it exists, otherwise returns an empty one.
    /// Fails if the `exists()` check fails.
    async fn load_or_default() -> miette::Result<Self>
    where
        Self: Default,
    {
        if Self::exists().await? {
            Self::load().await
        } else {
            Ok(Self::default())
        }
    }

    /// Loads the file from a specific path, if it exists, otherwise returns an empty one.
    /// Fails if the `exists()` check fails.
    async fn load_from_or_default<P>(path: P) -> miette::Result<Self>
    where
        Self: Default,
        P: AsRef<Path> + Send + Sync,
    {
        if Self::exists_at(&path).await? {
            Self::load_from(path).await
        } else {
            Ok(Self::default())
        }
    }

    /// Persists a file to the filesystem.
    async fn save<P>(&self, path: P) -> miette::Result<()>
    where
        P: AsRef<Path> + Send + Sync;
}
