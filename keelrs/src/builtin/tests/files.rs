//! `delete` and `move`: whole files, and what `seen` remembers of them.

use super::*;

/// What an `edit` of text not in `path` says when `path` counts as read.
const STALE: &str = "Err: `old_text` was not found, not even ignoring trailing whitespace: `b.txt` changed since it was last read, so read it again and copy `old_text` from what it holds now. `b.txt` was not changed; fix the edit and send it again.";

#[tokio::test]
async fn delete_removes_a_file() {
    let fx = Fixture::new().await;
    fx.put("a.txt", "one\n");
    assert_eq!(fx.delete("a.txt").await, "Deleted a.txt.");
    assert!(!fx.exists("a.txt"));
}

#[tokio::test]
async fn delete_refuses_a_directory_and_a_missing_file() {
    let fx = Fixture::new().await;
    fx.put("src/lib.rs", LIB);
    assert_eq!(
        fx.delete("src").await,
        "Err: src is not a regular file; `delete` removes files only"
    );
    assert_eq!(fx.delete("nope.txt").await, "Err: nope.txt: no such file");
    assert!(fx.exists("src/lib.rs"));
}

#[tokio::test]
async fn delete_forgets_what_was_read() {
    let fx = Fixture::new().await;
    fx.put("b.txt", "one\n");
    fx.read("b.txt", None, None).await;
    fx.delete("b.txt").await;
    fx.put("b.txt", "two\n");
    assert_ne!(fx.edit("b.txt", &[("one", "1", false)]).await, STALE);
}

#[tokio::test]
async fn move_renames_a_file_creating_the_new_folders() {
    let fx = Fixture::new().await;
    fx.put("a.txt", "one\n");
    assert_eq!(
        fx.move_file("a.txt", "new/b.txt", false).await,
        "Moved a.txt to new/b.txt."
    );
    assert!(!fx.exists("a.txt"));
    assert_eq!(fx.get("new/b.txt"), "one\n");
}

#[tokio::test]
async fn move_out_of_the_work_dir_names_the_path_that_leads_out() {
    let fx = Fixture::new().await;
    fx.put("a.rs", "a");
    assert_eq!(
        fx.move_file("a.rs", "../out.rs", false).await,
        "Err: ../out.rs is outside the work dir"
    );
    assert_eq!(
        fx.move_file("../out.rs", "a.rs", false).await,
        "Err: ../out.rs is outside the work dir"
    );
}

#[tokio::test]
async fn move_refuses_a_directory() {
    let fx = Fixture::new().await;
    fx.put("src/lib.rs", LIB);
    assert_eq!(
        fx.move_file("src", "lib", false).await,
        "Err: src is not a regular file; `move` moves files only"
    );
}

#[tokio::test]
async fn move_refuses_an_existing_target_unless_told_to_overwrite() {
    let fx = Fixture::new().await;
    fx.put("a.txt", "one\n").put("b.txt", "two\n");
    assert_eq!(
        fx.move_file("a.txt", "b.txt", false).await,
        "Err: b.txt already exists; set `overwrite` to replace it"
    );
    assert_eq!(fx.get("b.txt"), "two\n");
    assert_eq!(
        fx.move_file("a.txt", "b.txt", true).await,
        "Moved a.txt to b.txt."
    );
    assert_eq!(fx.get("b.txt"), "one\n");
}

#[tokio::test]
async fn move_carries_what_was_read_to_the_new_path() {
    let fx = Fixture::new().await;
    fx.put("a.txt", "one\n");
    fx.read("a.txt", None, None).await;
    fx.move_file("a.txt", "b.txt", false).await;
    fx.put("b.txt", "two\n");
    assert_eq!(fx.edit("b.txt", &[("one", "1", false)]).await, STALE);
}
