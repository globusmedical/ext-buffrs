# buffrs-multi-version-specs.md

## Use case

A monorepo contains multiple `Proto.toml` manifests (often one per CMake library target or Rust crate). Each `Proto.toml` is resolved independently and produces a local, gitignored `proto/vendor/` tree.

Different parts of the build may be enabled/disabled depending on which CMake targets are selected in a given build tree. A single final binary (exe/dll) may link multiple library targets, each with its own `Proto.toml`, and therefore potentially multiple versions of the same Buffrs package.

The multi-version scenario exists in two distinct forms:

1. **Build-tree coexistence**: multiple versions exist across different `proto/vendor/` trees in the same repository checkout, but are not necessarily linked into the same final link unit.
2. **Link-unit coexistence**: a single final binary links two different versions of the same protobuf API. If the `.proto` `package` declarations are identical across versions, C++ symbol collisions and ODR violations are expected.

The target outcome is to support (1) broadly and safely, and support (2) only when the protobuf namespaces are versioned (or another explicit isolation strategy is used).

## Problem statement

Buffrs currently enforces “one resolved version per package name” (single-version resolution). This breaks diamond dependencies and prevents independent subgraphs from selecting different versions.

Even if Buffrs allows multiple versions, C++ link safety imposes a hard constraint: if two different package versions declare the same `.proto package`, the generated C++ symbols will overlap. If both are linked into the same final binary, ODR/link-time failures or undefined behavior are likely.

A monorepo further complicates this because:
- resolution is per `Proto.toml`, not per repo
- builds often link “everything in vendor” (common globbing patterns), which magnifies conflicts and makes safe coexistence impossible even when the final binary does not require both versions

## Requirements

### Resolver controls

* DR-BUFFRS-1000 (Resolver default): Buffrs shall default to single-version resolution per package name within one `Proto.toml` resolution.
* DR-BUFFRS-1010 (Multi-version enablement): Buffrs shall support resolving multiple versions of the same package name within one `Proto.toml` resolution when explicitly enabled.
* DR-BUFFRS-1020 (Opt-in granularity): Buffrs shall support per-dependency opt-in to multi-version resolution in `Proto.toml`.
* DR-BUFFRS-1030 (Permission semantics): The per-dependency opt-in shall be treated as permission to allow multiple versions if constraints require it, not as a directive to force duplicates.
* DR-BUFFRS-1040 (Determinism): Resolution shall be deterministic given the same inputs (manifests, lockfile, registry contents).

### Vendor layout

* DR-BUFFRS-1100 (Version-qualified directories): When multi-version occurs, Buffrs shall store each version in a version-qualified vendor directory (e.g. `proto/vendor/<name>@<ver>/`).
* DR-BUFFRS-1110 (Include-path disambiguation): Buffrs shall provide a stable include-root strategy such that two versions can be referenced without path ambiguity.
* DR-BUFFRS-1120 (Locality): Each `Proto.toml` shall continue to resolve into its own local `proto/vendor/` tree.

### Link safety

* DR-BUFFRS-1200 (Namespace scan): Buffrs shall scan resolved `.proto` files for protobuf `package` declarations and build a namespace-to-(package@version) mapping.
* DR-BUFFRS-1210 (Link-unit conflict definition): A link-unit conflict shall be defined as two different package versions that declare the same protobuf namespace being linked into the same final binary.
* DR-BUFFRS-1220 (Default failure): By default, Buffrs-integrated builds shall fail when a link-unit conflict is detected.
* DR-BUFFRS-1230 (Actionable diagnostics): On conflict, the error output shall include both versions, the overlapping namespaces, the file paths, and the dependency paths from each root that caused each version to be present.
* DR-BUFFRS-1240 (Build-subset correctness): Conflict detection shall reflect the actual set of linked CMake targets in the current build, not the mere presence of files in `proto/vendor/`.

### Build integration

* DR-BUFFRS-1300 (No “link all vendor” requirement): Buffrs shall provide integration that does not require users to compile/link all vendored protos by default.
* DR-BUFFRS-1310 (Per-package build targets): Buffrs shall be able to emit build metadata enabling generation of per-package (and per-version) build targets for C++ (and analogously for Rust crates).
* DR-BUFFRS-1320 (Transitive closure linking): The integration shall allow consumers to link a single logical “proto library target” and obtain the correct transitive protobuf dependencies without globbing vendor directories.
* DR-BUFFRS-1330 (Monorepo coexistence): Multiple CMake targets, each owning its own `Proto.toml`, shall be able to coexist in the same build tree even if they resolve different versions, provided the final link unit does not link conflicting namespaces.

