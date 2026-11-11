//! Commands in the local sandbox, through the public API, on a temp dir:
//! where they start, their output, environment, timeouts and process group.

#![allow(clippy::disallowed_methods)] // the test sets up its dir on the host

use std::time::Duration;

use keelrs::sandbox::{self, Chunk, ExecRequest, Exit};

#[path = "common/temp_dir.rs"]
mod temp_dir;

use temp_dir::{TempDir, open};

/// Whether process `pid` ends within 5 s, checked from inside `sb`.
async fn gone(sb: &keelrs::sandbox::Sandbox, pid: &str) -> bool {
    let alive = format!("kill -0 {} 2>/dev/null && echo alive", pid.trim());
    for _ in 0..50 {
        let ran = sb.exec(ExecRequest::shell(&alive)).await.unwrap().run();
        if ran.await.unwrap().stdout.is_empty() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    false
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
    assert!(
        gone(&sb, &pid).await,
        "the background sleep outlived its dropped execution"
    );
}

#[test]
#[allow(clippy::disallowed_types)] // with its runtime gone, the check runs on the host
fn a_command_does_not_outlive_its_runtime() {
    let dir = TempDir::new();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let (pid, running) = runtime.block_on(async {
        let sb = open(&dir).await;
        let mut running = sb
            .exec(ExecRequest::shell("sleep 30 & echo $!; wait"))
            .await
            .unwrap();
        let Some(Chunk::Out(pid)) = running.next().await else {
            panic!("no pid");
        };
        (pid.trim().to_owned(), running)
    });
    // The execution outlives the runtime, so only the runtime's end can stop it.
    drop(runtime);
    drop(running);
    for _ in 0..50 {
        let alive = std::process::Command::new("kill")
            .args(["-0", &pid])
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap();
        if !alive.success() {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("the background sleep outlived its runtime");
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
    assert!(
        gone(&sb, &pid).await,
        "a member that ignored SIGTERM outlived the timeout"
    );
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
