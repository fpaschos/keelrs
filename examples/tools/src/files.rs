//! `list_files`: runs `ls` under the current directory.

use std::path::{Component, Path};

use keelrs::{Tool, ToolCall};
use schemars::JsonSchema;
use serde::Deserialize;

use crate::failure::{Failure, fail};

#[derive(Deserialize, JsonSchema)]
struct ListFiles {
    /// A directory relative to the current one; the current one when absent.
    path: Option<String>,
}

pub(crate) fn list_files() -> Tool {
    Tool::new(
        "list_files",
        "List the files in a directory under the current one, with `ls`.",
        async |call: ToolCall<ListFiles>| {
            let path = call.data.path.unwrap_or_else(|| ".".to_owned());
            if Path::new(&path)
                .components()
                .any(|c| !matches!(c, Component::Normal(_) | Component::CurDir))
            {
                return fail(format!(
                    "{path} is not a relative path under the current directory"
                ));
            }
            let out = tokio::process::Command::new("ls")
                .arg(&path)
                .output()
                .await
                .map_err(|e| Failure(e.to_string()))?;
            if !out.status.success() {
                return fail(String::from_utf8_lossy(&out.stderr).trim().to_owned());
            }
            Ok(String::from_utf8_lossy(&out.stdout).into_owned())
        },
    )
}
