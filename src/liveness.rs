// SPDX-License-Identifier: MIT
// Copyright (c) 2026 oat-agents contributors

//! The liveness block a timed-out `run wait` carries (ticket §4.4): the only moment a
//! meta-agent has to decide whether to keep waiting, so that is where the evidence goes —
//! and, for a session wedged on an interactive dialog nobody in a Run will answer, where the
//! CLI escalates it (issue #9).

use crate::environment::Environment;
use crate::event_log::{events, now_iso, EventLog, LogEntry};
use crate::session::tmux::Tmux;
use crate::store::{DispatchRecord, MessageKind, RunRecord, Store};
use crate::transcript;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// How long a choice dialog must persist at the bottom of a Dispatch's pane before the CLI
/// escalates it (issue #9). Like `transcript::QUIET_AFTER_MS`, a constant: a safety net behind
/// the meta-agent's own `run wait` cadence, not a tuning knob.
pub const DIALOG_AFTER_MS: i64 = 300_000;
/// How many trailing non-empty pane lines the dialog heuristic and a `pane_tail` look at.
const DIALOG_TAIL_LINES: usize = 6;
/// The marker a detected dialog keeps in the Dispatch's state directory, so persistence is
/// measured across `run wait` passes instead of guessed from one capture.
const DIALOG_MARKER: &str = "dialog.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DialogMarker {
    pub first_seen_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub escalated_at_ms: Option<i64>,
}

/// Per still-active Dispatch: `working` / `waiting` / `stalled`, how long it has been silent,
/// the outstanding tool where there is one, and whether the silence has outlasted the kind of
/// work it is doing. A Codex-backed Dispatch has no transcript this crate can read
/// (`transcript`'s module doc), so it is reported with no pulse rather than a guessed one.
///
/// Every entry also carries the dialog watch: `dialog_ms`, how long the session's pane has
/// ended in a choice dialog (null when it does not), and `escalated`, whether that has already
/// been raised — a `needs_human` entry in the workflow log plus an `escalation` Run-inbox
/// message. The Dispatch itself is left active: deciding what happens to it is the
/// meta-agent's, not the watch's.
pub fn report_for_run(env: &dyn Environment, store: &Store, log: &EventLog, run_id: &str) -> Result<Value> {
    let dispatches_dir = store.root().join(run_id).join("dispatches");
    let run = store.load_run(run_id).ok();
    let mut agents = Vec::new();
    if dispatches_dir.exists() {
        for entry in std::fs::read_dir(&dispatches_dir)? {
            let entry = entry?;
            let dispatch_id = entry.file_name().to_string_lossy().to_string();
            let Ok(dispatch) = store.load_dispatch(run_id, &dispatch_id) else {
                continue;
            };
            if dispatch.released_at.is_some() || dispatch.is_settled() {
                continue;
            }
            let now = now_ms();
            let pulse = transcript::pulse_for(env, &dispatch);
            // A Working Dispatch is doing something; its screen is not evidence of a dialog
            // waiting for a keystroke, and its own numbered output would false-positive.
            let consult = pulse
                .as_ref()
                .is_none_or(|p| p.liveness(now, transcript::QUIET_AFTER_MS) != transcript::Liveness::Working);
            let (dialog_ms, escalated) = if consult {
                watch_dialog(env, store, log, run.as_ref(), run_id, &dispatch, now)
            } else {
                clear_marker(store, run_id, &dispatch.id);
                (None, false)
            };
            let entry = match pulse {
                Some(pulse) => json!({
                    "dispatch_id": dispatch_id,
                    "role": dispatch.role,
                    "liveness": pulse.liveness(now, transcript::QUIET_AFTER_MS).as_str(),
                    "silent_ms": pulse.silent_ms(now),
                    "outstanding": pulse.outstanding(),
                    "overrunning": pulse.overrunning(now, transcript::QUIET_AFTER_MS, transcript::OVERRUN_AFTER_MS),
                    "dialog_ms": dialog_ms,
                    "escalated": escalated,
                }),
                None => json!({
                    "dispatch_id": dispatch_id,
                    "role": dispatch.role,
                    "liveness": Value::Null,
                    "note": "no transcript available for this backend",
                    "dialog_ms": dialog_ms,
                    "escalated": escalated,
                }),
            };
            agents.push(entry);
        }
    }
    Ok(json!({"agents": agents}))
}

