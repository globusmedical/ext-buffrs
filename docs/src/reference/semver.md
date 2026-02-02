# SemVer Compatibility

Buffrs follows [Semantic Versioning 2.0.0](https://semver.org/) for package
versions. This document describes how versioning works in buffrs.

## Version Format

A version consists of three components: `MAJOR.MINOR.PATCH`

- **MAJOR**: Incompatible API changes
- **MINOR**: Backwards-compatible functionality additions
- **PATCH**: Backwards-compatible bug fixes

Examples: `1.0.0`, `2.3.4`, `0.1.0`

## Version Requirements

In `Proto.toml`, dependencies specify version requirements:

```toml
[dependencies.my-api]
version = "1.2.3"
repository = "buffrs-local"
```

### Exact Versions

Currently, buffrs only supports exact version pinning:

```toml
version = "1.2.3"  # Exactly 1.2.3
```

### Pre-release Versions

Pre-release versions follow the SemVer format:

```toml
version = "1.0.0-alpha.1"
version = "2.0.0-beta.3"
version = "3.0.0-rc.1"
```

## Multi-version Resolution

When [`resolver = "multiversion"`](resolver.md) is enabled for a dependency,
multiple versions of the same package can coexist. This is useful when:

- Transitively depending on different major versions
- Gradual migration between major versions

## Best Practices

1. **Start at 0.x**: Use `0.x.y` during initial development
2. **Bump major for breaking changes**: Any protobuf field removal or renumbering
3. **Bump minor for additions**: New messages, fields, or services
4. **Bump patch for fixes**: Documentation or generator bug fixes

## See Also

- [Specifying Dependencies](specifying-dependencies.md)
- [Resolver Modes](resolver.md)
