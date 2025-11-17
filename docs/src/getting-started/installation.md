## Installation

### Install from crates.io

The easiest way to install `buffrs` is from [crates.io]:

```bash
cargo install buffrs
```

### Verify Installation

```bash
buffrs --version
```

### Authenticate with a Registry

To publish packages or access private dependencies, authenticate with your registry:

```bash
buffrs login --registry https://your-registry.example.com/artifactory
```

You'll be prompted for an authentication token. Contact your registry administrator for credentials.

> [!TIP]
> You can authenticate with multiple registries. Each registry uses separate credentials stored securely in your `~/.config/buffrs/credentials.toml`.

### Build from Source

Alternatively, clone the [Buffrs Repository] and build locally:

```bash
git clone https://github.com/globusmedical/buffrs
cd buffrs
cargo install --path .
```

---

**Next**: [First Steps with Buffrs](first-steps.md)

[crates.io]: https://crates.io
[Buffrs Repository]: https://github.com/globusmedical/buffrs
