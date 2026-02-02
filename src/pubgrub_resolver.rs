//! SAT-based dependency resolution using PubGrub algorithm.
//!
//! This module provides a conflict-aware dependency resolver that can find
//! consistent version assignments even in complex diamond dependency scenarios.
//!
//! # Advantages over Greedy Resolution
//!
//! - **Backtracking**: If a version choice leads to a conflict, PubGrub backtracks
//!   and tries alternative versions.
//! - **Diamond dependencies**: Handles cases where multiple packages depend on
//!   a shared transitive dependency with different version constraints.
//! - **Clear error messages**: When no solution exists, explains why with
//!   human-readable incompatibility chains.
//!
//! # Example
//!
//! ```text
//! root
//! ├── A ^1.0.0  →  A@1.5.0
//! │   └── C ^1.0.0  →  but we need C <1.5.0 from B!
//! └── B ^1.0.0  →  B@1.3.0
//!     └── C >=1.2.0, <1.5.0
//!
//! PubGrub solution: A@1.5.0, B@1.3.0, C@1.4.0 (satisfies both constraints)
//! ```

use pubgrub::{
    DefaultStringReporter, Dependencies, DependencyConstraints, DependencyProvider,
    PackageResolutionStatistics, PubGrubError, Ranges, Reporter,
};
use semver::{Comparator, Op, Version, VersionReq};
use std::collections::HashMap;
use std::convert::Infallible;
use std::fmt;
use std::sync::{Arc, RwLock};

use crate::package::PackageName;

/// A package identifier for PubGrub resolution.
///
/// Wraps `PackageName` with a special "root" variant for the resolution root.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PubGrubPackage {
    /// The root package being resolved
    Root,
    /// A regular package dependency
    Package(PackageName),
}

impl fmt::Display for PubGrubPackage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Root => write!(f, "root"),
            Self::Package(name) => write!(f, "{}", name),
        }
    }
}

impl From<PackageName> for PubGrubPackage {
    fn from(name: PackageName) -> Self {
        Self::Package(name)
    }
}

/// Version set type for PubGrub using semver::Version.
pub type SemverRanges = Ranges<Version>;

/// Converts a semver::VersionReq to PubGrub Ranges.
///
/// This handles all semver operators: `=`, `^`, `~`, `>`, `>=`, `<`, `<=`, `*`
pub fn version_req_to_ranges(req: &VersionReq) -> SemverRanges {
    if req.comparators.is_empty() {
        // Empty requirement matches everything (equivalent to `*`)
        return Ranges::full();
    }

    // Each comparator in a VersionReq is combined with AND
    let mut result = Ranges::full();

    for comp in &req.comparators {
        let comp_range = comparator_to_ranges(comp);
        result = result.intersection(&comp_range);
    }

    result
}

/// Converts a single semver Comparator to PubGrub Ranges.
fn comparator_to_ranges(comp: &Comparator) -> SemverRanges {
    let major = comp.major;
    let minor = comp.minor;
    let patch = comp.patch;

    match comp.op {
        Op::Exact => {
            // =X.Y.Z matches exactly that version
            let version = make_version(major, minor, patch, &comp.pre);
            Ranges::singleton(version)
        }
        Op::Greater => {
            // >X.Y.Z matches versions greater than X.Y.Z
            let version = make_version(major, minor, patch, &comp.pre);
            // Ranges::higher_than is exclusive of the bound, so we use strictly_lower_than's complement
            Ranges::strictly_lower_than(version.clone())
                .complement()
                .intersection(&Ranges::singleton(version).complement())
        }
        Op::GreaterEq => {
            // >=X.Y.Z matches versions >= X.Y.Z
            let version = make_version(major, minor, patch, &comp.pre);
            Ranges::strictly_lower_than(version).complement()
        }
        Op::Less => {
            // <X.Y.Z matches versions < X.Y.Z
            let version = make_version(major, minor, patch, &comp.pre);
            Ranges::strictly_lower_than(version)
        }
        Op::LessEq => {
            // <=X.Y.Z matches versions <= X.Y.Z
            let version = make_version(major, minor, patch, &comp.pre);
            Ranges::strictly_lower_than(version.clone()).union(&Ranges::singleton(version))
        }
        Op::Tilde => {
            // ~X.Y.Z matches >=X.Y.Z, <X.(Y+1).0
            let min = make_version(major, minor, patch, &comp.pre);
            let max = Version::new(major, minor.unwrap_or(0) + 1, 0);
            range_between(&min, &max)
        }
        Op::Caret => {
            // ^X.Y.Z - compatible updates
            caret_range(major, minor, patch, &comp.pre)
        }
        Op::Wildcard => {
            // X.* or X.Y.* - matches any version with that prefix
            wildcard_range(major, minor)
        }
        _ => {
            // Unknown operator - be permissive
            Ranges::full()
        }
    }
}

