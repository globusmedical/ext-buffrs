# Multi-Version Dependencies

Starting with buffrs 1.0.0, you can opt-in to allow multiple versions of the same package to coexist in your dependency graph. This is useful in monorepo environments where different parts of your codebase may require different versions of the same protobuf package.

## The Problem

By default, buffrs enforces single-version resolution: each package name can only resolve to one version. This prevents diamond dependency conflicts but can be limiting in large codebases where:

- Different teams work on different features requiring different API versions
- Gradual migration between API versions is in progress
- Independent subgraphs legitimately need different versions

**Critically, consumers don't control upstream proto definitions.** When you depend on `lib-algo-base@0.1.2` and `lib-algo-base@0.1.3`, you cannot mandate that the upstream maintainer change their `package gm.algo.base;` declaration. Buffrs solves this by automatically rewriting proto namespaces when multi-version is enabled.

## Enabling Multi-Version

To allow multiple versions of a specific dependency, add `resolver = "multiversion"` to that dependency in your `Proto.toml`:

```toml
[dependencies]
# Old version we still need
lib-algo-base = { version = "=0.1.2", resolver = "multiversion" }

# New version we're migrating to
[dependencies.lib-algo-base-new]
package = "lib-algo-base"
version = "=0.1.3"
resolver = "multiversion"
```

This grants *permission* for buffrs to resolve multiple versions of `lib-algo-base` if the dependency constraints require it. It does not force duplicates—if a single version satisfies all constraints, only one version is resolved.

**Which dependencies need the flag?** All dependencies of the same package that may coexist need `resolver = "multiversion"`. The flag means "I accept coexisting with other versions of this package."

## Automatic Namespace Rewriting

When multi-version resolution results in multiple versions of the same package, buffrs automatically rewrites protobuf `package` declarations to include version information. This ensures generated code has unique symbols without requiring upstream changes.

### How It Works

When you run `buffrs install` with multi-version enabled, buffrs:

1. Detects packages that have multiple versions resolved
2. Rewrites `package` declarations to include a version suffix
3. Writes the modified files to the vendor directory

**Example transformation:**

```protobuf
// Original (in published package):
package gm.algo.base;

// After buffrs install (version 0.1.2):
package gm.algo.base._v0_1_2;

// After buffrs install (version 0.1.3-SPINE-4384):
package gm.algo.base._v0_1_3_SPINE_4384;
```

### Version Suffix Format

The version suffix follows the format `_v<major>_<minor>_<patch>` with special characters sanitized:

| Version           | Suffix                   |
| ----------------- | ------------------------ |
| `0.1.2`           | `_v0_1_2`                |
| `1.0.0`           | `_v1_0_0`                |
| `0.1.3-SPINE-4384`| `_v0_1_3_SPINE_4384`     |
| `2.0.0-beta.1`    | `_v2_0_0_beta_1`         |

## Impact on C++ Code

**With multi-version enabled, C++ code MUST use versioned namespaces.**

The rewritten proto packages result in unique C++ namespaces:

| Original Proto          | Version | Rewritten Proto                 | C++ Namespace              |
| ----------------------- | ------- | ------------------------------- | -------------------------- |
| `package gm.algo.base;` | 0.1.2   | `package gm.algo.base._v0_1_2;` | `gm::algo::base::_v0_1_2`  |
| `package gm.algo.base;` | 0.1.3   | `package gm.algo.base._v0_1_3;` | `gm::algo::base::_v0_1_3`  |

### Consumer Code Example

```cpp
#include <gm/algo/base/_v0_1_2/types.pb.h>
#include <gm/algo/base/_v0_1_3/types.pb.h>

// Option 1: Use full versioned namespace
void process_old(const gm::algo::base::_v0_1_2::SomeMessage& msg);
void process_new(const gm::algo::base::_v0_1_3::SomeMessage& msg);

// Option 2: Use namespace aliases (recommended)
namespace algo_old = gm::algo::base::_v0_1_2;
namespace algo_new = gm::algo::base::_v0_1_3;

void adapt(const algo_old::Request& old_req) {
    algo_new::Request new_req;
    // ... convert between versions
}
```

### Rust Consumer Code Example

```rust
// The generated Rust modules also use versioned names
mod gm {
    pub mod algo {
        pub mod base {
            pub mod _v0_1_2 {
                include!(concat!(env!("OUT_DIR"), "/gm.algo.base._v0_1_2.rs"));
            }
            pub mod _v0_1_3 {
                include!(concat!(env!("OUT_DIR"), "/gm.algo.base._v0_1_3.rs"));
            }
        }
    }
}

use gm::algo::base::_v0_1_2 as algo_old;
use gm::algo::base::_v0_1_3 as algo_new;
```

## Impact on Python Code

**Python projects using single-version dependencies are NOT affected.** Multi-version namespace rewriting ONLY occurs when multiple versions of the same package are actually resolved. If your Python project uses only one version of each dependency (which is the common case for isolated Python scripts), no changes are needed.

**With multi-version enabled, Python imports MUST use versioned module paths.**

The rewritten proto packages result in unique Python module paths:

