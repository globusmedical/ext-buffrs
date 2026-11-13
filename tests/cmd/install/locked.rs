//! `buffrs install` with a `Proto.lock`: which registry requests it makes, and what it does when
//! the registry throttles them.

use std::path::Path;

use crate::{with_probed_test_registry, VirtualFileSystem};

const REPOSITORY: &str = "protos";

fn write_config(project_dir: &Path) {
    write_config_text(project_dir, "edition = \"0.50\"\n");
}

/// Writes a config of the given edition whose default registry is the alias `local`.
fn write_alias_config(project_dir: &Path, edition: &str, registry_url: &str) {
    write_config_text(
        project_dir,
        &format!(
            r#"edition = "{edition}"

[registry]
default = "local"

[registries]
local = "{registry_url}"
"#
        ),
    );
}

fn write_config_text(project_dir: &Path, config: &str) {
    std::fs::create_dir_all(project_dir.join(".buffrs")).unwrap();
    std::fs::write(project_dir.join(".buffrs/config.toml"), config).unwrap();
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
    publish_to(
        root,
        buffrs_home,
        registry_url,
        REPOSITORY,
        (kind, name, version),
        dependencies,
    );
}

/// Like [`publish`], into the given repository. `package` is the kind, name and version.
fn publish_to(
    root: &Path,
    buffrs_home: &Path,
    registry_url: &str,
    repository: &str,
    package: (&str, &str, &str),
    dependencies: &str,
) {
    let (kind, name, version) = package;
    let package_dir = root.join(format!("{repository}-{name}-{version}"));
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
            repository,
        ])
        .env("BUFFRS_HOME", buffrs_home)
        .current_dir(&package_dir)
        .assert()
        .success();
}

fn install_fails(project_dir: &Path, buffrs_home: &Path) {
    crate::cli!()
        .arg("install")
        .env("BUFFRS_HOME", buffrs_home)
        .current_dir(project_dir)
        .assert()
        .failure();
}

