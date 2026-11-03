//! `glob`: files by pattern, under the ignore rules.

use super::*;

#[tokio::test]
async fn glob_lists_matching_files_sorted_leaving_out_ignored_and_hidden_ones() {
    let fx = Fixture::new().await;
    fx.project();
    assert_eq!(
        fx.glob("**/*.rs", None, false).await,
        "src/deep/x.rs\nsrc/lib.rs\nsrc/main.rs"
    );
}

#[tokio::test]
async fn glob_is_a_path_glob_from_the_searched_directory() {
    let fx = Fixture::new().await;
    fx.project();
    assert_eq!(fx.glob("*.md", None, false).await, "README.md");
    assert_eq!(
        fx.glob("src/*", None, false).await,
        "src/lib.rs\nsrc/main.rs"
    );
    assert_eq!(
        fx.glob("*.rs", Some("src"), false).await,
        "src/lib.rs\nsrc/main.rs"
    );
    assert_eq!(
        fx.glob("./src/*", None, false).await,
        "src/lib.rs\nsrc/main.rs"
    );
}

#[tokio::test]
async fn glob_with_hidden_lists_hidden_files_but_never_ignored_ones() {
    let fx = Fixture::new().await;
    fx.project();
    assert_eq!(
        fx.glob("**/*.rs", None, true).await,
        ".hidden/notes.rs\nsrc/deep/x.rs\nsrc/lib.rs\nsrc/main.rs"
    );
}

#[tokio::test]
async fn glob_refuses_a_bad_glob_as_invalid_args_saying_why() {
    let fx = Fixture::new().await;
    fx.project();
    let failed = fx
        .call("glob", json!({"pattern": "src/["}))
        .await
        .unwrap_err();
    assert_eq!(failed.kind, FailKind::InvalidArgs);
    assert!(failed.message.starts_with("bad glob: "), "{failed}");
}

#[tokio::test]
async fn glob_stops_at_1000_paths() {
    let fx = Fixture::new().await;
    for n in 0..1001 {
        fx.put(&format!("many/{n:04}.txt"), "");
    }
    let listed = fx.glob("many/*", None, false).await;
    let lines: Vec<&str> = listed.lines().collect();
    assert_eq!(lines.len(), 1001);
    assert_eq!(lines[999], "many/0999.txt");
    assert_eq!(
        lines[1000],
        "[showing 1000 of 1001 paths; narrow the pattern or path]"
    );
}

#[tokio::test]
async fn glob_that_cannot_match_a_file_says_why() {
    let fx = Fixture::new().await;
    fx.project();
    let hint = "No files found. The pattern is a path glob from the searched directory, matching \
                files only: no trailing `/`, no `..`, and no absolute host path.";
    for pattern in ["src/", "../src/*.rs", "/tmp/src/*.rs"] {
        assert_eq!(fx.glob(pattern, None, false).await, hint, "{pattern}");
    }
    assert_eq!(fx.glob("*.txt", None, false).await, "No files found.");
}

#[tokio::test]
async fn glob_of_a_missing_path_says_it_is_missing() {
    let fx = Fixture::new().await;
    assert_eq!(
        fx.glob("*.rs", Some("nope"), false).await,
        "Err: nope: no such file or directory"
    );
}

#[tokio::test]
async fn glob_stops_at_50_kb_of_paths() {
    let fx = Fixture::new().await;
    let deep = "d".repeat(100);
    for n in 0..600 {
        fx.put(&format!("{deep}/{n:04}.txt"), "");
    }
    let listed = fx.glob("**/*.txt", None, false).await;
    let (shown, note) = listed.rsplit_once('\n').unwrap();
    assert!(shown.len() <= 50 * 1024, "{}", shown.len());
    assert!(shown.ends_with(".txt"));
    let count = shown.lines().count();
    assert_eq!(
        note,
        format!("[showing {count} of 600 paths; narrow the pattern or path]")
    );
}
