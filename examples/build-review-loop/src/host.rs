//! What runs on this machine, through the host `Sandbox`: `git` on this checkout and `gh` with
//! its login. The agents reach GitHub only through the read-only `issues` and `issue` host
//! tools; the push and the pull request are the flow's, so no token enters the sandbox.

use keelrs::sandbox::Sandbox;
use keelrs::{Tool, ToolCall};
use schemars::JsonSchema;
use serde::Deserialize;

/// Lists the issues a planner picks from.
pub fn issues(host: &Sandbox) -> Tool {
    let host = host.clone();
    Tool::host(
        "issues",
        "List the open issues labelled `agent-ready`, with their number, title, body and labels, \
         as JSON.",
        async move |_: ToolCall<()>| {
            host.run([
                "gh",
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
}

#[derive(Debug, Deserialize, JsonSchema)]
struct IssueArgs {
    /// The issue's number.
    number: u64,
}

/// Reads one issue with its comments.
pub fn issue(host: &Sandbox) -> Tool {
    let host = host.clone();
    Tool::host(
        "issue",
        "Read one GitHub issue with its comments, as JSON.",
        async move |call: ToolCall<IssueArgs>| {
            let number = call.data.number.to_string();
            let json = "number,title,body,comments";
            host.run(["gh", "issue", "view", &number, "--json", json])
                .await
        },
    )
}

/// Fetches `branch` from `bundle` into the `host` checkout, pushes it to `origin` and opens a
/// pull request into `main`; returns its URL.
pub async fn open_pr(
    host: &Sandbox,
    bundle: Vec<u8>,
    branch: &str,
    title: &str,
    body: &str,
) -> anyhow::Result<String> {
    let file = crate::git::temp_bundle("harvest");
    tokio::fs::write(&file, bundle).await?;
    let refspec = format!("{branch}:{branch}");
    host.run(["git", "fetch", &file, &refspec]).await?;
    tokio::fs::remove_file(&file).await?;
    host.run(["git", "push", "origin", branch]).await?;
    let pr = [
        "gh", "pr", "create", "--head", branch, "--base", "main", "--title", title, "--body", body,
    ];
    Ok(host.run(pr).await?)
}
