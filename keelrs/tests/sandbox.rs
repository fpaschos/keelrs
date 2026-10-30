//! The local sandbox, through the public API, on a temp dir; and a backend
//! of our own behind the same handle.

#![allow(clippy::disallowed_methods)] // the test sets up its dir on the host

use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use keelrs::sandbox::{
    self, Chunk, ContentHash, Contents, ExecRequest, Execution, Exit, Listing, Sandbox,
    SandboxBackend, SandboxError, SandboxPath, Stat,
};
use keelrs::{Error, Result};
use rig_tools::workspace::{Local, Workspace as _};

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
    std::fs::write(dir.0.join("inner/own.txt"), "yes").unwrap();
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
    assert!(!dir.0.join("escaped.txt").exists());
}

#[tokio::test]
async fn list_skips_a_name_that_is_not_utf8_and_lists_the_rest() {
    use std::os::unix::ffi::OsStrExt as _;
    let dir = TempDir::new();
    let odd = std::ffi::OsStr::from_bytes(b"odd\xff.txt");
    if std::fs::write(dir.0.join(odd), "x").is_err() {
        return; // the file system takes UTF-8 names only, as macOS's does
    }
    std::fs::write(dir.0.join("ok.txt"), "x").unwrap();
    let sb = open(&dir).await;
    let listed = sb.list(".", None, Listing::Visible).await.unwrap();
    assert_eq!(
        listed,
        [SandboxPath::new(dir.0.join("ok.txt").to_str().unwrap()).unwrap()]
    );
}

