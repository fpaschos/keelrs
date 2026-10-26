//! rig-tools' coding tools (`read`, `write`, `edit`, `search`, `list`, `shell`) as keelrs tools,
//! on this machine from the process's working directory, until keelrs has its own (#26).
//!
//! keelrs runs a reply's calls side by side and does not read Rig's footprints, so two `edit`s
//! of one file could each write over the other. One lock stands in for footprints: read-only
//! tools share it, the others take it alone.

use std::sync::Arc;
use std::time::Duration;

use keelrs::{Tool, ToolCall};
use rig_core::tool::{Tool as RigTool, ToolContext};
use rig_tools::{Edit, List, Read, Search, Shell, Write};
use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use tokio::sync::RwLock;

/// Longest a `shell` command runs; the model then reads that it timed out.
const SHELL_TIMEOUT: Duration = Duration::from_mins(20);

/// One set of coding tools; clones share the lock and what `read` has seen.
#[derive(Clone)]
pub struct CodingTools {
    lock: Arc<RwLock<()>>,
    read: Read,
    write: Write,
    edit: Edit,
}

impl CodingTools {
    pub fn new() -> Self {
        let read = Read::default();
        Self {
            lock: Arc::default(),
            write: Write {
                seen: read.seen.clone(),
                ..Write::default()
            },
            edit: Edit {
                seen: read.seen.clone(),
                ..Edit::default()
            },
            read,
        }
    }

    /// `read`, `search`, `list` and `shell`: looking around, and `git`/`gh` through `shell`.
    pub fn looking(&self) -> Vec<Tool> {
        vec![
            self.wrap(self.read.clone()),
            self.wrap(Search::default()),
            self.wrap(List::default()),
            self.wrap(Shell::default()).timeout(SHELL_TIMEOUT),
        ]
    }

    /// [`looking`](Self::looking), plus `write` and `edit`.
    pub fn coding(&self) -> Vec<Tool> {
        let mut tools = self.looking();
        tools.push(self.wrap(self.write.clone()));
        tools.push(self.wrap(self.edit.clone()));
        tools
    }

    /// `tool` as a keelrs tool that holds the lock while it runs and logs each call to stderr.
    fn wrap<T>(&self, tool: T) -> Tool
    where
        T: RigTool + Clone + 'static,
        T::Args: DeserializeOwned + JsonSchema + std::fmt::Debug + Send + 'static,
        T::Output: serde::Serialize + 'static,
        T::Error: Send + Sync,
    {
        let shared = tool.annotations().read_only_hint == Some(true);
        let lock = self.lock.clone();
        Tool::new(
            T::NAME,
            tool.description(),
            move |call: ToolCall<T::Args>| async move {
                eprintln!(
                    "  {} {}",
                    T::NAME,
                    rig_tools::shorten(&format!("{:?}", call.data), 160)
                );
                let _held = if shared {
                    Held::Shared(lock.read().await)
                } else {
                    Held::Alone(lock.write().await)
                };
                tool.call(&mut ToolContext::new(), call.data).await
            },
        )
    }
}

/// The lock while a call runs, shared or alone.
enum Held<'a> {
    Shared(#[expect(dead_code, reason = "held, not read")] tokio::sync::RwLockReadGuard<'a, ()>),
    Alone(#[expect(dead_code, reason = "held, not read")] tokio::sync::RwLockWriteGuard<'a, ()>),
}
