// SPDX-License-Identifier: MIT
// Copyright (c) 2026 oat-agents contributors

//! Getting the worktree into the environment.
//!
//! This file is the only place that knows what a hostPath is. A cluster on another machine
//! cannot mount this machine's disk, so it will need a second strategy here — and nothing
//! outside this file will have to change.

use super::config::{Profile, WorkspaceKind};
use super::provider::{ExecEnvironment, WorkspaceBinding, WorkspaceStrategy};
use crate::error::{codes, err};
use anyhow::Result;
use serde_json::json;
use std::path::Path;

pub fn for_profile(profile: &Profile) -> Box<dyn WorkspaceStrategy> {
    match profile.workspace {
        WorkspaceKind::Hostpath => Box::new(HostPathStrategy),
    }
}

/// The uid and gid that own the worktree. A backend that keeps files written inside the
/// environment owned by the operator outside it starts from this; one that leaves ownership to
/// the image ignores it. It lives here, not in any backend, because it is a fact about the
/// worktree — the same fact whichever provider mounts it.
pub fn worktree_owner(worktree: &Path) -> Option<(u32, u32)> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(worktree).ok().map(|metadata| (metadata.uid(), metadata.gid()))
}

/// The worktree is mounted from the node at the same absolute path it has on the host. Keeping
/// the path identical means a command's cwd needs no translation and its output names files
/// the operator can open.
pub struct HostPathStrategy;

impl WorkspaceStrategy for HostPathStrategy {
    fn bind(&self, worktree: &Path) -> Result<WorkspaceBinding> {
        if !worktree.is_absolute() {
            return Err(err(
                codes::ENV_OUTSIDE_WORKSPACE,
                format!("worktree path must be absolute: {}", worktree.display()),
            ));
        }
        let path = worktree.to_string_lossy().to_string();
        Ok(WorkspaceBinding {
            container_path: worktree.to_path_buf(),
            volume: json!({
                "name": "workspace",
                "hostPath": { "path": path, "type": "Directory" }
            }),
            mount: json!({ "name": "workspace", "mountPath": path }),
        })
    }

    /// Nothing to collect: the environment has been writing to the operator's own files all
    /// along.
    fn collect(&self, _environment: &dyn ExecEnvironment, _worktree: &Path) -> Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_workspace_keeps_the_path_it_has_outside() {
        let binding = HostPathStrategy.bind(Path::new("/home/a/worktree")).unwrap();
        assert_eq!(binding.container_path, Path::new("/home/a/worktree"));
        assert_eq!(binding.volume["hostPath"]["path"], "/home/a/worktree");
        assert_eq!(binding.volume["hostPath"]["type"], "Directory");
        assert_eq!(binding.mount["mountPath"], "/home/a/worktree");
        assert_eq!(binding.mount["name"], binding.volume["name"]);
    }

    #[test]
    fn a_relative_worktree_is_refused_rather_than_guessed_at() {
        let error = HostPathStrategy.bind(Path::new("worktree")).unwrap_err();
        assert_eq!(
            error
                .downcast_ref::<crate::error::CliFailure>()
                .map(|failure| failure.code.as_str()),
            Some(codes::ENV_OUTSIDE_WORKSPACE)
        );
    }
}
