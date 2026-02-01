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

//! Namespace scanning for protobuf files.
//!
//! This module provides functionality to scan `.proto` files and extract namespace
//! declarations (`package` statements). This information is used for:
//!
//! 1. Link-safety validation: Detecting namespace collisions when multiple versions
//!    of the same package are installed.
//! 2. Metadata emission: Generating `namespaces.json` for CMake integration to
//!    validate link-unit safety at build time.
//! 3. Content identity: Computing content hashes for `identical_only` policy support.
//! 4. Namespace rewriting: Modifying package declarations to include version suffixes
//!    for safe multi-version coexistence.

use std::collections::HashMap;
use std::path::Path;

use miette::{miette, IntoDiagnostic};
use semver::Version;
use sha2::{Digest as Sha2Digest, Sha256};
use tokio::fs;
use walkdir::WalkDir;

use crate::package::PackageName;
use crate::resolver::DependencyGraph;

/// Information about a protobuf namespace declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamespaceInfo {
    /// The protobuf package name (e.g., "lib.base.v1")
    pub namespace: String,
    /// The file containing this declaration
    pub source_file: String,
    /// SHA-256 hash of the file content (hex-encoded)
    pub content_hash: String,
}

/// Information about a package's namespace declarations.
#[derive(Debug, Clone)]
pub struct PackageNamespaces {
    /// Package name
    pub name: PackageName,
    /// Package version
    pub version: Version,
    /// All namespaces declared by this package
    pub namespaces: Vec<NamespaceInfo>,
}

/// Result of scanning the vendor directory for namespaces.
#[derive(Debug, Clone, Default)]
pub struct NamespaceScanResult {
    /// Mapping from namespace to list of packages declaring it with content hash.
    /// Tuple is (package_name, version, source_file, content_hash).
    /// If a namespace maps to multiple packages, there's a potential conflict.
    pub namespace_to_packages: HashMap<String, Vec<(PackageName, Version, String, String)>>,
    /// All packages with their namespace declarations.
    pub packages: Vec<PackageNamespaces>,
}

impl NamespaceScanResult {
    /// Returns namespaces that are declared by more than one package@version.
    pub fn conflicts(&self) -> Vec<(&String, &[(PackageName, Version, String, String)])> {
        self.namespace_to_packages
            .iter()
            .filter(|(_, pkgs)| {
                // Check if there are different package@version combinations
                if pkgs.len() <= 1 {
                    return false;
                }
                let first = (&pkgs[0].0, &pkgs[0].1);
                pkgs.iter().any(|(n, v, _, _)| (n, v) != first)
            })
            .map(|(ns, pkgs)| (ns, pkgs.as_slice()))
            .collect()
    }

    /// Returns namespaces where different versions have non-identical content.
    /// This is used for `identical_only` policy validation.
    pub fn content_conflicts(&self) -> Vec<(&String, &[(PackageName, Version, String, String)])> {
        self.namespace_to_packages
            .iter()
            .filter(|(_, pkgs)| {
                if pkgs.len() <= 1 {
                    return false;
                }
                // Check if there are different content hashes
                let first_hash = &pkgs[0].3;
                pkgs.iter().any(|(_, _, _, hash)| hash != first_hash)
            })
            .map(|(ns, pkgs)| (ns, pkgs.as_slice()))
            .collect()
    }
}

