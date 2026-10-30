// SPDX-License-Identifier: MIT
// Copyright (c) 2026 oat-agents contributors

//! The `env` command group, and the lifecycle calls a launch makes.
//!
//! Agents only ever reach for `env exec` (and `env doctor` when it fails). Creating and
//! removing environments belongs to the CLI, because an agent that has to set up its own
//! environment is an agent that will eventually forget to.

use super::config;
use super::docker::DockerProvider;
use super::fingerprint::{self, Fingerprint};
use super::image;
use super::kubernetes::KubernetesProvider;
use super::provider::ExecProvider;
use super::state::EnvStore;
use super::{EnvRecord, EnvSpec, ExecRequest, LedgerEntry};
use crate::environment::Environment;
use crate::error::{codes, err};
use crate::event_log::{events, now_iso, EventLog, LogEntry};
use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The key that tells `main` to exit with the command's own status instead of printing a
/// result. Everything a person needs was already streamed out.
pub const EXIT_MARKER: &str = "oat_exit";

#[derive(Debug, Args)]
pub struct EnvCommand {
    #[command(subcommand)]
    pub command: EnvSubcommand,
}

#[derive(Debug, Subcommand)]
pub enum EnvSubcommand {
    /// Create the execution environment for one role's worktree.
    Up(EnvUpArgs),
    /// Run one command inside the environment bound to this worktree.
    Exec(EnvExecArgs),
    /// Report the registered environments.
    Status(EnvStatusArgs),
    /// Report whether this machine can provide environments at all.
    Doctor(EnvDoctorArgs),
    /// Build or reuse the image a worktree's `.devcontainer/` describes.
    Image(EnvImageArgs),
    /// Rebuild the image this worktree's environment runs, and replace the container with one
    /// built from it.
    Rebuild(EnvRebuildArgs),
    /// Remove an environment.
    Down(EnvDownArgs),
    /// Remove environments left behind by a run that ended badly.
    Reap(EnvReapArgs),
    /// Report what has already been executed against the code as it stands.
    Evidence(EnvEvidenceArgs),
}

#[derive(Debug, Args)]
pub struct EnvUpArgs {
    #[arg(long, value_name = "NAME")]
    pub profile: Option<String>,
    #[arg(long, value_name = "ROLE")]
    pub role: String,
    #[arg(long, value_name = "PATH")]
    pub worktree: PathBuf,
    #[arg(long, value_name = "ID")]
    pub run: Option<String>,
    #[arg(long)]
    pub rebuild: bool,
}

#[derive(Debug, Args)]
pub struct EnvExecArgs {
    #[arg(long, value_name = "ID")]
    pub env: Option<String>,
    #[arg(long, value_name = "ROLE")]
    pub role: Option<String>,
    #[arg(long, value_name = "SECONDS")]
    pub timeout: Option<u64>,
    /// Follow an execution that is already running, after a dropped stream.
    #[arg(long, value_name = "EXEC-ID")]
    pub attach: Option<String>,
    /// Everything after `--`, handed to the environment unparsed.
    #[arg(last = true, value_name = "COMMAND")]
    pub command: Vec<String>,
}

#[derive(Debug, Args)]
pub struct EnvStatusArgs {
    #[arg(long, value_name = "ID")]
    pub run: Option<String>,
    #[arg(long, value_name = "ID")]
    pub env: Option<String>,
}

#[derive(Debug, Args)]
pub struct EnvDoctorArgs {
    #[arg(long, value_name = "NAME")]
    pub profile: Option<String>,
    #[arg(long, value_name = "PATH", default_value = ".")]
    pub repo: PathBuf,
}

#[derive(Debug, Args)]
pub struct EnvImageArgs {
    #[arg(long, value_name = "NAME")]
    pub profile: Option<String>,
    #[arg(long, value_name = "PATH", default_value = ".")]
    pub worktree: PathBuf,
    #[arg(long)]
    pub rebuild: bool,
}

#[derive(Debug, Args)]
pub struct EnvRebuildArgs {
    /// Which environment, when the working directory is not inside it.
    #[arg(long, value_name = "ID")]
    pub env: Option<String>,
    #[arg(long, value_name = "ROLE")]
    pub role: Option<String>,
}

#[derive(Debug, Args)]
pub struct EnvDownArgs {
    #[arg(long, value_name = "ID")]
    pub env: Option<String>,
    #[arg(long, value_name = "ID")]
    pub run: Option<String>,
}