/// One watching pass for a single active Dispatch whose screen is worth consulting: capture
/// its pane, keep the marker honest, escalate past the threshold. Every write is best effort —
/// a wedged session must not make `run wait` fail on top of everything else.
fn watch_dialog(
    env: &dyn Environment,
    store: &Store,
    log: &EventLog,
    run: Option<&RunRecord>,
    run_id: &str,
    dispatch: &DispatchRecord,
    now: i64,
) -> (Option<i64>, bool) {
    let Some(run) = run else {
        clear_marker(store, run_id, &dispatch.id);
        return (None, false);
    };
    let tmux = Tmux::from_env(env);
    let session = Tmux::session_name(&dispatch.hash_id(run));
    let text = if tmux.session_alive(&session).unwrap_or(false) {
        tmux.capture_pane(&session).ok()
    } else {
        None
    };
    let Some(text) = text.filter(|t| dialog_in_pane(t)) else {
        // A clean pane, a dead session and a tmux that will not answer all read the same:
        // whatever was on the screen before is gone.
        clear_marker(store, run_id, &dispatch.id);
        return (None, false);
    };
    let marker_path = store.dispatch_dir_path(run_id, &dispatch.id).join(DIALOG_MARKER);
    let mut marker =
        read_marker(&marker_path).unwrap_or(DialogMarker { first_seen_ms: now, escalated_at_ms: None });
    let dialog_ms = now.saturating_sub(marker.first_seen_ms);
    if marker.escalated_at_ms.is_none()
        && dialog_ms > DIALOG_AFTER_MS
        && escalate(store, log, run_id, dispatch, dialog_ms, &pane_tail(&text)).is_ok()
    {
        marker.escalated_at_ms = Some(now);
    }
    write_marker(&marker_path, &marker).ok();
    (Some(dialog_ms), marker.escalated_at_ms.is_some())
}

/// Raise a dialog that has outstayed its welcome: an inbox message the meta-agent can act on,
/// and a `needs_human` entry that names the Dispatch to whoever is watching the log. The
/// round-cap escalation is the precedent for a CLI-initiated raise; like it, nothing is
/// settled or released — informing is this watch's whole job.
fn escalate(
    store: &Store,
    log: &EventLog,
    run_id: &str,
    dispatch: &DispatchRecord,
    dialog_ms: i64,
    pane_tail: &str,
) -> Result<()> {
    let minutes = dialog_ms / 60_000;
    let message = format!(
        "The session of Dispatch {id} (role '{role}', backend {backend}) has been sitting on an \
         interactive choice dialog for about {minutes} minutes. Nobody in a Run answers it, and \
         the member agent in that session cannot progress past it on its own. Nothing waits for \
         a reply to this message and the Dispatch is still active: read its screen with \
         `dispatch read --source terminal --dispatch {id} --run {run_id}`, then either release \
         the Dispatch and fire the work again, or raise it with the operator.",
        id = dispatch.id,
        role = dispatch.role,
        backend = dispatch.backend,
        minutes = minutes,
        run_id = run_id,
    );
    store.append_inbox(run_id, &dispatch.id, MessageKind::Escalation, &message)?;
    log.record(&LogEntry {
        timestamp: now_iso(),
        run_id: run_id.to_string(),
        dispatch_id: Some(dispatch.id.clone()),
        agent: Some(dispatch.role.clone()),
        event: events::NEEDS_HUMAN.to_string(),
        details: Some(json!({
            "reason": "dialog_stall",
            "role": dispatch.role,
            "backend": dispatch.backend,
            "minutes": minutes,
            "dialog_ms": dialog_ms,
            "pane_tail": pane_tail,
        })),
    })?;
    Ok(())
}

fn read_marker(path: &Path) -> Option<DialogMarker> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
}

fn write_marker(path: &Path, marker: &DialogMarker) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, serde_json::to_string(marker)?)?;
    Ok(())
}

fn clear_marker(store: &Store, run_id: &str, dispatch_id: &str) {
    let _ = std::fs::remove_file(store.dispatch_dir_path(run_id, dispatch_id).join(DIALOG_MARKER));
}

/// Trims whitespace and box-drawing frame characters from both ends of a pane line, so a
/// bordered modal reads the same as a bare one.
fn frame_trim(line: &str) -> &str {
    line.trim_matches(|c: char| {
        c.is_whitespace() || matches!(c, '│' | '┃' | '|' | '╭' | '╮' | '╰' | '╯' | '┌' | '┐' | '└' | '┘' | '…')
    })
}

