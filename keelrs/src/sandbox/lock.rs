//! Per-file locks: what keeps a reply's file tools from overwriting each
//! other's changes on one sandbox.

use std::collections::HashMap;
use std::sync::Arc;

use futures::FutureExt as _;
use tokio::sync::{Mutex, OwnedMutexGuard};

use super::{Sandbox, SandboxError, SandboxPath};
use crate::Result;

/// The per-file locks of one sandbox, shared by every clone and narrowed
/// work dir. Holds only the files locked or waited on right now.
#[derive(Default)]
pub(crate) struct FileLocks(parking_lot::Mutex<HashMap<SandboxPath, Arc<Mutex<()>>>>);

/// Files of one sandbox held against the built-in file tools and other
/// holders; released on drop. Not re-entrant: taking a file you already
/// hold waits forever.
///
/// Dropping it (a cancelled call) releases the files even if a backend
/// write started under it is still running, as `Local`'s `spawn_blocking`
/// work does; that write can then overlap the next holder's. The stale
/// check in `edit` catches it afterwards.
#[must_use = "the lock is released when this is dropped"]
pub struct FileLock(Vec<Entry>);

impl std::fmt::Debug for FileLock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list()
            .entries(self.0.iter().map(|entry| &entry.path))
            .finish()
    }
}

/// One held file of a [`FileLock`].
struct Entry {
    guard: Option<OwnedMutexGuard<()>>,
    lock: Arc<Mutex<()>>,
    path: SandboxPath,
    locks: Arc<FileLocks>,
}

impl Drop for Entry {
    fn drop(&mut self) {
        self.guard.take(); // release first
        let mut map = self.locks.0.lock();
        // Only the map and this entry hold it: no holder, no waiter.
        if Arc::strong_count(&self.lock) == 2 {
            map.remove(&self.path);
        }
    }
}

impl Sandbox {
    /// How many files of this sandbox are locked or waited on now.
    #[cfg(test)]
    pub(crate) fn locked(&self) -> usize {
        self.locks.0.lock().len()
    }

    /// Holds `path` against the built-in file tools and other holders on
    /// this sandbox until the guard drops. Take it around a whole
    /// read-modify-write. Resolved as [`canonical`](Self::canonical) does,
    /// so it fails outside the work dir.
    pub async fn lock(&self, path: impl AsRef<str>) -> Result<FileLock> {
        self.lock_all([path]).await
    }

    /// Holds every one of `paths` at once, each once, in sorted canonical
    /// order, so two holders of overlapping sets can't deadlock.
    pub async fn lock_all<P: AsRef<str>>(
        &self,
        paths: impl IntoIterator<Item = P>,
    ) -> Result<FileLock> {
        let joined: Vec<SandboxPath> = paths
            .into_iter()
            .map(|path| self.work_dir.join(path.as_ref()))
            .collect();
        let canonical = self
            .race("canonical", self.backend.canonical_all(&joined).map(Ok))
            .await?;
        let work_dir = self.real_work_dir().await?;
        let mut paths = Vec::with_capacity(canonical.len());
        for path in canonical {
            let path = path?;
            if !path.starts_with(&work_dir) {
                return Err(SandboxError::OutsideWorkDir { path, work_dir }.into());
            }
            paths.push(path);
        }
        paths.sort();
        paths.dedup();
        let mut held = Vec::with_capacity(paths.len());
        for path in paths {
            // Cloned under the map mutex, so a releasing holder sees this waiter.
            let lock = self.locks.0.lock().entry(path.clone()).or_default().clone();
            let mut entry = Entry {
                guard: None,
                lock: lock.clone(),
                path,
                locks: self.locks.clone(),
            };
            entry.guard = Some(lock.lock_owned().await);
            held.push(entry);
        }
        Ok(FileLock(held))
    }
}
