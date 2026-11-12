//! The local sandbox's files through the public API, on a temp dir:
//! reads, writes, paths out of the work dir, links and listing.

#![allow(clippy::disallowed_methods)] // the test sets up its dir on the host

use keelrs::Error;
use keelrs::sandbox::{ContentHash, Listing, SandboxError, SandboxPath};
use rig_tools::workspace::{Local, Workspace as _};

use crate::support::{open, temp_dir};

#[tokio::test]
async fn a_written_file_reads_back_from_a_relative_path() {
    let dir = temp_dir();
    let sb = open(&dir).await;
    sb.write("notes/a.txt", b"hello".to_vec()).await.unwrap();
    let read = sb.read("notes/a.txt", 1024).await.unwrap();
    assert_eq!(read.bytes, b"hello");
    assert_eq!(read.size, 5);
    assert_eq!(std::fs::read(dir.join("notes/a.txt")).unwrap(), b"hello");
}

#[tokio::test]
async fn write_if_writes_over_what_was_seen_and_refuses_a_stale_hash() {
    let dir = temp_dir();
    let sb = open(&dir).await;
    sb.write("a.txt", b"one".to_vec()).await.unwrap();

    let fresh = sb
        .write_if("a.txt", b"two".to_vec(), ContentHash::of(b"one"))
        .await
        .unwrap();
    assert_eq!(fresh, None);

    let stale = sb
        .write_if("a.txt", b"three".to_vec(), ContentHash::of(b"one"))
        .await
        .unwrap();
    assert_eq!(stale.map(|now| now.bytes), Some(b"two".to_vec()));
    assert_eq!(std::fs::read(dir.join("a.txt")).unwrap(), b"two");
}

#[tokio::test]
async fn a_path_out_of_the_work_dir_is_refused() {
    let dir = temp_dir();
    std::fs::create_dir_all(dir.join("inner")).unwrap();
    std::fs::write(dir.join("secret.txt"), "no").unwrap();
    std::os::unix::fs::symlink(dir.join("secret.txt"), dir.join("inner/link.txt")).unwrap();
    let sb = open(&dir).await.work_dir("inner");

    for path in ["../secret.txt", "link.txt", "/etc/hosts"] {
        let refused = sb.read(path, 1024).await;
        assert!(
            matches!(
                refused,
                Err(Error::Sandbox(SandboxError::OutsideWorkDir { .. }))
            ),
            "{path}: {refused:?}"
        );
    }
    std::fs::write(dir.join("inner/own.txt"), "yes").unwrap();
    let read = sb
        .read_files(
            &["../secret.txt", "own.txt", "link.txt", "/etc/hosts"],
            1024,
        )
        .await
        .unwrap();
    assert_eq!(read, [None, Some(b"yes".to_vec()), None, None]);
    let write = sb.write("../escaped.txt", b"x".to_vec()).await;
    assert!(matches!(
        write,
        Err(Error::Sandbox(SandboxError::OutsideWorkDir { .. }))
    ));
    assert!(!dir.join("escaped.txt").exists());
}

#[tokio::test]
async fn a_gitignore_starting_with_a_byte_order_mark_keeps_its_first_rule() {
    let dir = temp_dir();
    std::fs::write(dir.join(".gitignore"), "\u{feff}gen/\n").unwrap();
    std::fs::create_dir_all(dir.join("gen")).unwrap();
    std::fs::write(dir.join("gen/out.rs"), "x").unwrap();
    std::fs::write(dir.join("lib.rs"), "x").unwrap();
    let sb = open(&dir).await;
    let listed = sb.list(".", None, Listing::Visible).await.unwrap();
    assert_eq!(
        listed,
        [SandboxPath::new(dir.join("lib.rs").to_str().unwrap()).unwrap()]
    );
}

#[tokio::test]
async fn list_skips_a_name_that_is_not_utf8_and_lists_the_rest() {
    use std::os::unix::ffi::OsStrExt as _;
    let dir = temp_dir();
    let odd = std::ffi::OsStr::from_bytes(b"odd\xff.txt");
    if std::fs::write(dir.join(odd), "x").is_err() {
        return; // the file system takes UTF-8 names only, as macOS's does
    }
    std::fs::write(dir.join("ok.txt"), "x").unwrap();
    let sb = open(&dir).await;
    let listed = sb.list(".", None, Listing::Visible).await.unwrap();
    assert_eq!(
        listed,
        [SandboxPath::new(dir.join("ok.txt").to_str().unwrap()).unwrap()]
    );
}

#[tokio::test]
async fn a_dangling_link_leads_where_it_points_and_out_is_refused() {
    let dir = temp_dir();
    std::fs::create_dir_all(dir.join("inner")).unwrap();
    let link = |to: &str, at: &str| {
        std::os::unix::fs::symlink(dir.join(to), dir.join(at)).unwrap();
    };
    link("outside/new.txt", "inner/out.txt");
    link("inner/made/new.txt", "inner/in.txt");
    let sb = open(&dir).await.work_dir("inner");

    let write = sb.write("out.txt", b"x".to_vec()).await;
    assert!(
        matches!(
            write,
            Err(Error::Sandbox(SandboxError::OutsideWorkDir { .. }))
        ),
        "{write:?}"
    );
    assert!(!dir.join("outside").exists());
    sb.write("in.txt", b"x".to_vec()).await.unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.join("inner/made/new.txt")).unwrap(),
        "x"
    );
}

/// A project with every kind of file a listing leaves out or keeps.
const PROJECT: [(&str, &str); 17] = [
    (".git/info/exclude", "excluded.txt\n"),
    (".gitignore", "*.log\nbuild/\n!keep.log\n"),
    (".ignore", "!forced.log\n"),
    (".env", "SECRET=1\n"),
    (".github/ci.yml", "on: push\n"),
    ("target/out.txt", "built\n"),
    ("build/x.txt", "built\n"),
    ("a.log", "log\n"),
    ("keep.log", "log\n"),
    ("forced.log", "log\n"),
    ("excluded.txt", "x\n"),
    ("src/main.rs", "fn main() {}\n"),
    ("src/.gitignore", "gen.rs\n!b.log\n"),
    ("src/gen.rs", "// generated\n"),
    ("src/b.log", "log\n"),
    ("src/inner/c.rs", "// c\n"),
    ("src/inner/d.log", "log\n"),
];

#[tokio::test]
async fn list_leaves_out_what_the_ignore_rules_do_as_rig_tools_lists_it() {
    let dir = temp_dir();
    for (path, text) in PROJECT {
        let at = dir.join(path);
        std::fs::create_dir_all(at.parent().unwrap()).unwrap();
        std::fs::write(at, text).unwrap();
    }
    let sb = open(&dir).await;
    for root in [".", "src"] {
        for (listing, rig) in [
            (Listing::Visible, rig_tools::workspace::Listing::Visible),
            (Listing::Hidden, rig_tools::workspace::Listing::Hidden),
        ] {
            let mut listed: Vec<String> = sb
                .list(root, None, listing)
                .await
                .unwrap()
                .iter()
                .map(ToString::to_string)
                .collect();
            listed.sort();
            let mut expected: Vec<String> = Local
                .files(&dir.join(root), None, rig)
                .unwrap()
                .iter()
                .map(|path| {
                    SandboxPath::new(path.to_str().unwrap())
                        .unwrap()
                        .to_string()
                })
                .collect();
            expected.sort();
            assert_eq!(listed, expected, "{root} {listing:?}");
        }
    }
}