#[derive(Debug, Args)]
pub struct EnvReapArgs {
    /// Which cluster to sweep. Reaping is what is left when everything else has already gone
    /// wrong, so it must not depend on a registration surviving to say where to look.
    #[arg(long, value_name = "NAME")]
    pub profile: Option<String>,
    #[arg(long, value_name = "ID")]
    pub run: Option<String>,
    #[arg(long)]
    pub all: bool,
}

#[derive(Debug, Args)]
pub struct EnvEvidenceArgs {
    /// Default: the worktree the current directory belongs to.
    #[arg(long, value_name = "PATH")]
    pub worktree: Option<PathBuf>,
    /// How far back to read. A ledger is never deleted, so without a window this would
    /// eventually answer with months of other work.
    #[arg(long, value_name = "HOURS", default_value = "168")]
    pub since_hours: u64,
    /// Leave out everything with a reason it cannot stand in for a re-run.
    #[arg(long)]
    pub matching: bool,
}

pub fn execute(environment: &dyn Environment, log: &EventLog, command: EnvSubcommand) -> Result<Value> {
    match command {
        EnvSubcommand::Up(args) => {
            let record = bring_up(
                environment,
                log,
                args.profile.as_deref(),
                args.run.as_deref(),
                &args.role,
                &args.worktree,
                args.rebuild,
            )?;
            Ok(serde_json::to_value(record).expect("env records always serialize"))
        }
        EnvSubcommand::Exec(args) => exec(environment, args),
        EnvSubcommand::Rebuild(args) => rebuild(environment, log, args),
        EnvSubcommand::Status(args) => status(environment, args),
        EnvSubcommand::Doctor(args) => doctor(environment, args),
        EnvSubcommand::Image(args) => {
            let worktree = canonical(&args.worktree)?;
            let config = config::load(environment, Some(&worktree))?;
            let (_, profile) = config::resolve(&config, args.profile.as_deref())?;
            let image = image::ensure(&profile, &worktree, args.rebuild)?;
            Ok(json!({
                "image_ref": image.reference,
                "image_id": image.id,
                "source_hash": image.source_hash,
                "rebuilt": image.rebuilt,
            }))
        }
        EnvSubcommand::Down(args) => down(environment, log, args),
        EnvSubcommand::Reap(args) => reap(environment, log, args),
        EnvSubcommand::Evidence(args) => evidence(environment, args),
    }
}

fn log_event(log: &EventLog, run_id: &str, event: &str, details: Value) {
    let _ = log.record(&LogEntry {
        timestamp: now_iso(),
        run_id: run_id.to_owned(),
        dispatch_id: None,
        agent: None,
        event: event.to_owned(),
        details: Some(details),
    });
}

/// Creates the environment a role will run in. Called by a launch before the agent starts, so
/// the first command the agent writes already has somewhere to go.
pub fn bring_up(
    environment: &dyn Environment,
    log: &EventLog,
    profile_name: Option<&str>,
    run_id: Option<&str>,
    role: &str,
    worktree: &Path,
    rebuild: bool,
) -> Result<EnvRecord> {
    let worktree = canonical(worktree)?;
    let config = config::load(environment, Some(&worktree))?;
    let (profile_name, profile) = config::resolve(&config, profile_name)?;
    let image = image::ensure(&profile, &worktree, rebuild)?;
    if let (Some(run_id), true) = (run_id, image.rebuilt) {
        log_event(
            log,
            run_id,
            events::ENV_IMAGE_BUILT,
            json!({
                "image_ref": image.reference,
                "image_id": image.id,
                "source_hash": image.source_hash,
            }),
        );
    }

    let spec = EnvSpec {
        env_id: super::env_id(run_id, role, &worktree),
        run_id: run_id.map(str::to_owned),
        role: role.to_owned(),
        worktree: worktree.clone(),
        user: super::workspace::worktree_owner(&worktree),
        image,
    };
    let provider = provider_for(environment, &profile_name, profile);
    let record = provider.ensure(&spec)?;
    let store = EnvStore::open(environment)?;
    store.save(&record)?;
    if let Some(run_id) = run_id {
        for event in [events::ENV_CREATED, events::ENV_READY] {
            log_event(
                log,
                run_id,
                event,
                json!({
                    "env_id": record.env_id,
                    "role": record.role,
                    "profile": record.profile,
                    "image_id": record.image_id,
                    "worktree": record.worktree.to_string_lossy(),
                }),
            );
        }
    }
    Ok(record)
}

