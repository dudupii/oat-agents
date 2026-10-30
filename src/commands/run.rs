// SPDX-License-Identifier: MIT
// Copyright (c) 2026 oat-agents contributors

use crate::env::integration::ExecEnvironments;
use crate::environment::Environment;
use crate::error::{codes, err};
use crate::event_log::{events, now_iso, EventLog, LogEntry};
use crate::liveness;
use crate::store::Store;
use crate::worktree;
use anyhow::Result;
use clap::{Args, Subcommand};
use serde_json::{json, Value};
use std::time::Duration;

#[derive(Subcommand, Debug)]
pub enum RunCommand {
    /// Block for the next delivery from the Run inbox; on timeout, report each active
    /// Dispatch's liveness. Only the meta-agent consumes the inbox.
    Wait(WaitArgs),
    /// Acknowledge a delivery so it is not handed over again.
    Ack(AckArgs),
    /// Answer one inbox message, unblocking the member agent's `dispatch ask`.
    Reply(ReplyArgs),
    /// Remove a finished Run's worktrees, and the branches its base already contains.
    Clean(CleanArgs),
}

#[derive(Args, Debug)]
pub struct WaitArgs {
    #[arg(long)]
    pub run: Option<String>,
    #[arg(long)]
    pub ack: bool,
    #[arg(long, default_value_t = 900_000)]
    pub timeout_ms: u64,
}

#[derive(Args, Debug)]
pub struct AckArgs {
    #[arg(long)]
    pub run: Option<String>,
    #[arg(long)]
    pub delivery: String,
}

#[derive(Args, Debug)]
pub struct ReplyArgs {
    #[arg(long)]
    pub run: Option<String>,
    #[arg(long)]
    pub message: u64,
    #[arg(long)]
    pub prompt: Option<String>,
    #[arg(long)]
    pub input_file: Option<std::path::PathBuf>,
}

#[derive(Args, Debug)]
pub struct CleanArgs {
    #[arg(long)]
    pub run: Option<String>,
    #[arg(long)]
    pub all: bool,
    #[arg(long)]
    pub force: bool,
}

fn bound_run(run: &Option<String>, env: &dyn Environment) -> Result<String> {
    run.clone()
        .or_else(|| env.var("OAT_RUN_ID"))
        .ok_or_else(|| err(codes::RUN_NOT_BOUND, "no run bound; pass --run or set OAT_RUN_ID"))
}

pub fn run(command: RunCommand, env: &dyn Environment, exec: &dyn ExecEnvironments) -> Result<Value> {
    match command {
        RunCommand::Wait(args) => wait(args, env, exec),
        RunCommand::Ack(args) => ack(args, env),
        RunCommand::Reply(args) => reply(args, env),
        RunCommand::Clean(args) => clean(args, env),
    }
}

fn wait(args: WaitArgs, env: &dyn Environment, exec: &dyn ExecEnvironments) -> Result<Value> {
    let run_id = bound_run(&args.run, env)?;
    let store = Store::open(env)?;
    let log = EventLog::open(env);
    let timeout = Duration::from_millis(args.timeout_ms);
    let poll = Duration::from_millis(500);

    // A place may have come free while the meta-agent was not waiting; a queued launch that
    // fails lands in the inbox and so in this very wait.
    let mut started = queue_started(env, exec, &run_id);
    let delivery = store.wait_inbox(&run_id, timeout, poll)?;
    // A delivery is usually a settlement, which is what frees a place.
    started.extend(queue_started(env, exec, &run_id));

    match delivery {
        Some(delivery) => {
            if args.ack {
                store.ack_inbox(&run_id, &delivery.delivery_id)?;
                log.record(&LogEntry {
                    timestamp: now_iso(),
                    run_id: run_id.clone(),
                    dispatch_id: None,
                    agent: None,
                    event: events::INBOX_ACK.to_string(),
                    details: Some(json!({"delivery_id": delivery.delivery_id})),
                })?;
            }
            for msg in &delivery.messages {
                log.record(&LogEntry {
                    timestamp: now_iso(),
                    run_id: run_id.clone(),
                    dispatch_id: Some(msg.dispatch_id.clone()),
                    agent: None,
                    event: events::INBOX_MESSAGE.to_string(),
                    details: Some(json!({"seq": msg.seq, "kind": msg.kind})),
                })?;
            }
            Ok(json!({
                "run_id": run_id,
                "delivery_id": delivery.delivery_id,
                "messages": delivery.messages,
                "acked": args.ack,
                "timed_out": false,
                "started_from_queue": started,
            }))
        }
        None => {
            log.record(&LogEntry {
                timestamp: now_iso(),
                run_id: run_id.clone(),
                dispatch_id: None,
                agent: None,
                event: events::INBOX_TIMEOUT.to_string(),
                details: None,
            })?;
            let liveness_report = liveness::report_for_run(env, &store, &log, &run_id)?;
            let queued: Vec<Value> = crate::concurrency::Queue::for_run(&store, &run_id)
                .waiting()?
                .into_iter()
                .map(|entry| json!({"dispatch_id": entry.dispatch_id, "role": entry.role, "name": entry.name}))
                .collect();
            Ok(json!({
                "run_id": run_id,
                "timed_out": true,
                "liveness": liveness_report,
                "queued": queued,
                "started_from_queue": started,
            }))
        }
    }
}