/// A dependency on the default registry, which the config names by alias.
fn remote_by_alias(name: &str, version_req: &str) -> String {
    format!(r#"{name} = {{ version = "{version_req}", repository = "{REPOSITORY}" }}"#)
}

fn artifact_path(name: &str, version: &str) -> String {
    format!("registry/{REPOSITORY}/{name}/{name}-{version}.tgz")
}

fn sorted_downloads(requests: &[String]) -> Vec<String> {
    let mut downloads: Vec<_> = requests
        .iter()
        .filter(|path| is_download(path))
        .cloned()
        .collect();
    downloads.sort();
    downloads
}

fn remove_cache(buffrs_home: &Path) {
    std::fs::remove_dir_all(buffrs_home.join(".buffrs/cache")).unwrap();
}

fn remote(name: &str, version_req: &str, registry_url: &str) -> String {
    remote_in(REPOSITORY, name, version_req, registry_url)
}

fn remote_in(repository: &str, name: &str, version_req: &str, registry_url: &str) -> String {
    format!(
        r#"{name} = {{ version = "{version_req}", registry = "{registry_url}", repository = "{repository}" }}"#
    )
}

/// Creates a consumer project with the given `[dependencies]` body and returns its directory.
fn consumer(root: &Path, dependencies: &str) -> std::path::PathBuf {
    consumer_named(root, "consumer", dependencies)
}

fn consumer_named(root: &Path, directory: &str, dependencies: &str) -> std::path::PathBuf {
    let consumer_dir = root.join(directory);
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

fn read_lock(consumer_dir: &Path) -> String {
    std::fs::read_to_string(consumer_dir.join("Proto.lock")).unwrap()
}

fn cache_files(buffrs_home: &Path) -> Vec<std::path::PathBuf> {
    let mut files: Vec<_> = std::fs::read_dir(buffrs_home.join(".buffrs/cache"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    files.sort();
    files
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

        // Every request, resent or not, travels over the one connection of the install.
        assert_eq!(probe.connections(), 1);
        assert_eq!(locked_version(&consumer_dir, "lib-shared"), "0.1.1");
    });
}

#[test]
fn alias_registry_installs_from_the_lockfile() {
    with_probed_test_registry(|registry_url, probe| {
        let vfs = VirtualFileSystem::empty();
        let buffrs_home = vfs.root().join("$HOME");
        let root = vfs.root();

        publish_api_with_two_lib_versions(&root, &buffrs_home, registry_url);
        let consumer_dir = consumer(&root, &remote_by_alias("api-a", "^0.1"));
        write_alias_config(&consumer_dir, "0.50", registry_url);
        install(&consumer_dir, &buffrs_home);

        probe.clear();
        install(&consumer_dir, &buffrs_home);
        assert_eq!(probe.get_requests(), Vec::<String>::new());

        remove_cache(&buffrs_home);
        probe.clear();
        install(&consumer_dir, &buffrs_home);
        let mut requests = probe.get_requests();
        requests.sort();
        assert_eq!(
            requests,
            [
                artifact_path("api-a", "0.1.0"),
                artifact_path("lib-shared", "0.1.1")
            ]
        );
    });
}

#[test]
fn removed_locked_version_is_resolved_against_the_registry() {
    with_probed_test_registry(|registry_url, probe| {
        let vfs = VirtualFileSystem::empty();
        let buffrs_home = vfs.root().join("$HOME");
        let root = vfs.root();

        publish_api_with_two_lib_versions(&root, &buffrs_home, registry_url);
        let consumer_dir = consumer(&root, &remote("api-a", "^0.1", registry_url));
        install(&consumer_dir, &buffrs_home);
        assert_eq!(locked_version(&consumer_dir, "lib-shared"), "0.1.1");

        probe.hide(&artifact_path("lib-shared", "0.1.1"));

        // The cache still holds the locked version, so the registry is not asked.
        probe.clear();
        install(&consumer_dir, &buffrs_home);
        assert_eq!(probe.get_requests(), Vec::<String>::new());
        assert_eq!(locked_version(&consumer_dir, "lib-shared"), "0.1.1");

        // Without the cache only the removed package is looked up in the registry.
        remove_cache(&buffrs_home);
        probe.clear();
        install(&consumer_dir, &buffrs_home);
        assert_eq!(locked_version(&consumer_dir, "lib-shared"), "0.1.0");

        let listings: Vec<_> = probe
            .get_requests()
            .into_iter()
            .filter(|path| is_listing(path))
            .collect();
        assert_eq!(listings.len(), 1, "{listings:?}");
        assert!(listings[0].contains("lib-shared"), "{listings:?}");
    });
}

#[test]
fn unlocked_versions_are_downloaded_once() {
    with_probed_test_registry(|registry_url, probe| {
        let vfs = VirtualFileSystem::empty();
        let buffrs_home = vfs.root().join("$HOME");
        let root = vfs.root();

        publish_api_with_two_lib_versions(&root, &buffrs_home, registry_url);
        let consumer_dir = consumer(&root, &remote("api-a", "^0.1", registry_url));

        probe.clear();
        install(&consumer_dir, &buffrs_home);
        assert_eq!(
            sorted_downloads(&probe.get_requests()),
            [
                artifact_path("api-a", "0.1.0"),
                artifact_path("lib-shared", "0.1.0"),
                artifact_path("lib-shared", "0.1.1"),
            ]
        );
    });
}

#[test]
fn legacy_resolver_accepts_an_alias_locked_as_url() {
    with_probed_test_registry(|registry_url, probe| {
        let vfs = VirtualFileSystem::empty();
        let buffrs_home = vfs.root().join("$HOME");
        let root = vfs.root();

        publish(
            &root,
            &buffrs_home,
            registry_url,
            "lib",
            "lib-shared",
            "0.1.0",
            "",
        );

        // A local library that depends on the published one through the default registry.
        let local_dir = root.join("lib-local");
        std::fs::create_dir(&local_dir).unwrap();
        write_alias_config(&local_dir, "0.10", registry_url);
        std::fs::write(
            local_dir.join("Proto.toml"),
            format!(
                r#"edition = "0.10"

[package]
type = "lib"
name = "lib-local"
version = "0.1.0"

[dependencies]
{}
"#,
                remote_by_alias("lib-shared", "^0.1")
            ),
        )
        .unwrap();
        std::fs::create_dir_all(local_dir.join("proto")).unwrap();
        std::fs::write(
            local_dir.join("proto/local.proto"),
            r#"syntax = "proto3";
package lib.local;

message Local {
    string id = 1;
}
"#,
        )
        .unwrap();

        let consumer_dir = consumer(&root, r#"lib-local = { path = "../lib-local" }"#);
        write_alias_config(&consumer_dir, "0.10", registry_url);

        install(&consumer_dir, &buffrs_home);
        assert_eq!(locked_version(&consumer_dir, "lib-shared"), "0.1.0");

        probe.clear();
        install(&consumer_dir, &buffrs_home);
        assert_eq!(probe.get_requests(), Vec::<String>::new());
    });
}

#[test]
fn persistent_throttling_fails_the_install() {
    with_probed_test_registry(|registry_url, probe| {
        let vfs = VirtualFileSystem::empty();
        let buffrs_home = vfs.root().join("$HOME");
        let root = vfs.root();

        publish_api_with_two_lib_versions(&root, &buffrs_home, registry_url);
        let consumer_dir = consumer(&root, &remote("api-a", "^0.1", registry_url));
        install(&consumer_dir, &buffrs_home);
        let lock = std::fs::read_to_string(consumer_dir.join("Proto.lock")).unwrap();

        remove_cache(&buffrs_home);
        probe.clear();
        probe.throttle_next(100);
        install_fails(&consumer_dir, &buffrs_home);

        // Six attempts at the first download, then the install gives up.
        let requests = probe.get_requests();
        assert_eq!(requests, vec![requests[0].clone(); 6]);
        assert_eq!(
            lock,
            std::fs::read_to_string(consumer_dir.join("Proto.lock")).unwrap()
        );
    });
}

#[test]
fn throttled_version_is_not_skipped_during_resolution() {
    with_probed_test_registry(|registry_url, probe| {
        let vfs = VirtualFileSystem::empty();
        let buffrs_home = vfs.root().join("$HOME");
        let root = vfs.root();

        publish_api_with_two_lib_versions(&root, &buffrs_home, registry_url);
        let consumer_dir = consumer(&root, &remote("api-a", "^0.1", registry_url));

        // Skipping the throttled 0.1.1 would lock lib-shared at 0.1.0.
        probe.throttle_path("lib-shared-0.1.1.tgz");
        install_fails(&consumer_dir, &buffrs_home);
        assert!(!consumer_dir.join("Proto.lock").exists());
    });
}

#[test]
fn registry_url_with_a_trailing_slash_matches_the_lockfile() {
    with_probed_test_registry(|registry_url, probe| {
        let vfs = VirtualFileSystem::empty();
        let buffrs_home = vfs.root().join("$HOME");
        let root = vfs.root();

        publish_api_with_two_lib_versions(&root, &buffrs_home, registry_url);
        let consumer_dir = consumer(&root, &remote_by_alias("api-a", "^0.1"));
        write_alias_config(&consumer_dir, "0.50", registry_url);
        install(&consumer_dir, &buffrs_home);
        let lock = read_lock(&consumer_dir);

        // The config now spells the same registry with a trailing slash.
        write_alias_config(&consumer_dir, "0.50", &format!("{registry_url}/"));

        probe.clear();
        install(&consumer_dir, &buffrs_home);
        assert_eq!(probe.get_requests(), Vec::<String>::new());

        remove_cache(&buffrs_home);
        probe.clear();
        install(&consumer_dir, &buffrs_home);
        let mut requests = probe.get_requests();
        requests.sort();
        assert_eq!(
            requests,
            [
                artifact_path("api-a", "0.1.0"),
                artifact_path("lib-shared", "0.1.1")
            ]
        );
        assert_eq!(lock, read_lock(&consumer_dir));
    });
}

#[test]
fn lockfile_with_a_trailing_slash_in_the_registry_url_is_current() {
    with_probed_test_registry(|registry_url, probe| {
        let vfs = VirtualFileSystem::empty();
        let buffrs_home = vfs.root().join("$HOME");
        let root = vfs.root();

        publish_api_with_two_lib_versions(&root, &buffrs_home, registry_url);
        let consumer_dir = consumer(&root, &remote("api-a", "^0.1", registry_url));
        install(&consumer_dir, &buffrs_home);
        let lock = read_lock(&consumer_dir);

        let with_slash = lock.replace(
            &format!("registry = \"{registry_url}\""),
            &format!("registry = \"{registry_url}/\""),
        );
        assert_ne!(lock, with_slash);
        std::fs::write(consumer_dir.join("Proto.lock"), with_slash).unwrap();

        probe.clear();
        install(&consumer_dir, &buffrs_home);
        assert_eq!(probe.get_requests(), Vec::<String>::new());

        // The lockfile is written back with the one spelling buffrs uses.
        assert_eq!(lock, read_lock(&consumer_dir));
    });
}

#[test]
fn package_required_from_two_repositories_installs_from_the_lockfile() {
    const MIRROR: &str = "mirror";

    with_probed_test_registry(|registry_url, probe| {
        let vfs = VirtualFileSystem::empty();
        let buffrs_home = vfs.root().join("$HOME");
        let root = vfs.root();

        // lib-shared is in both repositories. api-a reaches it through lib-mid in the mirror,
        // api-b directly in the main repository.
        for repository in [REPOSITORY, MIRROR] {
            for version in ["0.1.0", "0.1.1"] {
                publish_to(
                    &root,
                    &buffrs_home,
                    registry_url,
                    repository,
                    ("lib", "lib-shared", version),
                    "",
                );
            }
        }
        publish(
            &root,
            &buffrs_home,
            registry_url,
            "lib",
            "lib-mid",
            "0.1.0",
            &remote_in(MIRROR, "lib-shared", "^0.1", registry_url),
        );
        publish(
            &root,
            &buffrs_home,
            registry_url,
            "api",
            "api-a",
            "0.1.0",
            &remote("lib-mid", "^0.1", registry_url),
        );
        publish(
            &root,
            &buffrs_home,
            registry_url,
            "api",
            "api-b",
            "0.1.0",
            &remote("lib-shared", "^0.1", registry_url),
        );

        let consumer_dir = consumer(
            &root,
            &format!(
                "{}\n{}",
                remote("api-a", "^0.1", registry_url),
                remote("api-b", "^0.1", registry_url)
            ),
        );
        install(&consumer_dir, &buffrs_home);
        let lock = read_lock(&consumer_dir);

        // Lockfile current, packages cached: no request at all.
        for _ in 0..3 {
            probe.clear();
            install(&consumer_dir, &buffrs_home);
            assert_eq!(probe.get_requests(), Vec::<String>::new());
            assert_eq!(lock, read_lock(&consumer_dir));
        }

        // Lockfile current, cache empty: one download per locked package and no listing.
        remove_cache(&buffrs_home);
        probe.clear();
        install(&consumer_dir, &buffrs_home);
        let requests = probe.get_requests();
        assert!(
            !requests.iter().any(|path| is_listing(path)),
            "unexpected version listing in {requests:?}"
        );
        let names: Vec<_> = sorted_downloads(&requests)
            .iter()
            .map(|path| path.rsplit('/').next().unwrap().to_owned())
            .collect();
        assert_eq!(
            names,
            [
                "api-a-0.1.0.tgz",
                "api-b-0.1.0.tgz",
                "lib-mid-0.1.0.tgz",
                "lib-shared-0.1.1.tgz"
            ]
        );
        assert_eq!(lock, read_lock(&consumer_dir));
    });
}

#[test]
fn dropped_connection_is_retried() {
    with_probed_test_registry(|registry_url, probe| {
        let vfs = VirtualFileSystem::empty();
        let buffrs_home = vfs.root().join("$HOME");
        let root = vfs.root();

        publish_api_with_two_lib_versions(&root, &buffrs_home, registry_url);
        let consumer_dir = consumer(&root, &remote("api-a", "^0.1", registry_url));
        install(&consumer_dir, &buffrs_home);
        let lock = read_lock(&consumer_dir);

        // The registry closes the connection of the first download without answering.
        remove_cache(&buffrs_home);
        probe.clear();
        probe.drop_next_connections(1);
        install(&consumer_dir, &buffrs_home);

        assert_eq!(
            sorted_downloads(&probe.get_requests()),
            [
                artifact_path("api-a", "0.1.0"),
                artifact_path("lib-shared", "0.1.1")
            ]
        );
        assert_eq!(lock, read_lock(&consumer_dir));
    });
}

#[test]
fn stalled_request_times_out_and_is_retried() {
    with_probed_test_registry(|registry_url, probe| {
        let vfs = VirtualFileSystem::empty();
        let buffrs_home = vfs.root().join("$HOME");
        let root = vfs.root();

        publish_api_with_two_lib_versions(&root, &buffrs_home, registry_url);
        let consumer_dir = consumer(&root, &remote("api-a", "^0.1", registry_url));
        install(&consumer_dir, &buffrs_home);

        // The registry leaves the first download unanswered for longer than the timeout.
        remove_cache(&buffrs_home);
        probe.clear();
        probe.stall_next(1);
        crate::cli!()
            .arg("install")
            .env("BUFFRS_HOME", &buffrs_home)
            .env("BUFFRS_HTTP_TIMEOUT", "1")
            .current_dir(&consumer_dir)
            .assert()
            .success();

        let requests = probe.get_requests();
        assert_eq!(requests.len(), 3, "{requests:?}");
        assert_eq!(requests[0], requests[1]);
    });
}

/// Installs, cuts every cache entry in half and installs again.
fn install_with_truncated_cache(config_edition: &str) {
    with_probed_test_registry(|registry_url, probe| {
        let vfs = VirtualFileSystem::empty();
        let buffrs_home = vfs.root().join("$HOME");
        let root = vfs.root();

        publish_api_with_two_lib_versions(&root, &buffrs_home, registry_url);
        let consumer_dir = consumer(&root, &remote("api-a", "^0.1", registry_url));
        write_config_text(&consumer_dir, &format!("edition = \"{config_edition}\"\n"));
        install(&consumer_dir, &buffrs_home);
        let lock = read_lock(&consumer_dir);
        let lib_version = locked_version(&consumer_dir, "lib-shared");

        for file in cache_files(&buffrs_home) {
            let content = std::fs::read(&file).unwrap();
            std::fs::write(&file, &content[..content.len() / 2]).unwrap();
        }

        // Each locked package is downloaded once, in place of its unreadable cache entry.
        probe.clear();
        install(&consumer_dir, &buffrs_home);
        let requests = probe.get_requests();
        assert!(
            !requests.iter().any(|path| is_listing(path)),
            "unexpected version listing in {requests:?}"
        );
        assert_eq!(
            sorted_downloads(&requests),
            [
                artifact_path("api-a", "0.1.0"),
                artifact_path("lib-shared", &lib_version)
            ]
        );
        assert_eq!(lock, read_lock(&consumer_dir));

        // The downloads replaced the truncated entries.
        probe.clear();
        install(&consumer_dir, &buffrs_home);
        assert_eq!(probe.get_requests(), Vec::<String>::new());
    });
}

#[test]
fn truncated_cache_entry_is_downloaded_again() {
    install_with_truncated_cache("0.50");
}

#[test]
fn legacy_resolver_downloads_a_truncated_cache_entry_again() {
    install_with_truncated_cache("0.10");
}

#[test]
fn parallel_installs_share_one_cache() {
    const INSTALLS: usize = 6;

    with_probed_test_registry(|registry_url, _| {
        let vfs = VirtualFileSystem::empty();
        let buffrs_home = vfs.root().join("$HOME");
        let root = vfs.root();

        publish_api_with_two_lib_versions(&root, &buffrs_home, registry_url);
        let dependencies = remote("api-a", "^0.1", registry_url);
        let consumer_dirs: Vec<_> = (0..INSTALLS)
            .map(|index| consumer_named(&root, &format!("consumer-{index}"), &dependencies))
            .collect();

        install(&consumer_dirs[0], &buffrs_home);
        let lock = read_lock(&consumer_dirs[0]);
        for consumer_dir in &consumer_dirs[1..] {
            std::fs::write(consumer_dir.join("Proto.lock"), &lock).unwrap();
        }

        remove_cache(&buffrs_home);
        std::thread::scope(|scope| {
            for consumer_dir in &consumer_dirs {
                scope.spawn(|| install(consumer_dir, &buffrs_home));
            }
        });

        for consumer_dir in &consumer_dirs {
            assert_eq!(lock, read_lock(consumer_dir));
            assert!(consumer_dir.join("proto/vendor/lib-shared").is_dir());
        }

        // One entry per package, and no temporary file left behind.
        let cache = cache_files(&buffrs_home);
        assert_eq!(cache.len(), 2, "{cache:?}");
    });
}
