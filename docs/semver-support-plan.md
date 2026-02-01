# Full SemVer Support Implementation Plan

This document captures the plan to implement full semantic versioning support for buffrs,
addressing [globusmedical/ext-buffrs#16](https://github.com/globusmedical/ext-buffrs/issues/16).

## Background

### Current State

Buffrs currently only supports **exact version pinning** (`=x.y.z`). Any other semver operator
(caret, tilde, ranges) is rejected at download time with the error:

```text
{version} is not supported yet. Pin the exact version you want to use with '='.
For example: '=1.0.4' instead of '^1.0.0'
```

### Upstream Status

The upstream [helsing-ai/buffrs](https://github.com/helsing-ai/buffrs) has the same limitation,
tracked in [issue #205](https://github.com/helsing-ai/buffrs/issues/205). They suggest using
[PubGrub](https://github.com/pubgrub-rs/pubgrub) for SAT-based dependency resolution, but no
implementation has been started.

## Implementation Plan

### Phase 1: Registry Version Discovery

**Goal:** Query the registry for all available versions of a package.

#### 1.1 Add `list_versions()` to Artifactory

**File:** `src/registry/artifactory.rs`

Extend the existing `get_latest_version()` logic to return all versions instead of just the maximum:

```rust
/// Retrieves all available versions of a package from artifactory
pub async fn list_versions(
    &self,
    repository: String,
    name: PackageName,
) -> miette::Result<Vec<Version>> {
    // Reuse artifact search query from get_latest_version()
    // Parse and return ALL valid versions instead of just max
}
```

#### 1.2 Create Version Selection Module

**File:** `src/version.rs` (new)

```rust
use semver::{Version, VersionReq};

/// Selects the best matching version from a list of available versions.
///
/// Returns the highest version that satisfies the requirement.
pub fn select_version(req: &VersionReq, available: &[Version]) -> Option<Version> {
    available
        .iter()
        .filter(|v| req.matches(v))
        .max()
        .cloned()
}
```

### Phase 2: Resolver Changes

**Goal:** Support version ranges during dependency resolution.

#### 2.1 Modify Remote Dependency Resolution

**File:** `src/resolver.rs`

In `process_remote_dependency()`:

1. Check if version requirement is already exact (has `Op::Exact`)
2. If not exact, query `list_versions()` from registry
3. Select best matching version using `select_version()`
4. Create pinned dependency for download

```rust
async fn resolve_version_to_exact(
    &self,
    dependency: &RemoteDependency,
) -> miette::Result<Version> {
    let version_req = &dependency.manifest.version;

    // Fast path: already exact
    if is_exact_version(version_req) {
        return extract_exact_version(version_req);
    }

    // Query registry for available versions
    let registry = Artifactory::new(...)?;
    let available = registry
        .list_versions(dependency.manifest.repository.clone(), dependency.package.clone())
        .await?;

    // Select best match
    select_version(version_req, &available)
        .ok_or_else(|| miette!("no version of {} matches {}", dependency.package, version_req))
}
```

#### 2.2 Update Download Logic

**File:** `src/registry/mod.rs`

Change `dependency_version_string()` to work with resolved exact versions:

```rust
/// Converts a resolved Version to artifact path string
fn version_to_artifact_string(version: &Version) -> String {
    if version.pre.is_empty() {
        format!("{}.{}.{}", version.major, version.minor, version.patch)
    } else {
        format!("{}.{}.{}-{}", version.major, version.minor, version.patch, version.pre)
    }
}
```

### Phase 3: CLI Updates

**Goal:** Accept version ranges in `buffrs add`.

#### 3.1 Update Dependency Locator

**File:** `src/command.rs`

The `DependencyLocator` parser already accepts version requirements via `VersionReq::parse()`.
Update documentation and defaults:

- Default operator when none specified: `^` (caret/compatible)
- Support all semver operators: `=`, `^`, `~`, `>`, `>=`, `<`, `<=`, `*`

### Phase 4: Documentation

#### 4.1 Update SemVer Reference

**File:** `docs/src/reference/semver.md`

Document all supported version operators with examples.

## Supported Version Operators

| Operator | Example   | Meaning                                      |
| -------- | --------- | -------------------------------------------- |
| `=`      | `=1.2.3`  | Exact version (currently the only supported) |
| `^`      | `^1.2.3`  | Compatible updates (>=1.2.3, <2.0.0)         |
| `~`      | `~1.2.3`  | Patch updates only (>=1.2.3, <1.3.0)         |
| `>`      | `>1.2.3`  | Greater than                                 |
| `>=`     | `>=1.2.3` | Greater than or equal                        |
| `<`      | `<2.0.0`  | Less than                                    |
| `<=`     | `<=2.0.0` | Less than or equal                           |
| `*`      | `*`       | Any version (wildcard)                       |

Multiple requirements can be combined with commas: `>=1.2.3, <2.0.0`

## Files Changed

| File                           | Change                                                                                   |
| ------------------------------ | ---------------------------------------------------------------------------------------- |
| `src/version.rs`               | New module for version selection                                                         |
| `src/lib.rs`                   | Add `mod version`                                                                        |
| `src/registry/artifactory.rs`  | Add `list_versions()`                                                                    |
| `src/registry/mod.rs`          | Add `version_to_artifact_string()`, keep `dependency_version_string()` for compatibility |
| `src/resolver.rs`              | Add version resolution before download                                                   |
| `src/command.rs`               | Update default version operator                                                          |
| `docs/src/reference/semver.md` | Document all operators                                                                   |

## Testing Strategy

1. **Unit tests** for `select_version()` with various requirements
2. **Integration tests** with mock registry returning multiple versions
3. **Manual testing** against real Artifactory instance

## Migration Notes

- Existing `Proto.toml` files with `version = "1.2.3"` will be interpreted as `^1.2.3` (compatible)
- To maintain exact pinning, use explicit `version = "=1.2.3"`
- `Proto.lock` continues to store exact resolved versions

## Implementation Status

| Phase                         | Status      | Notes                                       |
| ----------------------------- | ----------- | ------------------------------------------- |
| 1. Registry Version Discovery | ✅ Complete | `list_versions()` added to artifactory.rs   |
| 2. Resolver Changes           | ✅ Complete | `resolve_version()` in resolver.rs          |
| 3. CLI Updates                | ✅ Complete | All semver operators accepted               |
| 4. Documentation              | ✅ Complete | `docs/src/reference/semver.md` updated      |
| 5. Unit Tests                 | ✅ Complete | 27 tests for greedy version selection       |
| 6. PubGrub SAT Resolution     | ✅ Complete | Full integration in resolver.rs             |

## Current Limitations

### Greedy Version Selection (No Backtracking)

The current implementation uses a **greedy resolution strategy**: for each dependency, it
selects the **highest version** that satisfies the version constraint. This is simple and
fast but has limitations:

#### Diamond Dependency Problem

Consider this dependency graph:

```text
root
├── package-A ^1.0.0 (resolves to A@1.5.0)
│   └── package-C ^1.0.0 (greedy: picks C@2.0.0)
└── package-B ^1.0.0 (resolves to B@1.3.0)
    └── package-C >=1.2.0, <1.5.0 (CONFLICT!)
```

**What happens:**
1. Resolver processes A first, resolves C to 2.0.0 (highest matching `^1.0.0`)
2. Resolver processes B, needs C `>=1.2.0, <1.5.0`
3. **Conflict!** C@2.0.0 was already selected but doesn't satisfy B's constraint

**What should happen:**
A SAT-based resolver would find C@1.4.0 (or similar) that satisfies **both** constraints.

#### No Conflict Detection

The current implementation doesn't detect when two dependencies require incompatible
versions of a transitive dependency. Resolution will succeed, but the lock file may
contain inconsistent versions.

#### No Resolution Backtracking

If selecting the highest version causes a conflict downstream, the resolver cannot
"try again" with a lower version. This is a fundamental limitation of greedy approaches.

### Pre-release Handling

Pre-release versions (e.g., `1.0.0-alpha.1`) are only matched by:
- Exact requirements: `=1.0.0-alpha.1`
- Explicit pre-release comparisons: `>=1.0.0-alpha.1`

They are **not** matched by caret/tilde requirements like `^1.0.0`. This follows
standard semver semantics but can be surprising.

### No Union Ranges

The semver crate uses comma as AND (intersection), not OR (union). You cannot express
"version 1.x OR version 3.x" in a single requirement.

## Why SAT-Based Resolution (PubGrub)?

### What is PubGrub?

[PubGrub](https://github.com/pubgrub-rs/pubgrub) is a version solving algorithm developed
by Natalie Weizenbaum for the Dart package manager. It uses **satisfiability (SAT)** 
principles to find a consistent set of package versions.

### Benefits of PubGrub

1. **Handles Diamond Dependencies**
   - Considers all constraints simultaneously
   - Finds versions that satisfy the entire dependency graph
   - Example: Would find C@1.4.0 in the scenario above

2. **Backtracking**
   - If a choice leads to a conflict, it "backtracks" and tries alternatives
   - Explores the solution space systematically

3. **Clear Error Messages**
   - When no solution exists, explains *why* using "incompatibility" tracking
   - Example: "Because A 1.5.0 requires C ^2.0.0 and B 1.3.0 requires C <1.5.0,
     A 1.5.0 is incompatible with B 1.3.0"

4. **Optimal Solutions**
   - Can be configured to prefer newer versions while still finding valid solutions
   - Minimizes version "downgrades" needed to resolve conflicts

### When Do You Need PubGrub?

You likely **don't need** SAT-based resolution if:
- Your dependency tree is shallow (1-2 levels)
- You have few dependencies with overlapping transitive deps
- You control all packages in your ecosystem

You likely **do need** SAT-based resolution if:
- Complex dependency graphs with deep transitive deps
- Multiple packages depending on shared libraries with different constraints
- Large ecosystem with many independent package authors
- Need clear diagnostics when resolution fails

### Implementation Effort

Adding PubGrub would require:

1. **New dependency**: `pubgrub = "0.2"` (or later)
2. **Trait implementation**: Implement `DependencyProvider` for the registry
3. **Version mapping**: Convert between buffrs types and PubGrub types
4. **Error handling**: Translate PubGrub incompatibilities to user-friendly errors

Estimated effort: **3-5 days** for a basic implementation.

## PubGrub Implementation (✅ Complete & Integrated)

The PubGrub SAT-based resolver is now fully integrated into the resolver pipeline.

### New Files

| File                       | Description                                    |
| -------------------------- | ---------------------------------------------- |
| `src/pubgrub_resolver.rs`  | PubGrub-based SAT resolver implementation      |

### Modified Files

| File                       | Changes                                        |
| -------------------------- | ---------------------------------------------- |
| `src/resolver.rs`          | Added `build_with_pubgrub()` method            |
| `src/config.rs`            | Added `use_greedy_resolver` config option      |
| `Cargo.toml`               | Added `pubgrub = "0.3"` dependency             |

### Resolver Selection

PubGrub is the default resolver.

To opt into the legacy greedy resolver, add to `.buffrs/config.toml`:

```toml
[resolver]
use_greedy_resolver = true
```

### How It Works

When using PubGrub, the resolver uses a three-phase approach:

1. **Discovery Phase**: Fetches all available versions and their dependencies from registries
2. **Resolution Phase**: Runs PubGrub algorithm to find consistent version assignments
3. **Download Phase**: Downloads packages with resolved versions and builds dependency graph

### Key Types

```rust
/// Package identifier for PubGrub resolution
pub enum PubGrubPackage {
    Root,                      // The root package being resolved
    Package(PackageName),      // A regular dependency
}

/// Convert semver::VersionReq to PubGrub Ranges<Version>
pub fn version_req_to_ranges(req: &VersionReq) -> Ranges<Version>;

/// Dependency provider for PubGrub
pub struct BuffrsDependencyProvider {
    packages: HashMap<PackageName, PackageInfo>,
    root_deps: Vec<PackageDependency>,
}

/// Run PubGrub resolution
pub fn resolve(provider: &BuffrsDependencyProvider) -> ResolutionResult;
```

### Features

- **Full semver conversion**: Converts `VersionReq` to PubGrub `Ranges` for all operators
- **Diamond dependency resolution**: Successfully resolves conflicts that greedy fails
- **Clear error messages**: Uses `DefaultStringReporter` for human-readable failures
- **15 unit tests**: Covers version ranges, simple/transitive/diamond resolution, conflicts
- **Automatic fallback**: Falls back to greedy when multi-version resolution is required

### Limitations

- **Multi-version resolution**: PubGrub produces single-version solutions per package. If
    multi-version resolution is required, buffrs falls back to greedy.
- **Performance**: Discovery phase downloads all versions to get dependency metadata (can be slow)
- **Network-heavy**: Requires fetching all package versions upfront

## Future Considerations

- **Lazy discovery**: Fetch metadata on-demand during PubGrub resolution
- **Local dependency support**: Extend PubGrub integration for local packages
- **Version caching** to reduce registry queries
- **Offline mode** using only cached/locked versions
- **Constraint intersection** to detect conflicts early during resolution
- **Resolution audit** command to show why specific versions were selected
