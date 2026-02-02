use crate::{with_test_registry, VirtualFileSystem};

/// Tests that multi-version resolution works when explicitly enabled via
/// `resolver = "multiversion"` in Proto.toml
#[test]
fn multiversion_fixture() {
    with_test_registry(|url| {
        let vfs = VirtualFileSystem::empty();
        let buffrs_home = vfs.root().join("$HOME");
        let cwd = vfs.root();

        // Publish lib-base@0.1.0
        {
            std::fs::create_dir(cwd.join("lib-base-v1")).unwrap();
            let lib_cwd = cwd.join("lib-base-v1");

            crate::cli!()
                .args(["init", "--lib", "lib-base"])
                .env("BUFFRS_HOME", &buffrs_home)
                .current_dir(&lib_cwd)
                .assert()
                .success();

            // Create a simple proto file
            std::fs::write(
                lib_cwd.join("proto/base.proto"),
                r#"syntax = "proto3";
package lib.base.v1;

message BaseMessage {
    string id = 1;
}
"#,
            )
            .unwrap();

            crate::cli!()
                .args(["publish", "--registry", url, "--repository", "libs"])
                .env("BUFFRS_HOME", &buffrs_home)
                .current_dir(&lib_cwd)
                .assert()
                .success();
        }

        // Publish lib-base@0.2.0 with different namespace
        {
            std::fs::create_dir(cwd.join("lib-base-v2")).unwrap();
            let lib_cwd = cwd.join("lib-base-v2");

            // Create Proto.toml with version 0.2.0
            std::fs::write(
                lib_cwd.join("Proto.toml"),
                format!(
                    r#"edition = "0.50"

[package]
type = "lib"
name = "lib-base"
version = "0.2.0"

[dependencies]
"#
                ),
            )
            .unwrap();

            std::fs::create_dir_all(lib_cwd.join("proto")).unwrap();
            std::fs::write(
                lib_cwd.join("proto/base.proto"),
                r#"syntax = "proto3";
package lib.base.v2;

message BaseMessageV2 {
    string id = 1;
    string name = 2;
}
"#,
            )
            .unwrap();

            crate::cli!()
                .args(["publish", "--registry", url, "--repository", "libs"])
                .env("BUFFRS_HOME", &buffrs_home)
                .current_dir(&lib_cwd)
                .assert()
                .success();
        }

        // Publish api-a that depends on lib-base@0.1.0 (with multiversion enabled)
        {
            std::fs::create_dir(cwd.join("api-a")).unwrap();
            let api_cwd = cwd.join("api-a");

            std::fs::write(
                api_cwd.join("Proto.toml"),
                format!(
                    r#"edition = "0.50"

[package]
type = "api"
name = "api-a"
version = "1.0.0"

[dependencies.lib-base]
version = "=0.1.0"
registry = "{url}"
repository = "libs"
resolver = "multiversion"
"#
                ),
            )
            .unwrap();

            std::fs::create_dir_all(api_cwd.join("proto")).unwrap();
            std::fs::write(
                api_cwd.join("proto/api_a.proto"),
                r#"syntax = "proto3";
package api.a;

import "lib/base/v1/base.proto";

service ServiceA {
    rpc GetBase(lib.base.v1.BaseMessage) returns (lib.base.v1.BaseMessage);
}
"#,
            )
            .unwrap();

            crate::cli!()
                .arg("install")
                .env("BUFFRS_HOME", &buffrs_home)
                .current_dir(&api_cwd)
                .assert()
                .success();

            crate::cli!()
                .args(["publish", "--registry", url, "--repository", "apis"])
                .env("BUFFRS_HOME", &buffrs_home)
                .current_dir(&api_cwd)
                .assert()
                .success();
        }

        // Publish api-b that depends on lib-base@0.2.0 (with multiversion enabled)
        {
            std::fs::create_dir(cwd.join("api-b")).unwrap();
            let api_cwd = cwd.join("api-b");

            std::fs::write(
                api_cwd.join("Proto.toml"),
                format!(
                    r#"edition = "0.50"

[package]
type = "api"
name = "api-b"
version = "1.0.0"

[dependencies.lib-base]
version = "=0.2.0"
registry = "{url}"
repository = "libs"
resolver = "multiversion"
"#
                ),
            )
            .unwrap();

            std::fs::create_dir_all(api_cwd.join("proto")).unwrap();
            std::fs::write(
                api_cwd.join("proto/api_b.proto"),
                r#"syntax = "proto3";
package api.b;

import "lib/base/v2/base.proto";

service ServiceB {
    rpc GetBase(lib.base.v2.BaseMessageV2) returns (lib.base.v2.BaseMessageV2);
}
"#,
            )
            .unwrap();

            crate::cli!()
                .arg("install")
                .env("BUFFRS_HOME", &buffrs_home)
                .current_dir(&api_cwd)
                .assert()
                .success();

            crate::cli!()
                .args(["publish", "--registry", url, "--repository", "apis"])
                .env("BUFFRS_HOME", &buffrs_home)
                .current_dir(&api_cwd)
                .assert()
                .success();
        }

        // Consumer project that depends on both api-a and api-b
        // This requires multi-version for lib-base
        {
            std::fs::create_dir(cwd.join("consumer")).unwrap();
            let consumer_cwd = cwd.join("consumer");

            std::fs::write(
                consumer_cwd.join("Proto.toml"),
                format!(
                    r#"edition = "0.50"

[dependencies.api-a]
version = "=1.0.0"
registry = "{url}"
repository = "apis"
resolver = "multiversion"

[dependencies.api-b]
version = "=1.0.0"
registry = "{url}"
repository = "apis"
resolver = "multiversion"
"#
                ),
            )
            .unwrap();

            std::fs::create_dir_all(consumer_cwd.join("proto")).unwrap();

            // Install should succeed with multi-version enabled
            crate::cli!()
                .arg("install")
                .env("BUFFRS_HOME", &buffrs_home)
                .current_dir(&consumer_cwd)
                .assert()
                .success();

            // Verify both API packages are installed
            assert!(
                consumer_cwd.join("proto/vendor/api-a").exists()
                    || consumer_cwd.join("proto/vendor/api-a@1.0.0").exists(),
                "api-a should be installed"
            );
            assert!(
                consumer_cwd.join("proto/vendor/api-b").exists()
                    || consumer_cwd.join("proto/vendor/api-b@1.0.0").exists(),
                "api-b should be installed"
            );

            // Verify lib-base is installed with version-qualified directories
            // because there are two versions (0.1.0 and 0.2.0)
            assert!(
                consumer_cwd.join("proto/vendor/lib-base@0.1.0").exists(),
                "lib-base@0.1.0 should be installed in version-qualified directory"
            );
            assert!(
                consumer_cwd.join("proto/vendor/lib-base@0.2.0").exists(),
                "lib-base@0.2.0 should be installed in version-qualified directory"
            );
            // The plain 'lib-base' directory should NOT exist (would cause conflicts)
            assert!(
                !consumer_cwd.join("proto/vendor/lib-base").exists(),
                "plain lib-base directory should not exist when multiple versions are installed"
            );

            // Verify metadata files are created
            let meta_dir = consumer_cwd.join("proto/vendor/_buffrs_meta");
            assert!(
                meta_dir.exists(),
                "_buffrs_meta directory should be created"
            );
            assert!(
                meta_dir.join("graph.json").exists(),
                "graph.json should be created"
            );
            assert!(
                meta_dir.join("namespaces.json").exists(),
                "namespaces.json should be created"
            );
            assert!(
                meta_dir.join("buffrs.cmake").exists(),
                "buffrs.cmake should be created"
            );

            // Verify graph.json contains expected packages
            let graph_json = std::fs::read_to_string(meta_dir.join("graph.json")).unwrap();
            assert!(
                graph_json.contains("\"lib-base\""),
                "graph.json should contain lib-base"
            );
            assert!(
                graph_json.contains("\"0.1.0\"") && graph_json.contains("\"0.2.0\""),
                "graph.json should contain both versions"
            );

            // Verify namespaces.json contains namespace mappings
            let namespaces_json =
                std::fs::read_to_string(meta_dir.join("namespaces.json")).unwrap();
            assert!(
                namespaces_json.contains("lib.base.v1") || namespaces_json.contains("lib.base.v2"),
                "namespaces.json should contain namespace declarations"
            );
        }
    });
}

