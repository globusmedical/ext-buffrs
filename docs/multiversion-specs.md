# buffrs-multi-version-specs.md

## Use case

A monorepo contains multiple `Proto.toml` manifests (often one per CMake library target or Rust crate). Each `Proto.toml` is resolved independently and produces a local, gitignored `proto/vendor/` tree.

Different parts of the build may be enabled/disabled depending on which CMake targets are selected in a given build tree. A single final binary (exe/dll) may link multiple library targets, each with its own `Proto.toml`, and therefore potentially multiple versions of the same Buffrs package.

The multi-version scenario exists in three distinct forms:

1. **Build-tree coexistence**: multiple versions exist across different `proto/vendor/` trees in the same repository checkout, but are not necessarily linked into the same final link unit.
2. **Link-unit coexistence**: a single final binary links two different versions of the same protobuf API from different CMake targets.
3. **Same-target coexistence**: a single CMake target (and its `Proto.toml`) consumes multiple versions of the same protobuf API simultaneously.

The target outcome is to support all three forms safely. Critically, consumers do not control upstream proto definitions—they cannot mandate that upstream packages use versioned `package` declarations. Therefore, Buffrs must provide automatic namespace disambiguation when multi-version is enabled.

## Problem statement

Buffrs currently enforces “one resolved version per package name” (single-version resolution). This breaks diamond dependencies and prevents independent subgraphs from selecting different versions.

Even if Buffrs allows multiple versions, C++ link safety imposes a hard constraint: if two different package versions declare the same `.proto package`, the generated C++ symbols will overlap. If both are linked into the same final binary, ODR/link-time failures or undefined behavior are likely.

**Critically, consumers do not control upstream proto definitions.** A downstream project consuming `lib-algo-base@0.1.2` and `lib-algo-base@0.1.3` cannot mandate that the upstream maintainer change their `package gm.algo.base;` declaration. Any viable solution must disambiguate generated code without requiring upstream changes.

A monorepo further complicates this because:

- resolution is per `Proto.toml`, not per repo
- builds often link "everything in vendor" (common globbing patterns), which magnifies conflicts and makes safe coexistence impossible even when the final binary does not require both versions
- a single target may legitimately need multiple versions of the same API (e.g., adapting between API versions)

## Requirements

### Resolver controls

- DR-BUFFRS-1000 (Resolver default): Buffrs must default to single-version resolution per package name within one `Proto.toml` resolution.
- DR-BUFFRS-1010 (Multi-version enablement): Buffrs must support resolving multiple versions of the same package name within one `Proto.toml` resolution when explicitly enabled.
- DR-BUFFRS-1020 (Opt-in granularity): Buffrs must support per-dependency opt-in to multi-version resolution in `Proto.toml`.
- DR-BUFFRS-1030 (Permission semantics): The per-dependency opt-in must be treated as permission to allow multiple versions if constraints require it, not as a directive to force duplicates.
- DR-BUFFRS-1040 (Determinism): Resolution must be deterministic given the same inputs (manifests, lockfile, registry contents).

### Vendor layout

- DR-BUFFRS-1100 (Version-qualified directories): When multi-version occurs, Buffrs must store each version in a version-qualified vendor directory (e.g. `proto/vendor/<name>@<ver>/`).
- DR-BUFFRS-1110 (Include-path disambiguation): Buffrs must provide a stable include-root strategy such that two versions can be referenced without path ambiguity.
- DR-BUFFRS-1120 (Locality): Each `Proto.toml` must continue to resolve into its own local `proto/vendor/` tree.

### Link safety

- DR-BUFFRS-1200 (Namespace scan): Buffrs must scan resolved `.proto` files for protobuf `package` declarations and build a namespace-to-(package@version) mapping.
- DR-BUFFRS-1210 (Link-unit conflict definition): A link-unit conflict must be defined as two different package versions that declare the same protobuf namespace being linked into the same final binary.
- DR-BUFFRS-1220 (Default failure): By default, Buffrs-integrated builds must fail when a link-unit conflict is detected.
- DR-BUFFRS-1230 (Actionable diagnostics): On conflict, the error output must include both versions, the overlapping namespaces, the file paths, and the dependency paths from each root that caused each version to be present.
- DR-BUFFRS-1240 (Build-subset correctness): Conflict detection must reflect the actual set of linked CMake targets in the current build, not the mere presence of files in `proto/vendor/`.

