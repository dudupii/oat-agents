// SPDX-License-Identifier: MIT
// Copyright (c) 2026 oat-agents contributors

use crate::env::integration::ExecEnvironments;
use crate::environment::Environment;
use crate::error::{codes, err};
use crate::event_log::{events, now_iso, EventLog, LogEntry};
use crate::session::tmux::Tmux;
use crate::store::{MessageKind, Settlement, Store};
use crate::worktree;
use anyhow::Result;
use clap::{Args, Subcommand};
use serde_json::{json, Value};
use std::time::Duration;

#[derive(Subcommand, Debug)]
pub enum DispatchCommand {
    /// Settle this Dispatch once, with an outcome and a report; the meta-agent receives it as
    /// `member_done`.
    Done(DoneArgs),
    /// Ask the meta-agent through the Run inbox and wait for its reply.
    Ask(AskArgs),
    /// A Dispatch's record and whether its session is still alive.
    Show(ShowArgs),
    /// A Dispatch's transcript or terminal screen.
    Read(ReadArgs),
    /// End a Dispatch's session; with `--remove-worktree`, also remove its worktree.
    Release(ReleaseArgs),
}

#[derive(Args, Debug)]
pub struct DoneArgs {
    #[arg(long)]
    pub dispatch: Option<String>,
    #[arg(long)]
    pub run: Option<String>,
    #[arg(long, default_value = "succeeded")]
    pub outcome: String,
    #[arg(long)]
    pub report: Option<String>,
    #[arg(long)]
    pub report_file: Option<std::path::PathBuf>,
    /// Structured findings as a JSON array (`-` reads stdin). The round's PASS/FAIL verdict is
    /// derived from them and the gate, and recorded beside this Dispatch's settlement.
    #[arg(long)]
    pub findings_file: Option<std::path::PathBuf>,
}

#[derive(Args, Debug)]
pub struct AskArgs {
    #[arg(long)]
    pub dispatch: Option<String>,
    #[arg(long)]
    pub run: Option<String>,
    #[arg(long)]
    pub body: Option<String>,
    #[arg(long)]
    pub input_file: Option<std::path::PathBuf>,
    #[arg(long, default_value_t = 900_000)]
    pub timeout_ms: u64,
}

#[derive(Args, Debug)]
pub struct ShowArgs {
    #[arg(long)]
    pub dispatch: Option<String>,
    #[arg(long)]
    pub run: Option<String>,
}

#[derive(Args, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ReadSource;

#[derive(Args, Debug)]
pub struct ReadArgs {
    #[arg(long)]
    pub dispatch: Option<String>,
    #[arg(long)]
    pub run: Option<String>,
    #[arg(long, default_value = "auto")]
    pub source: String,
    #[arg(long, default_value_t = 200)]
    pub limit: usize,
}

#[derive(Args, Debug)]
pub struct ReleaseArgs {
    #[arg(long)]
    pub dispatch: Option<String>,
    #[arg(long)]
    pub run: Option<String>,
    #[arg(long)]
    pub remove_worktree: bool,
    #[arg(long)]
    pub force: bool,
}

fn bound_run(run: &Option<String>, env: &dyn Environment) -> Result<String> {
    run.clone()
        .or_else(|| env.var("OAT_RUN_ID"))
        .ok_or_else(|| err(codes::RUN_NOT_BOUND, "no run bound; pass --run or set OAT_RUN_ID"))
}

fn bound_dispatch(dispatch: &Option<String>, env: &dyn Environment) -> Result<String> {
    dispatch
        .clone()
        .or_else(|| env.var("OAT_DISPATCH_ID"))
        .ok_or_else(|| {
            err(
                codes::DISPATCH_NOT_BOUND,
                "no dispatch bound; pass --dispatch or set OAT_DISPATCH_ID",
            )
        })
}

