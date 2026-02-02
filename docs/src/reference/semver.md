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
version = "^1.2.3"
repository = "buffrs-local"
```

Buffrs supports the full range of semver version requirements:

### Caret Requirements (Recommended)

The caret (`^`) operator allows SemVer-compatible updates:

```toml
version = "^1.2.3"  # >=1.2.3, <2.0.0
version = "^0.2.3"  # >=0.2.3, <0.3.0 (0.x is special)
version = "^0.0.3"  # >=0.0.3, <0.0.4
```

This is the recommended default for most dependencies.

### Tilde Requirements

The tilde (`~`) operator allows patch-level updates only:

```toml
version = "~1.2.3"  # >=1.2.3, <1.3.0
version = "~1.2"    # >=1.2.0, <1.3.0
```

### Exact Requirements

Use `=` for exact version pinning:

```toml
version = "=1.2.3"  # Exactly 1.2.3
```

### Range Requirements

Comparison operators can be combined with commas:

```toml
version = ">=1.2.0"           # 1.2.0 or higher
version = ">1.2.0"            # Greater than 1.2.0
version = "<2.0.0"            # Less than 2.0.0
version = "<=2.0.0"           # 2.0.0 or lower
version = ">=1.2.0, <2.0.0"   # Between 1.2.0 and 2.0.0
```

### Wildcard Requirements

The wildcard (`*`) matches any version:

```toml
version = "*"       # Any version
version = "1.*"     # Any 1.x version
version = "1.2.*"   # Any 1.2.x version
```

### Pre-release Versions

Pre-release versions follow the SemVer format:

```toml
version = "=1.0.0-alpha.1"
version = "=2.0.0-beta.3"
version = "=3.0.0-rc.1"
```

Note: Pre-release versions are only matched when explicitly specified.
For example, `^1.0.0` will NOT match `1.1.0-alpha.1`.

## Version Resolution

When installing dependencies, buffrs:

1. Queries the registry for all available versions
2. Selects the highest version that satisfies the requirement
3. Records the exact resolved version in `Proto.lock`

Future installs prefer the locked version for reproducibility when it is still compatible
with all version constraints. If it is no longer compatible, buffrs resolves a new version
and updates `Proto.lock`.

## Multi-version Resolution

When [`resolver = "multiversion"`](resolver.md) is enabled for a dependency,
multiple versions of the same package can coexist. This is useful when:

- Transitively depending on different major versions
- Gradual migration between major versions

## Best Practices

1. **Use caret requirements**: `^1.2.3` allows compatible updates
2. **Start at 0.x**: Use `0.x.y` during initial development
3. **Bump major for breaking changes**: Any protobuf field removal or renumbering
4. **Bump minor for additions**: New messages, fields, or services
5. **Bump patch for fixes**: Documentation or generator bug fixes
6. **Pin exact versions sparingly**: Only when necessary for stability

## See Also

- [Specifying Dependencies](specifying-dependencies.md)
- [Resolver Modes](resolver.md)