/// The Dispatches this pass over the queue started. Waiting must not fail because a launch
/// did: a failed one is already in the inbox.
fn queue_started(env: &dyn Environment, exec: &dyn ExecEnvironments, run_id: &str) -> Vec<Value> {
    super::role::drain_queue(env, exec, run_id)
        .ok()
        .and_then(|report| report["started"].as_array().cloned())
        .unwrap_or_default()
}

fn ack(args: AckArgs, env: &dyn Environment) -> Result<Value> {
    let run_id = bound_run(&args.run, env)?;
    let store = Store::open(env)?;
    let log = EventLog::open(env);
    let acked = store.ack_inbox(&run_id, &args.delivery)?;
    log.record(&LogEntry {
        timestamp: now_iso(),
        run_id: run_id.clone(),
        dispatch_id: None,
        agent: None,
        event: events::INBOX_ACK.to_string(),
        details: Some(json!({"delivery_id": args.delivery, "count": acked})),
    })?;
    Ok(json!({"run_id": run_id, "delivery_id": args.delivery, "acked": acked}))
}

fn reply(args: ReplyArgs, env: &dyn Environment) -> Result<Value> {
    let run_id = bound_run(&args.run, env)?;
    let text = crate::resolve_text_input(&args.prompt, &args.input_file)?;
    let store = Store::open(env)?;
    let log = EventLog::open(env);
    store.reply_to_message(&run_id, args.message, &text)?;
    log.record(&LogEntry {
        timestamp: now_iso(),
        run_id: run_id.clone(),
        dispatch_id: None,
        agent: None,
        event: events::INBOX_REPLY.to_string(),
        details: Some(json!({"message": args.message})),
    })?;
    Ok(json!({"run_id": run_id, "message": args.message}))
}

/// Removes every Dispatch worktree of one Run (including the meta-agent's), and its branch
/// once the base already contains it; a dirty worktree is kept with its reason (ticket §4.3).
pub fn clean_run(env: &dyn Environment, store: &Store, run_id: &str, force: bool) -> Result<Value> {
    let log = EventLog::open(env);
    let run_record = store.load_run(run_id)?;
    let repo = std::path::PathBuf::from(&run_record.repo);
    let dispatches_dir = store.root().join(run_id).join("dispatches");
    let mut results = Vec::new();
    if dispatches_dir.exists() {
        for entry in std::fs::read_dir(&dispatches_dir)? {
            let entry = entry?;
            let dispatch_id = entry.file_name().to_string_lossy().to_string();
            let Ok(dispatch) = store.load_dispatch(run_id, &dispatch_id) else {
                continue;
            };
            let path = std::path::PathBuf::from(&dispatch.worktree);
            let outcome = worktree::remove_worktree(
                &repo,
                &path,
                &dispatch.branch,
                &run_record.base_branch,
                force,
            )?;
            match outcome {
                worktree::RemovalOutcome::Removed => {
                    log.record(&LogEntry {
                        timestamp: now_iso(),
                        run_id: run_id.to_string(),
                        dispatch_id: Some(dispatch_id.clone()),
                        agent: None,
                        event: events::WORKTREE_REMOVED.to_string(),
                        details: None,
                    })?;
                    results.push(json!({"dispatch_id": dispatch_id, "removed": true}));
                }
                worktree::RemovalOutcome::Kept { reason } => {
                    results.push(json!({"dispatch_id": dispatch_id, "removed": false, "kept_reason": reason}));
                }
            }
        }
    }
    Ok(json!({"run_id": run_id, "dispatches": results}))
}

fn clean(args: CleanArgs, env: &dyn Environment) -> Result<Value> {
    let store = Store::open(env)?;
    let run_ids: Vec<String> = if args.all {
        let mut ids = Vec::new();
        for id in store.list_run_ids()? {
            let record = store.load_run(&id)?;
            if !record.is_open() {
                ids.push(id);
            }
        }
        ids
    } else {
        vec![bound_run(&args.run, env)?]
    };

    let mut results = Vec::new();
    for run_id in run_ids {
        let record = store.load_run(&run_id)?;
        if record.is_open() {
            return Err(err(
                codes::RUN_STILL_OPEN,
                format!("run '{run_id}' is still open; finish it before cleaning"),
            ));
        }
        results.push(clean_run(env, &store, &run_id, args.force)?);
    }
    Ok(json!({"runs": results}))
}
