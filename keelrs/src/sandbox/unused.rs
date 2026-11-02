//! A sandbox for tests whose agents must declare one but never reach it.

use super::{Contents, ExecRequest, Execution, Sandbox, SandboxBackend, SandboxPath, Stat};
use crate::Result;

struct Unused;

#[async_trait::async_trait]
impl SandboxBackend for Unused {
    async fn stat(&self, _: &SandboxPath) -> Result<Stat> {
        unreachable!()
    }

    async fn read(&self, _: &SandboxPath, _: u64) -> Result<Contents> {
        unreachable!()
    }

    async fn write(&self, _: &SandboxPath, _: Vec<u8>) -> Result<()> {
        unreachable!()
    }

    async fn remove(&self, _: &SandboxPath, _: bool) -> Result<()> {
        unreachable!()
    }

    async fn rename(&self, _: &SandboxPath, _: &SandboxPath) -> Result<()> {
        unreachable!()
    }

    async fn list(&self, _: &SandboxPath, _: Option<&str>) -> Result<Vec<SandboxPath>> {
        unreachable!()
    }

    async fn canonical(&self, _: &SandboxPath) -> Result<SandboxPath> {
        unreachable!()
    }

    async fn exec(&self, _: ExecRequest) -> Result<Execution> {
        unreachable!()
    }
}

/// A fresh sandbox, its own [`SandboxId`](super::SandboxId), that panics if used.
pub(crate) fn unused() -> Sandbox {
    Sandbox::new(Unused, SandboxPath::new("/").unwrap())
}
