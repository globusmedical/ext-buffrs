## First Steps with Buffrs

Let's create your first Protocol Buffer package with Buffrs.

### 1. Initialize a New Package

Create a new API package:

```bash
mkdir web-server && cd web-server
buffrs init --api
```

This creates:

```
.
├── Proto.toml          # Package manifest
└── proto/              # Your .proto files
    └── vendor/         # Dependencies install here
```

> [!NOTE]
> Use `--lib` for library packages, or omit both flags for consumer-only projects (e.g., server implementations without protobuf definitions to publish).

### 2. Review the Manifest

The `Proto.toml` file defines your package:

```toml
[package]
name = "web-server"
version = "0.1.0"
type = "api"

[dependencies]
```

### 3. Add a Dependency

Add a package from your registry:

```bash
buffrs add --registry https://your.registry.com datatypes/user@=0.1.0
```

This updates `Proto.toml`:

```toml
[dependencies.user]
version = "=0.1.0"
repository = "datatypes"
registry = "https://your.registry.com/"
```

### 4. Install Dependencies

Download and set up dependencies:

```bash
buffrs install
```

Dependencies are placed in `proto/vendor/` and ready to import.

### What's Next?

- **[Buffrs Guide](../guide/index.md)**: Learn core concepts and workflows
- **[Commands Reference](../commands/index.md)**: Explore all CLI commands
- **[Package Types](../guide/package-types.md)**: Understand APIs vs libraries
