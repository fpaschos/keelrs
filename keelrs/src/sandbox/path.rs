//! [`SandboxPath`]: how a sandbox names a file.

use std::fmt;

/// A path inside a sandbox: UTF-8, POSIX (`/`-separated) and absolute,
/// with `.` and `..` folded away. It serialises as its text.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize)]
#[serde(transparent)]
pub struct SandboxPath(String);

impl SandboxPath {
    /// `path` folded, or `None` when it is not absolute.
    #[must_use]
    pub fn new(path: &str) -> Option<Self> {
        path.starts_with('/').then(|| Self::fold(path))
    }

    /// The path as text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// `path` joined onto this one when relative, as it is when absolute,
    /// then folded.
    #[must_use]
    pub fn join(&self, path: &str) -> Self {
        if path.starts_with('/') {
            Self::fold(path)
        } else {
            Self::fold(&format!("{}/{path}", self.0))
        }
    }

    /// Whether this path is `base` or lies below it, part by part.
    #[must_use]
    pub fn starts_with(&self, base: &Self) -> bool {
        base.0 == "/"
            || self.0 == base.0
            || self
                .0
                .strip_prefix(&base.0)
                .is_some_and(|rest| rest.starts_with('/'))
    }

    /// An absolute path with `.`, `..` and empty parts folded; `..` stops at `/`.
    fn fold(path: &str) -> Self {
        let mut parts: Vec<&str> = Vec::new();
        for part in path.split('/') {
            match part {
                "" | "." => {}
                ".." => {
                    parts.pop();
                }
                part => parts.push(part),
            }
        }
        Self(format!("/{}", parts.join("/")))
    }
}

impl fmt::Display for SandboxPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Debug for SandboxPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.0, f)
    }
}