### Namespace disambiguation

- DR-BUFFRS-1250 (Automatic disambiguation): When multi-version resolution results in multiple versions of the same package, Buffrs must automatically rewrite protobuf `package` declarations to include version information, ensuring generated code has unique symbols.
- DR-BUFFRS-1260 (Disambiguation strategy): The rewritten package name must incorporate version information in a deterministic, reversible manner (e.g., `gm.algo.base` → `gm.algo.base._v0_1_2` for version 0.1.2).
- DR-BUFFRS-1270 (No upstream changes required): Namespace disambiguation must work without requiring any changes to upstream proto definitions. Consumers must be able to use multi-version even when upstream packages use identical namespace declarations.
- DR-BUFFRS-1280 (Same-target multi-version): A single `Proto.toml` (and therefore a single CMake target) must be able to depend on multiple versions of the same package simultaneously.
- DR-BUFFRS-1290 (Consumer code adaptation): Buffrs must document the rewritten namespace scheme so consumers can write code that references the correct versioned namespace (e.g., `gm::algo::base::_v0_1_2::` in C++).

### Build integration

- DR-BUFFRS-1300 (No “link all vendor” requirement): Buffrs must provide integration that does not require users to compile/link all vendored protos by default.
- DR-BUFFRS-1310 (Per-package build targets): Buffrs must be able to emit build metadata enabling generation of per-package (and per-version) build targets for C++ (and analogously for Rust crates).
- DR-BUFFRS-1320 (Transitive closure linking): The integration must allow consumers to link a single logical “proto library target” and obtain the correct transitive protobuf dependencies without globbing vendor directories.
- DR-BUFFRS-1330 (Monorepo coexistence): Multiple CMake targets, each owning its own `Proto.toml`, must be able to coexist in the same build tree even if they resolve different versions, provided the final link unit does not link conflicting namespaces.

### Lockfile compatibility

- DR-BUFFRS-1400 (Lockfile capture): `Proto.lock` must record the fully resolved dependency graph, including multiple versions when present.
- DR-BUFFRS-1410 (Lockfile reuse): A lockfile must be usable by the Buffrs version that created it.
- DR-BUFFRS-1420 (Config orthogonality): The presence of multi-version selections in `Proto.lock` must not silently enable multi-version if manifests no longer allow it; such mismatches must fail with an actionable error.
- DR-BUFFRS-1430 (Content identity): Buffrs must record a content hash per resolved package@version to support diagnostics and optional identity-based policies.

### Policy controls

- DR-BUFFRS-1500 (Opt-in syntax stability): The per-dependency opt-in syntax must be stable and forward-compatible for future resolver policies.
- DR-BUFFRS-1510 (Namespace policy): Buffrs must support a namespace overlap policy with a default of `rewrite` when `resolver = "multiversion"` is enabled.
- DR-BUFFRS-1520 (Identity-based exception): Buffrs must support an optional policy mode `identical_only`, allowing overlap without rewriting only if the effective `.proto` content hashes match.
- DR-BUFFRS-1530 (Policy scope): Namespace overlap policy overrides (if enabled) must be expressible per dependency edge.
- DR-BUFFRS-1540 (Forbidden policy): Buffrs must support `namespace_overlap = "forbidden"` to fail if multi-version would require namespace rewriting, for projects that cannot adapt consumer code.

## Architecture

### Resolution scope

Buffrs resolution is scoped to a single `Proto.toml`. This scope remains unchanged.

Multi-version resolution is introduced within that scope only. Cross-`Proto.toml` version collisions are not “resolver collisions”; they are **link-unit collisions**, handled by build integration.

### Two-phase safety model

1. **Resolve-time correctness** (within one `Proto.toml`):
   - allow multiple versions only when enabled per dependency edge
   - produce a complete lockfile and vendor tree
   - **rewrite proto `package` declarations** to include version suffixes when multi-version occurs
   - emit metadata describing packages, versions, proto files, and rewritten namespaces