### Lockfile compatibility

* DR-BUFFRS-1400 (Lockfile capture): `Proto.lock` shall record the fully resolved dependency graph, including multiple versions when present.
* DR-BUFFRS-1410 (Lockfile reuse): A lockfile shall be usable by the Buffrs version that created it.
* DR-BUFFRS-1420 (Config orthogonality): The presence of multi-version selections in `Proto.lock` shall not silently enable multi-version if manifests no longer allow it; such mismatches shall fail with an actionable error.
* DR-BUFFRS-1430 (Content identity): Buffrs shall record a content hash per resolved package@version to support diagnostics and optional identity-based policies.

### Policy controls

* DR-BUFFRS-1500 (Opt-in syntax stability): The per-dependency opt-in syntax shall be stable and forward-compatible for future resolver policies.
* DR-BUFFRS-1510 (Namespace policy): Buffrs shall support a namespace overlap policy with a default of `forbidden`.
* DR-BUFFRS-1520 (Identity-based exception): Buffrs shall support an optional policy mode `identical_only`, allowing overlap only if the effective `.proto` content hashes match.
* DR-BUFFRS-1530 (Policy scope): Namespace overlap policy overrides (if enabled) shall be expressible per dependency edge.

## Architecture

### Resolution scope

Buffrs resolution is scoped to a single `Proto.toml`. This scope remains unchanged.

Multi-version resolution is introduced within that scope only. Cross-`Proto.toml` version collisions are not “resolver collisions”; they are **link-unit collisions**, handled by build integration.

### Two-phase safety model

1. **Resolve-time correctness** (within one `Proto.toml`):
   - allow multiple versions only when enabled per dependency edge
   - produce a complete lockfile and vendor tree
   - emit metadata describing packages, versions, proto files, and namespaces

2. **Link-time correctness** (across multiple CMake targets):
   - detect whether a final binary links two different versions with overlapping protobuf namespaces
   - fail by default with actionable diagnostics
   - this check must reflect the *actual* set of targets linked in the current build configuration

This model addresses the monorepo reality: different builds link different subsets of targets.

## Detailed design

### Manifest syntax

Per-dependency opt-in, permission semantics:

