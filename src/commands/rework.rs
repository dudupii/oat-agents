// SPDX-License-Identifier: MIT
// Copyright (c) 2026 oat-agents contributors

use crate::environment::Environment;
use crate::error::{codes, err};
use crate::findings::{self, FindingsStore, WorkFindings};
use crate::store::Store;
use anyhow::Result;
use clap::{Args, Subcommand};
use serde_json::{json, Value};
use std::path::PathBuf;

#[derive(Subcommand, Debug)]
pub enum ReworkCommand {
    /// The worker's next brief for a piece of work whose review failed its gate: the original
    /// task, the findings to close, and the branch to continue.
    Brief(WorkArgs),
    /// The reviewer's next brief: the original brief, the findings, and only the fix diff —
    /// so a re-review round's cost stays bounded.
    Delta(WorkArgs),
    /// A work's review rounds and where its gate stands.
    Show(WorkArgs),
}

#[derive(Args, Debug)]
pub struct WorkArgs {
    #[arg(long)]
    pub run: Option<String>,
    /// The work's name: what its Dispatches were fired under (`kiln`, `kiln-fix1`, …).
    #[arg(long)]
    pub name: String,
}

pub fn run(command: ReworkCommand, env: &dyn Environment) -> Result<Value> {
    let run_id = command_run(&command).clone().or_else(|| env.var("OAT_RUN_ID")).ok_or_else(|| {
        err(codes::RUN_NOT_BOUND, "no run bound; pass --run or set OAT_RUN_ID")
    })?;
    match command {
        ReworkCommand::Brief(args) => brief(args, env, &run_id),
        ReworkCommand::Delta(args) => delta(args, env, &run_id),
        ReworkCommand::Show(args) => show(args, env, &run_id),
    }
}

fn command_run(command: &ReworkCommand) -> &Option<String> {
    match command {
        ReworkCommand::Brief(args) | ReworkCommand::Delta(args) | ReworkCommand::Show(args) => &args.run,
    }
}

/// The work's findings record, with the guards every next round needs: a round has settled, it
/// failed its gate, and the round cap has not been reached.
fn open_work(store: &Store, run_id: &str, name: &str) -> Result<WorkFindings> {
    let run_record = store.load_run(run_id)?;
    let Some(record) = FindingsStore::for_run(store, run_id).load(name)? else {
        return Err(err(
            codes::INVALID_INPUT,
            format!("no review has settled for work '{name}'"),
        ));
    };
    let rounds = record.rounds.len();
    let verdict = record
        .verdict()
        .expect("a findings record with a round has a verdict");
    if verdict == findings::Verdict::Pass {
        return Err(err(
            codes::INVALID_INPUT,
            format!(
                "review round {rounds} of '{}' passed its gate; there is nothing to rework",
                record.name
            ),
        ));
    }
    if rounds as u32 >= run_record.rework.max_rounds {
        return Err(err(
            codes::INVALID_INPUT,
            format!(
                "work '{}' reached the round cap ({}); it was escalated to the human",
                record.name, run_record.rework.max_rounds
            ),
        ));
    }
    Ok(record)
}

/// The Dispatches of one work, oldest first: every Dispatch whose name is the work's name or a
/// `-`-separated suffix of it, the same rule a checklist link claims them by.
fn work_dispatches(store: &Store, run_id: &str, work: &str) -> Vec<crate::store::DispatchRecord> {
    store
        .list_dispatches(run_id)
        .into_iter()
        .filter(|dispatch| {
            dispatch.name.as_deref().is_some_and(|name| crate::checklist::link_matches(work, name))
        })
        .collect()
}

/// The task the work was first fired with, verbatim.
fn original_brief(store: &Store, run_id: &str, dispatches: &[crate::store::DispatchRecord]) -> Result<String> {
    let first = dispatches.first().ok_or_else(|| {
        err(codes::INVALID_INPUT, "no Dispatch of this work was found to quote a task from")
    })?;
    let path = store.dispatch_dir_path(run_id, &first.id).join("task.md");
    std::fs::read_to_string(&path).map_err(|_| {
        err(
            codes::INVALID_INPUT,
            format!(
                "the first Dispatch of this work ({}) recorded no task; it was launched before \
                 tasks were recorded",
                first.id
            ),
        )
    })
}

/// The name the next round of this work fires under.
fn next_name(work: &str, rounds: usize) -> String {
    format!("{work}-fix{rounds}")
}

