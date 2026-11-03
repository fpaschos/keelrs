//! A backend of our own behind the `Sandbox` handle: the default methods it
//! gets, a refused operation, and how often a handle calls the backend.

use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;

use keelrs::sandbox::{
    ContentHash, Contents, ExecRequest, Execution, Sandbox, SandboxBackend, SandboxError,
    SandboxPath, Stat,
};
use keelrs::{Error, Result};
use parking_lot::Mutex;

/// Files in memory: a backend with only the required methods, and no exec.
#[derive(Default)]
struct Memory(Mutex<HashMap<SandboxPath, Vec<u8>>>);

#[keelrs::async_trait]
impl SandboxBackend for Memory {
    async fn stat(&self, path: &SandboxPath) -> Result<Stat> {
        Ok(match self.0.lock().get(path) {
            Some(bytes) => Stat::File {
                size: bytes.len() as u64,
                read_only: false,
                modified: None,
            },
            None => Stat::Missing,
        })
    }

    async fn read(&self, path: &SandboxPath, limit: u64) -> Result<Contents> {
        let files = self.0.lock();
        let bytes = files.get(path).ok_or_else(|| SandboxError::Io {
            operation: "read",
            path: path.to_string(),
            kind: std::io::ErrorKind::NotFound,
            message: "no such file".to_owned(),
        })?;
        Ok(Contents {
            bytes: bytes.iter().copied().take(limit as usize).collect(),
            size: bytes.len() as u64,
        })
    }

    async fn write(&self, path: &SandboxPath, bytes: Vec<u8>) -> Result<()> {
        self.0.lock().insert(path.clone(), bytes);
        Ok(())
    }

    async fn remove(&self, path: &SandboxPath, _recursive: bool) -> Result<()> {
        self.0.lock().remove(path);
        Ok(())
    }

    async fn rename(&self, from: &SandboxPath, to: &SandboxPath) -> Result<()> {
        let mut files = self.0.lock();
        if let Some(bytes) = files.remove(from) {
            files.insert(to.clone(), bytes);
        }
        Ok(())
    }

    async fn list(&self, root: &SandboxPath, _glob: Option<&str>) -> Result<Vec<SandboxPath>> {
        let files = self.0.lock();
        Ok(files
            .keys()
            .filter(|path| path.starts_with(root))
            .cloned()
            .collect())
    }

    async fn canonical(&self, path: &SandboxPath) -> Result<SandboxPath> {
        Ok(path.clone())
    }

    async fn exec(&self, _request: ExecRequest) -> Result<Execution> {
        Err(SandboxError::Unsupported { operation: "exec" }.into())
    }
}

#[tokio::test]
async fn a_backend_of_our_own_gets_write_if_and_read_files_by_default() {
    let sb = Sandbox::new(Memory::default(), SandboxPath::new("/work").unwrap());
    sb.write("a.txt", b"one".to_vec()).await.unwrap();

    let stale = sb
        .write_if("a.txt", b"two".to_vec(), ContentHash::of(b"zero"))
        .await
        .unwrap();
    assert_eq!(stale.map(|now| now.bytes), Some(b"one".to_vec()));
    let fresh = sb
        .write_if("a.txt", b"two".to_vec(), ContentHash::of(b"one"))
        .await
        .unwrap();
    assert_eq!(fresh, None);

    let files = sb
        .read_files(&["a.txt", "missing.txt"], 1024)
        .await
        .unwrap();
    assert_eq!(files, vec![Some(b"two".to_vec()), None]);
}

#[tokio::test]
async fn a_backend_refusing_an_operation_reaches_the_caller_as_unsupported() {
    let sb = Sandbox::new(Memory::default(), SandboxPath::new("/work").unwrap());
    let refused = sb.exec(ExecRequest::shell("true")).await;
    assert!(matches!(
        refused,
        Err(Error::Sandbox(SandboxError::Unsupported {
            operation: "exec"
        }))
    ));
}

/// The backend calls a sandbox made, by name.
type Calls = Arc<Mutex<HashMap<&'static str, usize>>>;

