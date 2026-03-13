# The Buffrs Book

![Buffrs Logo](images/buffrs.svg)

**Buffrs manages versioned Protocol Buffer and gRPC API packages.** It helps teams publish shared API definitions, consume them with full SemVer support, and keep migrations manageable with lockfiles and multi-version installs when needed.

## Quick Start

```bash
# Install Buffrs from GitHub Releases or build it from source first

# Check for an update later
buffrs self-update --check-only

# Initialize a new API package
buffrs init --api

# Add a dependency
buffrs add --registry https://your.registry.com datatypes/user@^1.2.0

# Publish your package
buffrs publish
```

[Get started →](getting-started/index.md)

## Why Buffrs?

- **📦 Full SemVer support for gRPC APIs**: declare compatible version ranges instead of pinning everything by hand
- **🔀 Multi-version support for staged migrations**: install two API versions side by side when different consumers need different releases
- **🏷️ Package aliases for side-by-side versions**: name each dependency entry clearly when one manifest references multiple versions of the same API
- **🛠️ Build tool integration**: works with Cargo, CMake, Python, and other generated-code workflows

## Documentation

- **[Getting Started](getting-started/index.md)**: Install Buffrs and create your first package.
- **[Buffrs Guide](guide/index.md)**: Learn core concepts and best practices.
- **[Commands Reference](commands/index.md)**: Browse the full CLI reference.
- **[FAQ](faq.md)**: Find common questions and troubleshooting guidance.

---

**Need help?** Check the [FAQ](faq.md) or visit our [GitHub repository](https://github.com/globusmedical/ext-buffrs).