#[tokio::test]
async fn a_dangling_link_leads_where_it_points_and_out_is_refused() {
    let dir = TempDir::new();
    std::fs::create_dir_all(dir.0.join("inner")).unwrap();
    let link = |to: &str, at: &str| {
        std::os::unix::fs::symlink(dir.0.join(to), dir.0.join(at)).unwrap();
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
    assert!(!dir.0.join("outside").exists());
    sb.write("in.txt", b"x".to_vec()).await.unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.0.join("inner/made/new.txt")).unwrap(),
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
    let dir = TempDir::new();
    for (path, text) in PROJECT {
        let at = dir.0.join(path);
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
                .files(&dir.0.join(root), None, rig)
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
async fn a_command_sees_the_allowlist_with_the_builder_changes_but_not_the_host() {
    // Cargo sets `CARGO_PKG_NAME` for this test process.
    assert!(std::env::var("CARGO_PKG_NAME").is_ok());
    let dir = TempDir::new();
    let sb = sandbox::local()
        .root(&dir.0)
        .env("KEELRS_GIVEN", "yes")
        .env_remove("HOME")
        .open()
        .await
        .unwrap();
    let ran = sb
        .exec(ExecRequest::shell(
            "echo \"$CARGO_PKG_NAME|$KEELRS_GIVEN|${PATH:+path}|${HOME-unset}\"",
        ))
        .await
        .unwrap()
        .run()
        .await
        .unwrap();
    assert_eq!(ran.stdout, "|yes|path|unset\n");
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

#[tokio::test]
async fn git_terminal_prompt_defaults_off_and_the_builder_overrides_it() {
    let dir = TempDir::new();
    let line = || ExecRequest::shell("echo $GIT_TERMINAL_PROMPT");
    let ran = open(&dir).await.exec(line()).await.unwrap().run().await;
    assert_eq!(ran.unwrap().stdout, "0\n");

    let sb = sandbox::local()
        .root(&dir.0)
        .env("GIT_TERMINAL_PROMPT", "1")
        .open()
        .await
        .unwrap();
    let ran = sb.exec(line()).await.unwrap().run().await;
    assert_eq!(ran.unwrap().stdout, "1\n");
}

#[tokio::test]
async fn a_shell_line_runs_in_the_shell_resolved_at_open_whatever_the_path() {
    let dir = TempDir::new();
    let sb = open(&dir).await;
    let shell = sb.shell().unwrap().to_string();
    assert!(
        shell.ends_with("/bash") || shell.ends_with("/sh"),
        "{shell}"
    );
    let ran = sb
        .exec(ExecRequest::shell("echo $0").env("PATH", "/nowhere"))
        .await
        .unwrap()
        .run()
        .await
        .unwrap();
    assert_eq!(ran.stdout, format!("{shell}\n"));
}

#[tokio::test]
async fn dropping_an_execution_kills_its_group() {
    let dir = TempDir::new();
    let sb = open(&dir).await;
    let mut running = sb
        .exec(ExecRequest::shell("sleep 30 & echo $!; wait"))
        .await
        .unwrap();
    let Some(Chunk::Out(pid)) = running.next().await else {
        panic!("no pid");
    };
    drop(running);
    let alive = format!("kill -0 {} 2>/dev/null && echo alive", pid.trim());
    for _ in 0..50 {
        let ran = sb.exec(ExecRequest::shell(&alive)).await.unwrap().run();
        if ran.await.unwrap().stdout.is_empty() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("the background sleep outlived its dropped execution");
}

#[tokio::test]
async fn a_background_leftover_does_not_hold_the_command_open() {
    let dir = TempDir::new();
    let sb = open(&dir).await;
    let started = std::time::Instant::now();
    let ran = sb
        .exec(ExecRequest::shell("sleep 100 & echo hi"))
        .await
        .unwrap()
        .run()
        .await
        .unwrap();
    assert_eq!(ran.stdout, "hi\n");
    assert_eq!(ran.exit, Exit::Code(0));
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "{:?}",
        started.elapsed()
    );
}

#[tokio::test]
async fn wait_discards_unread_output_instead_of_blocking_the_command() {
    let dir = TempDir::new();
    let sb = open(&dir).await;
    let running = sb
        .exec(ExecRequest::shell("head -c 100000000 /dev/zero"))
        .await
        .unwrap();
    let exit = tokio::time::timeout(Duration::from_secs(30), running.wait()).await;
    assert_eq!(exit.expect("wait hung").unwrap(), Exit::Code(0));
}

#[tokio::test]
async fn a_process_that_leaves_the_group_does_not_hold_the_command_open() {
    let dir = TempDir::new();
    let sb = open(&dir).await;
    let started = std::time::Instant::now();
    let ran = sb
        .exec(ExecRequest::shell(
            "perl -MPOSIX -e 'POSIX::setsid(); sleep 30' & echo hi",
        ))
        .await
        .unwrap()
        .run();
    let ran = tokio::time::timeout(Duration::from_secs(10), ran).await;
    assert_eq!(ran.expect("run hung").unwrap().stdout, "hi\n");
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
}

#[tokio::test]
async fn a_timeout_stops_a_group_member_that_ignores_sigterm() {
    let dir = TempDir::new();
    let sb = open(&dir).await;
    let pid = dir.0.join("pid");
    let line = format!(
        "(trap '' TERM; sleep 30) >/dev/null 2>&1 & echo $! > {}; sleep 30",
        pid.display()
    );
    let ran = sb
        .exec(ExecRequest::shell(line).timeout(Duration::from_millis(300)))
        .await
        .unwrap()
        .run()
        .await
        .unwrap();
    assert_eq!(ran.exit, Exit::TimedOut);
    let pid = std::fs::read_to_string(pid).unwrap();
    let alive = format!("kill -0 {} 2>/dev/null && echo alive", pid.trim());
    for _ in 0..50 {
        let ran = sb.exec(ExecRequest::shell(&alive)).await.unwrap().run();
        if ran.await.unwrap().stdout.is_empty() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("a member that ignored SIGTERM outlived the timeout");
}

#[tokio::test]
async fn a_long_line_is_split_between_characters() {
    let dir = TempDir::new();
    let sb = open(&dir).await;
    let ran = sb
        .exec(ExecRequest::shell(
            "perl -CO -e 'print \"a\", \"\\x{e9}\" x 100000'",
        ))
        .await
        .unwrap()
        .run()
        .await
        .unwrap();
    assert_eq!(ran.stdout, format!("a{}", "\u{e9}".repeat(100_000)));
}

#[tokio::test]
async fn run_keeps_the_last_64_mib_of_a_larger_output_without_slowing_down() {
    let dir = TempDir::new();
    let sb = open(&dir).await;
    let run = sb
        .exec(ExecRequest::shell("yes | head -c 80000000"))
        .await
        .unwrap()
        .run();
    let ran = tokio::time::timeout(Duration::from_secs(30), run)
        .await
        .expect("80 MB of output took over 30s")
        .unwrap();
    assert_eq!(ran.stdout.len(), 64 * 1024 * 1024);
    assert!(ran.stdout == "y\n".repeat(32 * 1024 * 1024));
}

/// Files in memory: a backend with only the required methods, and no exec.
#[derive(Default)]
struct Memory(Mutex<HashMap<SandboxPath, Vec<u8>>>);

#[keelrs::async_trait]
impl SandboxBackend for Memory {
    async fn stat(&self, path: &SandboxPath) -> Result<Stat> {
        Ok(match self.0.lock().unwrap().get(path) {
            Some(bytes) => Stat::File {
                size: bytes.len() as u64,
                read_only: false,
                modified: None,
            },
            None => Stat::Missing,
        })
    }

    async fn read(&self, path: &SandboxPath, limit: u64) -> Result<Contents> {
        let files = self.0.lock().unwrap();
        let bytes = files.get(path).ok_or_else(|| SandboxError::Io {
            operation: "read",
            path: path.to_string(),
            kind: std::io::ErrorKind::NotFound,
            message: "no such file".to_owned(),
        })?;
        Ok(Contents {
            bytes: bytes.iter().copied().take(limit as usize).collect(),
            size: bytes.len() as u64,
        })
    }

    async fn write(&self, path: &SandboxPath, bytes: Vec<u8>) -> Result<()> {
        self.0.lock().unwrap().insert(path.clone(), bytes);
        Ok(())
    }

    async fn remove(&self, path: &SandboxPath, _recursive: bool) -> Result<()> {
        self.0.lock().unwrap().remove(path);
        Ok(())
    }

    async fn rename(&self, from: &SandboxPath, to: &SandboxPath) -> Result<()> {
        let mut files = self.0.lock().unwrap();
        if let Some(bytes) = files.remove(from) {
            files.insert(to.clone(), bytes);
        }
        Ok(())
    }

    async fn list(&self, root: &SandboxPath, _glob: Option<&str>) -> Result<Vec<SandboxPath>> {
        let files = self.0.lock().unwrap();
        Ok(files
            .keys()
            .filter(|path| path.starts_with(root))
            .cloned()
            .collect())
    }

    async fn canonical(&self, path: &SandboxPath) -> Result<SandboxPath> {
        Ok(path.clone())
    }

    async fn exec(&self, _request: ExecRequest) -> Result<Execution> {
        Err(SandboxError::Unsupported { operation: "exec" }.into())
    }
}

#[tokio::test]
async fn a_backend_of_our_own_gets_write_if_and_read_files_by_default() {
    let sb = Sandbox::new(Memory::default(), SandboxPath::new("/work").unwrap());
    sb.write("a.txt", b"one".to_vec()).await.unwrap();

    let stale = sb
        .write_if("a.txt", b"two".to_vec(), ContentHash::of(b"zero"))
        .await
        .unwrap();
    assert_eq!(stale.map(|now| now.bytes), Some(b"one".to_vec()));
    let fresh = sb
        .write_if("a.txt", b"two".to_vec(), ContentHash::of(b"one"))
        .await
        .unwrap();
    assert_eq!(fresh, None);

    let files = sb
        .read_files(&["a.txt", "missing.txt"], 1024)
        .await
        .unwrap();
    assert_eq!(files, vec![Some(b"two".to_vec()), None]);
}

#[tokio::test]
async fn a_backend_refusing_an_operation_reaches_the_caller_as_unsupported() {
    let sb = Sandbox::new(Memory::default(), SandboxPath::new("/work").unwrap());
    let refused = sb.exec(ExecRequest::shell("true")).await;
    assert!(matches!(
        refused,
        Err(Error::Sandbox(SandboxError::Unsupported {
            operation: "exec"
        }))
    ));
}

/// The backend calls a sandbox made, by name.
type Calls = Arc<Mutex<HashMap<&'static str, usize>>>;

/// [`Memory`], counting the backend calls.
struct Counting {
    files: Memory,
    calls: Calls,
}

impl Counting {
    fn count(&self, call: &'static str) {
        *self.calls.lock().unwrap().entry(call).or_default() += 1;
    }
}

#[keelrs::async_trait]
impl SandboxBackend for Counting {
    async fn stat(&self, path: &SandboxPath) -> Result<Stat> {
        self.count("stat");
        self.files.stat(path).await
    }

    async fn read(&self, path: &SandboxPath, limit: u64) -> Result<Contents> {
        self.count("read");
        self.files.read(path, limit).await
    }

    async fn write(&self, path: &SandboxPath, bytes: Vec<u8>) -> Result<()> {
        self.count("write");
        self.files.write(path, bytes).await
    }

    async fn remove(&self, path: &SandboxPath, recursive: bool) -> Result<()> {
        self.count("remove");
        self.files.remove(path, recursive).await
    }

    async fn rename(&self, from: &SandboxPath, to: &SandboxPath) -> Result<()> {
        self.count("rename");
        self.files.rename(from, to).await
    }

    async fn list(&self, root: &SandboxPath, glob: Option<&str>) -> Result<Vec<SandboxPath>> {
        self.count("list");
        self.files.list(root, glob).await
    }

    async fn canonical(&self, path: &SandboxPath) -> Result<SandboxPath> {
        self.count("canonical");
        self.files.canonical(path).await
    }

    async fn canonical_all(&self, paths: &[SandboxPath]) -> Vec<Result<SandboxPath>> {
        self.count("canonical_all");
        self.files.canonical_all(paths).await
    }

    async fn read_files(&self, paths: &[SandboxPath], limit: u64) -> Vec<Option<Vec<u8>>> {
        self.count("read_files");
        self.files.read_files(paths, limit).await
    }

    async fn exec(&self, request: ExecRequest) -> Result<Execution> {
        self.files.exec(request).await
    }
}

/// The backend calls made during `work`, by name.
async fn counted(calls: &Calls, work: impl Future<Output = ()>) -> Vec<(&'static str, usize)> {
    calls.lock().unwrap().clear();
    work.await;
    let mut made: Vec<_> = calls.lock().unwrap().drain().collect();
    made.sort_unstable();
    made
}

#[tokio::test]
async fn an_operation_resolves_its_path_once_and_the_work_dir_once_per_handle() {
    let calls = Calls::default();
    let counting = Counting {
        files: Memory::default(),
        calls: calls.clone(),
    };
    let sb = Sandbox::new(counting, SandboxPath::new("/work").unwrap());

    let first = counted(&calls, async {
        sb.write("a.txt", b"one".to_vec()).await.unwrap();
    });
    assert_eq!(first.await, [("canonical", 2), ("write", 1)]);

    let clone = sb.clone();
    let read = counted(&calls, async {
        clone.read("a.txt", 1024).await.unwrap();
    });
    assert_eq!(read.await, [("canonical", 1), ("read", 1)]);
    let stat = counted(&calls, async {
        sb.stat("a.txt").await.unwrap();
    });
    assert_eq!(stat.await, [("canonical", 1), ("stat", 1)]);
    let canonical = counted(&calls, async {
        sb.canonical("a.txt").await.unwrap();
    });
    assert_eq!(canonical.await, [("canonical", 1)]);
    let write = counted(&calls, async {
        sb.write("b.txt", b"two".to_vec()).await.unwrap();
    });
    assert_eq!(write.await, [("canonical", 1), ("write", 1)]);

    let read_files = counted(&calls, async {
        sb.read_files(&["a.txt", "b.txt", "c.txt"], 1024)
            .await
            .unwrap();
    });
    assert_eq!(read_files.await, [("canonical_all", 1), ("read_files", 1)]);

    let narrowed = sb.work_dir("sub");
    let fresh = counted(&calls, async {
        narrowed.stat("a.txt").await.unwrap();
    });
    assert_eq!(fresh.await, [("canonical", 2), ("stat", 1)]);
}
