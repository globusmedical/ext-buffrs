//! Regression tests for issue #56: `buffrs install --generate-buf-yaml` must
//! produce a stable `buf.yaml` from run to run.

use crate::VirtualFileSystem;
use std::{path::Path, time::SystemTime};

const MANIFEST: &str = r#"edition = "0.10"

[package]
type = "api"
name = "some-test-api"
version = "0.1.0"

[dependencies]
"#;

/// Seven `ignore_only` keys, deliberately not in alphabetical order, so that a
/// hash-ordered or sorted serialisation matching the file order by luck is
/// improbable.
const IGNORE_ONLY_ORDER: [&str; 7] = [
    "RPC_RESPONSE_STANDARD_NAME",
    "PACKAGE_VERSION_SUFFIX",
    "ENUM_VALUE_PREFIX",
    "RPC_REQUEST_STANDARD_NAME",
    "FIELD_LOWER_SNAKE_CASE",
    "RPC_REQUEST_RESPONSE_UNIQUE",
    "COMMENT_FIELD",
];

fn ignore_only_block() -> String {
    IGNORE_ONLY_ORDER
        .iter()
        .map(|rule| format!("    {rule}:\n      - api/legacy.proto\n"))
        .collect()
}

fn project(buf_yaml: &str) -> VirtualFileSystem {
    let vfs = VirtualFileSystem::empty();
    let root = vfs.root();
    std::fs::write(root.join("Proto.toml"), MANIFEST).unwrap();
    std::fs::create_dir_all(root.join("proto")).unwrap();
    std::fs::write(
        root.join("proto/api.proto"),
        "syntax = \"proto3\";\npackage some.test.api;\n",
    )
    .unwrap();
    std::fs::write(root.join("proto/buf.yaml"), buf_yaml).unwrap();
    vfs
}

fn install_with_buf_yaml(root: &Path) {
    crate::cli!()
        .args(["install", "--generate-buf-yaml"])
        .current_dir(root)
        .assert()
        .success();
}

fn read_buf_yaml(root: &Path) -> (Vec<u8>, SystemTime) {
    let path = root.join("proto/buf.yaml");
    let bytes = std::fs::read(&path).unwrap();
    let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
    (bytes, modified)
}

fn ignore_only_keys(yaml: &[u8]) -> Vec<String> {
    let value: serde_norway::Value = serde_norway::from_slice(yaml).unwrap();
    value["lint"]["ignore_only"]
        .as_mapping()
        .unwrap()
        .keys()
        .map(|k| k.as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn regenerated_buf_yaml_keeps_ignore_only_order_and_second_run_is_a_no_op() {
    // A stale vendor module forces the first run to rewrite the file.
    let vfs = project(&format!(
        "version: v2\nmodules:\n  - path: vendor/removed-dependency\nlint:\n  use:\n    - STANDARD\n  ignore_only:\n{}",
        ignore_only_block()
    ));
    let root = vfs.root();

    install_with_buf_yaml(&root);
    let (first, first_modified) = read_buf_yaml(&root);
    assert!(
        !String::from_utf8_lossy(&first).contains("removed-dependency"),
        "first run should have rewritten the stale module list"
    );
    assert_eq!(ignore_only_keys(&first), IGNORE_ONLY_ORDER);

    for _ in 0..3 {
        install_with_buf_yaml(&root);
        let (again, again_modified) = read_buf_yaml(&root);
        assert_eq!(
            String::from_utf8_lossy(&again),
            String::from_utf8_lossy(&first)
        );
        assert_eq!(
            again_modified, first_modified,
            "an unchanged buf.yaml must not be rewritten"
        );
    }
}

#[test]
fn buf_yaml_that_already_matches_is_left_untouched_with_its_comments() {
    let original = format!(
        "# Lint configuration for some-test-api.\nversion: v2\nmodules:\n  - path: \".\"\n    excludes:\n      - vendor\nlint:\n  use:\n    - STANDARD\n  ignore_only:\n    # Remove once api/legacy.proto is retired.\n{}",
        ignore_only_block()
    );
    let vfs = project(&original);
    let root = vfs.root();

    install_with_buf_yaml(&root);

    let (bytes, _) = read_buf_yaml(&root);
    assert_eq!(String::from_utf8_lossy(&bytes), original);
}
