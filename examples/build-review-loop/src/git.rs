//! The sandbox boundary for code: [`seed`] puts a branch of this checkout into the sandbox as
//! bytes, and [`harvest`] takes a branch's new commits back out the same way. Both use only
//! `write`, `read` and `run`, so they work on any backend: no mount, no network, no token.

use anyhow::ensure;
use keelrs::sandbox::Sandbox;

/// The clone, relative to the sandbox's root.
pub const WORK: &str = "work";
/// What `seed` writes and `harvest` reads, at the sandbox's root.
const SEED: &str = "seed.bundle";
const HARVEST: &str = "harvest.bundle";
/// The largest bundle `harvest` takes back.
const MAX_BUNDLE: u64 = 64 * 1024 * 1024;

/// Bundles `branch` of the `host` checkout into `sandbox` and clones it at [`WORK`], committing
/// as the loop's identity; `origin` in the clone is the bundle, so `origin/<branch>` is the base.
pub async fn seed(host: &Sandbox, sandbox: &Sandbox, branch: &str) -> anyhow::Result<()> {
    let out = std::env::temp_dir().join("keelrs-build-review-loop-seed.bundle");
    let out = out.to_string_lossy();
    host.run(["git", "bundle", "create", &out, branch]).await?;
    sandbox.write(SEED, tokio::fs::read(&*out).await?).await?;
    sandbox
        .run(["git", "clone", "--branch", branch, SEED, WORK])
        .await?;
    let work = sandbox.work_dir(WORK);
    work.run(["git", "config", "user.name", "keelrs build-review-loop"])
        .await?;
    work.run([
        "git",
        "config",
        "user.email",
        "build-review-loop@keelrs.invalid",
    ])
    .await?;
    Ok(())
}

/// Commits everything in `work` with `message`, unless nothing changed.
pub async fn commit_all(work: &Sandbox, message: &str) -> anyhow::Result<()> {
    if work.run(["git", "status", "--porcelain"]).await?.is_empty() {
        return Ok(());
    }
    work.run(["git", "add", "--all"]).await?;
    work.run(["git", "commit", "--quiet", "--message", message])
        .await?;
    Ok(())
}

/// The commits on `branch` since the seeded `base`, as a bundle read out of `sandbox`.
pub async fn harvest(sandbox: &Sandbox, base: &str, branch: &str) -> anyhow::Result<Vec<u8>> {
    let range = format!("origin/{base}..{branch}");
    let out = format!("../{HARVEST}");
    let work = sandbox.work_dir(WORK);
    work.run(["git", "bundle", "create", &out, &range]).await?;
    // Through the root handle: `work` refuses a path outside its work dir.
    let bundle = sandbox.read(HARVEST, MAX_BUNDLE).await?;
    ensure!(
        bundle.size <= MAX_BUNDLE,
        "the bundle is {} bytes",
        bundle.size
    );
    Ok(bundle.bytes)
}