/// Removes one environment and says so in the log.
pub fn tear_down(environment: &dyn Environment, log: &EventLog, record: &EnvRecord) -> Result<()> {
    let provider = provider_from_record(environment, record);
    let collected = provider.collect(record);
    // The backend's word comes first, because a container the backend could not be asked
    // about is still running: dropping the record or logging a destruction now would leave
    // an orphan no later `env down` can find.
    provider.destroy(record).and(collected)?;
    let store = EnvStore::open(environment)?;
    store.remove(&record.env_id)?;
    if let Some(run_id) = record.run_id.as_deref() {
        log_event(
            log,
            run_id,
            events::ENV_DESTROYED,
            json!({ "env_id": record.env_id, "role": record.role }),
        );
    }
    Ok(())
}

/// Tears down every environment registered to `run_id`, continuing past one that fails so a
/// single unreachable pod does not leave the rest running.
pub fn tear_down_run(environment: &dyn Environment, log: &EventLog, run_id: &str) -> Value {
    let Ok(store) = EnvStore::open(environment) else {
        return json!({ "removed": [], "failed": [] });
    };
    let mut removed = Vec::new();
    let mut failed = Vec::new();
    for record in store.list().into_iter().filter(|record| record.run_id.as_deref() == Some(run_id)) {
        match tear_down(environment, log, &record) {
            Ok(()) => removed.push(record.env_id.clone()),
            Err(error) => failed.push(json!({ "env_id": record.env_id, "pod": record.pod, "error": format!("{error:#}") })),
        }
    }
    json!({ "removed": removed, "failed": failed })
}

pub fn ledger_summary(environment: &dyn Environment, env_id: &str) -> Value {
    match EnvStore::open(environment) {
        Ok(store) => store.ledger_summary(env_id),
        Err(_) => json!({ "commands": 0, "failed": 0, "total_ms": 0 }),
    }
}

/// Clears out whatever a crashed run left running. Meant to be called before every launch,
/// because the process that should have cleaned up is exactly the one that died.
pub fn reap_run(
    environment: &dyn Environment,
    log: &EventLog,
    repo: &Path,
    profile_name: Option<&str>,
    run_id: Option<&str>,
    all: bool,
) -> Result<Vec<String>> {
    let config = config::load(environment, Some(repo))?;
    let (resolved_name, profile) = config::resolve(&config, profile_name)?;
    let provider = provider_for(environment, &resolved_name, profile);
    let removed = provider.reap(run_id, all)?;
    let store = EnvStore::open(environment)?;
    for name in &removed {
        store.remove(name)?;
    }
    if let (Some(run_id), false) = (run_id, removed.is_empty()) {
        log_event(log, run_id, events::ENV_REAPED, json!({ "environments": removed }));
    }
    Ok(removed)
}

fn exec(environment: &dyn Environment, args: EnvExecArgs) -> Result<Value> {
    if args.attach.is_none() && args.command.is_empty() {
        return Err(err(
            codes::INVALID_CLI_ARGUMENTS,
            "env exec needs a command after `--`, or --attach <exec-id>",
        ));
    }
    if args.attach.is_some() && !args.command.is_empty() {
        return Err(err(
            codes::INVALID_CLI_ARGUMENTS,
            "--attach follows an execution that already has a command",
        ));
    }
    let cwd = std::env::current_dir().context("failed to read the working directory")?;
    let cwd = cwd.canonicalize().unwrap_or(cwd);
    let store = EnvStore::open(environment)?;
    let record = match args.env.as_deref() {
        Some(env_id) => store.load(env_id)?,
        None => store.find_for_cwd(&cwd, args.role.as_deref())?,
    };
    if args.role.as_deref().is_some_and(|role| record.role != role) {
        let role = args.role.as_deref().unwrap_or_default();
        return Err(err(
            codes::ENV_ROLE_MISMATCH,
            format!("environment {} belongs to the {} role, not {role}", record.env_id, record.role),
        ));
    }
    if !cwd.starts_with(&record.worktree) {
        return Err(err(
            codes::ENV_OUTSIDE_WORKSPACE,
            format!(
                "{} is outside {}, which is the only directory environment {} can reach",
                cwd.display(),
                record.worktree.display(),
                record.env_id
            ),
        ));
    }

    let request = ExecRequest {
        exec_id: args.attach.clone().unwrap_or_else(super::exec_id),
        argv: args.command.clone(),
        cwd: cwd.clone(),
        timeout: args.timeout.map(Duration::from_secs),
        attach: args.attach.is_some(),
    };
    let provider = provider_from_record(environment, &record);
    let target = provider.attach(&record)?;

    // The tree is read on both sides of the command. Before, because what a command verifies
    // is the code it started against; after, because a worktree edited while a suite was
    // running was never verified by it at all, and only two readings can tell that apart.
    let limits = fingerprint::Limits::default();
    let tree_start = fingerprint::of(&record.worktree, limits);
    store.append_exec_start(&record, &request, &tree_start)?;
    let outcome = target.exec(&request);
    let tree_end = fingerprint::of(&record.worktree, limits);
    store.append_exec_end(&record, &request, outcome.as_ref().ok(), &tree_end)?;
    let outcome = outcome?;
    Ok(json!({
        EXIT_MARKER: outcome.exit_code,
        "env_id": record.env_id,
        "exec_id": request.exec_id,
        "image_id": record.image_id,
        "ms": outcome.duration_ms,
    }))
}