/// Creates a version from components.
fn make_version(
    major: u64,
    minor: Option<u64>,
    patch: Option<u64>,
    pre: &semver::Prerelease,
) -> Version {
    let mut v = Version::new(major, minor.unwrap_or(0), patch.unwrap_or(0));
    v.pre = pre.clone();
    v
}

/// Helper to create a range [min, max).
fn range_between(min: &Version, max: &Version) -> SemverRanges {
    Ranges::between(min.clone(), max.clone())
}

/// Implements caret (^) version ranges.
///
/// - `^1.2.3` := `>=1.2.3, <2.0.0` (major is non-zero)
/// - `^0.2.3` := `>=0.2.3, <0.3.0` (major is zero, minor is non-zero)
/// - `^0.0.3` := `>=0.0.3, <0.0.4` (major and minor are zero)
fn caret_range(
    major: u64,
    minor: Option<u64>,
    patch: Option<u64>,
    pre: &semver::Prerelease,
) -> SemverRanges {
    let min = make_version(major, minor, patch, pre);

    let max = if major > 0 {
        Version::new(major + 1, 0, 0)
    } else if let Some(m) = minor {
        if m > 0 {
            Version::new(0, m + 1, 0)
        } else if let Some(p) = patch {
            Version::new(0, 0, p + 1)
        } else {
            Version::new(0, 1, 0)
        }
    } else {
        Version::new(1, 0, 0)
    };

    range_between(&min, &max)
}

/// Implements wildcard (*) version ranges.
fn wildcard_range(major: u64, minor: Option<u64>) -> SemverRanges {
    match minor {
        Some(m) => {
            // X.Y.* matches [X.Y.0, X.(Y+1).0)
            let min = Version::new(major, m, 0);
            let max = Version::new(major, m + 1, 0);
            range_between(&min, &max)
        }
        None => {
            // X.* matches [X.0.0, (X+1).0.0)
            let min = Version::new(major, 0, 0);
            let max = Version::new(major + 1, 0, 0);
            range_between(&min, &max)
        }
    }
}

/// A dependency with its version requirement.
#[derive(Debug, Clone)]
pub struct PackageDependency {
    /// The package name
    pub package: PackageName,
    /// The version requirement for this dependency
    pub version_req: VersionReq,
}

/// Cached package information for the resolver.
#[derive(Debug, Clone)]
pub struct PackageInfo {
    /// Available versions of this package
    pub versions: Vec<Version>,
    /// Dependencies for each version: version -> list of dependencies
    pub dependencies: HashMap<Version, Vec<PackageDependency>>,
}

/// A dependency provider that uses pre-fetched package data.
///
/// This provider is populated by querying the registry for all packages
/// before running the PubGrub resolution.
pub struct BuffrsDependencyProvider {
    /// Package data cache
    packages: Arc<RwLock<HashMap<PackageName, PackageInfo>>>,
    /// Root package dependencies
    root_deps: Vec<PackageDependency>,
    /// Preferred versions for reproducible installs (e.g., from Proto.lock).
    ///
    /// If a preferred version satisfies all constraints, it will be chosen even
    /// if a higher version exists.
    preferred_versions: Arc<RwLock<HashMap<PackageName, Version>>>,
}

