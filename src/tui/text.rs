// SPDX-License-Identifier: MIT
// Copyright (c) 2026 oat-agents contributors

//! Text the screens share: how a transcript is laid out, how Markdown reads,
//! how lines wrap, and the small glyph and colour tables every pane uses.

use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders};
use serde_json::Value;

use super::model::{self, AgentRow, string_field};
use crate::event_log::events;
use crate::transcript;

/// Braille frames for the one thing on the screen that has to move to mean
/// anything. The clock drives them, not the redraw: the view repaints on
/// every keystroke and at least every 200 ms, and a spinner tied to that
/// would race on a busy terminal and crawl on a quiet one.
/// The mark for a Run that is waiting on a person, in the Run view and in
/// the picker, so it is the same shape wherever it is met.
pub(crate) const WAITING_MARK: &str = "🙋";

/// The two columns every message row starts with: a wall-clock stamp, then
/// what kind of row it is. Named so the live signal can leave exactly that
/// much blank and start where the words do.
pub(crate) const STAMP_COLUMN: usize = 9;

pub(crate) const LABEL_COLUMN: usize = 8;

pub(crate) const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

pub(crate) fn spinner_frame(now_ms: i64) -> &'static str {
    SPINNER[(now_ms.max(0) / 120) as usize % SPINNER.len()]
}

/// `sonnet-5 medium`: what the agent is actually running as, without the
/// vendor prefix every row would otherwise repeat.
pub(crate) fn model_and_effort(session: &transcript::Session) -> String {
    let model = session
        .model
        .as_deref()
        .map(|model| model.trim_start_matches("claude-").to_owned())
        .unwrap_or_default();
    match session.effort.as_deref() {
        Some(effort) if !model.is_empty() => format!("{model} {effort}"),
        _ => model,
    }
}

/// What the agent is doing right now, from Claude Code's own transcript:
/// the tool it is inside, or how long since its last turn. Empty for an
/// agent the workflow log has already seen exit — a finished session's last
/// entry would otherwise read as work still in progress.
/// The turns of a transcript, laid out as the screen reads them.
///
/// The conversation — what the agent was asked and what it answered —
/// reads bright; its thinking, tool calls and tool output are the context
/// behind it. Shared, because the system console shows the same kind of
/// conversation as an agent's own page and they have to look alike.
pub(crate) fn transcript_lines(rows: &[transcript::Row]) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for row in rows {
        // The conversation — what the agent was asked and what it
        // answered — reads bright; its thinking, tool calls and tool
        // output are the context behind it.
        let (label, label_style, body_style) = match row.kind {
            transcript::RowKind::Text => (
                "say   ",
                Style::new().fg(Color::White).add_modifier(Modifier::BOLD),
                Style::new().fg(Color::White),
            ),
            transcript::RowKind::Prompt => (
                "prompt",
                Style::new().fg(Color::Blue).add_modifier(Modifier::BOLD),
                Style::new().fg(Color::White),
            ),
            transcript::RowKind::Thinking => (
                "think ",
                Style::new().fg(Color::DarkGray),
                Style::new().fg(Color::DarkGray),
            ),
            transcript::RowKind::Tool => (
                "tool  ",
                Style::new().fg(Color::Cyan),
                Style::new().fg(Color::DarkGray),
            ),
            transcript::RowKind::Result => (
                "result",
                Style::new().fg(Color::Green),
                Style::new().fg(Color::DarkGray),
            ),
            // The conversation at the session hosting this agent, which is where a message
            // sent from the live tab actually lands.
            transcript::RowKind::You => (
                "you   ",
                Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD),
                Style::new().fg(Color::Yellow),
            ),
        };
        let stamp = Span::styled(
            format!(
                "{:<STAMP_COLUMN$}",
                local_stamp(Some(&row.time), "%H:%M:%S")
            ),
            Style::new().fg(Color::DarkGray),
        );
        let label = Span::styled(format!("{label:<LABEL_COLUMN$}"), label_style);
        // A prompt or a reply is written text: its own line breaks and its
        // Markdown are how it was meant to be read.
        let rendered = if matches!(
            row.kind,
            transcript::RowKind::Prompt
                | transcript::RowKind::Text
                | transcript::RowKind::Thinking
                | transcript::RowKind::You
        ) {
            markdown_block(&row.text, body_style)
        } else {
            vec![vec![Span::styled(row.text.clone(), body_style)]]
        };
        // A message of several lines starts below its label, marked so a
        // reader knows the text continues rather than having ended.
        if rendered.len() > 1 {
            lines.push(Line::from(vec![
                stamp.clone(),
                label.clone(),
                Span::styled("⤵", Style::new().fg(Color::Cyan)),
            ]));
            for spans in rendered {
                let mut line = vec![Span::raw("  ")];
                line.extend(spans);
                lines.push(Line::from(line));
            }
        } else {
            let mut line = vec![stamp.clone(), label.clone()];
            line.extend(rendered.into_iter().flatten());
            lines.push(Line::from(line));
        }
    }
    lines
}

