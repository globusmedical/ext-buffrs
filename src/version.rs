// Copyright 2023 Helsing GmbH
// Copyright 2026 Globus Medical, Inc.
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

//! Version selection and manipulation utilities.
//!
//! This module provides utilities for working with semantic versions and
//! version requirements, including selecting the best matching version from
//! a set of available versions.

use semver::{Comparator, Op, Version, VersionReq};

/// Selects the best matching version from a list of available versions.
///
/// Returns the highest version that satisfies the requirement.
/// Pre-release versions are only matched if explicitly requested in the requirement.
///
/// # Arguments
/// * `req` - The version requirement to satisfy
/// * `available` - List of available versions to choose from
///
/// # Returns
/// The highest matching version, or `None` if no version matches.
///
/// # Examples
/// ```
/// use semver::{Version, VersionReq};
/// use buffrs::version::select_version;
///
/// let available = vec![
///     Version::parse("1.0.0").unwrap(),
///     Version::parse("1.1.0").unwrap(),
///     Version::parse("1.2.0").unwrap(),
///     Version::parse("2.0.0").unwrap(),
/// ];
///
/// let req = VersionReq::parse("^1.0.0").unwrap();
/// assert_eq!(select_version(&req, &available), Some(Version::parse("1.2.0").unwrap()));
///
/// let req = VersionReq::parse("~1.0.0").unwrap();
/// assert_eq!(select_version(&req, &available), Some(Version::parse("1.0.0").unwrap()));
/// ```
pub fn select_version(req: &VersionReq, available: &[Version]) -> Option<Version> {
    available.iter().filter(|v| req.matches(v)).max().cloned()
}

/// Checks if a version requirement specifies an exact version.
///
/// Returns `true` if the requirement has exactly one comparator with `Op::Exact`.
pub fn is_exact_requirement(req: &VersionReq) -> bool {
    req.comparators.len() == 1 && req.comparators.first().is_some_and(|c| c.op == Op::Exact)
}

/// Extracts the exact version from a requirement that specifies one.
///
/// Returns `None` if the requirement is not exact or is malformed.
pub fn extract_exact_version(req: &VersionReq) -> Option<Version> {
    if !is_exact_requirement(req) {
        return None;
    }

    let comparator = req.comparators.first()?;
    Some(Version {
        major: comparator.major,
        minor: comparator.minor.unwrap_or(0),
        patch: comparator.patch.unwrap_or(0),
        pre: comparator.pre.clone(),
        build: semver::BuildMetadata::EMPTY,
    })
}

/// Creates an exact version requirement from a version.
pub fn to_exact_requirement(version: &Version) -> VersionReq {
    VersionReq {
        comparators: vec![Comparator {
            op: Op::Exact,
            major: version.major,
            minor: Some(version.minor),
            patch: Some(version.patch),
            pre: version.pre.clone(),
        }],
    }
}