/// `--report-file` and `--findings-file` read the same way: `-` is stdin, anything else a
/// path, and a file that cannot be read is refused naming it.
fn read_text_arg(path: &std::path::Path) -> Result<String> {
    if path.as_os_str() == "-" {
        use std::io::Read;
        let mut buf = String::new();
        std::io::stdin()
            .read_to_string(&mut buf)
            .map_err(|e| err(codes::INVALID_INPUT, format!("could not read stdin: {e}")))?;
        Ok(buf)
    } else {
        std::fs::read_to_string(path).map_err(|e| {
            err(codes::INVALID_INPUT, format!("could not read {}: {e}", path.display()))
        })
    }
}

pub fn run(command: DispatchCommand, env: &dyn Environment, exec: &dyn ExecEnvironments) -> Result<Value> {
    match command {
        DispatchCommand::Done(args) => done(args, env),
        DispatchCommand::Ask(args) => ask(args, env),
        DispatchCommand::Show(args) => show(args, env),
        DispatchCommand::Read(args) => read(args, env),
        DispatchCommand::Release(args) => release(args, env, exec),
    }
}

fn done(args: DoneArgs, env: &dyn Environment) -> Result<Value> {
    let run_id = bound_run(&args.run, env)?;
    let dispatch_id = bound_dispatch(&args.dispatch, env)?;
    let store = Store::open(env)?;
    let log = EventLog::open(env);
    let mut dispatch = store.load_dispatch(&run_id, &dispatch_id)?;
    if dispatch.is_settled() {
        return Err(err(
            codes::ALREADY_SETTLED,
            format!("dispatch '{dispatch_id}' is already settled"),
        ));
    }

    let report = match (&args.report, &args.report_file) {
        (Some(_), Some(_)) => {
            return Err(err(
                codes::INVALID_CLI_ARGUMENTS,
                "pass at most one of --report or --report-file",
            ))
        }
        (Some(text), None) => text.clone(),
        (None, Some(path)) => read_text_arg(path)?,
        (None, None) => String::new(),
    };

    let settlement = match args.outcome.as_str() {
        "succeeded" => Settlement::Succeeded,
        "failed" => Settlement::Failed,
        other => {
            return Err(err(
                codes::INVALID_CLI_ARGUMENTS,
                format!("--outcome must be 'succeeded' or 'failed', got '{other}'"),
            ))
        }
    };

    // Findings are parsed and the round recorded before anything settles, so a round that
    // cannot be read leaves the Dispatch exactly as it was.
    let run_record = store.load_run(&run_id)?;
    let review = match &args.findings_file {
        Some(path) => {
            let text = read_text_arg(path)?;
            let filed = crate::findings::parse_round(&text)?;
            let name = dispatch.name.clone().ok_or_else(|| {
                err(
                    codes::FINDINGS_INVALID,
                    "findings attach to a named Dispatch; this one was launched with no --name",
                )
            })?;
            let reviewed_commit = if dispatch.worktree.is_empty() {
                None
            } else {
                worktree::current_commit(std::path::Path::new(&dispatch.worktree)).ok()
            };
            Some(
                crate::findings::FindingsStore::for_run(&store, &run_id).append_round(
                    &name,
                    &dispatch_id,
                    filed,
                    reviewed_commit,
                    &run_record.rework,
                )?,
            )
        }
        None => None,
    };

    // The settlement's report is the message the meta-agent receives: the reviewer's prose,
    // then the round as the gate derived it — and, when the round cap is reached, the stop
    // order, so the loop ends in the one channel the meta-agent reads.
    let mut message = report.clone();
    if let Some(appended) = &review {
        let block = crate::findings::render_round(&appended.round, &appended.merged);
        if message.trim().is_empty() {
            message = block;
        } else {
            message.push_str("\n\n");
            message.push_str(&block);
        }
        if appended.escalated {
            message.push_str(&format!(
                "\n\nRound cap reached: work '{}' has had {} failing review rounds (the cap is \
                 {}); it needs a human. Do not fire another round for it — surface it to the \
                 operator.",
                appended.work,
                appended.round.round,
                run_record.rework.max_rounds
            ));
        }
    }

    dispatch.settled = Some(settlement);
    dispatch.report = Some(message.clone());
    store.save_dispatch(&dispatch)?;

    store.append_inbox(&run_id, &dispatch_id, MessageKind::MemberDone, &message)?;
    log.record(&LogEntry {
        timestamp: now_iso(),
        run_id: run_id.clone(),
        dispatch_id: Some(dispatch_id.clone()),
        agent: Some(dispatch.role.clone()),
        event: events::AGENT_EXIT.to_string(),
        details: Some(json!({"outcome": args.outcome})),
    })?;

    let mut result = json!({"run_id": run_id, "dispatch_id": dispatch_id, "settled": args.outcome});
    if let Some(appended) = &review {
        let open = |severity: crate::findings::Severity| {
            appended
                .merged
                .iter()
                .filter(|finding| finding.severity == severity && finding.status == crate::findings::FindingStatus::Open)
                .count()
        };
        log.record(&LogEntry {
            timestamp: now_iso(),
            run_id: run_id.clone(),
            dispatch_id: Some(dispatch_id.clone()),
            agent: Some(dispatch.role.clone()),
            event: events::REVIEW_RESULT.to_string(),
            details: Some(json!({
                "work": appended.work,
                "round": appended.round.round,
                "verdict": appended.round.verdict,
                "open": {"P1": open(crate::findings::Severity::P1), "P2": open(crate::findings::Severity::P2), "P3": open(crate::findings::Severity::P3)},
                "reviewed_commit": appended.round.reviewed_commit,
            })),
        })?;
        if appended.escalated {
            log.record(&LogEntry {
                timestamp: now_iso(),
                run_id: run_id.clone(),
                dispatch_id: Some(dispatch_id.clone()),
                agent: Some(dispatch.role.clone()),
                event: events::NEEDS_HUMAN.to_string(),
                details: Some(json!({
                    "reason": "round_cap",
                    "work": appended.work,
                    "rounds": appended.round.round,
                    "max_rounds": run_record.rework.max_rounds,
                    "message": format!(
                        "work '{}' reached the round cap ({}) with its gate still failing",
                        appended.work, run_record.rework.max_rounds
                    ),
                })),
            })?;
        }
        result["review"] = json!({
            "work": appended.work,
            "round": appended.round.round,
            "verdict": appended.round.verdict,
            "escalated": appended.escalated,
        });
    }
    Ok(result)
}

