//! `write`: creating and replacing files, and its refusals.

use super::*;

#[tokio::test]
async fn write_creates_a_file_and_its_folders() {
    let fx = Fixture::new().await;
    assert_eq!(
        fx.write("src/new.rs", "fn main() {}\n").await,
        "Created src/new.rs."
    );
    assert_eq!(fx.get("src/new.rs"), "fn main() {}\n");
}

#[tokio::test]
async fn a_read_only_file_is_refused_as_permission_denied() {
    use std::os::unix::fs::PermissionsExt as _;
    let fx = Fixture::new().await;
    fx.put("frozen.txt", "old\n");
    let path = fx.dir.path("frozen.txt");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o400)).unwrap();
    let args = json!({"path": "frozen.txt", "content": "new\n"});
    let failed = fx.call("write", args).await.unwrap_err();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(failed.kind, FailKind::PermissionDenied);
}

#[tokio::test]
async fn write_replaces_a_file_it_cannot_read() {
    use std::os::unix::fs::PermissionsExt as _;
    let fx = Fixture::new().await;
    fx.put("locked.txt", "old\n");
    let path = fx.dir.path("locked.txt");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o200)).unwrap();
    assert_eq!(
        fx.write("locked.txt", "new\n").await,
        "Replaced locked.txt."
    );
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(fx.get("locked.txt"), "new\n");
}

#[tokio::test]
async fn write_through_a_dangling_link_out_of_the_work_dir_is_refused_as_permission_denied() {
    let fx = Fixture::new().await;
    let outside = fx.dir.0.with_extension("outside");
    std::os::unix::fs::symlink(outside.join("new.txt"), fx.dir.path("out.txt")).unwrap();
    let failed = fx
        .call("write", json!({"path": "out.txt", "content": "x"}))
        .await
        .unwrap_err();
    assert_eq!(failed.kind, FailKind::PermissionDenied);
    assert_eq!(failed.message, "out.txt is outside the work dir");
    assert!(!outside.exists());
}

#[tokio::test]
async fn write_replaces_a_file_and_says_how_long_it_was() {
    let fx = Fixture::new().await;
    fx.put("a.txt", "one\ntwo\n");
    assert_eq!(
        fx.write("a.txt", "three\n").await,
        "Replaced a.txt, which had 2 lines."
    );
    assert_eq!(fx.get("a.txt"), "three\n");
}
