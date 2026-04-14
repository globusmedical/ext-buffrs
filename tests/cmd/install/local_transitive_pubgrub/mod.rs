use crate::VirtualFileSystem;

/// Verifies that the PubGrub resolver (edition 0.50) resolves transitive local
/// path dependencies: root -> local-api-a -> local-lib-b
#[test]
fn fixture() {
    let vfs = VirtualFileSystem::copy(crate::parent_directory!().join("in"));

    crate::cli!()
        .arg("install")
        .current_dir(vfs.root())
        .assert()
        .success()
        .stdout(include_str!("stdout.log"))
        .stderr(include_str!("stderr.log"));

    vfs.verify_against(crate::parent_directory!().join("out"));
}
