// Copyright 2023 Helsing GmbH
// Copyright 2024 Globus Medical, Inc.
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

//! Metadata emission for build system integration.
//!
//! This module generates metadata files in `_buffrs_meta/` that enable build systems
//! (e.g., CMake) to:
//!
//! 1. Create per-package build targets with proper dependencies
//! 2. Validate link-unit safety by checking namespace conflicts
//! 3. Track which protobuf namespaces belong to which package versions
//!
//! ## Generated Files
//!
//! - `graph.json`: Complete dependency graph with versions, registries, and relationships
//! - `namespaces.json`: Mapping from protobuf namespaces to package@version
//! - `buffrs.cmake`: CMake script defining package targets with namespace properties

use std::collections::HashMap;
use std::path::Path;

use miette::{miette, IntoDiagnostic};
use semver::Version;
use serde::{Deserialize, Serialize};
use tokio::fs;

use crate::namespace_scan::{self, NamespaceScanResult};
use crate::package::PackageName;
use crate::resolver::{DependencyGraph, ResolvedDependency};

/// Metadata directory name within the vendor folder.
pub const METADATA_DIR: &str = "_buffrs_meta";

/// Graph metadata filename.
pub const GRAPH_JSON: &str = "graph.json";

/// Namespaces metadata filename.
pub const NAMESPACES_JSON: &str = "namespaces.json";

/// CMake targets filename.
pub const BUFFRS_CMAKE: &str = "buffrs.cmake";

/// Serializable representation of the dependency graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphMetadata {
    /// Buffrs version that generated this metadata.
    pub buffrs_version: String,
    /// Root dependencies (direct dependencies of the manifest).
    pub roots: Vec<PackageRef>,
    /// All resolved packages.
    pub packages: Vec<PackageMetadata>,
}

/// Reference to a package by name and version.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PackageRef {
    /// Package name.
    pub name: String,
    /// Package version.
    pub version: String,
}

impl PackageRef {
    /// Creates a new package reference.
    pub fn new(name: &PackageName, version: &Version) -> Self {
        Self {
            name: name.to_string(),
            version: version.to_string(),
        }
    }

    /// Returns the vendor directory name for this package reference.
    pub fn vendor_dir(&self, version_qualified: bool) -> String {
        if version_qualified {
            format!("{}@{}", self.name, self.version)
        } else {
            self.name.clone()
        }
    }
}

/// Metadata for a single resolved package.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageMetadata {
    /// Package name.
    pub name: String,
    /// Package version.
    pub version: String,
    /// Vendor directory name (may include version for multi-version).
    pub vendor_dir: String,
    /// Registry URL (for remote packages).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub registry: Option<String>,
    /// Repository name (for remote packages).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,
    /// Local path (for local packages).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_path: Option<String>,
    /// Direct dependencies of this package.
    pub dependencies: Vec<PackageRef>,
    /// Protobuf namespaces declared by this package.
    pub namespaces: Vec<String>,
}

/// Serializable namespace mapping.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NamespacesMetadata {
    /// Mapping from protobuf namespace to packages declaring it.
    /// Normally each namespace maps to exactly one package; multiple
    /// indicates a potential conflict.
    pub namespaces: HashMap<String, Vec<NamespaceOwner>>,
}

/// Information about a package that owns a namespace.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NamespaceOwner {
    /// Package name.
    pub name: String,
    /// Package version.
    pub version: String,
    /// Example file declaring this namespace.
    pub source_file: String,
}