2. **Link-time correctness** (across multiple CMake targets):
   - detect whether a final binary links two different versions with overlapping protobuf namespaces
   - fail by default with actionable diagnostics
   - this check must reflect the _actual_ set of targets linked in the current build configuration
   - with namespace rewriting, conflicts become impossible within a single `Proto.toml` but may still occur across different `Proto.toml`s that resolve overlapping versions without both enabling multiversion

This model addresses the monorepo reality: different builds link different subsets of targets.

## Detailed design

### Manifest syntax

Per-dependency opt-in, permission semantics:

```toml
[dependencies]
api-algo-autoscrewdesign = { version = "0.1.2-SPINE-4431", resolver = "multiversion" }
```

Optional namespace policy override (default `forbidden`):

```toml
[dependencies]
api-algo-autoscrewdesign = { version = "0.1.2-SPINE-4431", resolver = "multiversion", namespace_overlap = "forbidden" }
# namespace_overlap = "identical_only"
# namespace_overlap = "allowed"          # discouraged, explicit hazard
```

Notes:

- `resolver = "multiversion"` enables multi-version permission for this dependency edge only.
- `namespace_overlap` controls what happens if multi-version leads to overlapping protobuf namespaces _in the final link unit_.

### Vendor layout

When multi-version occurs:

- `proto/vendor/lib-algo-base@0.1.2/...`
- `proto/vendor/lib-algo-base@0.1.3/...`

A stable, version-agnostic include root is also emitted to support deterministic `-I` assembly. Two approaches are supported:

1. **Explicit versioned include roots**: include `proto/vendor/lib-algo-base@0.1.2` and `proto/vendor/lib-algo-base@0.1.3` as separate `-I` roots.
2. **Generated “include index” directory** (recommended for CMake):
   - Buffrs generates `proto/vendor/_buffrs_includes/<name>@<ver>/` symlinks (or copies on platforms without symlinks) pointing to the real vendor dirs.
   - This yields stable paths and avoids accidental include path shadowing.

### Lockfile

`Proto.lock` records:

- the full resolved graph (package name, version, source)
- the selected version set (including duplicates by name)
- content hashes per package@version
- dependency edges and which edges enabled multi-version permission (for diagnostics)

Lockfile validation rules:

- if `Proto.lock` contains two versions of the same name but no manifest edge enables multi-version for that name, fail with:
  - which name requires multiversion
  - which manifest(s) must be updated
  - how to regenerate the lockfile

### Metadata emission

Buffrs emits a machine-readable metadata file alongside vendor output, e.g.:

- `proto/vendor/_buffrs_meta/graph.json`
- `proto/vendor/_buffrs_meta/namespaces.json`

`namespaces.json` contains:

- `package@version` → list of `.proto` files
- `.proto package namespace` → `package@version` (one-to-many possible if multi-version)

This metadata is used for:

- diagnostics
- CMake integration generation
- link-unit conflict detection

### CMake integration

#### Core goal

Stop the default workflow from “link everything in vendor”.

Instead, make the default:

- compile only the `.proto` files owned by the current library target
- link only the needed transitive protobuf code generated for resolved dependencies

#### Generated CMake targets

For each resolved Buffrs package@version, Buffrs provides a CMake target name that is unique per version, e.g.:

- `buffrs::lib-algo-base@0.1.2`
- `buffrs::lib-algo-base@0.1.3`

For each `Proto.toml`, Buffrs provides:

- a generated CMake include file, e.g. `proto/vendor/_buffrs_meta/buffrs.cmake`
- a root target representing “this manifest’s proto API”, e.g. `buffrs::root`

Consumers link:

- their own generated proto library target
- and then `target_link_libraries(<their_target> PRIVATE buffrs::root)`

`buffrs::root` depends on the correct transitive per-package targets.

This design ensures:

- targets only pull what they need
- different manifests can resolve different versions without forcing them all into every binary

#### Link-unit conflict detection in CMake

Buffrs provides a CMake function:

- `buffrs_validate_link_unit(<final_target>)`

Implementation strategy:

