//! A sandbox with nothing in it, for tests whose agents must declare one
//! but whose tools never use it.

use super::{
    Contents, ExecRequest, Execution, Sandbox, SandboxBackend, SandboxError, SandboxPath, Stat,
};
use crate::Result;

/// Holds no file and runs no command.
struct Empty;

#[async_trait::async_trait]
impl SandboxBackend for Empty {
    async fn stat(&self, _: &SandboxPath) -> Result<Stat> {
        Ok(Stat::Missing)
    }

    async fn read(&self, _: &SandboxPath, _: u64) -> Result<Contents> {
        Err(SandboxError::Unsupported { operation: "read" }.into())
    }

    async fn write(&self, _: &SandboxPath, _: Vec<u8>) -> Result<()> {
        Err(SandboxError::Unsupported { operation: "write" }.into())
    }

    async fn remove(&self, _: &SandboxPath, _: bool) -> Result<()> {
        Err(SandboxError::Unsupported {
            operation: "remove",
        }
        .into())
    }

    async fn rename(&self, _: &SandboxPath, _: &SandboxPath) -> Result<()> {
        Err(SandboxError::Unsupported {
            operation: "rename",
        }
        .into())
    }

    async fn list(&self, _: &SandboxPath, _: Option<&str>) -> Result<Vec<SandboxPath>> {
        Ok(Vec::new())
    }

    async fn canonical(&self, path: &SandboxPath) -> Result<SandboxPath> {
        Ok(path.clone())
    }

    async fn exec(&self, _: ExecRequest) -> Result<Execution> {
        Err(SandboxError::Unsupported { operation: "exec" }.into())
    }
}

/// A fresh sandbox with nothing in it, its own [`SandboxId`](super::SandboxId).
pub(crate) fn empty() -> Sandbox {
    Sandbox::new(Empty, SandboxPath::new("/").unwrap())
}
