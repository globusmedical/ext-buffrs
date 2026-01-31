# Multi-Version Dependencies

Starting with buffrs 0.50.0, you can opt-in to allow multiple versions of the same package to coexist in your dependency graph. This is useful in monorepo environments where different parts of your codebase may require different versions of the same protobuf package.

## The Problem

By default, buffrs enforces single-version resolution: each package name can only resolve to one version. This prevents diamond dependency conflicts but can be limiting in large codebases where:

- Different teams work on different features requiring different API versions
- Gradual migration between API versions is in progress
- Independent subgraphs legitimately need different versions

## Enabling Multi-Version

To allow multiple versions of a specific dependency, add `resolver = "multiversion"` to that dependency in your `Proto.toml`:

```toml
[dependencies.lib-algo-base]
version = "=0.1.2"
repository = "algo"
registry = "https://registry.example.com"
resolver = "multiversion"
```

This grants *permission* for buffrs to resolve multiple versions of `lib-algo-base` if the dependency constraints require it. It does not force duplicates—if a single version satisfies all constraints, only one version is resolved.

## Vendor Layout

When multi-version resolution results in multiple versions of the same package, buffrs uses version-qualified directory names:

```
proto/vendor/
├── lib-algo-base@0.1.2/
│   └── ...
├── lib-algo-base@0.1.3/
│   └── ...
└── other-package/
    └── ...
```

Packages without version conflicts continue to use simple directory names (e.g., `other-package/`).

## Namespace Overlap Policy

When multiple versions of a package exist, there's a risk that they declare the same protobuf `package` namespace. This can cause symbol collisions when linking C++ or other compiled code.

Buffrs provides a `namespace_overlap` policy to control this:

### `forbidden` (default)

Fail if two versions declare the same protobuf namespace:

```toml
[dependencies.my-package]
version = "=1.0.0"
resolver = "multiversion"
namespace_overlap = "forbidden"
```

### `identical_only`

Allow overlap only if the proto file content hashes match (i.e., the files are identical):

```toml
[dependencies.my-package]
version = "=1.0.0"
resolver = "multiversion"
namespace_overlap = "identical_only"
```

### `allowed`

Allow overlap unconditionally. **Use with caution**—this acknowledges that you understand the risk of symbol collisions:

```toml
[dependencies.my-package]
version = "=1.0.0"
resolver = "multiversion"
namespace_overlap = "allowed"
```

## Best Practices

1. **Use sparingly**: Multi-version should be the exception, not the rule. Prefer updating all consumers to a single version when possible.

2. **Version your proto namespaces**: If you need multiple API versions to coexist safely, consider versioning your protobuf `package` declarations:
   ```protobuf
   // v1
   package mycompany.api.v1;
   
   // v2
   package mycompany.api.v2;
   ```

3. **Audit your dependency graph**: Before enabling multi-version, understand why different versions are needed. Sometimes the root cause is an outdated transitive dependency that should be updated.

4. **Test thoroughly**: Multiple versions can introduce subtle runtime issues. Ensure your test coverage includes scenarios with multi-version dependencies.

## Monorepo Considerations

In monorepos with multiple `Proto.toml` files (e.g., one per CMake target), different manifests can independently resolve different versions. This is safe as long as the final linked binary doesn't include conflicting protobuf namespaces.

See the [CMake Integration](#cmake-integration) section for information on link-time safety checks.

## CMake Integration

> **Note**: CMake integration with link-unit validation is planned for a future release.

Buffrs will provide a `buffrs_validate_link_unit()` CMake function that checks whether your final binary links multiple versions with overlapping namespaces. This catch conflicts at configure time rather than at runtime.

## Limitations

- Multi-version resolution is per-`Proto.toml`, not global across a monorepo
- Namespace policy enforcement requires future metadata emission features
- CMake link-time validation is not yet implemented
