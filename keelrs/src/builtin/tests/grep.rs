//! `grep`: matching lines, under the ignore rules.

use super::*;

/// Whatever line endings a file has, `grep` finds every line the pattern
/// matches, as if each line were matched alone: anchors at a later line's
/// start and end (also before `\r\n`), text anchors and flags the pattern
/// turns off, a `.` over a `\r` inside a line, and case ignored. The cases
/// are rig-tools' (`search/tests.rs`), whose whole-text prefilter `grep`
/// copies.
#[tokio::test]
async fn grep_finds_every_matching_line_whatever_the_line_ends() {
    let fx = Fixture::new().await;
    let cases = [
        ("first\nsecond\n", "^second", false),
        ("first\r\nsecond\r\nthird\r\n", "second$", false),
        ("first\r\nsecond\r\n", r"^second\z", false),
        ("first\nsecond\n", r"\Asecond", false),
        ("first\nsecond\n", "(?-m)^second$", false),
        ("first\na\rb\n", "a.b", false),
        ("first\nSECOND\n", "^second$", true),
    ];
    for (index, (text, pattern, ignore_case)) in cases.into_iter().enumerate() {
        let file = format!("{index}.txt");
        fx.put(&file, text);
        let args = json!({"path": file, "ignore_case": ignore_case});
        let found = fx.grep(pattern, args).await;
        assert!(
            found.starts_with(&format!("{file}:2: ")) && found.lines().count() == 1,
            "{pattern:?} in {text:?}: {found}"
        );
    }
}

#[tokio::test]
async fn grep_stops_at_100_kb_however_much_context_is_asked() {
    let fx = Fixture::new().await;
    fx.put("long.txt", &format!("{}\n", "x".repeat(99)).repeat(2000));
    fx.put("short.txt", "x\n");
    let found = fx.grep("x", json!({"context": 100_000})).await;
    let (shown, note) = found.rsplit_once('\n').unwrap();
    assert!(shown.len() <= 100 * 1024, "{}", shown.len());
    assert!(shown.starts_with("long.txt:1: x"));
    assert_eq!(note, "[output cut at 100 KB; narrow your search]");
}

#[tokio::test]
async fn grep_lists_matching_lines_leaving_out_ignored_and_hidden_files_unless_asked() {
    let fx = Fixture::new().await;
    fx.project();
    assert_eq!(
        fx.grep("fn ", json!({})).await,
        "README.md:1: # fn readme
docs/guide.md:1: fn guide
src/deep/x.rs:1: fn deep() {}
src/lib.rs:1: pub fn greet(name: &str) -> String {
src/lib.rs:5: pub fn twice(x: i32) -> i32 {
src/lib.rs:9: pub fn thrice(x: i32) -> i32 {
src/main.rs:1: fn main() {"
    );
    // With `hidden`, hidden files are searched, ignored ones still not.
    assert_eq!(
        fx.grep("fn (hidden|generated)", json!({"hidden": true}))
            .await,
        ".hidden/notes.rs:1: fn hidden() {}"
    );
}

#[tokio::test]
async fn grep_searches_a_path_or_one_file_and_only_files_matching_a_glob() {
    let fx = Fixture::new().await;
    fx.project();
    let in_src = fx
        .grep(r"x \* \d", json!({"path": "src", "glob": "*.rs"}))
        .await;
    assert_eq!(in_src, "src/lib.rs:6:     x * 2\nsrc/lib.rs:10:     x * 3");
    let one_file = fx.grep("thrice", json!({"path": "src/lib.rs"})).await;
    assert_eq!(one_file, "src/lib.rs:9: pub fn thrice(x: i32) -> i32 {");
}

#[tokio::test]
async fn grep_matches_literal_text_and_ignores_case_when_asked() {
    let fx = Fixture::new().await;
    fx.project();
    assert_eq!(
        fx.grep("x * 2", json!({"literal": true})).await,
        "src/lib.rs:6:     x * 2"
    );
    assert_eq!(
        fx.grep("FN MAIN", json!({"ignore_case": true})).await,
        "src/main.rs:1: fn main() {"
    );
}

#[tokio::test]
async fn grep_shows_context_lines_and_separates_groups() {
    let fx = Fixture::new().await;
    fx.project();
    assert_eq!(
        fx.grep(r"x \* [23]", json!({"context": 1})).await,
        "src/lib.rs-5- pub fn twice(x: i32) -> i32 {
src/lib.rs:6:     x * 2
src/lib.rs-7- }
--
src/lib.rs-9- pub fn thrice(x: i32) -> i32 {
src/lib.rs:10:     x * 3
src/lib.rs-11- }"
    );
}

#[tokio::test]
async fn grep_names_the_files_it_could_not_search() {
    let fx = Fixture::new().await;
    fx.put("a.txt", "needle\n");
    fs::write(fx.dir.path("b.bin"), b"needle \xff\n").unwrap();
    assert_eq!(
        fx.grep("needle", json!({})).await,
        "a.txt:1: needle\n[not searched, over 16 MB, not UTF-8 or unreadable: b.bin]"
    );
    for n in 0..6 {
        fs::write(fx.dir.path(&format!("c{n}.bin")), b"\xff").unwrap();
    }
    assert_eq!(
        fx.grep("haystack", json!({})).await,
        "No matches.\n[not searched, over 16 MB, not UTF-8 or unreadable: \
         b.bin, c0.bin, c1.bin, c2.bin, c3.bin, and 2 more]"
    );
}

#[tokio::test]
async fn grep_with_the_largest_context_shows_the_whole_file() {
    let fx = Fixture::new().await;
    fx.put("a.txt", "one\ntwo\nthree\n");
    assert_eq!(
        fx.grep("two", json!({"context": usize::MAX})).await,
        "a.txt-1- one\na.txt:2: two\na.txt-3- three"
    );
}

#[tokio::test]
async fn grep_stops_at_100_matches_and_cuts_lines_at_500_characters() {
    let fx = Fixture::new().await;
    let wide = "hit ".repeat(200);
    fx.put("many.txt", &format!("{wide}\n").repeat(150));
    let found = fx.grep("hit", json!({})).await;
    let lines: Vec<&str> = found.lines().collect();
    assert_eq!(lines.len(), 101);
    assert_eq!(lines[0], format!("many.txt:1: {}", &wide[..500]));
    assert_eq!(
        lines[100],
        "[showing the first 100 matches; narrow your search]"
    );
}

#[tokio::test]
async fn grep_finds_matches_in_every_file_in_path_order_however_many_files() {
    let fx = Fixture::new().await;
    for n in 0..150 {
        let text = if n == 0 || n == 149 {
            "hit\n"
        } else {
            "miss\n"
        };
        fx.put(&format!("many/{n:03}.txt"), text);
    }
    assert_eq!(
        fx.grep("hit", json!({})).await,
        "many/000.txt:1: hit\nmany/149.txt:1: hit"
    );
}

#[tokio::test]
async fn grep_says_when_nothing_matches_and_refuses_a_bad_pattern() {
    let fx = Fixture::new().await;
    fx.project();
    assert_eq!(fx.grep("nowhere at all", json!({})).await, "No matches.");
    let bad = fx.grep("(", json!({})).await;
    assert!(bad.starts_with("Err: bad pattern: "), "{bad}");
    let bad = fx.grep("fn", json!({"glob": "**/{a"})).await;
    assert!(bad.starts_with("Err: bad glob: "), "{bad}");
}
