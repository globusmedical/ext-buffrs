## Editions

Editions of buffrs mark a specific evolutionary state of the package manager.
The edition system exists to allow for fast development of buffrs while
allowing you to already migrate existing protobufs to buffrs even though it
has not yet reached a stable version.

Editions can be either explicitly stated in the `Proto.toml` or are
automatically inlined once a package is created using buffrs. This ensures that
you dont need to care about them as a user but get the benefits.

> Note: If you release a package with an edition that is incompatible with
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
      unsupported manifest edition, supported editions of 0.50.0 are: 0.50
```

### Edition Compatibility

Buffrs 0.50.0 supports the following editions:

| Edition | Buffrs Version | Notes                                      |
| ------- | -------------- | ------------------------------------------ |
| `0.50`  | 0.50.x         | Current edition with multi-version support |
| `0.10`  | 0.10.x         | Previous stable edition                    |
| `0.9`   | 0.9.x          | Legacy edition                             |
| `0.8`   | 0.8.x          | Legacy edition                             |
| `0.7`   | 0.7.x          | Legacy edition                             |

### Canary Editions

```toml
edition = "0.50"
```

Canary editions are short-lived editions that are attached to a specific
minor release of buffrs in the `0.x.x` version range. The edition name contains
the minor version it is usable for. E.g. the edition `0.50` is usable /
supported by all `0.50.x` buffrs releases. Compatibility beyond minor releases
is not guaranteed as fundamental breaking changes may be introduced between
editions.

### Edition 0.50 Features

Edition 0.50 introduces:

- **Per-dependency multi-version support**: Use `resolver = "multiversion"` to allow multiple versions of the same package
- **Namespace overlap policies**: Control how proto namespace collisions are handled with `namespace_overlap`
- **Version-qualified vendor directories**: Multi-version packages use `name@version/` directory format

See [Multi-Version Dependencies](../guide/multi-version-dependencies.md) for details.