/// Emits metadata files to the `_buffrs_meta/` directory.
///
/// Only creates metadata if there are packages in the graph.
pub async fn emit_metadata(graph: &DependencyGraph, vendor_root: &Path) -> miette::Result<()> {
    // Skip metadata emission if graph is empty
    if graph.is_empty() {
        return Ok(());
    }

    let meta_dir = vendor_root.join(METADATA_DIR);

    // Create metadata directory
    fs::create_dir_all(&meta_dir)
        .await
        .into_diagnostic()
        .map_err(|e| miette!("failed to create metadata directory: {}", e))?;

    // Scan namespaces
    let namespace_scan = namespace_scan::scan_dependency_graph(graph, vendor_root).await?;

    // Generate and write graph.json
    let graph_meta = build_graph_metadata(graph, &namespace_scan)?;
    let graph_json = serde_json::to_string_pretty(&graph_meta)
        .into_diagnostic()
        .map_err(|e| miette!("failed to serialize graph metadata: {}", e))?;
    fs::write(meta_dir.join(GRAPH_JSON), graph_json)
        .await
        .into_diagnostic()
        .map_err(|e| miette!("failed to write {}: {}", GRAPH_JSON, e))?;

    // Generate and write namespaces.json
    let namespaces_meta = build_namespaces_metadata(&namespace_scan);
    let namespaces_json = serde_json::to_string_pretty(&namespaces_meta)
        .into_diagnostic()
        .map_err(|e| miette!("failed to serialize namespaces metadata: {}", e))?;
    fs::write(meta_dir.join(NAMESPACES_JSON), namespaces_json)
        .await
        .into_diagnostic()
        .map_err(|e| miette!("failed to write {}: {}", NAMESPACES_JSON, e))?;

    // Generate and write buffrs.cmake
    let cmake_content = build_cmake_targets(graph, &namespace_scan)?;
    fs::write(meta_dir.join(BUFFRS_CMAKE), cmake_content)
        .await
        .into_diagnostic()
        .map_err(|e| miette!("failed to write {}: {}", BUFFRS_CMAKE, e))?;

    tracing::debug!(":: emitted metadata to {}", meta_dir.display());

    Ok(())
}

/// Builds the graph metadata from the dependency graph.
fn build_graph_metadata(
    graph: &DependencyGraph,
    namespace_scan: &NamespaceScanResult,
) -> miette::Result<GraphMetadata> {
    let roots: Vec<PackageRef> = graph
        .roots()
        .iter()
        .map(|id| PackageRef::new(id.name(), id.version()))
        .collect();

    let mut packages = Vec::new();

    for (id, resolved) in graph.iter() {
        let vendor_dir = id.vendor_dir_name(graph);

        let (registry, repository, local_path) = match resolved {
            ResolvedDependency::Remote {
                registry,
                repository,
                ..
            } => (Some(registry.to_string()), Some(repository.clone()), None),
            ResolvedDependency::Local { path, .. } => {
                (None, None, Some(path.to_string_lossy().to_string()))
            }
        };

        let dependencies: Vec<PackageRef> = resolved
            .depends_on()
            .iter()
            .map(|dep_id| PackageRef::new(dep_id.name(), dep_id.version()))
            .collect();

        // Find namespaces for this package
        let namespaces: Vec<String> = namespace_scan
            .packages
            .iter()
            .find(|p| &p.name == id.name() && &p.version == id.version())
            .map(|p| p.namespaces.iter().map(|ns| ns.namespace.clone()).collect())
            .unwrap_or_default();

        packages.push(PackageMetadata {
            name: id.name().to_string(),
            version: id.version().to_string(),
            vendor_dir,
            registry,
            repository,
            local_path,
            dependencies,
            namespaces,
        });
    }

    // Sort packages for deterministic output
    packages.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.version.cmp(&b.version)));

    Ok(GraphMetadata {
        buffrs_version: env!("CARGO_PKG_VERSION").to_string(),
        roots,
        packages,
    })
}

/// Builds the namespaces metadata from the scan result.
fn build_namespaces_metadata(namespace_scan: &NamespaceScanResult) -> NamespacesMetadata {
    let mut namespaces: HashMap<String, Vec<NamespaceOwner>> = HashMap::new();

    for (ns, owners) in &namespace_scan.namespace_to_packages {
        let entries: Vec<NamespaceOwner> = owners
            .iter()
            .map(|(name, version, file, _content_hash)| NamespaceOwner {
                name: name.to_string(),
                version: version.to_string(),
                source_file: file.clone(),
            })
            .collect();
        namespaces.insert(ns.clone(), entries);
    }

    NamespacesMetadata { namespaces }
}