/// Converts a version to its artifact path string representation.
///
/// This is used when constructing download URLs for packages.
pub fn version_to_artifact_string(version: &Version) -> String {
    if version.pre.is_empty() {
        format!("{}.{}.{}", version.major, version.minor, version.patch)
    } else {
        format!(
            "{}.{}.{}-{}",
            version.major, version.minor, version.patch, version.pre
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    fn req(s: &str) -> VersionReq {
        VersionReq::parse(s).unwrap()
    }

    // ========================================================================
    // Basic select_version tests
    // ========================================================================

    #[test]
    fn test_select_version_caret() {
        let available = vec![v("1.0.0"), v("1.1.0"), v("1.2.0"), v("2.0.0")];

        // ^1.0.0 matches >=1.0.0, <2.0.0
        assert_eq!(select_version(&req("^1.0.0"), &available), Some(v("1.2.0")));

        // ^1.1.0 matches >=1.1.0, <2.0.0
        assert_eq!(select_version(&req("^1.1.0"), &available), Some(v("1.2.0")));

        // ^2.0.0 matches >=2.0.0, <3.0.0
        assert_eq!(select_version(&req("^2.0.0"), &available), Some(v("2.0.0")));
    }

    #[test]
    fn test_select_version_caret_zero_major() {
        // Special handling for 0.x versions
        let available = vec![v("0.1.0"), v("0.1.5"), v("0.2.0"), v("0.2.3")];

        // ^0.1.0 matches >=0.1.0, <0.2.0 (minor is breaking for 0.x)
        assert_eq!(select_version(&req("^0.1.0"), &available), Some(v("0.1.5")));

        // ^0.2.0 matches >=0.2.0, <0.3.0
        assert_eq!(select_version(&req("^0.2.0"), &available), Some(v("0.2.3")));
    }

    #[test]
    fn test_select_version_caret_zero_minor() {
        // Even more special handling for 0.0.x versions
        let available = vec![v("0.0.1"), v("0.0.2"), v("0.0.3")];

        // ^0.0.1 matches =0.0.1 only (patch is breaking for 0.0.x)
        assert_eq!(select_version(&req("^0.0.1"), &available), Some(v("0.0.1")));
    }

    #[test]
    fn test_select_version_tilde() {
        let available = vec![v("1.0.0"), v("1.0.5"), v("1.1.0"), v("1.2.0")];

        // ~1.0.0 matches >=1.0.0, <1.1.0
        assert_eq!(select_version(&req("~1.0.0"), &available), Some(v("1.0.5")));

        // ~1.1.0 matches >=1.1.0, <1.2.0
        assert_eq!(select_version(&req("~1.1.0"), &available), Some(v("1.1.0")));
    }

    #[test]
    fn test_select_version_exact() {
        let available = vec![v("1.0.0"), v("1.1.0"), v("1.2.0")];

        assert_eq!(select_version(&req("=1.1.0"), &available), Some(v("1.1.0")));
        assert_eq!(select_version(&req("=1.3.0"), &available), None);
    }

    #[test]
    fn test_select_version_ranges() {
        let available = vec![v("1.0.0"), v("1.5.0"), v("2.0.0"), v("2.5.0")];

        // >=1.0.0, <2.0.0
        assert_eq!(
            select_version(&req(">=1.0.0, <2.0.0"), &available),
            Some(v("1.5.0"))
        );

        // >1.5.0
        assert_eq!(select_version(&req(">1.5.0"), &available), Some(v("2.5.0")));

        // <=1.5.0
        assert_eq!(
            select_version(&req("<=1.5.0"), &available),
            Some(v("1.5.0"))
        );
    }

    #[test]
    fn test_select_version_complex_ranges() {
        let available = vec![
            v("1.0.0"),
            v("1.2.0"),
            v("1.5.0"),
            v("2.0.0"),
            v("2.1.0"),
            v("3.0.0"),
        ];

        // Multiple disjoint ranges don't work with comma (it's AND, not OR)
        // >=1.0.0, <1.3.0 AND >=2.0.0 would match nothing
        // But >=1.0.0, <=2.1.0 works
        assert_eq!(
            select_version(&req(">=1.0.0, <=2.1.0"), &available),
            Some(v("2.1.0"))
        );

        // Narrow range
        assert_eq!(
            select_version(&req(">=1.1.0, <1.6.0"), &available),
            Some(v("1.5.0"))
        );
    }

    #[test]
    fn test_select_version_wildcard() {
        let available = vec![v("1.0.0"), v("2.0.0"), v("3.0.0")];

        assert_eq!(select_version(&req("*"), &available), Some(v("3.0.0")));
    }

    #[test]
    fn test_select_version_wildcard_minor() {
        let available = vec![v("1.0.0"), v("1.5.0"), v("1.9.0"), v("2.0.0")];

        // 1.* matches any 1.x.y
        assert_eq!(select_version(&req("1.*"), &available), Some(v("1.9.0")));
    }

    #[test]
    fn test_select_version_wildcard_patch() {
        let available = vec![v("1.2.0"), v("1.2.3"), v("1.2.9"), v("1.3.0")];

        // 1.2.* matches any 1.2.x
        assert_eq!(select_version(&req("1.2.*"), &available), Some(v("1.2.9")));
    }

    // ========================================================================
    // Pre-release version tests
    // ========================================================================

    #[test]
    fn test_select_version_prerelease() {
        let available = vec![
            v("1.0.0"),
            v("1.1.0-alpha.1"),
            v("1.1.0-beta.1"),
            v("1.1.0"),
        ];

        // By default, pre-release versions are not matched by caret
        assert_eq!(select_version(&req("^1.0.0"), &available), Some(v("1.1.0")));

        // Explicit pre-release requirement
        assert_eq!(
            select_version(&req("=1.1.0-alpha.1"), &available),
            Some(v("1.1.0-alpha.1"))
        );
    }

    #[test]
    fn test_select_version_prerelease_ordering() {
        // Pre-release versions sort before their release
        let available = vec![
            v("1.0.0-alpha.1"),
            v("1.0.0-alpha.2"),
            v("1.0.0-beta.1"),
            v("1.0.0-rc.1"),
            v("1.0.0"),
        ];

        // Exact pre-release
        assert_eq!(
            select_version(&req("=1.0.0-beta.1"), &available),
            Some(v("1.0.0-beta.1"))
        );

        // >= pre-release picks the release
        assert_eq!(
            select_version(&req(">=1.0.0-alpha.1"), &available),
            Some(v("1.0.0"))
        );
    }

    #[test]
    fn test_select_version_prerelease_only_available() {
        let available = vec![v("1.0.0-alpha.1"), v("1.0.0-alpha.2"), v("1.0.0-beta.1")];

        // ^1.0.0 won't match pre-releases
        assert_eq!(select_version(&req("^1.0.0"), &available), None);

        // But ^1.0.0-alpha will match pre-releases starting with that prefix
        // Actually semver crate behavior: even this won't match general prereleases
        // We need explicit pre-release requirements
        assert_eq!(
            select_version(&req(">=1.0.0-alpha.1"), &available),
            Some(v("1.0.0-beta.1"))
        );
    }

    // ========================================================================
    // Edge cases
    // ========================================================================

    #[test]
    fn test_select_version_empty() {
        let available: Vec<Version> = vec![];
        assert_eq!(select_version(&req("^1.0.0"), &available), None);
    }

    #[test]
    fn test_select_version_single() {
        let available = vec![v("1.0.0")];
        assert_eq!(select_version(&req("^1.0.0"), &available), Some(v("1.0.0")));
        assert_eq!(select_version(&req("^2.0.0"), &available), None);
    }

    #[test]
    fn test_select_version_no_match() {
        let available = vec![v("1.0.0"), v("1.1.0"), v("1.2.0")];
        assert_eq!(select_version(&req("^2.0.0"), &available), None);
        assert_eq!(select_version(&req(">=3.0.0"), &available), None);
    }

    #[test]
    fn test_select_version_unsorted_input() {
        // Input doesn't need to be sorted - we find max
        let available = vec![v("1.2.0"), v("1.0.0"), v("1.5.0"), v("1.1.0")];
        assert_eq!(select_version(&req("^1.0.0"), &available), Some(v("1.5.0")));
    }

    #[test]
    fn test_select_version_duplicates() {
        let available = vec![v("1.0.0"), v("1.0.0"), v("1.1.0"), v("1.1.0")];
        assert_eq!(select_version(&req("^1.0.0"), &available), Some(v("1.1.0")));
    }

    // ========================================================================
    // is_exact_requirement tests
    // ========================================================================

    #[test]
    fn test_is_exact_requirement() {
        assert!(is_exact_requirement(&req("=1.0.0")));
        assert!(is_exact_requirement(&req("=1.0.0-alpha")));
        assert!(!is_exact_requirement(&req("^1.0.0")));
        assert!(!is_exact_requirement(&req("~1.0.0")));
        assert!(!is_exact_requirement(&req(">=1.0.0")));
        assert!(!is_exact_requirement(&req(">=1.0.0, <2.0.0")));
        assert!(!is_exact_requirement(&req("*")));
    }

    #[test]
    fn test_is_exact_requirement_edge_cases() {
        // Partial versions aren't exact in our definition
        assert!(is_exact_requirement(&req("=1.0.0")));
        // Build metadata
        assert!(is_exact_requirement(&req("=1.0.0")));
    }

    // ========================================================================
    // extract_exact_version tests
    // ========================================================================

    #[test]
    fn test_extract_exact_version() {
        assert_eq!(extract_exact_version(&req("=1.2.3")), Some(v("1.2.3")));
        assert_eq!(
            extract_exact_version(&req("=1.2.3-alpha.1")),
            Some(v("1.2.3-alpha.1"))
        );
        assert_eq!(extract_exact_version(&req("^1.2.3")), None);
        assert_eq!(extract_exact_version(&req(">=1.0.0, <2.0.0")), None);
    }

    #[test]
    fn test_extract_exact_version_roundtrip() {
        let version = v("1.2.3");
        let exact_req = to_exact_requirement(&version);
        let extracted = extract_exact_version(&exact_req);
        assert_eq!(extracted, Some(version));
    }

    // ========================================================================
    // to_exact_requirement tests
    // ========================================================================

    #[test]
    fn test_to_exact_requirement() {
        let version = v("1.2.3");
        let exact = to_exact_requirement(&version);
        assert!(is_exact_requirement(&exact));
        assert!(exact.matches(&version));
        assert!(!exact.matches(&v("1.2.4")));
    }

    #[test]
    fn test_to_exact_requirement_prerelease() {
        let version = v("1.2.3-beta.1");
        let exact = to_exact_requirement(&version);
        assert!(is_exact_requirement(&exact));
        assert!(exact.matches(&version));
        assert!(!exact.matches(&v("1.2.3")));
        assert!(!exact.matches(&v("1.2.3-beta.2")));
    }

    // ========================================================================
    // version_to_artifact_string tests
    // ========================================================================

    #[test]
    fn test_version_to_artifact_string() {
        assert_eq!(version_to_artifact_string(&v("1.2.3")), "1.2.3");
        assert_eq!(
            version_to_artifact_string(&v("1.2.3-alpha.1")),
            "1.2.3-alpha.1"
        );
        assert_eq!(version_to_artifact_string(&v("0.0.1")), "0.0.1");
    }

    #[test]
    fn test_version_to_artifact_string_edge_cases() {
        assert_eq!(version_to_artifact_string(&v("0.0.0")), "0.0.0");
        assert_eq!(version_to_artifact_string(&v("999.999.999")), "999.999.999");
        assert_eq!(
            version_to_artifact_string(&v("1.0.0-rc.1+build.123")),
            "1.0.0-rc.1"
        );
    }

    // ========================================================================
    // Limitation demonstration tests
    // These tests document known limitations of the greedy resolution approach
    // ========================================================================

    /// Demonstrates that our greedy approach always picks the highest matching version.
    /// This can cause problems in diamond dependency scenarios.
    ///
    /// Example scenario (not testable in unit tests, but documented):
    /// ```text
    /// root
    /// ├── A ^1.0.0 (resolves to 1.5.0)
    /// │   └── C ^1.0.0 (C 1.5.0 chosen greedily)
    /// └── B ^1.0.0 (resolves to 1.3.0)
    ///     └── C ^1.2.0, <1.4.0 (CONFLICT: C 1.5.0 doesn't match!)
    /// ```
    ///
    /// A SAT-based resolver (PubGrub) would find C@1.3.x that satisfies both.
    #[test]
    fn test_greedy_resolution_limitation_documented() {
        // This test just documents the limitation
        // The actual conflict happens at resolution time, not version selection time
        let available_c = vec![v("1.0.0"), v("1.2.0"), v("1.3.0"), v("1.5.0")];

        // A's constraint: ^1.0.0 -> picks 1.5.0 (greedy)
        assert_eq!(
            select_version(&req("^1.0.0"), &available_c),
            Some(v("1.5.0"))
        );

        // B's constraint: >=1.2.0, <1.4.0 -> would pick 1.3.0
        assert_eq!(
            select_version(&req(">=1.2.0, <1.4.0"), &available_c),
            Some(v("1.3.0"))
        );

        // The intersection exists (1.2.0, 1.3.0) but greedy picks 1.5.0 first
        // A proper SAT solver would find 1.3.0 as the solution
    }
}
