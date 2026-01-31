# Dependency Resolution
Buffrs uses a deterministic dependency resolver to compute the set of packages needed for your project. This page describes how resolution works and its interaction with multi-version support.

## Resolution Scope

Each `Proto.toml` is resolved independently. The resolver reads:
- Direct dependencies from `[dependencies]`
- Transitive dependencies from each resolved package's manifest
- Version constraints from the lockfile (`Proto.lock`)

## Single-Version Resolution (Default)

By default, buffrs enforces single-version resolution: each package name can only resolve to one version within a single `Proto.toml`. If two dependencies require incompatible versions of the same package, resolution fails with an error.

Example error:
```
Error: a dependency of your project requires lib-algo-base@=0.1.3 which collides with lib-algo-base@0.1.2 required by api-algo-autoscrewdesign
```

## Multi-Version Resolution

Starting with buffrs 0.50.0, you can opt-in to allow multiple versions per dependency by adding `resolver = "multiversion"` to the dependency declaration:

```toml
[dependencies.lib-algo-base]
version = "=0.1.2"
resolver = "multiversion"
```

This grants permission—not a directive—for the resolver to keep multiple versions if constraints require it.

### Permission Semantics

- If all constraints can be satisfied by a single version, only one version is resolved
- If constraints conflict (e.g., `=0.1.2` and `=0.1.3`), both versions are resolved only if at least one dependency edge has `resolver = "multiversion"`
- Permission propagates: if package A depends on B with multiversion permission, B can have multiple versions

### Version-Qualified Directories

When multi-version occurs, vendored packages use version-qualified directories:
- `proto/vendor/lib-algo-base@0.1.2/`
- `proto/vendor/lib-algo-base@0.1.3/`

## Lockfile Integration

The lockfile (`Proto.lock`) captures the resolved dependency graph:
- All resolved package versions
- Content hashes for integrity verification
- Source registry and repository information

If the lockfile contains multi-version entries but the manifest no longer permits them, resolution fails with guidance on how to update the manifest or regenerate the lockfile.

## Determinism

Resolution is deterministic given:
- The same `Proto.toml` manifest
- The same `Proto.lock` lockfile
- The same registry contents

This ensures reproducible builds across different machines and CI environments.