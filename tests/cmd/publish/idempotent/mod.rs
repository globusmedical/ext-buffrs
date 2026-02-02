use crate::{with_test_registry, VirtualFileSystem};
use predicates::prelude::*;

/// Test that publishing the same package twice succeeds (idempotent publish).
/// The second publish should detect the existing artifact and skip the upload.
#[test]
fn publish_twice_succeeds() {
    with_test_registry(|url| {
        let vfs = VirtualFileSystem::copy(crate::parent_directory!().join("in"));

        // First publish - should upload
        crate::cli!()
            .arg("publish")
            .arg("--registry")
            .arg(url)
            .arg("--repository")
            .arg("my-repository")
            .current_dir(vfs.root())
            .assert()
            .success()
            .stdout(predicate::str::contains("published"));

        // Second publish - should skip (already exists)
        crate::cli!()
            .arg("publish")
            .arg("--registry")
            .arg(url)
            .arg("--repository")
            .arg("my-repository")
            .current_dir(vfs.root())
            .assert()
            .success()
            .stdout(
                predicate::str::contains("skipped")
                    .and(predicate::str::contains("already published")),
            );
    });
}
