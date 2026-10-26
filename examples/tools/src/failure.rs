//! The error every tool returns: the model reads its message.

use std::fmt;

/// A tool failure the model reads as its message.
#[derive(Debug)]
pub(crate) struct Failure(pub(crate) String);

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Failure {}

pub(crate) fn fail<T>(message: impl Into<String>) -> Result<T, Failure> {
    Err(Failure(message.into()))
}
