//! A temp dir with a local sandbox on it, for the sandbox tests.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use keelrs::sandbox::{self, Sandbox};

/// A fresh dir under the system temp dir, removed on drop.
pub(crate) struct TempDir(pub(crate) PathBuf);

impl TempDir {
    pub(crate) fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("keelrs-sandbox-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&path).unwrap();
        Self(std::fs::canonicalize(path).unwrap())
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A local sandbox rooted at `dir`.
pub(crate) async fn open(dir: &TempDir) -> Sandbox {
    sandbox::local().root(&dir.0).open().await.unwrap()
}