/// What has already been run against the code in this worktree.
fn evidence(environment: &dyn Environment, args: EnvEvidenceArgs) -> Result<Value> {
    let from = match args.worktree {
        Some(path) => canonical(&path)?,
        None => {
            let cwd = std::env::current_dir().context("failed to read the working directory")?;
            cwd.canonicalize().unwrap_or(cwd)
        }
    };
    let store = EnvStore::open(environment)?;
    let record = store.find_for_cwd(&from, None)?;
    let since = super::now_ms().saturating_sub(args.since_hours.saturating_mul(3_600_000));

    let now = fingerprint::of(&record.worktree, fingerprint::Limits::default());
    let entries = store.ledgers_for_worktree(&record.worktree, Some(since));
    let mut report = classify(&entries, &now, &record);
    if args.matching {
        if let Some(rows) = report["entries"].as_array() {
            let kept: Vec<Value> = rows
                .iter()
                .filter(|row| row["reuse_blocked_by"].as_array().is_some_and(|list| list.is_empty()))
                .cloned()
                .collect();
            report["entries"] = Value::Array(kept);
        }
    }
    Ok(report)
}

/// Pure over its inputs, so what counts as prior verification can be tested without a state
/// directory or a cluster.
pub fn classify(entries: &[LedgerEntry], now: &Fingerprint, against: &EnvRecord) -> Value {
    let rows: Vec<Value> = entries
        .iter()
        .map(|entry| {
            let mut blocked: Vec<&str> = Vec::new();

            let finished = entry.finished();
            let compared = match finished {
                true => entry.settled_tree(),
                false => Some(&entry.tree_start),
            };
            let tree_match = compared.is_some_and(|tree| fingerprint::same(tree, now));
            if !now.is_known() || !entry.tree_start.is_known() {
                blocked.push("tree_unknown");
            } else if finished
                && entry.tree_end.as_ref().is_none_or(|end| !fingerprint::same(&entry.tree_start, end))
            {
                blocked.push("tree_moved_during_run");
            } else if !tree_match {
                blocked.push("tree_changed");
            }

            let image_match = entry.image_id == against.image_id;
            if !image_match {
                blocked.push("image_changed");
            }
            if entry.profile != against.profile || entry.context != against.context || entry.namespace != against.namespace {
                blocked.push("placement_changed");
            }
            match entry.exit {
                None => blocked.push("never_finished"),
                Some(exit) if exit != 0 => blocked.push("nonzero_exit"),
                Some(_) => {}
            }

            json!({
                "exec_id": entry.exec_id,
                "argv": entry.argv,
                "role": entry.role,
                "env_id": entry.env_id,
                "cwd": entry.cwd.to_string_lossy(),
                "ts": entry.ts,
                "exit": entry.exit,
                "ms": entry.ms,
                "tree_match": tree_match,
                "image_match": image_match,
                "reuse_blocked_by": blocked,
            })
        })
        .collect();

    json!({
        "worktree": against.worktree.to_string_lossy(),
        "tree": now,
        "image_id": against.image_id,
        "entries": rows,
        "note": "reuse_blocked_by lists only mechanical disqualifications. An empty list \
    is not a judgement that the command is worth reusing: whether a command \
    is reproducible at all is a question about the command, not the ledger.",
    })
}

