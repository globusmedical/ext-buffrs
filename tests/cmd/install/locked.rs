//! `buffrs install` with a `Proto.lock`: which registry requests it makes, and what it does when
//! the registry throttles them.

use std::path::Path;

use crate::{with_probed_test_registry, VirtualFileSystem};

const REPOSITORY: &str = "protos";

fn write_config(project_dir: &Path) {
    std::fs::create_dir_all(project_dir.join(".buffrs")).unwrap();
    std::fs::write(
        project_dir.join(".buffrs/config.toml"),
        "edition = \"0.50\"\n",
    )
    .unwrap();
}

fn install(project_dir: &Path, buffrs_home: &Path) {
    crate::cli!()
        .arg("install")
        .env("BUFFRS_HOME", buffrs_home)
        .current_dir(project_dir)
        .assert()
        .success();
}

/// Publishes `{name}@{version}` with the given `[dependencies]` body from a fresh directory.
fn publish(
    root: &Path,
    buffrs_home: &Path,
    registry_url: &str,
    kind: &str,
    name: &str,
    version: &str,
    dependencies: &str,
) {
    let package_dir = root.join(format!("{name}-{version}"));
    std::fs::create_dir(&package_dir).unwrap();
    write_config(&package_dir);
    std::fs::write(
        package_dir.join("Proto.toml"),
        format!(
            r#"edition = "0.10"

[package]
type = "{kind}"
name = "{name}"
version = "{version}"

[dependencies]
{dependencies}
"#
        ),
    )
    .unwrap();

    let proto_package = name.replace('-', ".");
    std::fs::create_dir_all(package_dir.join("proto")).unwrap();
    std::fs::write(
        package_dir.join("proto/messages.proto"),
        format!(
            r#"syntax = "proto3";
package {proto_package};

message Marker {{
    string id = 1;
}}
"#
        ),
    )
    .unwrap();

    install(&package_dir, buffrs_home);

    crate::cli!()
        .args([
            "publish",
            "--registry",
            registry_url,
            "--repository",
            REPOSITORY,
        ])
        .env("BUFFRS_HOME", buffrs_home)
        .current_dir(&package_dir)
        .assert()
        .success();
}

fn remote(name: &str, version_req: &str, registry_url: &str) -> String {
    format!(
        r#"{name} = {{ version = "{version_req}", registry = "{registry_url}", repository = "{REPOSITORY}" }}"#
    )
}

/// Creates a consumer project with the given `[dependencies]` body and returns its directory.
fn consumer(root: &Path, dependencies: &str) -> std::path::PathBuf {
    let consumer_dir = root.join("consumer");
    std::fs::create_dir(&consumer_dir).unwrap();
    write_config(&consumer_dir);
    write_consumer_manifest(&consumer_dir, dependencies);
    std::fs::create_dir_all(consumer_dir.join("proto")).unwrap();
    consumer_dir
}

fn write_consumer_manifest(consumer_dir: &Path, dependencies: &str) {
    std::fs::write(
        consumer_dir.join("Proto.toml"),
        format!("edition = \"0.10\"\n\n[dependencies]\n{dependencies}\n"),
    )
    .unwrap();
}

fn is_listing(path: &str) -> bool {
    path.contains("/api/storage/")
}

fn is_download(path: &str) -> bool {
    Path::new(path)
        .extension()
        .is_some_and(|extension| extension == "tgz")
}

fn locked_version(consumer_dir: &Path, name: &str) -> String {
    let lock: toml::Value =
        toml::from_str(&std::fs::read_to_string(consumer_dir.join("Proto.lock")).unwrap()).unwrap();

    lock.get("packages")
        .and_then(toml::Value::as_array)
        .unwrap()
        .iter()
        .find(|package| package.get("name").and_then(toml::Value::as_str) == Some(name))
        .and_then(|package| package.get("version"))
        .and_then(toml::Value::as_str)
        .unwrap_or_else(|| panic!("{name} should be present in Proto.lock"))
        .to_owned()
}

