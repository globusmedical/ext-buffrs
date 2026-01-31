# Configuration

Buffrs can be configured via a `.buffrs/config.toml` file in your project directory.

## Configuration File

Create `.buffrs/config.toml` in your project root:

```toml
edition = "0.50"

[registry]
default = "company"

[registries]
company = "https://artifactory.company.com/artifactory"
public = "https://registry.buffrs.dev"

[commands]
default_args = ["--insecure"]

[commands.install]
default_args = ["--generate-buf-yaml"]

[resolver]
allow_multiple_versions = true
skip_link_safety_check = false
```

### `[registry]` Section

| Field     | Description                                      |
| --------- | ------------------------------------------------ |
| `default` | Default registry alias to use when not specified |

### `[registries]` Section

Define registry aliases for convenience:

```toml
[registries]
company = "https://artifactory.company.com/artifactory"
```

Then use in `Proto.toml`:

```toml
[dependencies.my-pkg]
registry = "company"  # Uses the alias
```

### `[commands]` Section

Set default arguments for buffrs commands:

```toml
[commands]
default_args = ["--insecure"]  # Applied to all commands

[commands.install]
default_args = ["--generate-buf-yaml", "--generate-tonic-proto-module", "src/proto.rs"]
```

### `[resolver]` Section

Control dependency resolution behavior:

| Field                     | Default | Description                                                         |
| ------------------------- | ------- | ------------------------------------------------------------------- |
| `allow_multiple_versions` | `false` | Global multi-version permission (deprecated; prefer per-dependency) |
| `skip_link_safety_check`  | `false` | Skip proto namespace collision checks                               |

> **Note**: The `allow_multiple_versions` config option is deprecated. Prefer using `resolver = "multiversion"` per-dependency in `Proto.toml`.

## Authentication

Buffrs uses a local credential storage for authenticating with registries. The [`login`](../commands/buffrs-login.md) command can be used to add new credentials to the storage. Once saved, credentials are automatically used for authenticating with the registry they are associated with. Registries are identified by their URL.

Note that credentials are optional, if they are missing for a given registry URL, no authentication is attempted.

## TLS Configuration

Buffrs will automatically pick up the `SSL_CERT_FILE` environment variable if it's been set, and attempt to use the native subsystem to parse and load the specified root certificate into the certificate store. No additional configuration is needed to apply custom root certificates.

## Proxy Support

Buffrs will automatically pick up on `HTTP_PROXY` and `HTTPS_PROXY` environment variables if they've been set, and use the specified proxy URLs for the associated remote requests. No additional configuration is needed.
