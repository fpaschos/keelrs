//! Helpers shared by the integration tests.

use keelrs::sandbox::{self, Sandbox};

/// A local sandbox rooted at the system temp dir.
pub(crate) async fn temp_sandbox() -> Sandbox {
    sandbox::local()
        .root(std::env::temp_dir())
        .open()
        .await
        .unwrap()
}