/// Compute SHA-256 hash of content (hex-encoded).
pub fn content_hash(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Extracts the protobuf `package` statement from file contents.
///
/// This handles comments (// and /* */) and extracts the first valid package declaration.
pub fn extract_proto_package(contents: &str) -> Option<String> {
    // Strip comments while keeping a simple character stream.
    let mut out = String::with_capacity(contents.len());
    let mut chars = contents.chars().peekable();
    let mut in_block = false;

    while let Some(c) = chars.next() {
        if in_block {
            if c == '*' {
                if let Some('/') = chars.peek().copied() {
                    chars.next();
                    in_block = false;
                }
            }
            continue;
        }

        if c == '/' {
            match chars.peek().copied() {
                Some('/') => {
                    // line comment
                    while let Some(nc) = chars.next() {
                        if nc == '\n' {
                            out.push('\n');
                            break;
                        }
                    }
                    continue;
                }
                Some('*') => {
                    chars.next();
                    in_block = true;
                    continue;
                }
                _ => {}
            }
        }

        out.push(c);
    }

    // Find first `package ...;` statement.
    let bytes = out.as_bytes();
    let mut i = 0;
    while i + 7 <= bytes.len() {
        // look for "package" keyword
        if &bytes[i..i + 7] == b"package" {
            let prev_ok =
                i == 0 || !matches!(bytes[i - 1], b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_');
            let next_ok =
                i + 7 == bytes.len() || matches!(bytes[i + 7], b' ' | b'\t' | b'\r' | b'\n');
            if prev_ok && next_ok {
                let mut j = i + 7;
                while j < bytes.len() && matches!(bytes[j], b' ' | b'\t' | b'\r' | b'\n') {
                    j += 1;
                }
                let start = j;
                while j < bytes.len()
                    && matches!(
                        bytes[j],
                        b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_' | b'.'
                    )
                {
                    j += 1;
                }
                if start == j {
                    return None;
                }
                while j < bytes.len() && matches!(bytes[j], b' ' | b'\t' | b'\r' | b'\n') {
                    j += 1;
                }
                if j < bytes.len() && bytes[j] == b';' {
                    let pkg = String::from_utf8_lossy(&bytes[start..j]).trim().to_string();
                    return Some(pkg);
                }
            }
        }

        i += 1;
    }

    None
}

/// Scans a directory for `.proto` files and extracts namespace declarations.
pub async fn scan_directory(dir: &Path) -> miette::Result<Vec<NamespaceInfo>> {
    let mut namespaces = Vec::new();

    for entry in WalkDir::new(dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().map_or(false, |ext| ext == "proto"))
    {
        let path = entry.path();
        let contents = fs::read_to_string(path)
            .await
            .into_diagnostic()
            .map_err(|e| miette!("failed to read {}: {}", path.display(), e))?;

        if let Some(namespace) = extract_proto_package(&contents) {
            namespaces.push(NamespaceInfo {
                namespace,
                source_file: path
                    .strip_prefix(dir)
                    .unwrap_or(path)
                    .to_string_lossy()
                    .to_string(),
                content_hash: content_hash(&contents),
            });
        }
    }

    Ok(namespaces)
}

/// Scans all packages in a dependency graph and extracts namespace declarations.
///
/// # Arguments
/// * `graph` - The resolved dependency graph
/// * `vendor_root` - Path to the vendor directory (e.g., `proto/vendor/`)
///
/// # Returns
/// A `NamespaceScanResult` containing namespace-to-package mappings.
pub async fn scan_dependency_graph(
    graph: &DependencyGraph,
    vendor_root: &Path,
) -> miette::Result<NamespaceScanResult> {
    let mut result = NamespaceScanResult::default();

    for id in graph.keys() {
        let pkg_dir = vendor_root.join(id.vendor_dir_name(graph));

        if !pkg_dir.exists() {
            tracing::warn!(
                ":: skipping namespace scan for {} (directory does not exist: {})",
                id,
                pkg_dir.display()
            );
            continue;
        }

        let namespaces = scan_directory(&pkg_dir).await?;

        // Build the mappings
        for ns_info in &namespaces {
            result
                .namespace_to_packages
                .entry(ns_info.namespace.clone())
                .or_default()
                .push((
                    id.name().clone(),
                    id.version().clone(),
                    ns_info.source_file.clone(),
                    ns_info.content_hash.clone(),
                ));
        }

        result.packages.push(PackageNamespaces {
            name: id.name().clone(),
            version: id.version().clone(),
            namespaces,
        });
    }

    Ok(result)
}

/// Converts a semver Version to a namespace suffix.
///
/// The suffix format is `_v<major>_<minor>_<patch>` with any pre-release identifiers
/// appended after replacing non-alphanumeric characters with underscores.
///
/// # Examples
/// - `0.1.2` → `_v0_1_2`
/// - `0.1.2-SPINE-4384` → `_v0_1_2_SPINE_4384`
/// - `1.0.0-rc.1` → `_v1_0_0_rc_1`
pub fn version_to_suffix(version: &Version) -> String {
    let mut suffix = format!("_v{}_{}", version.major, version.minor);

    // Only add patch if non-zero or if there's pre-release
    if version.patch > 0 || !version.pre.is_empty() {
        suffix.push_str(&format!("_{}", version.patch));
    }

    // Add pre-release identifiers
    if !version.pre.is_empty() {
        let pre_str = version.pre.to_string();
        // Replace non-alphanumeric chars with underscores
        let sanitized: String = pre_str
            .chars()
            .map(|c| if c.is_alphanumeric() { c } else { '_' })
            .collect();
        suffix.push('_');
        suffix.push_str(&sanitized);
    }

    suffix
}

/// Rewrites the protobuf `package` declaration in file contents to include a version suffix.
///
/// This is used for multi-version support where multiple versions of the same package
/// need to coexist with unique namespaces.
///
/// # Arguments
/// * `contents` - The original proto file contents
/// * `version` - The package version to append as a suffix
///
/// # Returns
/// The rewritten contents with the versioned package declaration, or the original
/// contents if no package declaration was found.
///
/// # Example
/// ```ignore
/// let contents = "package gm.algo.base;";
/// let version = Version::parse("0.1.2").unwrap();
/// let rewritten = rewrite_proto_package(contents, &version);
/// assert!(rewritten.contains("package gm.algo.base._v0_1_2;"));
/// ```
pub fn rewrite_proto_package(contents: &str, version: &Version) -> String {
    let suffix = version_to_suffix(version);

    // Find the package declaration and rewrite it
    // We need to handle the full structure: `package <name>;`
    let mut result = String::with_capacity(contents.len() + suffix.len());
    let mut chars = contents.chars().peekable();
    let mut in_block_comment = false;
    let mut found_package = false;

    while let Some(c) = chars.next() {
        // Handle block comments
        if in_block_comment {
            result.push(c);
            if c == '*' {
                if let Some('/') = chars.peek().copied() {
                    result.push(chars.next().unwrap());
                    in_block_comment = false;
                }
            }
            continue;
        }

        // Handle comment starts
        if c == '/' {
            match chars.peek().copied() {
                Some('/') => {
                    // Line comment - copy until newline
                    result.push(c);
                    result.push(chars.next().unwrap());
                    while let Some(nc) = chars.next() {
                        result.push(nc);
                        if nc == '\n' {
                            break;
                        }
                    }
                    continue;
                }
                Some('*') => {
                    // Block comment
                    result.push(c);
                    result.push(chars.next().unwrap());
                    in_block_comment = true;
                    continue;
                }
                _ => {}
            }
        }

        // Look for 'package' keyword (only if we haven't found one yet)
        if !found_package && c == 'p' {
            // Check if this starts "package"
            let remaining: String = chars.clone().take(6).collect();
            if remaining == "ackage" {
                // Verify it's a keyword boundary (not part of another word)
                let peek_after: String = chars.clone().skip(6).take(1).collect();
                if peek_after.chars().next().map_or(true, |ch| {
                    ch == ' ' || ch == '\t' || ch == '\n' || ch == '\r'
                }) {
                    // Found the package keyword
                    result.push(c);
                    for _ in 0..6 {
                        result.push(chars.next().unwrap());
                    }

                    // Skip whitespace
                    while let Some(&ws) = chars.peek() {
                        if ws == ' ' || ws == '\t' || ws == '\n' || ws == '\r' {
                            result.push(chars.next().unwrap());
                        } else {
                            break;
                        }
                    }

                    // Collect the package name
                    let mut pkg_name = String::new();
                    while let Some(&ch) = chars.peek() {
                        if ch.is_alphanumeric() || ch == '_' || ch == '.' {
                            pkg_name.push(chars.next().unwrap());
                        } else {
                            break;
                        }
                    }

                    // Write the rewritten package name with version suffix
                    // Add a dot before the version suffix to create a new namespace segment
                    result.push_str(&pkg_name);
                    result.push('.');
                    result.push_str(&suffix);
                    found_package = true;
                    continue;
                }
            }
        }

        result.push(c);
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_proto_package_simple() {
        let contents = r#"
syntax = "proto3";
package lib.base.v1;

message Foo {}
"#;
        assert_eq!(
            extract_proto_package(contents),
            Some("lib.base.v1".to_string())
        );
    }

    #[test]
    fn test_extract_proto_package_with_comments() {
        let contents = r#"
// This is a comment
syntax = "proto3";
/* Block comment */
package api.service.v2;

// Another comment
message Bar {}
"#;
        assert_eq!(
            extract_proto_package(contents),
            Some("api.service.v2".to_string())
        );
    }

    #[test]
    fn test_extract_proto_package_no_package() {
        let contents = r#"
syntax = "proto3";

message Foo {}
"#;
        assert_eq!(extract_proto_package(contents), None);
    }

    #[test]
    fn test_extract_proto_package_comment_package() {
        let contents = r#"
syntax = "proto3";
// package fake.package;
package real.package;
"#;
        assert_eq!(
            extract_proto_package(contents),
            Some("real.package".to_string())
        );
    }

    #[test]
    fn test_extract_proto_package_block_comment_package() {
        let contents = r#"
syntax = "proto3";
/* package fake.package; */
package real.package.v1;
"#;
        assert_eq!(
            extract_proto_package(contents),
            Some("real.package.v1".to_string())
        );
    }

    #[test]
    fn test_version_to_suffix_simple() {
        use semver::Version;
        assert_eq!(version_to_suffix(&Version::new(0, 1, 2)), "_v0_1_2");
        assert_eq!(version_to_suffix(&Version::new(1, 0, 0)), "_v1_0");
        assert_eq!(version_to_suffix(&Version::new(2, 3, 4)), "_v2_3_4");
    }

    #[test]
    fn test_version_to_suffix_with_prerelease() {
        use semver::Version;
        let v = Version::parse("0.1.2-SPINE-4384").unwrap();
        assert_eq!(version_to_suffix(&v), "_v0_1_2_SPINE_4384");

        let v = Version::parse("1.0.0-rc.1").unwrap();
        assert_eq!(version_to_suffix(&v), "_v1_0_0_rc_1");

        let v = Version::parse("0.1.0-alpha").unwrap();
        assert_eq!(version_to_suffix(&v), "_v0_1_0_alpha");
    }

    #[test]
    fn test_rewrite_proto_package_simple() {
        use semver::Version;
        let contents = r#"
syntax = "proto3";
package gm.algo.base;

message Foo {}
"#;
        let version = Version::new(0, 1, 2);
        let rewritten = rewrite_proto_package(contents, &version);
        assert!(rewritten.contains("package gm.algo.base._v0_1_2;"));
        assert!(!rewritten.contains("package gm.algo.base;"));
    }

    #[test]
    fn test_rewrite_proto_package_with_comments() {
        use semver::Version;
        let contents = r#"
// This is a comment
syntax = "proto3";
/* Block comment */
package api.service.v2;

// Another comment
message Bar {}
"#;
        let version = Version::new(1, 0, 0);
        let rewritten = rewrite_proto_package(contents, &version);
        assert!(rewritten.contains("package api.service.v2._v1_0;"));
    }

    #[test]
    fn test_rewrite_proto_package_preserves_structure() {
        use semver::Version;
        let contents = r#"syntax = "proto3";

// Package declaration
package my.package;

message Test {
  string name = 1;
}
"#;
        let version = Version::parse("2.0.0-beta").unwrap();
        let rewritten = rewrite_proto_package(contents, &version);

        // Check that rewriting happened correctly
        assert!(rewritten.contains("package my.package._v2_0_0_beta;"));
        // Check that comments are preserved
        assert!(rewritten.contains("// Package declaration"));
        // Check that message is preserved
        assert!(rewritten.contains("message Test"));
    }

    #[test]
    fn test_rewrite_proto_package_no_package() {
        use semver::Version;
        let contents = r#"
syntax = "proto3";

message Foo {}
"#;
        let version = Version::new(0, 1, 2);
        let rewritten = rewrite_proto_package(contents, &version);
        // No package declaration, so content should be mostly unchanged
        assert!(!rewritten.contains("_v0_1_2"));
    }
}
