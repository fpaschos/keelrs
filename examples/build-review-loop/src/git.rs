//! The sandbox boundary for code: [`seed`] puts a branch of this checkout into the sandbox as
//! bytes, and [`harvest`] takes a branch's new commits back out the same way. Both use only
//! `write`, `read` and `exec`, so they work on any backend: no mount, no network, no token.

use anyhow::ensure;
use keelrs::sandbox::{ExecRequest, Exit, Sandbox};

use crate::host;

/// The clone, relative to the sandbox's root.
pub const WORK: &str = "work";
/// What `seed` writes and `harvest` reads, at the sandbox's root.
const SEED: &str = "seed.bundle";
const HARVEST: &str = "harvest.bundle";
/// The largest bundle `harvest` takes back.
const MAX_BUNDLE: u64 = 64 * 1024 * 1024;

/// Bundles `branch` of this checkout into `sandbox` and clones it at [`WORK`], committing as
/// the loop's identity; `origin` in the clone is the bundle, so `origin/<branch>` is the base.
pub async fn seed(sandbox: &Sandbox, branch: &str) -> anyhow::Result<()> {
    let bundle = host::git(&["bundle", "create", "-", branch]).await?;
    sandbox.write(SEED, bundle).await?;
    run(sandbox, ["git", "clone", "--branch", branch, SEED, WORK]).await?;
    let work = sandbox.work_dir(WORK);
    run(
        &work,
        ["git", "config", "user.name", "keelrs build-review-loop"],
    )
    .await?;
    run(
        &work,
        [
            "git",
            "config",
            "user.email",
            "build-review-loop@keelrs.invalid",
        ],
    )
    .await?;
    Ok(())
}

/// Commits everything in `work` with `message`, unless nothing changed.
pub async fn commit_all(work: &Sandbox, message: &str) -> anyhow::Result<()> {
    if run(work, ["git", "status", "--porcelain"])
        .await?
        .is_empty()
    {
        return Ok(());
    }
    run(work, ["git", "add", "--all"]).await?;
    run(work, ["git", "commit", "--quiet", "--message", message]).await?;
    Ok(())
}

/// The commits on `branch` since the seeded `base`, as a bundle read out of `sandbox`.
pub async fn harvest(sandbox: &Sandbox, base: &str, branch: &str) -> anyhow::Result<Vec<u8>> {
    let range = format!("origin/{base}..{branch}");
    let out = format!("../{HARVEST}");
    let work = sandbox.work_dir(WORK);
    run(
        &work,
        ["git", "bundle", "create", out.as_str(), range.as_str()],
    )
    .await?;
    // Through the root handle: `work` refuses a path outside its work dir.
    let bundle = sandbox.read(HARVEST, MAX_BUNDLE).await?;
    ensure!(
        bundle.size <= MAX_BUNDLE,
        "the bundle is {} bytes",
        bundle.size
    );
    Ok(bundle.bytes)
}

/// Runs `argv` in `sandbox`'s work dir and returns its stdout, or an error naming the command.
pub async fn run<const N: usize>(sandbox: &Sandbox, argv: [&str; N]) -> anyhow::Result<String> {
    let ran = sandbox.exec(ExecRequest::argv(argv)).await?.run().await?;
    ensure!(
        ran.exit == Exit::Code(0),
        "`{}` ended with {:?}: {}",
        argv.join(" "),
        ran.exit,
        ran.stderr.trim()
    );
    Ok(ran.stdout)
}
