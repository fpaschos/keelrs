//! `bash`: command lines, their output and how they end.

use super::*;

#[tokio::test]
async fn bash_runs_in_the_work_dir_and_says_how_it_ended() {
    let fx = Fixture::new().await;
    fx.put("a.txt", "");
    assert_eq!(fx.bash("ls; exit 3", None).await, "a.txt\n[exit code 3]");
    assert_eq!(fx.bash("true", None).await, "[exit code 0]");
}

#[tokio::test]
async fn bash_merges_stderr_into_stdout_in_order() {
    let fx = Fixture::new().await;
    assert_eq!(
        fx.bash("echo out; echo err >&2; echo again", None).await,
        "out\nerr\nagain\n[exit code 0]"
    );
}

#[tokio::test]
async fn bash_keeps_the_last_2000_lines_and_says_how_many_were_cut() {
    let fx = Fixture::new().await;
    let ran = fx.bash("seq 1 2500", None).await;
    let lines: Vec<&str> = ran.lines().collect();
    assert_eq!(lines.len(), 2002);
    assert_eq!(lines[0], "[500 earlier lines cut]");
    assert_eq!(lines[1], "501");
    assert_eq!(lines[2000], "2500");
    assert_eq!(lines[2001], "[exit code 0]");
}

#[tokio::test]
async fn bash_counts_the_lines_cut_from_output_past_100_kb() {
    let fx = Fixture::new().await;
    let ran = fx.bash("seq 1 30000", None).await;
    let lines: Vec<&str> = ran.lines().collect();
    assert_eq!(lines[0], "[28000 earlier lines cut]");
    assert_eq!(lines[1], "28001");
}

#[tokio::test]
async fn bash_shows_the_end_of_a_last_line_longer_than_50_kb() {
    let fx = Fixture::new().await;
    let ran = fx
        .bash("head -c 60000 /dev/zero | tr '\\0' a; echo END", None)
        .await;
    let (note, rest) = ran.split_once('\n').unwrap();
    assert_eq!(note, "[showing the last 50 KB of a longer line]");
    assert!(rest.ends_with("aaaEND\n[exit code 0]"), "{rest:.80}");
    assert_eq!(rest.len(), 50 * 1024 + "[exit code 0]".len());
}

#[tokio::test]
async fn bash_stops_a_command_once_it_has_printed_10_mb() {
    let fx = Fixture::new().await;
    let ran = tokio::time::timeout(std::time::Duration::from_secs(30), fx.bash("yes", None))
        .await
        .expect("`yes` was not stopped");
    let lines: Vec<&str> = ran.lines().collect();
    assert!(lines[0].ends_with(" earlier lines cut]"), "{}", lines[0]);
    assert_eq!(lines.len(), 2002);
    assert!(lines[1..2001].iter().all(|line| *line == "y"));
    assert_eq!(lines[2001], "[stopped after printing 10 MB]");
}

#[tokio::test]
async fn bash_redacts_the_secrets_a_command_prints() {
    let fx = Fixture::new().await;
    let printed = "printf 'OPENAI_API_KEY=sk-test-probe-1234567890abcdef\\n\
        -----BEGIN RSA PRIVATE KEY-----\\nMIIEsecretbody\\n-----END RSA PRIVATE KEY-----\\n\
        after\\n'";
    assert_eq!(
        fx.bash(printed, None).await,
        "OPENAI_API_KEY=[REDACTED]\n[REDACTED]\nafter\n[exit code 0]"
    );
}

#[tokio::test]
async fn bash_drops_terminal_escape_sequences() {
    let fx = Fixture::new().await;
    assert_eq!(
        fx.bash(
            "printf '\\033[31mred\\033[0m and \\033]8;;http://x\\007link\\033]8;;\\007\\n'",
            None
        )
        .await,
        "red and link\n[exit code 0]"
    );
}

#[tokio::test]
async fn bash_past_its_timeout_is_stopped() {
    let fx = Fixture::new().await;
    assert_eq!(
        fx.bash("echo started; sleep 5", Some(1)).await,
        "started\n[timed out after 1s and was stopped]"
    );
    assert_eq!(
        fx.bash("sleep 1", Some(0)).await,
        format!("[timed out after 0s and was stopped{SILENT}]")
    );
}

/// What a timed-out `bash` adds when the command printed nothing.
const SILENT: &str = ". It printed nothing: a program whose output is not a terminal may hold \
                      it back until it ends, so make it print as it goes (such as `python3 -u` \
                      or `stdbuf -oL`) to see how far it got";

#[tokio::test]
async fn bash_names_the_signal_that_ended_a_command_and_what_likely_sent_it() {
    let fx = Fixture::new().await;
    assert_eq!(
        fx.bash("echo before; kill -TERM $$", None).await,
        "before\n[ended by SIGTERM (signal 15): something on this machine asked it to stop]"
    );
}

#[test]
fn bash_names_a_shell_that_is_not_bash() {
    let description = |shell: &str| {
        crate::builtin::bash::tool(Some(&SandboxPath::new(shell).unwrap())).description
    };
    let (bash, sh) = (description("/bin/bash"), description("/bin/sh"));
    assert!(!bash.contains("not bash"), "{bash}");
    assert!(
        sh.contains("The shell is `/bin/sh`, not bash: write POSIX sh"),
        "{sh}"
    );
}