/// Rebuilds the environment bound to this worktree and replaces its container. The record's
/// backend is the one that gets the replacement: a profile that has since changed `kind` is
/// refused rather than followed, because a rebuild that silently swapped backends would
/// overwrite the record with an environment the previous one's coordinates know nothing about.
fn rebuild(environment: &dyn Environment, log: &EventLog, args: EnvRebuildArgs) -> Result<Value> {
    let store = EnvStore::open(environment)?;
    let record = match args.env.as_deref() {
        Some(env_id) => store.load(env_id)?,
        None => {
            let cwd = std::env::current_dir().context("failed to read the working directory")?;
            let cwd = cwd.canonicalize().unwrap_or(cwd);
            store.find_for_cwd(&cwd, args.role.as_deref())?
        }
    };
    let config = config::load(environment, Some(&record.worktree))?;
    let (_, profile) = config::resolve(&config, Some(&record.profile))?;
    if profile.kind != record.kind {
        return Err(err(
            codes::ENV_BACKEND_ERROR,
            format!(
                "profile '{}' now selects the {} backend, but environment {} was placed by the \
                 {} backend; rebuilding it under another backend would replace it somewhere the \
                 record has never been. Point the profile back at {} or remove the environment \
                 with `env down` first",
                record.profile,
                backend_name(profile.kind),
                record.env_id,
                backend_name(record.kind),
                backend_name(record.kind),
            ),
        ));
    }
    let previous = record.image_id.clone();
    let rebuilt = bring_up(environment, log, Some(&record.profile), record.run_id.as_deref(), &record.role, &record.worktree, true)?;
    Ok(json!({
        "env_id": rebuilt.env_id,
        "role": rebuilt.role,
        "image_ref": rebuilt.image_ref,
        "image_id": rebuilt.image_id,
        "previous_image_id": previous,
        "changed": rebuilt.image_id != previous,
    }))
}

/// What a backend is called in messages a person reads. The record already carries the enum;
/// this is only its prose.
fn backend_name(kind: config::ProviderKind) -> &'static str {
    match kind {
        config::ProviderKind::Kubernetes => "kubernetes",
        config::ProviderKind::Docker => "docker",
    }
}

fn status(environment: &dyn Environment, args: EnvStatusArgs) -> Result<Value> {
    let store = EnvStore::open(environment)?;
    let records: Vec<EnvRecord> = store
        .list()
        .into_iter()
        .filter(|record| args.run.as_deref().is_none_or(|run| record.run_id.as_deref() == Some(run)))
        .filter(|record| args.env.as_deref().is_none_or(|env| record.env_id == env))
        .collect();
    let environments: Vec<Value> = records
        .iter()
        .map(|record| {
            let mut value = serde_json::to_value(record).expect("env records always serialize");
            if let Some(object) = value.as_object_mut() {
                object.insert("ledger".to_owned(), store.ledger_summary(&record.env_id));
            }
            value
        })
        .collect();
    Ok(json!({ "environments": environments }))
}

fn doctor(environment: &dyn Environment, args: EnvDoctorArgs) -> Result<Value> {
    let repo = canonical(&args.repo)?;
    let config = config::load(environment, Some(&repo))?;
    let (profile_name, profile) = config::resolve(&config, args.profile.as_deref())?;
    let devcontainer = image::devcontainer_dir(&repo);
    let provider = provider_for(environment, &profile_name, profile);
    let mut report = provider.doctor()?;
    if let Some(object) = report.as_object_mut() {
        object.insert(
            "devcontainer".to_owned(),
            json!({ "path": devcontainer.to_string_lossy(), "present": devcontainer.is_dir() }),
        );
        object.insert(
            "state_dir".to_owned(),
            json!(super::state::resolve_state_dir(environment).map(|dir| dir.to_string_lossy().to_string())),
        );
    }
    Ok(report)
}

