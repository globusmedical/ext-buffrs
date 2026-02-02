## Editions

Editions of buffrs mark a specific evolutionary state of the `Proto.toml` manifest format.
The edition system exists to allow for format evolution while maintaining backward
compatibility with existing packages.

Editions can be either explicitly stated in the `Proto.toml` or are
automatically inlined once a package is created using buffrs. This ensures that
you dont need to care about them as a user but get the benefits.

> Note: The proto.toml edition is independent of the buffrs crate version.
> The edition only changes when the Proto.toml format itself changes.
> If you release a package with an edition that is incompatible with
> another one (e.g. if `0.7` is incompatible with `0.8`) you will need to
> re-release the package for the new edition (by bumping the version, or
> overriding the existing package) to regain compatibility.

You may see errors like this if you try to consume (or work on) a package of
another edition.

```
Error:   × could not deserialize Proto.toml
  ╰─▶ TOML parse error at line 1, column 1
        |
      1 | edition = "0.7"
        | ^^^^^^^^^^^^^^^
      unsupported manifest edition, supported editions of 1.0.0 are: 0.50
```

### Edition Compatibility

Buffrs 1.0.0 supports the following editions:

| Edition | Notes                                      |
| ------- | ------------------------------------------ |
| `0.50`  | Current edition with multi-version support |
| `0.10`  | Previous stable edition                    |
| `0.9`   | Legacy edition                             |
| `0.8`   | Legacy edition                             |
| `0.7`   | Legacy edition                             |

### Current Edition

```toml
edition = "0.50"
```

The proto.toml edition is independent of the buffrs program version. Edition `0.50`
introduces multi-version dependencies in buffrs 1.0.0.

### Buffrs 1.0.0 Features

Buffrs 1.0.0 introduces:

- **Per-dependency multi-version support**: Use `resolver = "multiversion"` to allow multiple versions of the same package
- **Namespace overlap policies**: Control how proto namespace collisions are handled with `namespace_overlap`
- **Version-qualified vendor directories**: Multi-version packages use `name@version/` directory format

See [Multi-Version Dependencies](../guide/multi-version-dependencies.md) for details.