/// The option numbers a pane line carries: a digit `1..=9` at a boundary — the line's start,
/// or after whitespace or a cursor marker (`› > ‣ ● …`) — followed by `.` or `)`, whitespace,
/// and option text.
fn option_numbers(line: &str) -> Vec<u32> {
    let chars: Vec<char> = line.chars().collect();
    let mut numbers = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let at_boundary = i == 0
            || chars[i - 1].is_whitespace()
            || matches!(chars[i - 1], '›' | '>' | '‣' | '●' | '…');
        let c = chars[i];
        if at_boundary
            && c.is_ascii_digit()
            && c >= '1'
            && i + 3 < chars.len()
            && matches!(chars[i + 1], '.' | ')')
            && chars[i + 2].is_whitespace()
            && !chars[i + 3].is_whitespace()
        {
            numbers.push(c.to_digit(10).expect("checked ascii digit"));
            i += 4;
        } else {
            i += 1;
        }
    }
    numbers
}

/// Whether the visible screen of a session ends in a numbered-choice dialog — the shape an
/// interactive backend raises when it wants a keystroke nobody in a Run will type (issue #9's
/// codex app-server feature-settings modal is the known case). At least one option sits on
/// the last non-empty line and two or more across the trailing window, so an agent's own
/// numbered output counts only while it is the bottom of the screen, and a backend that draws
/// a composer line below its modal is not matched. Best-effort evidence behind an
/// inform-only signal, not a guarantee.
pub fn dialog_in_pane(pane_text: &str) -> bool {
    let mut tail: Vec<&str> = pane_text
        .lines()
        .map(frame_trim)
        .filter(|line| !line.is_empty())
        .collect();
    if tail.len() > DIALOG_TAIL_LINES {
        tail.drain(..tail.len() - DIALOG_TAIL_LINES);
    }
    let Some(bottom) = tail.last() else {
        return false;
    };
    if option_numbers(bottom).is_empty() {
        return false;
    }
    let distinct: std::collections::BTreeSet<u32> =
        tail.iter().flat_map(|line| option_numbers(line)).collect();
    distinct.len() >= 2
}

/// The frame-trimmed bottom of a captured pane, for a `needs_human` entry's details.
fn pane_tail(pane_text: &str) -> String {
    let mut tail: Vec<&str> = pane_text
        .lines()
        .map(frame_trim)
        .filter(|line| !line.is_empty())
        .rev()
        .take(DIALOG_TAIL_LINES)
        .collect();
    tail.reverse();
    tail.join("\n").chars().take(600).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The modal from issue #9, as the field battery captured it.
    const FIELD_MODAL: &str = "Background server has incompatible feature settings\nThis session requires api_key_model_discovery to be disabled\n… 1. Run without daemon this time   2. Restart with these settings   › 3. Cancel\n";

    #[test]
    fn a_pane_ending_in_a_numbered_choice_dialog_is_a_dialog() {
        assert!(dialog_in_pane(FIELD_MODAL), "the field modal verbatim");
        assert!(
            dialog_in_pane(
                "╭─ codex ─────────────────────────────────────╮\n\
                 │ Background server has incompatible feature  │\n\
                 ╰─ 1. Run without daemon   2. Restart   › 3. Cancel ─╯\n"
            ),
            "a boxed modal"
        );
        assert!(
            dialog_in_pane("… 1. Run without daemon this time\n   2. Restart with these settings\n   › 3. Cancel\n"),
            "options wrapped one per line"
        );
        assert!(
            dialog_in_pane("› 1. Run without daemon this time\n  2. Restart with these settings\n  3. Cancel\n"),
            "options stacked with the cursor above"
        );
    }

    #[test]
    fn a_pane_not_ending_in_a_menu_is_not_a_dialog() {
        assert!(!dialog_in_pane("Analyzing the shard…\n│ · Ctrl+C to interrupt │ 42% │\n"), "a healthy status line");
        assert!(
            !dialog_in_pane("Here is my plan:\n1. First\n2. Second\n3. Third\n│ > compose a message │\n"),
            "the agent's own numbered list with a composer line below it"
        );
        assert!(!dialog_in_pane("Retry now?\n1. Retry\n"), "a lone option is not a menu");
        assert!(
            !dialog_in_pane("The review found nothing to fix, so I will not\nchoose 3. Cancel anything.\n"),
            "prose that merely mentions an option"
        );
        assert!(!dialog_in_pane(""), "an empty pane");
    }

    #[test]
    fn a_dialog_marker_round_trips_with_and_without_its_escalation() {
        let fresh: DialogMarker = serde_json::from_str("{\"first_seen_ms\":1790000000000}").unwrap();
        assert_eq!(fresh.escalated_at_ms, None);
        let text = serde_json::to_string(&fresh).unwrap();
        assert!(!text.contains("escalated_at_ms"), "absent while unset: {text}");
        let raised = DialogMarker { first_seen_ms: 1, escalated_at_ms: Some(2) };
        let back: DialogMarker = serde_json::from_str(&serde_json::to_string(&raised).unwrap()).unwrap();
        assert_eq!(back.escalated_at_ms, Some(2));
    }
}