impl BuffrsDependencyProvider {
    /// Creates a new dependency provider with the given root dependencies.
    pub fn new(root_deps: Vec<PackageDependency>) -> Self {
        Self {
            packages: Arc::new(RwLock::new(HashMap::new())),
            root_deps,
            preferred_versions: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Registers package information (versions and dependencies).
    pub fn add_package(&self, name: PackageName, info: PackageInfo) {
        let mut packages = self.packages.write().unwrap();
        packages.insert(name, info);
    }

    /// Sets a preferred version for a package.
    ///
    /// This is typically used to prefer versions pinned in `Proto.lock`.
    pub fn set_preferred_version(&self, name: PackageName, version: Version) {
        let mut preferred = self.preferred_versions.write().unwrap();
        preferred.insert(name, version);
    }

    /// Gets the available versions for a package.
    pub fn get_versions(&self, name: &PackageName) -> Option<Vec<Version>> {
        let packages = self.packages.read().unwrap();
        packages.get(name).map(|info| info.versions.clone())
    }
}

impl DependencyProvider for BuffrsDependencyProvider {
    type P = PubGrubPackage;
    type V = Version;
    type VS = SemverRanges;
    type M = String;
    type Err = Infallible;
    type Priority = u32;

    fn choose_version(
        &self,
        package: &Self::P,
        range: &Self::VS,
    ) -> Result<Option<Self::V>, Self::Err> {
        match package {
            PubGrubPackage::Root => {
                // Root always has version 0.0.0
                Ok(Some(Version::new(0, 0, 0)))
            }
            PubGrubPackage::Package(name) => {
                let packages = self.packages.read().unwrap();
                if let Some(info) = packages.get(name) {
                    // Prefer lockfile-pinned versions for reproducibility when compatible.
                    // If dependency metadata is missing for a version, `get_dependencies` will
                    // treat it as having no dependencies (current behavior). When metadata is
                    // available for at least some versions, prefer versions we have metadata for.
                    if let Some(preferred) = self.preferred_versions.read().unwrap().get(name) {
                        let has_metadata = info.dependencies.is_empty()
                            || info.dependencies.contains_key(preferred);
                        if range.contains(preferred) && has_metadata {
                            return Ok(Some(preferred.clone()));
                        }
                    }

                    // Fall back to the highest version that matches the range.
                    // When dependency metadata exists, restrict to versions we have metadata for.
                    let matching = info
                        .versions
                        .iter()
                        .filter(|v| {
                            range.contains(v)
                                && (info.dependencies.is_empty()
                                    || info.dependencies.contains_key(*v))
                        })
                        .max()
                        .cloned();

                    Ok(matching)
                } else {
                    Ok(None)
                }
            }
        }
    }

    fn prioritize(
        &self,
        package: &Self::P,
        range: &Self::VS,
        _conflicts: &PackageResolutionStatistics,
    ) -> Self::Priority {
        // Prioritize packages with fewer matching versions
        // This helps PubGrub fail faster on constrained packages
        match package {
            PubGrubPackage::Root => u32::MAX, // Always resolve root first
            PubGrubPackage::Package(name) => {
                let packages = self.packages.read().unwrap();
                if let Some(info) = packages.get(name) {
                    // Prioritize based on viable versions (those with dependency metadata).
                    let count = info
                        .versions
                        .iter()
                        .filter(|v| {
                            range.contains(v)
                                && (info.dependencies.is_empty()
                                    || info.dependencies.contains_key(*v))
                        })
                        .count();
                    // Fewer versions = higher priority (prioritize most constrained)
                    u32::MAX - count as u32
                } else {
                    0 // Unknown package - low priority
                }
            }
        }
    }

    fn get_dependencies(
        &self,
        package: &Self::P,
        version: &Self::V,
    ) -> Result<Dependencies<Self::P, Self::VS, Self::M>, Self::Err> {
        match package {
            PubGrubPackage::Root => {
                // Root package dependencies
                let mut deps = DependencyConstraints::default();
                for dep in &self.root_deps {
                    let range = version_req_to_ranges(&dep.version_req);
                    deps.insert(PubGrubPackage::Package(dep.package.clone()), range);
                }
                Ok(Dependencies::Available(deps))
            }
            PubGrubPackage::Package(name) => {
                let packages = self.packages.read().unwrap();
                if let Some(info) = packages.get(name) {
                    if let Some(version_deps) = info.dependencies.get(version) {
                        let mut deps = DependencyConstraints::default();
                        for dep in version_deps {
                            let range = version_req_to_ranges(&dep.version_req);
                            deps.insert(PubGrubPackage::Package(dep.package.clone()), range);
                        }
                        Ok(Dependencies::Available(deps))
                    } else {
                        // Version exists but no dependency info - assume no deps
                        Ok(Dependencies::Available(DependencyConstraints::default()))
                    }
                } else {
                    Ok(Dependencies::Unavailable(format!(
                        "package {} not found in registry",
                        name
                    )))
                }
            }
        }
    }
}

/// Result of PubGrub resolution.
pub type ResolutionResult = Result<HashMap<PackageName, Version>, ResolutionError>;

/// Error returned when resolution fails.
#[derive(Debug, Clone)]
pub struct ResolutionError {
    /// Human-readable explanation of why resolution failed
    pub message: String,
}

impl fmt::Display for ResolutionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for ResolutionError {}

/// Runs PubGrub resolution with the given dependency provider.
///
/// Returns a map of package names to resolved versions.
pub fn resolve(provider: &BuffrsDependencyProvider) -> ResolutionResult {
    let root = PubGrubPackage::Root;
    let root_version = Version::new(0, 0, 0);

    match pubgrub::resolve(provider, root, root_version) {
        Ok(solution) => {
            let mut result = HashMap::new();
            for (package, version) in solution {
                if let PubGrubPackage::Package(name) = package {
                    result.insert(name, version);
                }
            }
            Ok(result)
        }
        Err(PubGrubError::NoSolution(mut derivation_tree)) => {
            derivation_tree.collapse_no_versions();
            let report = DefaultStringReporter::report(&derivation_tree);
            Err(ResolutionError { message: report })
        }
        Err(err) => Err(ResolutionError {
            message: format!("{:?}", err),
        }),
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

    fn pkg(s: &str) -> PackageName {
        PackageName::unchecked(s)
    }

    // ========================================================================
    // Version requirement to ranges conversion tests
    // ========================================================================

    #[test]
    fn test_version_req_to_ranges_exact() {
        let ranges = version_req_to_ranges(&req("=1.2.3"));
        assert!(ranges.contains(&v("1.2.3")));
        assert!(!ranges.contains(&v("1.2.4")));
        assert!(!ranges.contains(&v("1.2.2")));
    }

    #[test]
    fn test_version_req_to_ranges_caret() {
        let ranges = version_req_to_ranges(&req("^1.2.3"));
        assert!(ranges.contains(&v("1.2.3")));
        assert!(ranges.contains(&v("1.9.9")));
        assert!(!ranges.contains(&v("2.0.0")));
        assert!(!ranges.contains(&v("1.2.2")));
    }

    #[test]
    fn test_version_req_to_ranges_caret_zero() {
        // ^0.2.3 = >=0.2.3, <0.3.0
        let ranges = version_req_to_ranges(&req("^0.2.3"));
        assert!(ranges.contains(&v("0.2.3")));
        assert!(ranges.contains(&v("0.2.9")));
        assert!(!ranges.contains(&v("0.3.0")));
        assert!(!ranges.contains(&v("0.2.2")));
    }

    #[test]
    fn test_version_req_to_ranges_tilde() {
        // ~1.2.3 = >=1.2.3, <1.3.0
        let ranges = version_req_to_ranges(&req("~1.2.3"));
        assert!(ranges.contains(&v("1.2.3")));
        assert!(ranges.contains(&v("1.2.9")));
        assert!(!ranges.contains(&v("1.3.0")));
        assert!(!ranges.contains(&v("1.2.2")));
    }

    #[test]
    fn test_version_req_to_ranges_greater() {
        let ranges = version_req_to_ranges(&req(">1.0.0"));
        assert!(!ranges.contains(&v("1.0.0")));
        assert!(ranges.contains(&v("1.0.1")));
        assert!(ranges.contains(&v("2.0.0")));
    }

    #[test]
    fn test_version_req_to_ranges_greater_eq() {
        let ranges = version_req_to_ranges(&req(">=1.0.0"));
        assert!(ranges.contains(&v("1.0.0")));
        assert!(ranges.contains(&v("1.0.1")));
        assert!(!ranges.contains(&v("0.9.9")));
    }

    #[test]
    fn test_version_req_to_ranges_less() {
        let ranges = version_req_to_ranges(&req("<2.0.0"));
        assert!(ranges.contains(&v("1.9.9")));
        assert!(!ranges.contains(&v("2.0.0")));
    }

    #[test]
    fn test_version_req_to_ranges_less_eq() {
        let ranges = version_req_to_ranges(&req("<=2.0.0"));
        assert!(ranges.contains(&v("2.0.0")));
        assert!(ranges.contains(&v("1.9.9")));
        assert!(!ranges.contains(&v("2.0.1")));
    }

    #[test]
    fn test_version_req_to_ranges_combined() {
        // >=1.0.0, <2.0.0
        let ranges = version_req_to_ranges(&req(">=1.0.0, <2.0.0"));
        assert!(ranges.contains(&v("1.0.0")));
        assert!(ranges.contains(&v("1.5.0")));
        assert!(!ranges.contains(&v("2.0.0")));
        assert!(!ranges.contains(&v("0.9.0")));
    }

    #[test]
    fn test_version_req_to_ranges_wildcard() {
        let ranges = version_req_to_ranges(&req("*"));
        assert!(ranges.contains(&v("0.0.0")));
        assert!(ranges.contains(&v("999.999.999")));
    }

    // ========================================================================
    // PubGrub resolution tests
    // ========================================================================

    #[test]
    fn test_simple_resolution() {
        let provider = BuffrsDependencyProvider::new(vec![PackageDependency {
            package: pkg("foo"),
            version_req: req("^1.0.0"),
        }]);

        provider.add_package(
            pkg("foo"),
            PackageInfo {
                versions: vec![v("1.0.0"), v("1.1.0"), v("1.2.0")],
                dependencies: HashMap::new(),
            },
        );

        let result = resolve(&provider).unwrap();
        assert_eq!(result.get(&pkg("foo")), Some(&v("1.2.0")));
    }

    #[test]
    fn test_transitive_resolution() {
        // root -> foo ^1.0.0 -> bar ^1.0.0
        let provider = BuffrsDependencyProvider::new(vec![PackageDependency {
            package: pkg("foo"),
            version_req: req("^1.0.0"),
        }]);

        let mut foo_deps = HashMap::new();
        foo_deps.insert(
            v("1.0.0"),
            vec![PackageDependency {
                package: pkg("bar"),
                version_req: req("^1.0.0"),
            }],
        );

        provider.add_package(
            pkg("foo"),
            PackageInfo {
                versions: vec![v("1.0.0")],
                dependencies: foo_deps,
            },
        );

        provider.add_package(
            pkg("bar"),
            PackageInfo {
                versions: vec![v("1.0.0"), v("1.1.0")],
                dependencies: HashMap::new(),
            },
        );

        let result = resolve(&provider).unwrap();
        assert_eq!(result.get(&pkg("foo")), Some(&v("1.0.0")));
        assert_eq!(result.get(&pkg("bar")), Some(&v("1.1.0")));
    }

    #[test]
    fn test_diamond_dependency_resolution() {
        // This is the key test: diamond dependency that greedy resolution would fail
        //
        // root
        // ├── A ^1.0.0
        // │   └── C ^1.0.0 (greedy would pick C@2.0.0)
        // └── B ^1.0.0
        //     └── C >=1.0.0, <1.5.0 (conflict with greedy!)
        //
        // PubGrub should find: A@1.0.0, B@1.0.0, C@1.4.0

        let provider = BuffrsDependencyProvider::new(vec![
            PackageDependency {
                package: pkg("A"),
                version_req: req("^1.0.0"),
            },
            PackageDependency {
                package: pkg("B"),
                version_req: req("^1.0.0"),
            },
        ]);

        let mut a_deps = HashMap::new();
        a_deps.insert(
            v("1.0.0"),
            vec![PackageDependency {
                package: pkg("C"),
                version_req: req("^1.0.0"),
            }],
        );

        let mut b_deps = HashMap::new();
        b_deps.insert(
            v("1.0.0"),
            vec![PackageDependency {
                package: pkg("C"),
                version_req: req(">=1.0.0, <1.5.0"),
            }],
        );

        provider.add_package(
            pkg("A"),
            PackageInfo {
                versions: vec![v("1.0.0")],
                dependencies: a_deps,
            },
        );

        provider.add_package(
            pkg("B"),
            PackageInfo {
                versions: vec![v("1.0.0")],
                dependencies: b_deps,
            },
        );

        provider.add_package(
            pkg("C"),
            PackageInfo {
                versions: vec![v("1.0.0"), v("1.2.0"), v("1.4.0"), v("2.0.0")],
                dependencies: HashMap::new(),
            },
        );

        let result = resolve(&provider).unwrap();

        // A and B should be resolved
        assert_eq!(result.get(&pkg("A")), Some(&v("1.0.0")));
        assert_eq!(result.get(&pkg("B")), Some(&v("1.0.0")));

        // C should be <1.5.0 (satisfies both A's ^1.0.0 and B's >=1.0.0, <1.5.0)
        let c_version = result.get(&pkg("C")).unwrap();
        assert!(c_version < &v("1.5.0"));
        assert!(c_version >= &v("1.0.0"));
        // Should pick the highest compatible: 1.4.0
        assert_eq!(c_version, &v("1.4.0"));
    }

    #[test]
    fn test_unsatisfiable_dependency() {
        // root -> foo ^1.0.0, but foo only has 2.x versions
        let provider = BuffrsDependencyProvider::new(vec![PackageDependency {
            package: pkg("foo"),
            version_req: req("^1.0.0"),
        }]);

        provider.add_package(
            pkg("foo"),
            PackageInfo {
                versions: vec![v("2.0.0"), v("2.1.0")],
                dependencies: HashMap::new(),
            },
        );

        let result = resolve(&provider);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.message.contains("foo"));
    }

    #[test]
    fn test_conflict_detection() {
        // root -> A ^1.0.0 -> C ^2.0.0
        // root -> B ^1.0.0 -> C ^1.0.0
        // Both require C but with incompatible ranges

        let provider = BuffrsDependencyProvider::new(vec![
            PackageDependency {
                package: pkg("A"),
                version_req: req("^1.0.0"),
            },
            PackageDependency {
                package: pkg("B"),
                version_req: req("^1.0.0"),
            },
        ]);

        let mut a_deps = HashMap::new();
        a_deps.insert(
            v("1.0.0"),
            vec![PackageDependency {
                package: pkg("C"),
                version_req: req("^2.0.0"),
            }],
        );

        let mut b_deps = HashMap::new();
        b_deps.insert(
            v("1.0.0"),
            vec![PackageDependency {
                package: pkg("C"),
                version_req: req("^1.0.0"),
            }],
        );

        provider.add_package(
            pkg("A"),
            PackageInfo {
                versions: vec![v("1.0.0")],
                dependencies: a_deps,
            },
        );

        provider.add_package(
            pkg("B"),
            PackageInfo {
                versions: vec![v("1.0.0")],
                dependencies: b_deps,
            },
        );

        provider.add_package(
            pkg("C"),
            PackageInfo {
                versions: vec![v("1.0.0"), v("1.5.0"), v("2.0.0"), v("2.5.0")],
                dependencies: HashMap::new(),
            },
        );

        let result = resolve(&provider);
        assert!(result.is_err());
        let err = result.unwrap_err();
        // Error should mention the conflict
        assert!(
            err.message.contains("C") || err.message.contains("A") || err.message.contains("B")
        );
    }
}