fn down(environment: &dyn Environment, log: &EventLog, args: EnvDownArgs) -> Result<Value> {
    let store = EnvStore::open(environment)?;
    let records: Vec<EnvRecord> = match (args.env.as_deref(), args.run.as_deref()) {
        (Some(env_id), _) => vec![store.load(env_id)?],
        (None, Some(run)) => store.list().into_iter().filter(|record| record.run_id.as_deref() == Some(run)).collect(),
        (None, None) => {
            return Err(err(codes::INVALID_CLI_ARGUMENTS, "env down needs --env or --run"));
        }
    };
    let mut removed = Vec::new();
    for record in &records {
        tear_down(environment, log, record)?;
        removed.push(record.env_id.clone());
    }
    Ok(json!({ "removed": removed }))
}

fn reap(environment: &dyn Environment, log: &EventLog, args: EnvReapArgs) -> Result<Value> {
    let store = EnvStore::open(environment)?;
    let matching = |record: &&EnvRecord| args.run.as_deref().is_none_or(|run| record.run_id.as_deref() == Some(run));
    let records = store.list();
    let known = records.iter().find(matching);
    let profile = args.profile.as_deref().or_else(|| known.map(|record| record.profile.as_str()));
    let repo = known
        .map(|record| record.worktree.clone())
        .unwrap_or(std::env::current_dir().context("failed to read the working directory")?);
    let removed = reap_run(environment, log, &repo, profile, args.run.as_deref(), args.all)?;
    Ok(json!({ "reaped": removed }))
}

/// The one place a profile becomes a backend. The kind decides it and nothing else does, so a
/// second backend is a profile in a TOML file, never a code change anywhere above this.
fn provider_for(environment: &dyn Environment, profile_name: &str, profile: config::Profile) -> Box<dyn ExecProvider> {
    match profile.kind {
        config::ProviderKind::Kubernetes => Box::new(KubernetesProvider::new(profile_name, profile)),
        config::ProviderKind::Docker => Box::new(DockerProvider::new(environment, profile_name, profile)),
    }
}

/// A registered environment carries its own coordinates, so reaching it again never depends on
/// a config file that may since have changed — not even on the machine still resolving the
/// same backend, which is why the record carries its kind.
fn provider_from_record(environment: &dyn Environment, record: &EnvRecord) -> Box<dyn ExecProvider> {
    let profile = config::Profile {
        kind: record.kind,
        context: record.context.clone(),
        namespace: record.namespace.clone(),
        workspace: config::WorkspaceKind::Hostpath,
        run_as_uid: config::RunAsUid::Host,
        idle_ttl: None,
        kubeconfig: None,
        ready_timeout_secs: None,
        image: config::ImageConfig::default(),
    };
    match record.kind {
        config::ProviderKind::Kubernetes => Box::new(KubernetesProvider::new(&record.profile, profile)),
        config::ProviderKind::Docker => Box::new(DockerProvider::new(environment, &record.profile, profile)),
    }
}

