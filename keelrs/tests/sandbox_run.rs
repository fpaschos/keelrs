//! `Sandbox::run`, a command checked to its end, and the local sandbox's
//! environment, through the public API on a temp dir.

#![allow(clippy::disallowed_methods)] // the test sets up its dir on the host

#[path = "common/temp_dir.rs"]
mod temp_dir;

use std::time::Duration;

use keelrs::Error;
use keelrs::sandbox::{self, Command, ExecRequest, Exit, Ran, SandboxError};
use temp_dir::{TempDir, open};

#[tokio::test]
async fn run_returns_stdout_without_its_trailing_newlines() {
    let dir = TempDir::new();
    let sb = open(&dir).await;
    let out = sb.run(["sh", "-c", "printf ' a\\n\\n'"]).await.unwrap();
    assert_eq!(out, " a");
}

#[tokio::test]
async fn a_command_that_exits_non_zero_fails_with_what_it_printed() {
    let dir = TempDir::new();
    let sb = open(&dir).await;
    let error = sb
        .run(["sh", "-c", "echo out; echo oops >&2; exit 3"])
        .await
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "`sh -c echo out; echo oops >&2; exit 3` ended with exit code 3: oops"
    );
    let Error::Sandbox(SandboxError::CommandFailed { command, ran }) = error else {
        panic!("{error:?}");
    };
    assert_eq!(
        command,
        Command::Argv(vec![
            "sh".into(),
            "-c".into(),
            "echo out; echo oops >&2; exit 3".into()
        ])
    );
    assert_eq!(ran.exit, Exit::Code(3));
    assert_eq!(ran.stdout, "out\n");
    assert_eq!(ran.stderr, "oops\n");
}

#[tokio::test]
async fn a_command_past_its_timeout_fails_timed_out() {
    let dir = TempDir::new();
    let sb = open(&dir).await;
    let request = ExecRequest::argv(["sleep", "5"]).timeout(Duration::from_millis(50));
    let error = sb.run(request).await.unwrap_err();
    let Error::Sandbox(SandboxError::CommandFailed { ran, .. }) = error else {
        panic!("{error:?}");
    };
    assert_eq!(ran.exit, Exit::TimedOut);
}

#[tokio::test]
async fn a_shell_line_runs_in_the_shell() {
    let dir = TempDir::new();
    let sb = open(&dir).await;
    let out = sb.run(ExecRequest::shell("echo a | tr a b")).await;
    assert_eq!(out.unwrap(), "b");
}

#[tokio::test]
async fn run_feeds_stdin() {
    let dir = TempDir::new();
    let sb = open(&dir).await;
    let request = ExecRequest::argv(["cat"]).stdin("hi");
    assert_eq!(sb.run(request).await.unwrap(), "hi");
}

#[tokio::test]
async fn a_missing_program_fails_with_io_because_it_never_ran() {
    let dir = TempDir::new();
    let sb = open(&dir).await;
    let error = sb.run(["keelrs-no-such-program"]).await.unwrap_err();
    assert!(
        matches!(error, Error::Sandbox(SandboxError::Io { .. })),
        "{error:?}"
    );
}

/// A variable off the allowlist that Cargo sets for this test process, so
/// no test has to change the process-wide environment.
const OFF_ALLOWLIST: &str = "CARGO_PKG_NAME";

#[tokio::test]
async fn the_allowlist_keeps_a_host_variable_out_and_inherit_env_lets_it_in() {
    assert_eq!(std::env::var(OFF_ALLOWLIST).unwrap(), "keelrs");
    let dir = TempDir::new();
    let print = || ["printenv", OFF_ALLOWLIST];

    let error = open(&dir).await.run(print()).await.unwrap_err();
    assert!(
        matches!(error, Error::Sandbox(SandboxError::CommandFailed { .. })),
        "{error:?}"
    );

    let host = sandbox::local().root(&dir.0).inherit_env().open().await;
    assert_eq!(host.unwrap().run(print()).await.unwrap(), "keelrs");

    let host = sandbox::local()
        .root(&dir.0)
        .inherit_env()
        .env_remove(OFF_ALLOWLIST)
        .open()
        .await
        .unwrap();
    assert!(host.run(print()).await.is_err());
}

#[tokio::test]
async fn run_starts_in_a_narrowed_work_dir() {
    let dir = TempDir::new();
    std::fs::create_dir_all(dir.0.join("sub")).unwrap();
    let sb = open(&dir).await.work_dir("sub");
    let pwd = sb.run(["pwd"]).await.unwrap();
    assert_eq!(pwd, dir.0.join("sub").display().to_string());
}

#[test]
fn the_exec_types_round_trip_through_json() {
    fn round_trip<T: serde::Serialize + serde::de::DeserializeOwned>(value: &T) -> T {
        serde_json::from_str(&serde_json::to_string(value).unwrap()).unwrap()
    }
    let request = ExecRequest::argv(["ls", "-l"])
        .cwd("sub")
        .env("A", "1")
        .env_remove("B")
        .stdin("in")
        .timeout(Duration::from_millis(1500));
    let back = round_trip(&request);
    assert_eq!(back.command, request.command);
    assert_eq!(back.cwd.as_deref(), Some("sub"));
    assert_eq!(back.env, request.env);
    assert_eq!(back.stdin.as_deref(), Some(&b"in"[..]));
    assert_eq!(back.timeout, Some(Duration::from_millis(1500)));
    assert!(!back.merge_stderr);

    let shell = Command::Shell("echo a | tr a b".into());
    assert_eq!(round_trip(&shell), shell);
    for exit in [
        Exit::Code(3),
        Exit::Signaled(9),
        Exit::TimedOut,
        Exit::Killed,
    ] {
        assert_eq!(round_trip(&exit), exit);
    }
    let ran = Ran {
        stdout: "out\n".into(),
        stderr: "err\n".into(),
        exit: Exit::Code(1),
    };
    assert_eq!(round_trip(&ran), ran);
}