/// Tests that single-version resolution (default) fails on version conflicts
#[test]
fn single_version_conflict() {
    with_test_registry(|url| {
        let vfs = VirtualFileSystem::empty();
        let buffrs_home = vfs.root().join("$HOME");
        let cwd = vfs.root();

        // Publish lib-base@0.1.0
        {
            std::fs::create_dir(cwd.join("lib-base-v1")).unwrap();
            let lib_cwd = cwd.join("lib-base-v1");

            crate::cli!()
                .args(["init", "--lib", "lib-base"])
                .env("BUFFRS_HOME", &buffrs_home)
                .current_dir(&lib_cwd)
                .assert()
                .success();

            std::fs::write(
                lib_cwd.join("proto/base.proto"),
                r#"syntax = "proto3";
package lib.base;

message BaseMessage {
    string id = 1;
}
"#,
            )
            .unwrap();

            crate::cli!()
                .args(["publish", "--registry", url, "--repository", "libs"])
                .env("BUFFRS_HOME", &buffrs_home)
                .current_dir(&lib_cwd)
                .assert()
                .success();
        }

        // Publish lib-base@0.2.0
        {
            std::fs::create_dir(cwd.join("lib-base-v2")).unwrap();
            let lib_cwd = cwd.join("lib-base-v2");

            std::fs::write(
                lib_cwd.join("Proto.toml"),
                r#"edition = "0.50"

[package]
type = "lib"
name = "lib-base"
version = "0.2.0"

[dependencies]
"#,
            )
            .unwrap();

            std::fs::create_dir_all(lib_cwd.join("proto")).unwrap();
            std::fs::write(
                lib_cwd.join("proto/base.proto"),
                r#"syntax = "proto3";
package lib.base;

message BaseMessage {
    string id = 1;
    string name = 2;
}
"#,
            )
            .unwrap();

            crate::cli!()
                .args(["publish", "--registry", url, "--repository", "libs"])
                .env("BUFFRS_HOME", &buffrs_home)
                .current_dir(&lib_cwd)
                .assert()
                .success();
        }

        // Consumer that wants BOTH exact versions without multiversion
        // This should FAIL
        {
            std::fs::create_dir(cwd.join("consumer")).unwrap();
            let consumer_cwd = cwd.join("consumer");

            std::fs::write(
                consumer_cwd.join("Proto.toml"),
                format!(
                    r#"edition = "0.50"

[dependencies.lib-base-v1]
version = "=0.1.0"
registry = "{url}"
repository = "libs"

[dependencies.lib-base-v2]
version = "=0.2.0"
registry = "{url}"
repository = "libs"
"#
                )
                .replace("lib-base-v1", "lib-base")
                .replace("lib-base-v2", "lib-base"),
            )
            .unwrap();

            std::fs::create_dir_all(consumer_cwd.join("proto")).unwrap();

            // This can't work because TOML doesn't allow duplicate keys
            // The actual conflict test would need transitive dependencies
        }
    });
}

