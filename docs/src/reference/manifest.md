# The Manifest Format

The `Proto.toml` manifest file describes your buffrs package and its dependencies. This page documents the complete manifest format.

## `[package]` Section

The package section describes your package's identity. It is optional for "consumer" projects that only consume dependencies.

```toml
[package]
type = "lib"        # "lib" or "api"
name = "my-package"
version = "1.0.0"
description = "Optional description"
```

### Fields

| Field         | Required | Description                                                 |
| ------------- | -------- | ----------------------------------------------------------- |
| `type`        | Yes      | Package type: `lib` (library) or `api` (service definition) |
| `name`        | Yes      | Package name (lowercase, alphanumeric, hyphens allowed)     |
| `version`     | Yes      | SemVer version string                                       |
| `description` | No       | Human-readable package description                          |

## `[dependencies]` Section

Dependencies can be declared in two formats:

### Inline Format

```toml
[dependencies]
google = { version = "=1.0.0", registry = "https://registry.example.com", repository = "common" }
```

### Table Format

```toml
[dependencies.google]
version = "=1.0.0"
registry = "https://registry.example.com"
repository = "common"
```

### Dependency Fields

| Field               | Required | Description                                                   |
| ------------------- | -------- | ------------------------------------------------------------- |
| `version`           | Yes      | Version requirement (e.g., `=1.0.0`, `^1.0`, `>=1.0,<2.0`)    |
| `registry`          | Yes*     | Registry URL or alias                                         |
| `repository`        | Yes*     | Repository name within the registry                           |
| `resolver`          | No       | Resolution mode: `default` or `multiversion`                  |
| `namespace_overlap` | No       | For multiversion: `forbidden`, `identical_only`, or `allowed` |

*Required for remote dependencies; omit for local dependencies.

### Local Dependencies

For development, you can reference local packages:

```toml
[dependencies.my-local-lib]
path = "../my-local-lib"
```

With optional publish information:

```toml
[dependencies.my-local-lib]
path = "../my-local-lib"
version = "=1.0.0"
registry = "https://registry.example.com"
repository = "libs"
```

## Multi-Version Options

Starting with buffrs 0.50.0, per-dependency multi-version control is available:

```toml
[dependencies.lib-algo-base]
version = "=0.1.2"
repository = "algo"
registry = "https://registry.example.com"
resolver = "multiversion"           # Allow multiple versions
namespace_overlap = "forbidden"     # Default: fail on namespace collision
```

### `resolver` Values

| Value          | Description                                              |
| -------------- | -------------------------------------------------------- |
| `default`      | Single-version resolution (one version per package name) |
| `multiversion` | Allow multiple versions if constraints require it        |

### `namespace_overlap` Values

| Value            | Description                                                   |
| ---------------- | ------------------------------------------------------------- |
| `forbidden`      | Fail if multiple versions declare the same protobuf namespace |
| `identical_only` | Allow overlap only if proto file content hashes match         |
| `allowed`        | Allow overlap (explicit hazard acknowledgment)                |

## Edition

The edition field at the root level specifies which buffrs edition the manifest uses:

```toml
edition = "0.50"
```

See [Editions](editions.md) for more information.

## Complete Example

```toml
edition = "0.50"

[package]
type = "api"
name = "my-service-api"
version = "2.0.0"
description = "API definitions for my service"

[dependencies.google]
version = "=1.0.0"
registry = "https://registry.example.com"
repository = "common"

[dependencies.lib-common-types]
version = "=0.5.0"
registry = "https://registry.example.com"
repository = "libs"
resolver = "multiversion"
namespace_overlap = "identical_only"

[dependencies.local-dev-lib]
path = "../local-dev-lib"
```