```toml
[dependencies]
api-algo-autoscrewdesign = { version = "0.1.2-SPINE-4431", resolver = "multiversion" }
````

Optional namespace policy override (default `forbidden`):

```toml
[dependencies]
api-algo-autoscrewdesign = { version = "0.1.2-SPINE-4431", resolver = "multiversion", namespace_overlap = "forbidden" }
# namespace_overlap = "identical_only"
# namespace_overlap = "allowed"          # discouraged, explicit hazard
```

Notes:

* `resolver = "multiversion"` enables multi-version permission for this dependency edge only.
* `namespace_overlap` controls what happens if multi-version leads to overlapping protobuf namespaces *in the final link unit*.

### Vendor layout

When multi-version occurs:

* `proto/vendor/lib-algo-base@0.1.2/...`
* `proto/vendor/lib-algo-base@0.1.3/...`

A stable, version-agnostic include root is also emitted to support deterministic `-I` assembly. Two approaches are supported:

1. **Explicit versioned include roots**: include `proto/vendor/lib-algo-base@0.1.2` and `proto/vendor/lib-algo-base@0.1.3` as separate `-I` roots.
2. **Generated “include index” directory** (recommended for CMake):

   * Buffrs generates `proto/vendor/_buffrs_includes/<name>@<ver>/` symlinks (or copies on platforms without symlinks) pointing to the real vendor dirs.
   * This yields stable paths and avoids accidental include path shadowing.

### Lockfile

`Proto.lock` records:

* the full resolved graph (package name, version, source)
* the selected version set (including duplicates by name)
* content hashes per package@version
* dependency edges and which edges enabled multi-version permission (for diagnostics)

Lockfile validation rules:

* if `Proto.lock` contains two versions of the same name but no manifest edge enables multi-version for that name, fail with:

  * which name requires multiversion
  * which manifest(s) must be updated
  * how to regenerate the lockfile

### Metadata emission

Buffrs emits a machine-readable metadata file alongside vendor output, e.g.:

* `proto/vendor/_buffrs_meta/graph.json`
* `proto/vendor/_buffrs_meta/namespaces.json`

`namespaces.json` contains:

* `package@version` → list of `.proto` files
* `.proto package namespace` → `package@version` (one-to-many possible if multi-version)

This metadata is used for:

* diagnostics
* CMake integration generation
* link-unit conflict detection

### CMake integration

#### Core goal

Stop the default workflow from “link everything in vendor”.

Instead, make the default:

* compile only the `.proto` files owned by the current library target
* link only the needed transitive protobuf code generated for resolved dependencies

#### Generated CMake targets

For each resolved Buffrs package@version, Buffrs provides a CMake target name that is unique per version, e.g.:

* `buffrs::lib-algo-base@0.1.2`
* `buffrs::lib-algo-base@0.1.3`

For each `Proto.toml`, Buffrs provides:

* a generated CMake include file, e.g. `proto/vendor/_buffrs_meta/buffrs.cmake`
* a root target representing “this manifest’s proto API”, e.g. `buffrs::root`

Consumers link:

* their own generated proto library target
* and then `target_link_libraries(<their_target> PRIVATE buffrs::root)`

`buffrs::root` depends on the correct transitive per-package targets.

This design ensures:

* targets only pull what they need
* different manifests can resolve different versions without forcing them all into every binary

#### Link-unit conflict detection in CMake

Buffrs provides a CMake function:

* `buffrs_validate_link_unit(<final_target>)`

Implementation strategy:

* Each generated per-package target carries an INTERFACE property containing its protobuf namespace signature, e.g.:

  * `BUFFRS_PROTO_NAMESPACES` = `lib.algo.base=lib-algo-base@0.1.2;...`
* `buffrs_validate_link_unit` walks the transitive link closure of `<final_target>` and collects all namespace signatures.
* If the same namespace appears with two different `package@version` values, it fails the configure step (or generate step) with an actionable error including:

  * the namespace
  * both `package@version`
  * the CMake target chain that introduced each version
  * pointers to the relevant `Proto.toml` roots

This satisfies build-subset correctness because it runs on the actual selected `<final_target>`s in that build tree.

Recommended integration point:

* call `buffrs_validate_link_unit()` for executables and shared libraries (dll/so), not for intermediate static libs.

### Link safety policy handling

Default:

* `namespace_overlap = "forbidden"`

Optional:

* `namespace_overlap = "identical_only"`:

  * allow overlap only if content hashes of the effective `.proto` file sets match
  * error if hashes differ
* `namespace_overlap = "allowed"`:

  * allowed only if explicitly set per dependency edge
  * emits a prominent warning in diagnostics output
  * still provides full reporting of overlapping namespaces

Rationale:

* “allowed” cannot make C++ safe; it only acknowledges deliberate risk (e.g., custom toolchains with symbol hiding or dynamic isolation).

### Handling “multiple versions in one final binary”

Buffrs cannot make this safe when protobuf namespaces are identical.

Therefore:

* Buffrs supports it only when protobuf namespaces are versioned (or otherwise isolated).
* Buffrs helps by producing actionable conflict errors that point upstream to the necessary change:

  * versioned `package` declarations (preferred)
  * or API-major-based namespaces if semver-derived names are too granular

Buffrs may optionally generate developer aids (non-normative, but recommended):

* a small `README.buffrs.md` under `proto/vendor/_buffrs_meta/` explaining:

  * which namespaces were found
  * which targets introduce each version
  * recommended versioned namespace scheme examples

## Diagnostics

On conflict, error output includes:

* conflicting namespace: `lib.algo.base`
* versions:

  * `lib-algo-base@0.1.2`
  * `lib-algo-base@0.1.3`
* example `.proto` file paths under each vendor directory showing the declarations
* link provenance:

  * final target
  * transitive CMake link chain to each `buffrs::...@...` target
* manifest provenance:

  * which `Proto.toml` roots pulled each version

## Non-goals

* Automatic rewriting of protobuf `package` declarations.
* Attempting to “hide” symbol conflicts in C++ without explicit isolation mechanisms.
* Cross-`Proto.toml` global resolution. Resolution remains per manifest; safety is enforced at link-unit boundaries.
