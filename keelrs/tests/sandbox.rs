//! The local sandbox, through the public API, on a temp dir.

#![allow(clippy::disallowed_methods)] // the test sets up its dir on the host

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use keelrs::Error;
use keelrs::sandbox::{self, ContentHash, ExecRequest, Exit, Sandbox, SandboxError};

/// A fresh dir under the system temp dir, removed on drop.
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("keelrs-sandbox-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&path).unwrap();
        Self(std::fs::canonicalize(path).unwrap())
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

async fn open(dir: &TempDir) -> Sandbox {
    sandbox::local().root(&dir.0).open().await.unwrap()
}

#[tokio::test]
async fn a_written_file_reads_back_from_a_relative_path() {
    let dir = TempDir::new();
    let sb = open(&dir).await;
    sb.write("notes/a.txt", b"hello".to_vec()).await.unwrap();
    let read = sb.read("notes/a.txt", 1024).await.unwrap();
    assert_eq!(read.bytes, b"hello");
    assert_eq!(read.size, 5);
    assert_eq!(std::fs::read(dir.0.join("notes/a.txt")).unwrap(), b"hello");
}

#[tokio::test]
async fn write_if_writes_over_what_was_seen_and_refuses_a_stale_hash() {
    let dir = TempDir::new();
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
    assert_eq!(std::fs::read(dir.0.join("a.txt")).unwrap(), b"two");
}

#[tokio::test]
async fn a_path_out_of_the_work_dir_is_refused() {
    let dir = TempDir::new();
    std::fs::create_dir_all(dir.0.join("inner")).unwrap();
    std::fs::write(dir.0.join("secret.txt"), "no").unwrap();
    std::os::unix::fs::symlink(dir.0.join("secret.txt"), dir.0.join("inner/link.txt")).unwrap();
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
    let write = sb.write("../escaped.txt", b"x".to_vec()).await;
    assert!(matches!(
        write,
        Err(Error::Sandbox(SandboxError::OutsideWorkDir { .. }))
    ));
    assert!(!dir.0.join("escaped.txt").exists());
}

#[tokio::test]
async fn a_command_starts_in_the_work_dir_and_reports_its_exit() {
    let dir = TempDir::new();
    std::fs::create_dir_all(dir.0.join("inner")).unwrap();
    let sb = open(&dir).await.work_dir("inner");
    let ran = sb
        .exec(ExecRequest::shell("pwd; echo oops >&2; exit 3"))
        .await
        .unwrap()
        .run()
        .await
        .unwrap();
    assert_eq!(ran.stdout, format!("{}\n", dir.0.join("inner").display()));
    assert_eq!(ran.stderr, "oops\n");
    assert_eq!(ran.exit, Exit::Code(3));
}

#[tokio::test]
async fn a_command_past_its_timeout_ends_timed_out() {
    let dir = TempDir::new();
    let sb = open(&dir).await;
    let started = std::time::Instant::now();
    let ran = sb
        .exec(ExecRequest::shell("sleep 30").timeout(Duration::from_millis(200)))
        .await
        .unwrap()
        .run()
        .await
        .unwrap();
    assert_eq!(ran.exit, Exit::TimedOut);
    assert!(started.elapsed() < Duration::from_secs(10));
}

#[tokio::test]
async fn merge_stderr_keeps_both_streams_in_order_on_stdout() {
    let dir = TempDir::new();
    let sb = open(&dir).await;
    let ran = sb
        .exec(ExecRequest::shell("echo one; echo two >&2; echo three").merge_stderr())
        .await
        .unwrap()
        .run()
        .await
        .unwrap();
    assert_eq!(ran.stdout, "one\ntwo\nthree\n");
    assert_eq!(ran.stderr, "");
}

#[tokio::test]
async fn a_command_sees_the_allowlist_and_the_builder_env_but_not_the_host() {
    // Cargo sets `CARGO_PKG_NAME` for this test process.
    assert!(std::env::var("CARGO_PKG_NAME").is_ok());
    let dir = TempDir::new();
    let sb = sandbox::local()
        .root(&dir.0)
        .env("KEELRS_GIVEN", "yes")
        .open()
        .await
        .unwrap();
    let ran = sb
        .exec(ExecRequest::shell(
            "echo \"$CARGO_PKG_NAME|$KEELRS_GIVEN|${PATH:+path}\"",
        ))
        .await
        .unwrap()
        .run()
        .await
        .unwrap();
    assert_eq!(ran.stdout, "|yes|path\n");
}

#[tokio::test]
async fn a_killed_command_ends_killed() {
    let dir = TempDir::new();
    let sb = open(&dir).await;
    let started = std::time::Instant::now();
    let mut running = sb.exec(ExecRequest::shell("sleep 30")).await.unwrap();
    running.kill();
    assert_eq!(running.wait().await.unwrap(), Exit::Killed);
    assert!(started.elapsed() < Duration::from_secs(10));
}