fn brief(args: WorkArgs, env: &dyn Environment, run_id: &str) -> Result<Value> {
    let store = Store::open(env)?;
    let record = open_work(&store, run_id, &args.name)?;
    let dispatches = work_dispatches(&store, run_id, &record.name);
    let original = original_brief(&store, run_id, &dispatches)?;
    let newest = dispatches.last().ok_or_else(|| {
        err(codes::INVALID_INPUT, "no Dispatch of this work was found to continue from")
    })?;
    let rounds = record.rounds.len();
    let next = next_name(&record.name, rounds);
    let findings_text = findings::render_findings(&record.merged());
    let brief_text = format!(
        "Round {next_round} of the rework loop for \"{work}\".\n\n\
         ## Original brief\n\n{original}\n\n\
         ## Findings to close\n\n{findings_text}\n\n\
         ## Your job\n\n\
         Fix exactly the open findings above — do not widen the work; a waived finding needs no \
         fix. Continue the attempt on branch `{branch}` (worktree {worktree}): merge it into your \
         worktree first, then commit your fixes there. Verify what you changed before you settle.",
        next_round = rounds + 1,
        work = record.name,
        branch = newest.branch,
        worktree = newest.worktree,
    );
    Ok(json!({
        "run_id": run_id,
        "work": record.name,
        "round": rounds + 1,
        "next_name": next,
        "branch": newest.branch,
        "worktree": newest.worktree,
        "brief": brief_text,
    }))
}

fn delta(args: WorkArgs, env: &dyn Environment, run_id: &str) -> Result<Value> {
    let store = Store::open(env)?;
    let record = open_work(&store, run_id, &args.name)?;
    let dispatches = work_dispatches(&store, run_id, &record.name);
    let original = original_brief(&store, run_id, &dispatches)?;
    let last_round = record.rounds.last().expect("open_work guarantees a round");
    let base = last_round.reviewed_commit.clone().ok_or_else(|| {
        err(
            codes::INVALID_INPUT,
            format!(
                "review round {} recorded no commit to diff the fix from; its worktree was gone \
                 when it settled",
                last_round.round
            ),
        )
    })?;
    let newest = dispatches.last().ok_or_else(|| {
        err(codes::INVALID_INPUT, "no Dispatch of this work was found to read the fix from")
    })?;
    let worktree = PathBuf::from(&newest.worktree);
    let head = crate::worktree::current_commit(&worktree)?;
    let diff = crate::worktree::run_git(&worktree, &["--no-pager", "diff", &format!("{base}..{head}")])?;
    if !diff.status.success() {
        return Err(err(
            codes::INTERNAL_ERROR,
            format!(
                "git diff failed: {}",
                String::from_utf8_lossy(&diff.stderr).trim()
            ),
        ));
    }
    let diff_text = String::from_utf8_lossy(&diff.stdout).into_owned();
    let rounds = record.rounds.len();
    let findings_text = findings::render_findings(&record.merged());
    let next = next_name(&record.name, rounds);
    let brief_text = format!(
        "Round {next_round} of the rework loop for \"{work}\": re-review only the fix.\n\n\
         ## Original brief\n\n{original}\n\n\
         ## Findings after round {rounds}\n\n{findings_text}\n\n\
         ## The fix diff since round {rounds} reviewed\n\n```diff\n{diff_text}\n```\n\n\
         ## Your job\n\n\
         For each finding above, judge the fix: keep the id and set `status` to `fixed`, or leave \
         it `open` with what still fails. File a new finding (a fresh id) only for a defect the \
         fix introduces; a P3 you accept as unfixable may be `waived` with a note. Settle with \
         `dispatch done --findings-file`.",
        next_round = rounds + 1,
        work = record.name,
    );
    Ok(json!({
        "run_id": run_id,
        "work": record.name,
        "round": rounds + 1,
        "next_name": next,
        "base": base,
        "head": head,
        "brief": brief_text,
    }))
}

fn show(args: WorkArgs, env: &dyn Environment, run_id: &str) -> Result<Value> {
    let store = Store::open(env)?;
    let run_record = store.load_run(run_id)?;
    let Some(record) = FindingsStore::for_run(&store, run_id).load(&args.name)? else {
        return Err(err(
            codes::INVALID_INPUT,
            format!("no review has settled for work '{}'", args.name),
        ));
    };
    Ok(json!({
        "run_id": run_id,
        "work": record.name,
        "rounds": record.rounds,
        "findings": record.merged(),
        "gate": {
            "verdict": record.verdict(),
            "max_rounds": run_record.rework.max_rounds,
            "p2_required": run_record.rework.p2_required,
        },
    }))
}