fn ask(args: AskArgs, env: &dyn Environment) -> Result<Value> {
    let run_id = bound_run(&args.run, env)?;
    let dispatch_id = bound_dispatch(&args.dispatch, env)?;
    let text = crate::resolve_text_input(&args.body, &args.input_file)?;
    let store = Store::open(env)?;
    let log = EventLog::open(env);

    let seq = store.append_inbox(&run_id, &dispatch_id, MessageKind::Question, &text)?;
    log.record(&LogEntry {
        timestamp: now_iso(),
        run_id: run_id.clone(),
        dispatch_id: Some(dispatch_id.clone()),
        agent: None,
        event: events::INBOX_MESSAGE.to_string(),
        details: Some(json!({"seq": seq, "kind": "question"})),
    })?;

    let timeout = Duration::from_millis(args.timeout_ms);
    let poll = Duration::from_millis(500);
    let reply = store.wait_reply(&run_id, seq, timeout, poll)?;

    Ok(json!({
        "run_id": run_id,
        "dispatch_id": dispatch_id,
        "message": seq,
        "timed_out": reply.is_none(),
        "reply": reply.map(|r| r.message),
    }))
}

fn show(args: ShowArgs, env: &dyn Environment) -> Result<Value> {
    let run_id = bound_run(&args.run, env)?;
    let dispatch_id = bound_dispatch(&args.dispatch, env)?;
    let store = Store::open(env)?;
    let dispatch = store.load_dispatch(&run_id, &dispatch_id)?;
    let run_record = store.load_run(&run_id)?;

    let alive = if dispatch.released_at.is_some() {
        false
    } else {
        let tmux = Tmux::from_env(env);
        let hid = dispatch.hash_id(&run_record);
        let session = Tmux::session_name(&hid);
        tmux.session_alive(&session).unwrap_or(false)
    };

    Ok(json!({
        "dispatch": dispatch,
        "alive": alive,
    }))
}

