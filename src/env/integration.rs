// SPDX-License-Identifier: MIT
// Copyright (c) 2026 oat-agents contributors

//! The seam a role launch uses to reach an execution environment.
//!
//! `role fire` only ever needs two things from this module: bring one environment up (or reuse
//! it) for a worktree, and read back what the ledger already proves about that worktree. Both
//! are behind a trait so a launch's decision to give a role an environment — and to fold the
//! ledger's reusable entries into its prompt — can be tested without a cluster.

use super::fingerprint::{self, Fingerprint};
use super::state::EnvStore;
use super::EnvRecord;
use crate::environment::Environment;
use crate::event_log::EventLog;
use anyhow::Result;
use serde_json::Value;
use std::path::Path;

pub trait ExecEnvironments {
    /// Brings up (or reuses) the environment for one role's worktree under the Run's chosen
    /// profile.
    fn ensure(
        &self,
        environment: &dyn Environment,
        log: &EventLog,
        run_id: &str,
        profile: &str,
        role: &str,
        worktree: &Path,
    ) -> Result<EnvRecord>;

    /// The ledger's reusable entries for this worktree, judged against the environment a role
    /// is about to run in.
    fn reusable(&self, environment: &dyn Environment, worktree: &Path, against: &EnvRecord) -> Value;

    /// Removes every environment registered to a Run, as it closes. Reports what it removed
    /// and what it could not, rather than failing: a cluster that is unreachable must not keep
    /// a Run from finishing.
    fn remove_run(&self, environment: &dyn Environment, log: &EventLog, run_id: &str) -> Value;
}

/// The real implementation: creates an environment through whichever backend the resolved
/// profile selects — Kubernetes or Docker, the two this repository ships — and reads the
/// on-disk ledger. Used by every live launch; a test that wants to exercise `role fire`'s
/// wiring without a cluster or a daemon provides its own `ExecEnvironments` instead.
pub struct LiveExecEnvironments;

impl ExecEnvironments for LiveExecEnvironments {
    fn ensure(
        &self,
        environment: &dyn Environment,
        log: &EventLog,
        run_id: &str,
        profile: &str,
        role: &str,
        worktree: &Path,
    ) -> Result<EnvRecord> {
        super::commands::bring_up(environment, log, Some(profile), Some(run_id), role, worktree, false)
    }

    fn reusable(&self, environment: &dyn Environment, worktree: &Path, against: &EnvRecord) -> Value {
        let Ok(store) = EnvStore::open(environment) else {
            return Value::Null;
        };
        let now: Fingerprint = fingerprint::of(worktree, fingerprint::Limits::default());
        let entries = store.ledgers_for_worktree(worktree, None);
        super::commands::classify(&entries, &now, against)
    }

    fn remove_run(&self, environment: &dyn Environment, log: &EventLog, run_id: &str) -> Value {
        super::commands::tear_down_run(environment, log, run_id)
    }
}