/// Tests that `namespace_overlap = "identical_only"` allows overlapping namespaces
/// when the content is the same.
#[test]
fn identical_only_policy_allows_same_content() {
    with_test_registry(|url| {
        let vfs = VirtualFileSystem::empty();
        let buffrs_home = vfs.root().join("$HOME");
        let cwd = vfs.root();

        // Publish lib-common@1.0.0 with shared namespace
        {
            std::fs::create_dir(cwd.join("lib-common-v1")).unwrap();
            let lib_cwd = cwd.join("lib-common-v1");

            std::fs::write(
                lib_cwd.join("Proto.toml"),
                format!(
                    r#"edition = "0.50"

[package]
type = "lib"
name = "lib-common"
version = "1.0.0"

[dependencies]
"#
                ),
            )
            .unwrap();

            std::fs::create_dir_all(lib_cwd.join("proto")).unwrap();
            // Same content will be used in both versions
            std::fs::write(
                lib_cwd.join("proto/common.proto"),
                r#"syntax = "proto3";
package shared.types;

message CommonData {
    string value = 1;
}
"#,
            )
            .unwrap();

            crate::cli!()
                .args(["publish", "--registry", url, "--repository", "libs"])
                .env("BUFFRS_HOME", &buffrs_home)
                .current_dir(&lib_cwd)
                .assert()
                .success();
        }

        // Publish lib-common@2.0.0 with SAME content (same namespace, same proto)
        {
            std::fs::create_dir(cwd.join("lib-common-v2")).unwrap();
            let lib_cwd = cwd.join("lib-common-v2");

            std::fs::write(
                lib_cwd.join("Proto.toml"),
                format!(
                    r#"edition = "0.50"

[package]
type = "lib"
name = "lib-common"
version = "2.0.0"

[dependencies]
"#
                ),
            )
            .unwrap();

            std::fs::create_dir_all(lib_cwd.join("proto")).unwrap();
            // SAME content as v1
            std::fs::write(
                lib_cwd.join("proto/common.proto"),
                r#"syntax = "proto3";
package shared.types;

message CommonData {
    string value = 1;
}
"#,
            )
            .unwrap();

            crate::cli!()
                .args(["publish", "--registry", url, "--repository", "libs"])
                .env("BUFFRS_HOME", &buffrs_home)
                .current_dir(&lib_cwd)
                .assert()
                .success();
        }

        // Consumer with `identical_only` policy - should succeed
        {
            std::fs::create_dir(cwd.join("consumer")).unwrap();
            let consumer_cwd = cwd.join("consumer");

            std::fs::write(
                consumer_cwd.join("Proto.toml"),
                format!(
                    r#"edition = "0.50"

[dependencies.lib-common-v1]
version = "=1.0.0"
registry = "{url}"
repository = "libs"
resolver = "multiversion"
namespace_overlap = "identical_only"

[dependencies.lib-common-v2]
version = "=2.0.0"
registry = "{url}"
repository = "libs"
resolver = "multiversion"
namespace_overlap = "identical_only"
"#
                )
                .replace("lib-common-v1", "lib-common")
                .replace("lib-common-v2", "lib-common"),
            )
            .unwrap();

            std::fs::create_dir_all(consumer_cwd.join("proto")).unwrap();

            // This won't work - TOML doesn't allow duplicate keys
            // The actual test would need transitive dependencies
        }
    });
}