/// Builds CMake content for package targets.
fn build_cmake_targets(
    graph: &DependencyGraph,
    namespace_scan: &NamespaceScanResult,
) -> miette::Result<String> {
    let mut cmake = String::new();

    cmake.push_str("# Auto-generated by buffrs. Do not edit.\n");
    cmake.push_str(&format!(
        "# Generated by buffrs version {}\n\n",
        env!("CARGO_PKG_VERSION")
    ));

    cmake.push_str("# Guard against multiple inclusion\n");
    cmake.push_str("if(DEFINED _BUFFRS_METADATA_INCLUDED)\n");
    cmake.push_str("  return()\n");
    cmake.push_str("endif()\n");
    cmake.push_str("set(_BUFFRS_METADATA_INCLUDED TRUE)\n\n");

    // Emit package list
    cmake.push_str("# All resolved packages\n");
    cmake.push_str("set(BUFFRS_PACKAGES\n");
    for id in graph.keys() {
        cmake.push_str(&format!("  \"{}@{}\"\n", id.name(), id.version()));
    }
    cmake.push_str(")\n\n");

    // Emit per-package variables
    for (id, resolved) in graph.iter() {
        let vendor_dir = id.vendor_dir_name(graph);
        let safe_name = cmake_safe_name(&format!("{}@{}", id.name(), id.version()));

        cmake.push_str(&format!("# Package: {}@{}\n", id.name(), id.version()));
        cmake.push_str(&format!(
            "set(BUFFRS_PKG_{}_NAME \"{}\")\n",
            safe_name,
            id.name()
        ));
        cmake.push_str(&format!(
            "set(BUFFRS_PKG_{}_VERSION \"{}\")\n",
            safe_name,
            id.version()
        ));
        cmake.push_str(&format!(
            "set(BUFFRS_PKG_{}_VENDOR_DIR \"{}\")\n",
            safe_name, vendor_dir
        ));

        // Dependencies
        let deps: Vec<String> = resolved
            .depends_on()
            .iter()
            .map(|dep_id| format!("{}@{}", dep_id.name(), dep_id.version()))
            .collect();
        cmake.push_str(&format!(
            "set(BUFFRS_PKG_{}_DEPENDENCIES {})\n",
            safe_name,
            if deps.is_empty() {
                "".to_string()
            } else {
                format!("\"{}\"", deps.join("\" \""))
            }
        ));

        // Namespaces
        let namespaces: Vec<String> = namespace_scan
            .packages
            .iter()
            .find(|p| &p.name == id.name() && &p.version == id.version())
            .map(|p| p.namespaces.iter().map(|ns| ns.namespace.clone()).collect())
            .unwrap_or_default();
        cmake.push_str(&format!(
            "set(BUFFRS_PKG_{}_NAMESPACES {})\n",
            safe_name,
            if namespaces.is_empty() {
                "".to_string()
            } else {
                format!("\"{}\"", namespaces.join("\" \""))
            }
        ));

        cmake.push_str("\n");
    }

    // Emit root packages
    cmake.push_str("# Root (direct) dependencies\n");
    cmake.push_str("set(BUFFRS_ROOT_PACKAGES\n");
    for id in graph.roots() {
        cmake.push_str(&format!("  \"{}@{}\"\n", id.name(), id.version()));
    }
    cmake.push_str(")\n");

    // Note: Link-unit validation functions are intentionally NOT emitted here.
    // Build systems should provide their own validation logic using the data above,
    // as they have better visibility into the actual link graph across multiple
    // Proto.toml resolutions. See BuffrsIntegration.cmake for an example.

    Ok(cmake)
}

/// Converts a package name to a CMake-safe variable name.
fn cmake_safe_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cmake_safe_name() {
        assert_eq!(cmake_safe_name("lib-base@0.1.0"), "LIB_BASE_0_1_0");
        assert_eq!(cmake_safe_name("api.service.v2"), "API_SERVICE_V2");
        assert_eq!(cmake_safe_name("MyPackage"), "MYPACKAGE");
    }

    #[test]
    fn test_package_ref_vendor_dir() {
        let pkg = PackageRef {
            name: "lib-base".to_string(),
            version: "0.1.0".to_string(),
        };
        assert_eq!(pkg.vendor_dir(false), "lib-base");
        assert_eq!(pkg.vendor_dir(true), "lib-base@0.1.0");
    }
}
