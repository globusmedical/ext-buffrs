# Installation

## Install from GitHub Releases

Download the appropriate archive for your platform from [GitHub Releases], extract it, and place the `buffrs` binary on your `PATH`.

Use this option if you want the published fork binary exactly as released internally.

## Build from Source

Alternatively, clone the [Buffrs Repository] and build locally:

```bash
git clone https://github.com/globusmedical/ext-buffrs
cd ext-buffrs
cargo install --path .
```

## Update an Existing Installation

If `buffrs` is already installed, use the built-in updater to check for and install the latest published release:

```bash
buffrs self-update --check-only
buffrs self-update
```

## Verify Installation

```bash
buffrs --version
```

## Authenticate with a Registry

To publish packages or access private dependencies, authenticate with your registry:

```bash
buffrs login --registry https://your-registry.example.com/artifactory
```

You'll be prompted for an authentication token. Contact your registry administrator for credentials.

> [!TIP]
> You can authenticate with multiple registries. Each registry uses separate credentials stored securely in your `~/.config/buffrs/credentials.toml`.

---

**Next**: [First Steps with Buffrs](first-steps.md)

[GitHub Releases]: https://github.com/globusmedical/ext-buffrs/releases
[Buffrs Repository]: https://github.com/globusmedical/ext-buffrs