/// Publishes `lib-shared` 0.1.0 and 0.1.1 and an `api-a` 0.1.0 that accepts either.
fn publish_api_with_two_lib_versions(root: &Path, buffrs_home: &Path, registry_url: &str) {
    publish(
        root,
        buffrs_home,
        registry_url,
        "lib",
        "lib-shared",
        "0.1.0",
        "",
    );
    publish(
        root,
        buffrs_home,
        registry_url,
        "lib",
        "lib-shared",
        "0.1.1",
        "",
    );
    publish(
        root,
        buffrs_home,
        registry_url,
        "api",
        "api-a",
        "0.1.0",
        &remote("lib-shared", "^0.1", registry_url),
    );
}

#[test]
fn current_lockfile_installs_without_listing_versions() {
    with_probed_test_registry(|registry_url, probe| {
        let vfs = VirtualFileSystem::empty();
        let buffrs_home = vfs.root().join("$HOME");
        let root = vfs.root();

        publish_api_with_two_lib_versions(&root, &buffrs_home, registry_url);
        let consumer_dir = consumer(&root, &remote("api-a", "^0.1", registry_url));

        // Without a lockfile the resolver has to ask the registry which versions exist.
        probe.clear();
        install(&consumer_dir, &buffrs_home);
        assert!(probe.get_requests().iter().any(|path| is_listing(path)));
        assert_eq!(locked_version(&consumer_dir, "lib-shared"), "0.1.1");
        let lock = std::fs::read_to_string(consumer_dir.join("Proto.lock")).unwrap();

        // Lockfile current, packages cached: no request at all.
        probe.clear();
        install(&consumer_dir, &buffrs_home);
        assert_eq!(probe.get_requests(), Vec::<String>::new());

        // Lockfile current, cache empty: one download per locked package and no listing.
        std::fs::remove_dir_all(buffrs_home.join(".buffrs/cache")).unwrap();
        probe.clear();
        install(&consumer_dir, &buffrs_home);
        let requests = probe.get_requests();
        assert!(
            !requests.iter().any(|path| is_listing(path)),
            "unexpected version listing in {requests:?}"
        );
        let mut downloads: Vec<_> = requests.iter().filter(|path| is_download(path)).collect();
        downloads.sort();
        assert_eq!(
            downloads,
            [
                &format!("registry/{REPOSITORY}/api-a/api-a-0.1.0.tgz"),
                &format!("registry/{REPOSITORY}/lib-shared/lib-shared-0.1.1.tgz"),
            ]
        );

        assert_eq!(
            lock,
            std::fs::read_to_string(consumer_dir.join("Proto.lock")).unwrap()
        );
    });
}

#[test]
fn stale_lockfile_falls_back_to_the_registry() {
    with_probed_test_registry(|registry_url, _| {
        let vfs = VirtualFileSystem::empty();
        let buffrs_home = vfs.root().join("$HOME");
        let root = vfs.root();

        publish_api_with_two_lib_versions(&root, &buffrs_home, registry_url);
        publish(
            &root,
            &buffrs_home,
            registry_url,
            "api",
            "api-b",
            "0.1.0",
            &remote("lib-shared", "=0.1.0", registry_url),
        );

        let api_a = remote("api-a", "^0.1", registry_url);
        let consumer_dir = consumer(&root, &api_a);
        install(&consumer_dir, &buffrs_home);
        assert_eq!(locked_version(&consumer_dir, "lib-shared"), "0.1.1");

        // api-b pins lib-shared to 0.1.0, which the locked 0.1.1 cannot satisfy.
        let api_b = remote("api-b", "^0.1", registry_url);
        write_consumer_manifest(&consumer_dir, &format!("{api_a}\n{api_b}"));
        install(&consumer_dir, &buffrs_home);
        assert_eq!(locked_version(&consumer_dir, "lib-shared"), "0.1.0");
    });
}

#[test]
fn throttled_requests_are_retried() {
    with_probed_test_registry(|registry_url, probe| {
        let vfs = VirtualFileSystem::empty();
        let buffrs_home = vfs.root().join("$HOME");
        let root = vfs.root();

        publish_api_with_two_lib_versions(&root, &buffrs_home, registry_url);
        let consumer_dir = consumer(&root, &remote("api-a", "^0.1", registry_url));

        probe.clear();
        probe.throttle_next(3);
        install(&consumer_dir, &buffrs_home);

        // The first request is rejected three times before the registry serves it.
        let requests = probe.get_requests();
        assert_eq!(requests[..4], vec![requests[0].clone(); 4]);
        assert_eq!(locked_version(&consumer_dir, "lib-shared"), "0.1.1");
    });
}