fn read(args: ReadArgs, env: &dyn Environment) -> Result<Value> {
    let run_id = bound_run(&args.run, env)?;
    let dispatch_id = bound_dispatch(&args.dispatch, env)?;
    let store = Store::open(env)?;
    let run_record = store.load_run(&run_id)?;
    let dispatch = store.load_dispatch(&run_id, &dispatch_id)?;

    let hid = dispatch.hash_id(&run_record);
    let session = Tmux::session_name(&hid);
    let tmux = Tmux::from_env(env);

    let source = args.source.as_str();
    let use_transcript = source == "transcript" || source == "auto";
    if use_transcript {
        if let Ok(Some(text)) = crate::transcript::read_transcript(env, &dispatch, args.limit) {
            return Ok(json!({"source": "transcript", "text": text}));
        }
        if source == "transcript" {
            return Err(err(codes::TRANSCRIPT_NOT_FOUND, "no transcript found for this dispatch"));
        }
    }

    if dispatch.released_at.is_none() && tmux.session_alive(&session).unwrap_or(false) {
        let text = tmux.capture_pane(&session)?;
        return Ok(json!({"source": "terminal", "text": text}));
    }

    let saved_path = store
        .dispatch_dir_path(&run_id, &dispatch_id)
        .join("screen.txt");
    if saved_path.exists() {
        let text = std::fs::read_to_string(saved_path)?;
        return Ok(json!({"source": "terminal", "text": text}));
    }

    Err(err(codes::TRANSCRIPT_NOT_FOUND, "no transcript or terminal screen available"))
}

fn release(args: ReleaseArgs, env: &dyn Environment, exec: &dyn ExecEnvironments) -> Result<Value> {
    let run_id = bound_run(&args.run, env)?;
    let dispatch_id = bound_dispatch(&args.dispatch, env)?;
    let store = Store::open(env)?;
    let log = EventLog::open(env);
    let run_record = store.load_run(&run_id)?;
    let mut dispatch = store.load_dispatch(&run_id, &dispatch_id)?;

    let tmux = Tmux::from_env(env);
    let hid = dispatch.hash_id(&run_record);
    let session = Tmux::session_name(&hid);

    if tmux.session_alive(&session).unwrap_or(false) {
        if let Ok(text) = tmux.capture_pane(&session) {
            let _ = std::fs::write(
                store.dispatch_dir_path(&run_id, &dispatch_id).join("screen.txt"),
                text,
            );
        }
        let _ = tmux.kill_session(&session);
    }

    let mut removal = None;
    if args.remove_worktree {
        let repo = std::path::PathBuf::from(&run_record.repo);
        let path = std::path::PathBuf::from(&dispatch.worktree);
        let outcome = worktree::remove_worktree(
            &repo,
            &path,
            &dispatch.branch,
            &run_record.base_branch,
            args.force,
        )?;
        match outcome {
            worktree::RemovalOutcome::Removed => {
                log.record(&LogEntry {
                    timestamp: now_iso(),
                    run_id: run_id.clone(),
                    dispatch_id: Some(dispatch_id.clone()),
                    agent: None,
                    event: events::WORKTREE_REMOVED.to_string(),
                    details: None,
                })?;
                removal = Some(json!({"removed": true}));
            }
            worktree::RemovalOutcome::Kept { reason } => {
                removal = Some(json!({"removed": false, "kept_reason": reason}));
            }
        }
    }

    dispatch.released_at = Some(now_iso());
    store.save_dispatch(&dispatch)?;

    // The release is done whatever the queue does; a failure to start what was waiting is
    // reported beside it, not in place of it.
    let queue = super::role::drain_queue(env, exec, &run_id)
        .unwrap_or_else(|error| json!({"error": format!("{error:#}")}));

    Ok(json!({
        "run_id": run_id,
        "dispatch_id": dispatch_id,
        "released": true,
        "worktree": removal,
        "queue": queue,
    }))
}