- Each generated per-package target carries an INTERFACE property containing its protobuf namespace signature, e.g.:
  - `BUFFRS_PROTO_NAMESPACES` = `lib.algo.base=lib-algo-base@0.1.2;...`

- `buffrs_validate_link_unit` walks the transitive link closure of `<final_target>` and collects all namespace signatures.
- If the same namespace appears with two different `package@version` values, it fails the configure step (or generate step) with an actionable error including:
  - the namespace
  - both `package@version`
  - the CMake target chain that introduced each version
  - pointers to the relevant `Proto.toml` roots

This satisfies build-subset correctness because it runs on the actual selected `<final_target>`s in that build tree.

Recommended integration point:

- call `buffrs_validate_link_unit()` for executables and shared libraries (dll/so), not for intermediate static libs.

### Link safety policy handling

Default:

- `namespace_overlap = "forbidden"`

Optional:

- `namespace_overlap = "identical_only"`:
  - allow overlap only if content hashes of the effective `.proto` file sets match
  - error if hashes differ

- `namespace_overlap = "allowed"`:
  - allowed only if explicitly set per dependency edge
  - emits a prominent warning in diagnostics output
  - still provides full reporting of overlapping namespaces

Rationale:

- “allowed” cannot make C++ safe; it only acknowledges deliberate risk (e.g., custom toolchains with symbol hiding or dynamic isolation).

### Handling “multiple versions in one final binary”

When multi-version resolution is enabled and results in multiple versions of the same package, Buffrs automatically rewrites proto `package` declarations to ensure safe coexistence.

#### Namespace rewriting mechanism

When extracting a package with `resolver = "multiversion"` enabled, Buffrs:

1. Scans all `.proto` files in the package
2. Identifies `package` declarations (e.g., `package gm.algo.base;`)
3. Rewrites them to include a version suffix (e.g., `package gm.algo.base._v0_1_2;`)
4. Writes the rewritten files to the vendor directory

The version suffix format is `_v<major>_<minor>_<patch>` with dots and hyphens replaced by underscores. Examples:

- `0.1.2` → `_v0_1_2`
- `0.1.2-SPINE-4384` → `_v0_1_2_SPINE_4384`

#### Generated code namespaces

The rewritten proto packages result in unique C++ namespaces:

| Original Proto          | Version | Rewritten Proto                 | C++ Namespace             |
| ----------------------- | ------- | ------------------------------- | ------------------------- |
| `package gm.algo.base;` | 0.1.2   | `package gm.algo.base._v0_1_2;` | `gm::algo::base::_v0_1_2` |
| `package gm.algo.base;` | 0.1.3   | `package gm.algo.base._v0_1_3;` | `gm::algo::base::_v0_1_3` |

#### Consumer code adaptation

Consumers using multi-version must adapt their code to use the versioned namespaces:

```cpp
// Instead of:
// gm::algo::base::SomeMessage msg;

// Use versioned namespace for specific version:
gm::algo::base::_v0_1_2::SomeMessage msg_v012;
gm::algo::base::_v0_1_3::SomeMessage msg_v013;

// Or use namespace aliases:
namespace algo_v012 = gm::algo::base::_v0_1_2;
namespace algo_v013 = gm::algo::base::_v0_1_3;
```

#### When rewriting occurs

Namespace rewriting is triggered when:

1. `resolver = "multiversion"` is set on a dependency, AND
2. Multiple versions of that package are actually resolved

If only one version is resolved (constraints compatible), no rewriting occurs and the original namespace is preserved.

## Diagnostics

On conflict, error output includes:

- conflicting namespace: `lib.algo.base`
- versions:
  - `lib-algo-base@0.1.2`
  - `lib-algo-base@0.1.3`

- example `.proto` file paths under each vendor directory showing the declarations
- link provenance:
  - final target
  - transitive CMake link chain to each `buffrs::...@...` target

- manifest provenance:
  - which `Proto.toml` roots pulled each version

## Non-goals

- Cross-`Proto.toml` global resolution. Resolution remains per manifest; safety is enforced at link-unit boundaries.
- Automatic detection of "compatible" API changes between versions. Buffrs treats different versions as potentially incompatible.
