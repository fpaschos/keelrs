//! What runs on this machine, outside the sandbox: `git` on this checkout and `gh` with its
//! login. The agents reach GitHub only through the read-only `issues` and `issue` host tools;
//! the push and the pull request are the flow's, so no token enters the sandbox.

use std::io;
use std::sync::LazyLock;

use keelrs::{Tool, ToolCall};
use schemars::JsonSchema;
use serde::Deserialize;
use tokio::process::Command;

/// Lists the issues a planner picks from.
pub static ISSUES: LazyLock<Tool> = LazyLock::new(|| {
    Tool::host(
        "issues",
        "List the open issues labelled `agent-ready`, with their number, title, body and labels, \
         as JSON.",
        async |_: ToolCall<()>| {
            gh(&[
                "issue",
                "list",
                "--state",
                "open",
                "--label",
                "agent-ready",
                "--json",
                "number,title,body,labels",
            ])
            .await
        },
    )
});

#[derive(Debug, Deserialize, JsonSchema)]
struct IssueArgs {
    /// The issue's number.
    number: u64,
}

/// Reads one issue with its comments.
pub static ISSUE: LazyLock<Tool> = LazyLock::new(|| {
    Tool::host(
        "issue",
        "Read one GitHub issue with its comments, as JSON.",
        async |call: ToolCall<IssueArgs>| {
            let number = call.data.number.to_string();
            gh(&[
                "issue",
                "view",
                &number,
                "--json",
                "number,title,body,comments",
            ])
            .await
        },
    )
});

/// Fetches `branch` from `bundle` into this checkout, pushes it to `origin` and opens a pull
/// request into `main`; returns its URL.
pub async fn open_pr(bundle: Vec<u8>, branch: &str, title: &str, body: &str) -> io::Result<String> {
    let file = std::env::temp_dir().join("keelrs-build-review-loop.bundle");
    tokio::fs::write(&file, bundle).await?;
    let file = file.to_string_lossy();
    git(&["fetch", &file, &format!("{branch}:{branch}")]).await?;
    git(&["push", "origin", branch]).await?;
    let url = gh(&[
        "pr", "create", "--head", branch, "--base", "main", "--title", title, "--body", body,
    ])
    .await?;
    Ok(url.trim().to_owned())
}

/// `git` on this checkout; its stdout.
pub async fn git(args: &[&str]) -> io::Result<Vec<u8>> {
    output(Command::new("git").args(args)).await
}

async fn gh(args: &[&str]) -> io::Result<String> {
    let stdout = output(Command::new("gh").args(args)).await?;
    Ok(String::from_utf8_lossy(&stdout).into_owned())
}

/// The command's stdout, or its stderr as the error when it fails.
async fn output(command: &mut Command) -> io::Result<Vec<u8>> {
    let out = command.output().await?;
    if out.status.success() {
        Ok(out.stdout)
    } else {
        let stderr = String::from_utf8_lossy(&out.stderr);
        Err(io::Error::other(stderr.trim().to_owned()))
    }
}