| Original Proto          | Version | Rewritten Proto                 | Python Module Path                |
| ----------------------- | ------- | ------------------------------- | --------------------------------- |
| `package gm.algo.base;` | 0.1.2   | `package gm.algo.base._v0_1_2;` | `gm.algo.base._v0_1_2`            |
| `package gm.algo.base;` | 0.1.3   | `package gm.algo.base._v0_1_3;` | `gm.algo.base._v0_1_3`            |

### Single-Version Python (No Changes Required)

If your Python project only depends on one version of each API, nothing changes:

```python
# Proto.toml (single version - no multi-version flag needed)
# [dependencies]
# lib-algo-base = { version = "0.1.2", ... }

# Your Python code continues to work unchanged:
from gm.algo import base_pb2
msg = base_pb2.SomeMessage()
```

### Multi-Version Python Consumer Example

When using multi-version, Python imports must reference the versioned module:

```python
# Proto.toml has:
# lib-algo-base = { version = "=0.1.2", ..., resolver = "multiversion" }
# lib-algo-base-new = { package = "lib-algo-base", version = "=0.1.3", ..., resolver = "multiversion" }

# Import from versioned module paths
from gm.algo.base._v0_1_2 import base_pb2 as base_v012
from gm.algo.base._v0_1_3 import base_pb2 as base_v013

# Use with version-specific aliases
msg_old = base_v012.SomeMessage(value="old")
msg_new = base_v013.SomeMessage(value="new", extra="field")

# Convert between versions if needed
def upgrade_message(old_msg: base_v012.SomeMessage) -> base_v013.SomeMessage:
    return base_v013.SomeMessage(
        value=old_msg.value,
        extra="default"
    )
```

### Python gRPC Services with Multi-Version

```python
# gRPC stubs also follow the versioned module pattern
from gm.algo.base._v0_1_2 import service_pb2_grpc as service_v012
from gm.algo.base._v0_1_3 import service_pb2_grpc as service_v013

# Create clients for different API versions
channel = grpc.insecure_channel('localhost:50051')
client_old = service_v012.BaseServiceStub(channel)
client_new = service_v013.BaseServiceStub(channel)
```

### Running buffrs install for Python

Python projects typically run `buffrs install` as part of their setup. After installation, run the protobuf compiler to generate Python code:

```bash
# Install buffrs dependencies
buffrs install

# Generate Python code from proto files (example using grpc_tools)
python -m grpc_tools.protoc \
    -I proto \
    -I proto/vendor \
    --python_out=. \
    --grpc_python_out=. \
    proto/vendor/lib-algo-base@0.1.2/*.proto \
    proto/vendor/lib-algo-base@0.1.3/*.proto
```

The generated `_pb2.py` and `_pb2_grpc.py` files will be placed according to the rewritten package names, creating the versioned module structure automatically.

## Vendor Layout

When multi-version resolution results in multiple versions of the same package, buffrs uses version-qualified directory names:

```text
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

When multiple versions of a package exist, buffrs provides a `namespace_overlap` policy to control how conflicts are handled.

### `rewrite` (default)

Automatically rewrite `package` declarations to include version suffixes. This is the default and recommended policy:

```toml
[dependencies.my-package]
version = "=1.0.0"
resolver = "multiversion"
# namespace_overlap = "rewrite" is implied
```

### `identical_only`

Skip rewriting and allow overlap only if the proto file content hashes match (i.e., the files are identical):

```toml
[dependencies.my-package]
version = "=1.0.0"
resolver = "multiversion"
namespace_overlap = "identical_only"
```

This is useful when you know different versions share common base types with identical definitions.

### `forbidden`

Fail if multi-version would result in namespace overlap. Use this when you cannot adapt consumer code to use versioned namespaces:

```toml
[dependencies.my-package]
version = "=1.0.0"
resolver = "multiversion"
namespace_overlap = "forbidden"
```

## Best Practices

1. **Use sparingly**: Multi-version should be the exception, not the rule. Prefer updating all consumers to a single version when possible.

2. **Plan for versioned namespaces**: When enabling multi-version, anticipate that your C++, Rust, and Python code will need to use versioned namespace/module prefixes like `_v0_1_2`.

3. **Create namespace/module aliases**: Make your code cleaner with aliases:

   ```cpp
   // C++
   namespace algo_v1 = gm::algo::base::_v0_1_2;
   namespace algo_v2 = gm::algo::base::_v0_1_3;
   ```

   ```python
   # Python
   from gm.algo.base._v0_1_2 import base_pb2 as algo_v1
   from gm.algo.base._v0_1_3 import base_pb2 as algo_v2
   ```

4. **Audit your dependency graph**: Before enabling multi-version, understand why different versions are needed. Sometimes the root cause is an outdated transitive dependency that should be updated.

5. **Test thoroughly**: Multiple versions can introduce subtle runtime issues. Ensure your test coverage includes scenarios with multi-version dependencies.

6. **Isolated Python projects are safe**: If your Python project only uses one version of each dependency, multi-version changes won't affect you. The namespace rewriting only activates when multiple versions are actually resolved.

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
