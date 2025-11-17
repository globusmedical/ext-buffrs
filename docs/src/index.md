# The Buffrs Book

![Buffrs Logo](images/buffrs.svg)

**Buffrs is a package manager for Protocol Buffers.** It helps you manage dependencies, create distributable packages, and publish them to registries—bringing modern package management to your protobuf workflow.

## Quick Start

```bash
# Install Buffrs
cargo install buffrs

# Initialize a new API package
buffrs init --api

# Add a dependency
buffrs add --registry https://your.registry.com datatypes/user@=0.1.0

# Publish your package
buffrs publish
```

[Get started →](getting-started/index.md)

## Why Buffrs?

- **📦 Dependency Management**: Version and distribute protobuf definitions like any modern package
- **🔄 Multi-Registry Support**: Connect to multiple registries simultaneously
- **🛠️ Build Tool Integration**: Works seamlessly with Cargo, Poetry, npm, and more
- **🚀 Simple Workflow**: Familiar commands inspired by Cargo and npm

## Documentation

| Section | Description |
|---------|-------------|
| **[Getting Started](getting-started/index.md)** | Install Buffrs and create your first package |
| **[Buffrs Guide](guide/index.md)** | Learn core concepts and best practices |
| **[Commands Reference](commands/index.md)** | Complete CLI command documentation |
| **[FAQ](faq.md)** | Common questions and troubleshooting |

---

**Need help?** Check the [FAQ](faq.md) or visit our [GitHub repository](https://github.com/globusmedical/buffrs).
