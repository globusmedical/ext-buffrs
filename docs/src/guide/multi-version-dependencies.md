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

**Which dependencies need the flag?** Only the "outlier" dependency needs `resolver = "multiversion"`. For example, if six targets use `lib-algo-base@0.1.3` and one target uses `@0.1.2`, only the `@0.1.2` dependency needs the flag. The flag means "I accept coexisting with other versions of this package."

## Impact on C++ Code

**In most cases, no C++ code changes are required.**

Generated C++ code paths and namespaces come from the `package` declarations inside `.proto` files (e.g., `package gm.algo.base.v1;`), **not** from vendor directory names. This means:

| Scenario                          | C++ Code Changes? | Reason                                                          |
| --------------------------------- | ----------------- | --------------------------------------------------------------- |
| Same namespace, identical content | No                | Files are identical                                             |
| Same namespace, different content | N/A               | Build fails (`namespace_overlap = "forbidden"`)                 |
| Versioned namespaces (v1 vs v2)   | Maybe             | Different C++ namespaces; update includes if switching versions |

The vendor directory layout (`lib-algo-base@0.1.2/` vs `lib-algo-base@0.1.3/`) is an implementation detail that does not affect your `#include` statements or C++ namespace usage.

**When C++ code might need changes:**

- If you explicitly set `namespace_overlap = "allowed"` with different proto content, you risk ODR (One Definition Rule) violations at link time. This is strongly discouraged.
- If you're migrating from one API version to another (e.g., `v1` → `v2`), you'll update your code to use the new namespace regardless of multi-version resolution.

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

Buffrs emits metadata in `_buffrs_meta/` that build systems can use for link-unit validation:

- `graph.json` - Full dependency graph with versions, registries, relationships
- `namespaces.json` - Mapping from protobuf namespaces to packages
- `buffrs.cmake` - CMake variables for integration (data only)

### Link-Unit Validation

Build systems should implement their own `buffrs_validate_link_unit()` function to check whether a final binary links multiple targets with conflicting namespace sources. The validation logic:

1. Collect `BUFFRS_NAMESPACE_SOURCES` properties from all linked targets
2. For each namespace, verify all sources resolve to the same `package@version`
3. Fail at configure time if conflicts are detected

Example CMake implementation pattern:

```cmake
function(buffrs_validate_link_unit TARGET)
    # Collect namespace sources from target and all its dependencies
    # Check for conflicts (same namespace from different package@version)
    # Report error if conflicts found
endfunction()
```

This catches multi-version namespace conflicts at configure time rather than experiencing mysterious runtime failures.

## Limitations

- Multi-version resolution is per-`Proto.toml`, not global across a monorepo
- Build systems must implement their own link-unit validation using the emitted metadata