pub(crate) fn pulse_spans(
    session: Option<&transcript::Session>,
    live: bool,
    now_ms: i64,
) -> Vec<Span<'static>> {
    let Some(session) = session else {
        return Vec::new();
    };
    // Whether the one thing it is inside has outlasted the explanation for
    // it. Kept apart from the verdict because the agent is still live and
    // the spinner should still turn — what changes is that the row now
    // says so on its own instead of leaving a reader to do the arithmetic
    // on an age and decide for themselves that four hours is too many.
    let overrun = session.pulse.as_ref().is_some_and(|pulse| {
        pulse.overrunning(
            now_ms,
            transcript::QUIET_AFTER_MS,
            transcript::OVERRUN_AFTER_MS,
        )
    });
    let (glyph, state_word, body) = match (live, session.pulse.as_ref()) {
        (true, Some(pulse)) => {
            let age = model::human_span(pulse.silent_ms(now_ms) / 1000);
            match pulse.liveness(now_ms, transcript::QUIET_AFTER_MS) {
                // Inside a tool, its name is more use than the word for it.
                transcript::Liveness::Working => (
                    spinner_frame(now_ms),
                    if overrun { "overrun" } else { "live" },
                    match &pulse.doing {
                        transcript::Doing::Tool(name) => format!("{name} {age}"),
                        transcript::Doing::Turn => format!("working {age}"),
                    },
                ),
                // Naming the job matters: its output is on disk under that
                // id, which is where a reader goes next.
                transcript::Liveness::Waiting => (
                    spinner_frame(now_ms),
                    if overrun { "overrun" } else { "live" },
                    format!("waiting {age}"),
                ),
                transcript::Liveness::Stalled => ("·", "stalled", age),
            }
        }
        // An agent the Run has seen exit says nothing about what it is doing,
        // but what it left in its context is still worth reading.
        (false, _) if session.context.is_some() => ("✓", "done", String::new()),
        _ => return Vec::new(),
    };
    // How full its context is, which cumulative tokens do not say: a session
    // can have spent millions and still have room, or have spent little and
    // be nearly out.
    let share = session.context_share();
    let body = match share {
        Some(share) if body.is_empty() => format!("ctx {share:.0}%"),
        Some(share) => format!("{body} · ctx {share:.0}%"),
        None => body,
    };
    if body.is_empty() {
        return Vec::new();
    }
    // The pane's own cyan, so the signal reads as part of the frame it sits
    // on rather than as another line of dim detail. Alive and stopped are
    // told apart by the glyph — a turning spinner against a still dot —
    // which leaves the colour free to do nothing but be seen. A context
    // close to full is the exception: that is worth interrupting for.
    let cyan = if overrun {
        Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD)
    } else {
        Style::new().fg(Color::Cyan)
    };
    let body_style = match (overrun, share) {
        (true, _) => Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        (_, Some(share)) if share >= 90.0 => {
            Style::new().fg(Color::Red).add_modifier(Modifier::BOLD)
        }
        (_, Some(share)) if share >= 75.0 => Style::new().fg(Color::Yellow),
        _ => cyan,
    };
    // In the text column, under the messages rather than beside them: the
    // stamp and label columns have nothing to put here, and padding the
    // signal out to fill them left a hole through the middle of it. Blank
    // where a message's stamp and label would be, then one unbroken phrase
    // where its words would be, bracketed as the one row nobody wrote.
    let indent = " ".repeat(STAMP_COLUMN + LABEL_COLUMN);
    let body = if body.is_empty() {
        String::new()
    } else {
        format!(" {body}")
    };
    vec![
        Span::raw(indent),
        Span::styled(format!("[ {glyph} {state_word}"), cyan),
        Span::styled(format!("{body} ]"), body_style),
    ]
}

/// When an agent was last heard from, which is its exit if it has one.
pub(crate) fn last_seen(agent: &AgentRow) -> String {
    agent
        .exited
        .clone()
        .or_else(|| agent.entered.clone())
        .unwrap_or_default()
}