/// [`Memory`], counting the backend calls.
struct Counting {
    files: Memory,
    calls: Calls,
}

impl Counting {
    fn count(&self, call: &'static str) {
        *self.calls.lock().entry(call).or_default() += 1;
    }
}

#[keelrs::async_trait]
impl SandboxBackend for Counting {
    async fn stat(&self, path: &SandboxPath) -> Result<Stat> {
        self.count("stat");
        self.files.stat(path).await
    }

    async fn read(&self, path: &SandboxPath, limit: u64) -> Result<Contents> {
        self.count("read");
        self.files.read(path, limit).await
    }

    async fn write(&self, path: &SandboxPath, bytes: Vec<u8>) -> Result<()> {
        self.count("write");
        self.files.write(path, bytes).await
    }

    async fn remove(&self, path: &SandboxPath, recursive: bool) -> Result<()> {
        self.count("remove");
        self.files.remove(path, recursive).await
    }

    async fn rename(&self, from: &SandboxPath, to: &SandboxPath) -> Result<()> {
        self.count("rename");
        self.files.rename(from, to).await
    }

    async fn list(&self, root: &SandboxPath, glob: Option<&str>) -> Result<Vec<SandboxPath>> {
        self.count("list");
        self.files.list(root, glob).await
    }

    async fn canonical(&self, path: &SandboxPath) -> Result<SandboxPath> {
        self.count("canonical");
        self.files.canonical(path).await
    }

    async fn canonical_all(&self, paths: &[SandboxPath]) -> Vec<Result<SandboxPath>> {
        self.count("canonical_all");
        self.files.canonical_all(paths).await
    }

    async fn read_files(&self, paths: &[SandboxPath], limit: u64) -> Vec<Option<Vec<u8>>> {
        self.count("read_files");
        self.files.read_files(paths, limit).await
    }

    async fn exec(&self, request: ExecRequest) -> Result<Execution> {
        self.files.exec(request).await
    }
}

/// The backend calls made during `work`, by name.
async fn counted(calls: &Calls, work: impl Future<Output = ()>) -> Vec<(&'static str, usize)> {
    calls.lock().clear();
    work.await;
    let mut made: Vec<_> = calls.lock().drain().collect();
    made.sort_unstable();
    made
}

#[tokio::test]
async fn an_operation_resolves_its_path_once_and_the_work_dir_once_per_handle() {
    let calls = Calls::default();
    let counting = Counting {
        files: Memory::default(),
        calls: calls.clone(),
    };
    let sb = Sandbox::new(counting, SandboxPath::new("/work").unwrap());

    let first = counted(&calls, async {
        sb.write("a.txt", b"one".to_vec()).await.unwrap();
    });
    assert_eq!(first.await, [("canonical", 2), ("write", 1)]);

    let clone = sb.clone();
    let read = counted(&calls, async {
        clone.read("a.txt", 1024).await.unwrap();
    });
    assert_eq!(read.await, [("canonical", 1), ("read", 1)]);
    let stat = counted(&calls, async {
        sb.stat("a.txt").await.unwrap();
    });
    assert_eq!(stat.await, [("canonical", 1), ("stat", 1)]);
    let canonical = counted(&calls, async {
        sb.canonical("a.txt").await.unwrap();
    });
    assert_eq!(canonical.await, [("canonical", 1)]);
    let write = counted(&calls, async {
        sb.write("b.txt", b"two".to_vec()).await.unwrap();
    });
    assert_eq!(write.await, [("canonical", 1), ("write", 1)]);

    let read_files = counted(&calls, async {
        sb.read_files(&["a.txt", "b.txt", "c.txt"], 1024)
            .await
            .unwrap();
    });
    assert_eq!(read_files.await, [("canonical_all", 1), ("read_files", 1)]);

    let narrowed = sb.work_dir("sub");
    let fresh = counted(&calls, async {
        narrowed.stat("a.txt").await.unwrap();
    });
    assert_eq!(fresh.await, [("canonical", 2), ("stat", 1)]);
}