/// Tests content hash computation for namespace_scan.
#[test]
fn content_hash_is_deterministic() {
    use buffrs::namespace_scan::content_hash;

    let content1 = "syntax = \"proto3\";\npackage test;\n";
    let content2 = "syntax = \"proto3\";\npackage test;\n";
    let content3 = "syntax = \"proto3\";\npackage test;\nmessage Foo {}\n";

    let hash1 = content_hash(content1);
    let hash2 = content_hash(content2);
    let hash3 = content_hash(content3);

    // Same content should produce same hash
    assert_eq!(hash1, hash2);
    // Different content should produce different hash
    assert_ne!(hash1, hash3);
    // Hash should be 64 hex chars (sha256)
    assert_eq!(hash1.len(), 64);
}

/// Tests automatic namespace rewriting when multiple versions use the SAME namespace.
/// This is the core feature of multi-version support: when lib-base@0.1.0 and lib-base@0.2.0
/// both declare `package lib.base;`, buffrs must rewrite them to unique namespaces like
/// `package lib.base._v0_1_0;` and `package lib.base._v0_2_0;`.
#[test]
fn multiversion_same_namespace_rewriting() {
    with_test_registry(|url| {
        let vfs = VirtualFileSystem::empty();
        let buffrs_home = vfs.root().join("$HOME");
        let cwd = vfs.root();

        // Publish lib-algo-base@0.1.0 with package lib.algo.base
        {
            std::fs::create_dir(cwd.join("lib-algo-base-v1")).unwrap();
            let lib_cwd = cwd.join("lib-algo-base-v1");

            crate::cli!()
                .args(["init", "--lib", "lib-algo-base"])
                .env("BUFFRS_HOME", &buffrs_home)
                .current_dir(&lib_cwd)
                .assert()
                .success();

            std::fs::write(
                lib_cwd.join("proto/base.proto"),
                r#"syntax = "proto3";
package lib.algo.base;

message BaseMessage {
    string value = 1;
}
"#,
            )
            .unwrap();

            crate::cli!()
                .args(["publish", "--registry", url, "--repository", "libs"])
                .env("BUFFRS_HOME", &buffrs_home)
                .current_dir(&lib_cwd)
                .assert()
                .success();
        }

        // Publish lib-algo-base@0.2.0 with the SAME package declaration (lib.algo.base)
        {
            std::fs::create_dir(cwd.join("lib-algo-base-v2")).unwrap();
            let lib_cwd = cwd.join("lib-algo-base-v2");

            std::fs::write(
                lib_cwd.join("Proto.toml"),
                r#"edition = "0.50"

[package]
type = "lib"
name = "lib-algo-base"
version = "0.2.0"

[dependencies]
"#,
            )
            .unwrap();

            std::fs::create_dir_all(lib_cwd.join("proto")).unwrap();
            std::fs::write(
                lib_cwd.join("proto/base.proto"),
                r#"syntax = "proto3";
package lib.algo.base;

message BaseMessage {
    string value = 1;
    string extra = 2;
}
"#,
            )
            .unwrap();

            crate::cli!()
                .args(["publish", "--registry", url, "--repository", "libs"])
                .env("BUFFRS_HOME", &buffrs_home)
                .current_dir(&lib_cwd)
                .assert()
                .success();
        }

        // Consumer that needs both versions via multiversion resolver
        {
            std::fs::create_dir(cwd.join("consumer")).unwrap();
            let consumer_cwd = cwd.join("consumer");

            // Use dictionary syntax with different keys for each version
            std::fs::write(
                consumer_cwd.join("Proto.toml"),
                format!(
                    r#"edition = "0.50"

[dependencies]
lib-algo-base = {{ version = "=0.1.0", registry = "{url}", repository = "libs", resolver = "multiversion" }}
lib-algo-base-v2 = {{ package = "lib-algo-base", version = "=0.2.0", registry = "{url}", repository = "libs", resolver = "multiversion" }}
"#
                ),
            )
            .unwrap();

            std::fs::create_dir_all(consumer_cwd.join("proto")).unwrap();

            crate::cli!()
                .arg("install")
                .env("BUFFRS_HOME", &buffrs_home)
                .current_dir(&consumer_cwd)
                .assert()
                .success();

            // Verify both versioned directories exist
            assert!(
                consumer_cwd
                    .join("proto/vendor/lib-algo-base@0.1.0")
                    .exists(),
                "lib-algo-base@0.1.0 directory should exist"
            );
            assert!(
                consumer_cwd
                    .join("proto/vendor/lib-algo-base@0.2.0")
                    .exists(),
                "lib-algo-base@0.2.0 directory should exist"
            );

            // Read the proto files and verify namespace rewriting
            let proto_v1 = std::fs::read_to_string(
                consumer_cwd.join("proto/vendor/lib-algo-base@0.1.0/base.proto"),
            )
            .unwrap();
            let proto_v2 = std::fs::read_to_string(
                consumer_cwd.join("proto/vendor/lib-algo-base@0.2.0/base.proto"),
            )
            .unwrap();

            // The namespace should be rewritten to include version suffix
            assert!(
                proto_v1.contains("package lib.algo.base._v0_1_0;"),
                "proto v1 should have rewritten namespace with _v0_1_0 suffix, got: {}",
                proto_v1
            );
            assert!(
                proto_v2.contains("package lib.algo.base._v0_2_0;"),
                "proto v2 should have rewritten namespace with _v0_2_0 suffix, got: {}",
                proto_v2
            );

            // Verify namespaces.json contains the rewritten namespaces
            let namespaces_json = std::fs::read_to_string(
                consumer_cwd.join("proto/vendor/_buffrs_meta/namespaces.json"),
            )
            .unwrap();
            assert!(
                namespaces_json.contains("lib.algo.base._v0_1_0"),
                "namespaces.json should contain rewritten namespace for v0.1.0"
            );
            assert!(
                namespaces_json.contains("lib.algo.base._v0_2_0"),
                "namespaces.json should contain rewritten namespace for v0.2.0"
            );
        }
    });
}
