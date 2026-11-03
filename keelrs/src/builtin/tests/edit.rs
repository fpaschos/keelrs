//! `edit`: exact and evened-out replacements, stale files, and its refusals.

use super::*;

#[tokio::test]
async fn edit_replaces_exact_text_and_shows_the_lines_around_it() {
    let fx = Fixture::new().await;
    fx.put("src/lib.rs", LIB);
    fx.read("src/lib.rs", None, None).await;
    assert_eq!(
        fx.edit("src/lib.rs", &[("    x * 2", "    x + x", false)])
            .await,
        "Edited src/lib.rs: line 6.\n[lines 3-9]\n}\n\npub fn twice(x: i32) -> i32 {\n    x + x\n}\n\npub fn thrice(x: i32) -> i32 {"
    );
    assert_eq!(fx.get("src/lib.rs"), LIB.replace("x * 2", "x + x"));
}

#[tokio::test]
async fn edit_of_text_found_nowhere_shows_the_closest_text() {
    let fx = Fixture::new().await;
    fx.put(
        "main.rs",
        "fn main() {\n    let total = add(1, 2);\n    println!(\"{total}\");\n}\n",
    );
    fx.read("main.rs", None, None).await;
    let missed = fx
        .edit(
            "main.rs",
            &[(
                "    let total = add(1, 3);\n",
                "    let total = add(2, 3);\n",
                false,
            )],
        )
        .await;
    assert!(
        missed.contains("copy it from the closest text in the file, shown below")
            && missed.contains("is line 2:\n    let total = add(1, 2);")
            && !missed.contains("read it again"),
        "{missed}"
    );
}

#[tokio::test]
async fn edit_of_a_file_changed_elsewhere_since_it_was_read_keeps_that_change() {
    let fx = Fixture::new().await;
    fx.put("src/lib.rs", LIB);
    fx.read("src/lib.rs", None, None).await;
    fx.put("src/lib.rs", &format!("{LIB}// someone else\n"));
    assert_eq!(
        fx.edit("src/lib.rs", &[("    x * 3", "    x + x + x", false)])
            .await,
        "Edited src/lib.rs: line 10.\n[lines 7-12]\n}\n\npub fn thrice(x: i32) -> i32 {\n    x + x + x\n}\n// someone else"
    );
    assert_eq!(
        fx.get("src/lib.rs"),
        format!("{}// someone else\n", LIB.replace("x * 3", "x + x + x"))
    );
}

#[tokio::test]
async fn edit_of_text_changed_since_it_was_read_asks_for_a_new_read() {
    let fx = Fixture::new().await;
    fx.put("src/lib.rs", LIB);
    fx.read("src/lib.rs", None, None).await;
    fx.put("src/lib.rs", &LIB.replace("x * 3", "3 * x"));
    assert_eq!(
        fx.edit("src/lib.rs", &[("    x * 3", "    x + x + x", false)])
            .await,
        "Err: `old_text` was not found, not even ignoring trailing whitespace: `src/lib.rs` changed since it was last read, so read it again and copy `old_text` from what it holds now. `src/lib.rs` was not changed; fix the edit and send it again."
    );
}

#[tokio::test]
async fn edit_of_a_file_grown_past_16_mb_since_it_was_read_is_refused_and_leaves_it_whole() {
    let fx = Fixture::new().await;
    fx.put("big.txt", "keep\n");
    fx.read("big.txt", None, None).await;
    let grown = format!("keep\n{}", "x\n".repeat(9 * 1024 * 1024));
    fx.put("big.txt", &grown);
    assert_eq!(
        fx.edit("big.txt", &[("keep", "kept", false)]).await,
        "Err: big.txt is larger than 16 MB"
    );
    assert_eq!(fx.get("big.txt").len(), grown.len());
}