pub(crate) fn role_color(role: Option<&str>) -> Color {
    match role {
        Some("meta" | "oat-meta") => Color::Magenta,
        Some("planner") => Color::Cyan,
        Some("worker") => Color::Green,
        Some("reviewer") => Color::Yellow,
        _ => Color::Gray,
    }
}

pub(crate) fn event_glyph(event: &str) -> (&'static str, Color) {
    match event {
        events::AGENT_ENTER => ("▸", Color::Green),
        events::AGENT_EXIT => ("✓", Color::Blue),
        events::RUN_CREATED => ("◆", Color::White),
        events::RUN_CLOSED => ("■", Color::White),
        events::INBOX_MESSAGE => ("✉", Color::Cyan),
        events::INBOX_REPLY => ("↩", Color::Cyan),
        events::INBOX_ACK => ("·", Color::DarkGray),
        events::INBOX_TIMEOUT => ("⏱", Color::DarkGray),
        events::REVIEW_RESULT => ("★", Color::Yellow),
        events::ENV_IMAGE_BUILT => ("⬢", Color::Magenta),
        events::ENV_CREATED | events::ENV_READY => ("▣", Color::Blue),
        events::ENV_DESTROYED | events::ENV_REAPED => ("▢", Color::DarkGray),
        events::EXEC_PROFILE_SELECTED | events::EXEC_PROFILE_NONE => ("Σ", Color::Cyan),
        // A role that asked for a pod and ran on the host is the one environment event an
        // operator must not scroll past.
        events::ENV_SKIPPED => ("!", Color::Red),
        events::BLOCKER => ("!", Color::Red),
        events::NEEDS_HUMAN => (WAITING_MARK, Color::Yellow),
        events::HUMAN_ANSWER => ("✅", Color::Green),
        events::DECISION | events::DELEGATION | events::RETRY | events::NOTE => {
            ("◆", Color::Magenta)
        }
        _ => ("·", Color::Gray),
    }
}

/// An image id is a hash with a prefix; the first few characters are enough to
/// tell two of them apart, which is all anyone reads it for.
pub(crate) fn short_image(image_id: &str) -> String {
    crate::hashing::short(image_id).chars().take(12).collect()
}

/// One-line summary of an event for the timeline pane.
pub(crate) fn event_detail(row: &Value) -> String {
    let event = string_field(row, "event").unwrap_or_default();
    let mut parts = Vec::new();
    match event.as_str() {
        events::AGENT_ENTER => {
            if let Some(role) = string_field(row, "role") {
                parts.push(role);
            }
            if let Some(backend) = string_field(row, "backend") {
                parts.push(format!("({backend})"));
            }
        }
        events::AGENT_EXIT => {
            if let Some(outcome) = string_field(row, "outcome") {
                parts.push(outcome);
            }
        }
        events::INBOX_MESSAGE => {
            if let Some(kind) = string_field(row, "kind") {
                parts.push(kind);
            }
        }
        events::EXEC_PROFILE_SELECTED => {
            if let Some(profile) = string_field(row, "profile") {
                parts.push(format!("pod:{profile}"));
            }
        }
        events::EXEC_PROFILE_NONE => parts.push("host".to_owned()),
        events::ENV_SKIPPED => parts.push("HOST (no pod)".to_owned()),
        events::RUN_CREATED => {
            if let Some(goal) = crate::event_log::run_goal(row) {
                parts.push(goal.lines().next().unwrap_or_default().to_owned());
            }
        }
        _ => {}
    }
    for key in ["message", "reason", "summary"] {
        if let Some(text) = string_field(row, key) {
            parts.push(text.lines().next().unwrap_or_default().to_owned());
        }
    }
    parts.join(" ")
}

/// Wall-clock time in the operator's own zone, which is the only clock that
/// compares against what a terminal is showing right now.
pub(crate) fn clock(row: &Value) -> String {
    let recorded = string_field(row, "time");
    model::local_time(None, recorded.as_deref(), "%H:%M:%S")
        .unwrap_or_else(|| "--:--:--".to_owned())
}

/// Same, for a stamp that only exists as text.
pub(crate) fn local_stamp(recorded: Option<&str>, format: &str) -> String {
    recorded
        .and_then(|text| model::local_time(None, Some(text), format))
        .unwrap_or_else(|| "-".to_owned())
}

