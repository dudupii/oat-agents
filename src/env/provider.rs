// SPDX-License-Identifier: MIT
// Copyright (c) 2026 oat-agents contributors

//! The seam between the workflow and whatever actually runs a command.

use super::{EnvRecord, EnvSpec, ExecOutcome, ExecRequest};
use anyhow::Result;
use serde_json::Value;
use std::path::Path;

/// A ready environment. This is the only shape the rest of the CLI knows about.
pub trait ExecEnvironment {
    /// Runs one command, streaming its output to this process's stdout and stderr and
    /// returning the command's own exit code.
    fn exec(&self, request: &ExecRequest) -> Result<ExecOutcome>;
}

/// Creates, finds, and removes environments. One implementation per backend; Kubernetes and
/// Docker are the two this repository ships, and which one a profile gets is decided solely by
/// its `kind` (ticket Architecture).
pub trait ExecProvider {
    /// Creates the environment if it is not already there and returns the record that lets any
    /// later process find it again.
    fn ensure(&self, spec: &EnvSpec) -> Result<EnvRecord>;
    fn attach(&self, record: &EnvRecord) -> Result<Box<dyn ExecEnvironment>>;
    /// Brings back anything the workspace strategy has to carry out of the environment before
    /// it is removed. A mounted workspace has nothing to carry; a copied one has everything.
    fn collect(&self, record: &EnvRecord) -> Result<()>;
    fn destroy(&self, record: &EnvRecord) -> Result<()>;
    /// Removes environments left behind by a crashed run and reports what went.
    fn reap(&self, run_id: Option<&str>, all: bool) -> Result<Vec<String>>;
    /// Reports what a person would otherwise have to check by hand before believing a failure
    /// is theirs and not the backend's.
    fn doctor(&self) -> Result<Value>;
}

/// How the worktree gets into the environment, and what has to come back out.
pub trait WorkspaceStrategy {
    fn bind(&self, worktree: &Path) -> Result<WorkspaceBinding>;
    fn collect(&self, environment: &dyn ExecEnvironment, worktree: &Path) -> Result<()>;
}

/// The volume and mount a provider should attach, described as the API objects themselves so
/// the strategy owns every detail of how the files get there.
#[derive(Debug, Clone)]
pub struct WorkspaceBinding {
    pub container_path: std::path::PathBuf,
    pub volume: Value,
    pub mount: Value,
}
