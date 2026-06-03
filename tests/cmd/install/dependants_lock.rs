use crate::{with_test_registry, VirtualFileSystem};
use std::path::Path;
use toml::Value;

fn write_config(project_dir: &Path) {
    std::fs::create_dir_all(project_dir.join(".buffrs")).unwrap();
    std::fs::write(
        project_dir.join(".buffrs/config.toml"),
        r#"edition = "0.10"
"#,
    )
    .unwrap();
}

fn package_dependants(lock_contents: &str, name: &str, version: &str) -> i64 {
    let lock: Value = toml::from_str(lock_contents).unwrap();
    let package = lock
        .get("packages")
        .and_then(Value::as_array)
        .unwrap()
        .iter()
        .find(|package| {
            package.get("name").and_then(Value::as_str) == Some(name)
                && package.get("version").and_then(Value::as_str) == Some(version)
        })
        .unwrap_or_else(|| panic!("package {name}@{version} should be present in Proto.lock"));

    package
        .get("dependants")
        .and_then(Value::as_integer)
        .unwrap()
}

fn publish_lib_shared(root: &Path, buffrs_home: &Path, registry_url: &str) {
    let package_dir = root.join("lib-shared");
    std::fs::create_dir(&package_dir).unwrap();
    write_config(&package_dir);
    std::fs::write(
        package_dir.join("Proto.toml"),
        r#"edition = "0.10"

[package]
type = "lib"
name = "lib-shared"
version = "0.1.0"

[dependencies]
"#,
    )
    .unwrap();
    std::fs::create_dir_all(package_dir.join("proto")).unwrap();
    std::fs::write(
        package_dir.join("proto/shared.proto"),
        r#"syntax = "proto3";
package lib.shared;

message SharedMessage {
    string id = 1;
}
"#,
    )
    .unwrap();

    crate::cli!()
        .args([
            "publish",
            "--registry",
            registry_url,
            "--repository",
            "libs",
        ])
        .env("BUFFRS_HOME", buffrs_home)
        .current_dir(&package_dir)
        .assert()
        .success();
}

fn publish_api_a_with_duplicate_alias_edges(root: &Path, buffrs_home: &Path, registry_url: &str) {
    let package_dir = root.join("api-a");
    std::fs::create_dir(&package_dir).unwrap();
    write_config(&package_dir);
    std::fs::write(
        package_dir.join("Proto.toml"),
        format!(
            r#"edition = "0.10"

[package]
type = "api"
name = "api-a"
version = "0.1.0"

[dependencies]
lib-shared = {{ version = "=0.1.0", registry = "{registry_url}", repository = "libs" }}
lib-shared-alias = {{ package = "lib-shared", version = "=0.1.0", registry = "{registry_url}", repository = "libs" }}
"#
        ),
    )
    .unwrap();
    std::fs::create_dir_all(package_dir.join("proto")).unwrap();
    std::fs::write(
        package_dir.join("proto/api_a.proto"),
        r#"syntax = "proto3";
package api.a;

service ApiA {}
"#,
    )
    .unwrap();

    crate::cli!()
        .arg("install")
        .env("BUFFRS_HOME", buffrs_home)
        .current_dir(&package_dir)
        .assert()
        .success();

    crate::cli!()
        .args([
            "publish",
            "--registry",
            registry_url,
            "--repository",
            "apis",
        ])
        .env("BUFFRS_HOME", buffrs_home)
        .current_dir(&package_dir)
        .assert()
        .success();
}

fn publish_api_b(root: &Path, buffrs_home: &Path, registry_url: &str) {
    let package_dir = root.join("api-b");
    std::fs::create_dir(&package_dir).unwrap();
    write_config(&package_dir);
    std::fs::write(
        package_dir.join("Proto.toml"),
        format!(
            r#"edition = "0.10"

[package]
type = "api"
name = "api-b"
version = "0.1.0"

[dependencies]
lib-shared = {{ version = "=0.1.0", registry = "{registry_url}", repository = "libs" }}
"#
        ),
    )
    .unwrap();
    std::fs::create_dir_all(package_dir.join("proto")).unwrap();
    std::fs::write(
        package_dir.join("proto/api_b.proto"),
        r#"syntax = "proto3";
package api.b;

service ApiB {}
"#,
    )
    .unwrap();

    crate::cli!()
        .arg("install")
        .env("BUFFRS_HOME", buffrs_home)
        .current_dir(&package_dir)
        .assert()
        .success();

    crate::cli!()
        .args([
            "publish",
            "--registry",
            registry_url,
            "--repository",
            "apis",
        ])
        .env("BUFFRS_HOME", buffrs_home)
        .current_dir(&package_dir)
        .assert()
        .success();
}

#[test]
fn duplicate_alias_edges_do_not_inflate_lockfile_dependants() {
    with_test_registry(|registry_url| {
        let vfs = VirtualFileSystem::empty();
        let buffrs_home = vfs.root().join("$HOME");
        let root = vfs.root();

        publish_lib_shared(&root, &buffrs_home, registry_url);
        publish_api_a_with_duplicate_alias_edges(&root, &buffrs_home, registry_url);
        publish_api_b(&root, &buffrs_home, registry_url);

        let consumer_dir = root.join("consumer");
        std::fs::create_dir(&consumer_dir).unwrap();
        write_config(&consumer_dir);
        std::fs::write(
            consumer_dir.join("Proto.toml"),
            format!(
                r#"edition = "0.10"

[dependencies]
api-a = {{ version = "=0.1.0", registry = "{registry_url}", repository = "apis" }}
api-b = {{ version = "=0.1.0", registry = "{registry_url}", repository = "apis" }}
"#
            ),
        )
        .unwrap();
        std::fs::create_dir_all(consumer_dir.join("proto")).unwrap();

        crate::cli!()
            .arg("install")
            .env("BUFFRS_HOME", &buffrs_home)
            .current_dir(&consumer_dir)
            .assert()
            .success();

        let first_lock = std::fs::read_to_string(consumer_dir.join("Proto.lock")).unwrap();

        assert_eq!(package_dependants(&first_lock, "api-a", "0.1.0"), 1);
        assert_eq!(package_dependants(&first_lock, "api-b", "0.1.0"), 1);
        assert_eq!(package_dependants(&first_lock, "lib-shared", "0.1.0"), 2);

        crate::cli!()
            .arg("install")
            .env("BUFFRS_HOME", &buffrs_home)
            .current_dir(&consumer_dir)
            .assert()
            .success();

        let second_lock = std::fs::read_to_string(consumer_dir.join("Proto.lock")).unwrap();
        assert_eq!(first_lock, second_lock);
    });
}
