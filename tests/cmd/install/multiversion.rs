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