fn canonical(path: &Path) -> Result<PathBuf> {
    path.canonicalize().with_context(|| format!("failed to resolve {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env::fingerprint::{Fingerprint, UnknownReason};

    fn known(digest: &str) -> Fingerprint {
        Fingerprint::Known { digest: digest.to_owned(), head: Some("0".repeat(40)), dirty: false, untracked: 0 }
    }

    fn unknown() -> Fingerprint {
        Fingerprint::Unknown { reason: UnknownReason::NotAGitWorktree }
    }

    fn reviewer_environment() -> EnvRecord {
        EnvRecord {
            env_id: "oat-abc-reviewer-1".to_owned(),
            run_id: Some("run-1".to_owned()),
            role: "reviewer".to_owned(),
            profile: "local".to_owned(),
            kind: config::ProviderKind::Kubernetes,
            worktree: PathBuf::from("/w"),
            container_path: PathBuf::from("/w"),
            image_ref: "oat-x:abc".to_owned(),
            image_id: "sha256:cafe".to_owned(),
            pod: "oat-abc-reviewer-1".to_owned(),
            namespace: "agents".to_owned(),
            context: "local".to_owned(),
            created_ms: 2,
        }
    }

    fn entry(start: Fingerprint, end: Option<Fingerprint>) -> LedgerEntry {
        LedgerEntry {
            exec_id: "7f3a91c0".to_owned(),
            argv: vec!["cargo".to_owned(), "test".to_owned()],
            cwd: PathBuf::from("/w"),
            worktree: PathBuf::from("/w"),
            env_id: "oat-abc-worker-1".to_owned(),
            role: "worker".to_owned(),
            image_id: "sha256:cafe".to_owned(),
            profile: "local".to_owned(),
            context: "local".to_owned(),
            namespace: "agents".to_owned(),
            started_ms: 1,
            ts: "2026-09-23T00:00:00Z".to_owned(),
            tree_start: start,
            tree_end: end,
            exit: Some(0),
            ms: Some(431_200),
            timeout_secs: None,
            attached: false,
        }
    }

    fn blockers(report: &Value) -> Vec<String> {
        report["entries"][0]["reuse_blocked_by"]
            .as_array()
            .unwrap()
            .iter()
            .map(|value| value.as_str().unwrap().to_owned())
            .collect()
    }

    fn against(entries: &[LedgerEntry], now: &Fingerprint) -> Value {
        classify(entries, now, &reviewer_environment())
    }

    #[test]
    fn a_command_run_against_this_exact_tree_has_nothing_standing_in_its_way() {
        let tree = known("aaaa");
        let report = against(&[entry(tree.clone(), Some(tree.clone()))], &tree);
        assert!(blockers(&report).is_empty());
        assert_eq!(report["entries"][0]["tree_match"], true);
        assert_eq!(report["entries"][0]["image_match"], true);
        assert_eq!(report["tree"]["digest"], "aaaa");
        assert!(report["note"].as_str().unwrap().contains("not a judgement"));
    }

    #[test]
    fn code_edited_after_the_command_ran_is_code_it_never_saw() {
        let then = known("aaaa");
        let report = against(&[entry(then.clone(), Some(then))], &known("bbbb"));
        assert_eq!(blockers(&report), ["tree_changed"]);
        assert_eq!(report["entries"][0]["tree_match"], false);
    }

    #[test]
    fn code_edited_while_the_command_ran_verified_neither_version() {
        let report = against(&[entry(known("aaaa"), Some(known("bbbb")))], &known("bbbb"));
        assert_eq!(blockers(&report), ["tree_moved_during_run"]);
    }

    #[test]
    fn a_command_still_outstanding_is_blocked_by_that_and_nothing_else() {
        let tree = known("aaaa");
        let mut unfinished = entry(tree.clone(), None);
        unfinished.exit = None;
        unfinished.ms = None;
        let report = against(&[unfinished], &tree);
        assert_eq!(blockers(&report), ["never_finished"]);

        let mut elsewhere = entry(known("bbbb"), None);
        elsewhere.exit = None;
        assert_eq!(blockers(&against(&[elsewhere], &tree)), ["tree_changed", "never_finished"]);
    }

    #[test]
    fn a_tree_nobody_could_fingerprint_never_qualifies() {
        let tree = known("aaaa");
        assert_eq!(blockers(&against(&[entry(unknown(), Some(unknown()))], &tree)), ["tree_unknown"]);
        assert_eq!(blockers(&against(&[entry(tree.clone(), Some(tree))], &unknown())), ["tree_unknown"]);
    }

    #[test]
    fn a_different_image_blocks_reuse_even_on_the_same_tree() {
        let tree = known("aaaa");
        let mut elsewhere = entry(tree.clone(), Some(tree.clone()));
        elsewhere.image_id = "sha256:beef".to_owned();
        let report = against(&[elsewhere], &tree);
        assert_eq!(blockers(&report), ["image_changed"]);
        assert_eq!(report["entries"][0]["tree_match"], true);
    }

    #[test]
    fn the_same_tree_on_another_cluster_is_another_measurement() {
        let tree = known("aaaa");
        let mut moved = entry(tree.clone(), Some(tree.clone()));
        moved.context = "remote".to_owned();
        assert_eq!(blockers(&against(&[moved], &tree)), ["placement_changed"]);
    }

    #[test]
    fn a_failed_command_is_reported_rather_than_hidden() {
        let tree = known("aaaa");
        let mut failed = entry(tree.clone(), Some(tree.clone()));
        failed.exit = Some(101);
        let report = against(&[failed], &tree);
        assert_eq!(blockers(&report), ["nonzero_exit"]);
        assert_eq!(report["entries"].as_array().unwrap().len(), 1);
        assert_eq!(report["entries"][0]["exit"], 101);
    }

    #[test]
    fn following_a_dropped_stream_is_not_by_itself_a_disqualification() {
        let tree = known("aaaa");
        let mut rejoined = entry(tree.clone(), Some(tree.clone()));
        rejoined.attached = true;
        assert!(blockers(&against(&[rejoined], &tree)).is_empty());
    }
}
