//! `read`: a whole file or a window of its lines, and its refusals.

use super::*;

#[tokio::test]
async fn read_shows_a_whole_file() {
    let fx = Fixture::new().await;
    fx.put("a.txt", "one\ntwo\n");
    assert_eq!(fx.read("a.txt", None, None).await, "one\ntwo\n");
}

#[tokio::test]
async fn read_of_a_missing_file_is_not_found_and_names_the_path_it_was_given() {
    let fx = Fixture::new().await;
    let args = json!({"path": "src/missing.rs"});
    let failed = fx.call("read", args).await.unwrap_err();
    assert_eq!(failed.kind, FailKind::NotFound);
    assert_eq!(failed.message, "src/missing.rs: no such file");
}
