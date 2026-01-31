# Specifying Dependencies
This page covers how to specify dependencies in your `Proto.toml` manifest file.

## Basic Syntax

Dependencies are declared in the `[dependencies]` section:

```toml
[dependencies]
google = { version = "=1.0.0", registry = "https://registry.example.com", repository = "common" }
```

Or using table format for clarity:

```toml
[dependencies.google]
version = "=1.0.0"
registry = "https://registry.example.com"
repository = "common"
```

## Version Requirements

Buffrs supports several version requirement formats:

| Format | Example | Description |
|--------|---------|-------------|
| Exact | `=1.0.0` | Exactly this version |
| Caret | `^1.0.0` | Compatible with 1.x.x |
| Tilde | `~1.0.0` | Compatible with 1.0.x |
| Range | `>=1.0.0,<2.0.0` | Within range |
| Wildcard | `1.*` | Any 1.x version |

## Registry and Repository

Each remote dependency requires:

- **registry**: URL of the Artifactory instance (or alias from config)
- **repository**: Name of the repository within that registry

```toml
[dependencies.my-package]
version = "=1.0.0"
registry = "https://artifactory.company.com/artifactory"
repository = "proto-packages"
```

### Registry Aliases

You can define registry aliases in `.buffrs/config.toml`:

```toml
edition = "0.50"

[registry]
default = "company"

[registries]
company = "https://artifactory.company.com/artifactory"
```

Then reference by alias:

```toml
[dependencies.my-package]
version = "=1.0.0"
registry = "company"
repository = "proto-packages"
```

## Local Dependencies

For development, reference packages on the local filesystem:

```toml
[dependencies.local-lib]
path = "../local-lib"
```

### Publishing Local Dependencies

To publish a package with local dependencies, add publish information:

```toml
[dependencies.local-lib]
path = "../local-lib"
version = "=1.0.0"
registry = "https://registry.example.com"
repository = "libs"
```

During `buffrs publish`, local paths are rewritten to registry references.

## Multi-Version Dependencies

To allow multiple versions of a dependency (buffrs 0.50.0+):

```toml
[dependencies.lib-algo-base]
version = "=0.1.2"
registry = "https://registry.example.com"
repository = "algo"
resolver = "multiversion"
```

See [Multi-Version Dependencies](../guide/multi-version-dependencies.md) for details.

## Namespace Overlap Policy

When using multi-version, control namespace collision behavior:

```toml
[dependencies.lib-algo-base]
version = "=0.1.2"
resolver = "multiversion"
namespace_overlap = "forbidden"     # Default
# namespace_overlap = "identical_only"
# namespace_overlap = "allowed"
```