/// Renders written text as lines of styled spans: the author's own line
/// breaks are kept, and the Markdown they wrote is shown as emphasis rather
/// than as punctuation.
pub(crate) fn markdown_block(text: &str, base: Style) -> Vec<Vec<Span<'static>>> {
    let mut out = Vec::new();
    let mut fenced = false;
    for raw in text.split('\n') {
        let line = raw.trim_end();
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") {
            fenced = !fenced;
            out.push(vec![Span::styled(
                line.to_owned(),
                Style::new().fg(Color::DarkGray),
            )]);
            continue;
        }
        if fenced {
            out.push(vec![Span::styled(
                line.to_owned(),
                Style::new().fg(Color::Cyan),
            )]);
            continue;
        }
        if line.is_empty() {
            out.push(vec![Span::raw("")]);
            continue;
        }
        if let Some(heading) = trimmed.strip_prefix('#') {
            let heading = heading.trim_start_matches('#').trim();
            out.push(vec![Span::styled(
                heading.to_owned(),
                base.add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
            )]);
            continue;
        }
        if trimmed.starts_with("> ") {
            out.push(vec![Span::styled(
                line.to_owned(),
                Style::new().fg(Color::DarkGray),
            )]);
            continue;
        }
        let indent = &line[..line.len() - trimmed.len()];
        let mut spans = Vec::new();
        if !indent.is_empty() {
            spans.push(Span::raw(indent.to_owned()));
        }
        if let Some(rest) = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "))
        {
            spans.push(Span::styled("• ", Style::new().fg(Color::Cyan)));
            spans.extend(markdown_inline(rest, base));
        } else {
            spans.extend(markdown_inline(trimmed, base));
        }
        out.push(spans);
    }
    out
}

/// Inline emphasis: `code` and **bold**, shown rather than spelled out.
pub(crate) fn markdown_inline(text: &str, base: Style) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut plain = String::new();
    let mut rest = text;
    while !rest.is_empty() {
        let marker = ["`", "**"]
            .iter()
            .filter_map(|marker| rest.find(marker).map(|index| (index, *marker)))
            .min();
        let Some((index, marker)) = marker else { break };
        let after = &rest[index + marker.len()..];
        let Some(end) = after.find(marker) else {
            break;
        };
        plain.push_str(&rest[..index]);
        if !plain.is_empty() {
            spans.push(Span::styled(std::mem::take(&mut plain), base));
        }
        let inner = &after[..end];
        spans.push(Span::styled(
            inner.to_owned(),
            if marker == "`" {
                Style::new().fg(Color::Cyan)
            } else {
                base.add_modifier(Modifier::BOLD)
            },
        ));
        rest = &after[end + marker.len()..];
    }
    plain.push_str(rest);
    if !plain.is_empty() {
        spans.push(Span::styled(plain, base));
    }
    spans
}

/// Wraps styled lines at word boundaries, keeping each span's style and
/// indenting continuations so a wrapped body stays readable.
pub(crate) fn wrap_lines(lines: Vec<Line<'static>>, width: usize) -> Vec<Line<'static>> {
    if width < 8 {
        return lines;
    }
    let mut wrapped = Vec::new();
    for line in lines {
        let mut current: Vec<Span<'static>> = Vec::new();
        let mut used = 0usize;
        for span in line.spans {
            let style = span.style;
            for word in span.content.split_inclusive(' ') {
                let length = word.chars().count();
                if used + length > width && used > 0 {
                    wrapped.push(Line::from(std::mem::take(&mut current)));
                    used = 2;
                    current.push(Span::raw("  "));
                }
                used += length;
                current.push(Span::styled(word.to_owned(), style));
            }
        }
        wrapped.push(Line::from(current));
    }
    wrapped
}

pub(crate) fn pane(title: &str, focused: bool) -> Block<'_> {
    let border = if focused {
        Color::Cyan
    } else {
        Color::DarkGray
    };
    Block::default()
        .borders(Borders::ALL)
        .border_style(Style::new().fg(border))
        .title(title.to_owned())
}

/// How an agent ended, and when — or that it has not. One column rather
/// than three: what happened to an agent and when it happened are read
/// together, and a running agent has nothing to say about either.
pub(crate) fn agent_status(agent: &AgentRow) -> String {
    if agent.state() == "active" {
        return "running".to_owned();
    }
    let outcome = agent.outcome.clone().unwrap_or_else(|| "exited".to_owned());
    format!(
        "{outcome} at {}",
        local_stamp(agent.exited.as_deref(), "%H:%M")
    )
}

/// Red for a failure and quiet for everything else: a finished agent that
/// did what it was asked is not news, and a running one is already marked
/// as running by the word and by the ● beside its name.
pub(crate) fn status_color(agent: &AgentRow) -> Color {
    if agent.state() == "active" {
        return Color::White;
    }
    match agent.outcome.as_deref() {
        Some(outcome) if outcome.contains("fail") || outcome.contains("error") => Color::Red,
        _ => Color::DarkGray,
    }
}
