// SPDX-License-Identifier: MIT
// Copyright (c) 2026 oat-agents contributors

//! The rework loop's state: what a review round filed against one piece of named work, and
//! the gate that turns those findings into the round's verdict.

use crate::error::{codes, err};
use anyhow::Result;
use serde::{Deserialize, Serialize};

/// How severe one finding is. The gate reads these: a P1 must be fixed, a P3 may instead be
/// waived with a note, and a P2 joins the P1s or the P3s as the Run's rework settings decide.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Severity {
    P1,
    P2,
    P3,
}

/// Where one finding stood when its round settled. Every finding ends its life `fixed` or
/// `waived` — an `open` finding always fails the gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingStatus {
    #[default]
    Open,
    Fixed,
    Waived,
}

/// One defect a review round filed: where it is, what is wrong, and what would fix it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Finding {
    /// Stable across rounds: a later round sets this finding's `status`, a new one gets a new id.
    pub id: String,
    pub severity: Severity,
    pub file: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    pub description: String,
    pub required_fix: String,
    #[serde(default)]
    pub status: FindingStatus,
    /// Why a waived finding will not be fixed. Required for a waiver, and a P1 is never waived.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// One round's filed findings, as the reviewer wrote them. Ids unique, and every waiver says
/// why — a waiver without a note, or on a P1, is refused rather than read loosely. An empty
/// array is a clean review: no findings, and the gate passes the round.
pub fn parse_round(text: &str) -> Result<Vec<Finding>> {
    let invalid = |message: String| err(codes::FINDINGS_INVALID, message);
    let filed: Vec<Finding> = serde_json::from_str(text)
        .map_err(|e| invalid(format!("findings must be a JSON array of findings: {e}")))?;
    let mut seen = std::collections::BTreeSet::new();
    for finding in &filed {
        if finding.id.trim().is_empty() {
            return Err(invalid(format!(
                "a finding with no id ({}); every finding carries a stable id like F1",
                finding.file
            )));
        }
        if !seen.insert(finding.id.clone()) {
            return Err(invalid(format!("finding {} filed twice in one round", finding.id)));
        }
        if finding.file.trim().is_empty() {
            return Err(invalid(format!(
                "finding {} names no file; cite where the defect is",
                finding.id
            )));
        }
        if finding.description.trim().is_empty() {
            return Err(invalid(format!("finding {} says nothing about what is wrong", finding.id)));
        }
        if finding.status == FindingStatus::Waived {
            if finding.severity == Severity::P1 {
                return Err(invalid(format!(
                    "finding {} is a P1 and cannot be waived; fix it",
                    finding.id
                )));
            }
            if finding.note.as_deref().is_none_or(|note| note.trim().is_empty()) {
                return Err(invalid(format!(
                    "finding {} is waived without a note; say why it will not be fixed",
                    finding.id
                )));
            }
        }
    }
    Ok(filed)
}

/// A round's verdict, derived from its findings by the gate — never chosen by the reviewer,
/// and never the Dispatch's settlement: a review that failed its gate still settles
/// `succeeded` (the role protocol's contract).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Pass,
    Fail,
}

/// The gate (`issue #2` scope 4): PASS requires every P1 fixed, every P2 fixed unless the
/// repository made P2s advisory, every P3 fixed or waived. Nothing passes while open.
pub fn evaluate(merged: &[Finding], p2_required: bool) -> Verdict {
    let resolved = |finding: &Finding| match finding.severity {
        Severity::P1 => finding.status == FindingStatus::Fixed,
        Severity::P2 => finding.status == FindingStatus::Fixed
            || (!p2_required && finding.status == FindingStatus::Waived),
        Severity::P3 => {
            finding.status == FindingStatus::Fixed || finding.status == FindingStatus::Waived
        }
    };
    if merged.iter().all(resolved) {
        Verdict::Pass
    } else {
        Verdict::Fail
    }
}

/// One review of one piece of named work, as it settled: what the reviewer filed, the verdict
/// the gate derived, and the commit the reviewer read.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewRound {
    pub round: u32,
    pub dispatch_id: String,
    pub verdict: Verdict,
    /// As filed this round. A finding a later round does not mention keeps its last state.
    pub findings: Vec<Finding>,
    /// The HEAD of the worktree this round read, so the next round's fix diff starts here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reviewed_commit: Option<String>,
    pub created_at: String,
}

/// A work's findings record, one file under the Run's state: every review round it has settled,
/// oldest first.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkFindings {
    pub name: String,
    pub rounds: Vec<ReviewRound>,
}

impl WorkFindings {
    /// Every finding ever filed, each in the state its latest round left it, ordered by id.
    pub fn merged(&self) -> Vec<Finding> {
        let mut by_id = std::collections::BTreeMap::new();
        for round in &self.rounds {
            for finding in &round.findings {
                by_id.insert(finding.id.clone(), finding.clone());
            }
        }
        by_id.into_values().collect()
    }

    pub fn verdict(&self) -> Option<Verdict> {
        self.rounds.last().map(|round| round.verdict)
    }
}

/// What one settled round added: the work it belongs to (its canonical name), the round as
/// recorded, the merged state the verdict was derived from, and whether the round cap
/// escalated this work to the human.
pub struct AppendedRound {
    pub work: String,
    pub round: ReviewRound,
    pub merged: Vec<Finding>,
    pub escalated: bool,
}

/// A Run's findings records, one JSON file per piece of named work under
/// `<state>/<run_id>/findings/`.
pub struct FindingsStore {
    dir: std::path::PathBuf,
}

impl FindingsStore {
    pub fn for_run(store: &crate::store::Store, run_id: &str) -> Self {
        Self { dir: store.findings_dir(run_id) }
    }

    /// For tests and callers that already hold the directory.
    pub fn for_dir(dir: std::path::PathBuf) -> Self {
        Self { dir }
    }

    fn path(&self, name: &str) -> std::path::PathBuf {
        self.dir.join(format!("{name}.json"))
    }

    /// The work's findings record, `None` when no round has settled under any name it could be.
    pub fn load(&self, name: &str) -> Result<Option<WorkFindings>> {
        let Some(canonical) = self.resolve(name) else {
            return Ok(None);
        };
        let content = std::fs::read_to_string(self.path(&canonical))
            .map_err(|e| err(codes::INTERNAL_ERROR, format!("could not read findings for {canonical}: {e}")))?;
        Ok(Some(serde_json::from_str(&content)?))
    }

    /// The canonical name of the work `name` belongs to: the longest existing record whose name
    /// is `name` or a `-`-separated prefix of it — the same rule a checklist link claims its
    /// Dispatches by, so `kiln`, `kiln-fix1` and `kiln-fix1-review` are one work.
    pub fn resolve(&self, name: &str) -> Option<String> {
        let Ok(read) = std::fs::read_dir(&self.dir) else {
            return None;
        };
        read.filter_map(|entry| entry.ok())
            .filter_map(|entry| {
                let stem = entry.file_name().to_string_lossy().to_string();
                stem.strip_suffix(".json").map(str::to_owned)
            })
            .filter(|stem| crate::checklist::link_matches(stem, name))
            .max_by_key(|stem| stem.len())
    }

    /// Records one settled round under the work the settling Dispatch's name belongs to,
    /// creating the record when it is the first, and derives the round's verdict from the
    /// merged state of every round so far.
    pub fn append_round(
        &self,
        dispatch_name: &str,
        dispatch_id: &str,
        filed: Vec<Finding>,
        reviewed_commit: Option<String>,
        settings: &ReworkSettings,
    ) -> Result<AppendedRound> {
        let work = self
            .resolve(dispatch_name)
            .unwrap_or_else(|| dispatch_name.to_string());
        let mut record = self.load_exact(&work)?;
        let round_no = record.rounds.len() as u32 + 1;
        let mut merged = record.merged();
        for finding in &filed {
            if let Some(existing) = merged.iter_mut().find(|f| f.id == finding.id) {
                *existing = finding.clone();
            } else {
                merged.push(finding.clone());
            }
        }
        let verdict = evaluate(&merged, settings.p2_required);
        let round = ReviewRound {
            round: round_no,
            dispatch_id: dispatch_id.to_string(),
            verdict,
            findings: filed,
            reviewed_commit,
            created_at: crate::event_log::now_iso(),
        };
        record.rounds.push(round.clone());
        std::fs::create_dir_all(&self.dir)?;
        std::fs::write(self.path(&work), serde_json::to_string_pretty(&record)?)?;
        let escalated = verdict == Verdict::Fail && round_no >= settings.max_rounds;
        Ok(AppendedRound { work, round, merged, escalated })
    }

    /// Loads the record filed under exactly `name`, an empty one when there is none yet.
    fn load_exact(&self, name: &str) -> Result<WorkFindings> {
        let content = match std::fs::read_to_string(self.path(name)) {
            Ok(content) => content,
            Err(_) => return Ok(WorkFindings { name: name.to_string(), rounds: Vec::new() }),
        };
        Ok(serde_json::from_str(&content)?)
    }
}

/// Every finding as readable lines, for the inbox message and the briefs.
pub fn render_findings(merged: &[Finding]) -> String {
    merged
        .iter()
        .map(render_finding)
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// One finding as a readable line with its fix, for the inbox message and the briefs.
pub fn render_finding(finding: &Finding) -> String {
    let location = match finding.line {
        Some(line) => format!("{}:{line}", finding.file),
        None => finding.file.clone(),
    };
    let mut out = format!("[{:?}] {} {location} — {}", finding.severity, finding.id, finding.description);
    if !finding.required_fix.trim().is_empty() {
        out.push_str(&format!("\n     fix: {}", finding.required_fix));
    }
    match finding.status {
        FindingStatus::Fixed => out.push_str(" (fixed)"),
        FindingStatus::Waived => out.push_str(&format!(
            " (waived: {})",
            finding.note.as_deref().unwrap_or("no note given")
        )),
        FindingStatus::Open => {}
    }
    out
}

/// A settled round as one message block: the verdict the gate derived and every finding in its
/// merged state.
pub fn render_round(round: &ReviewRound, merged: &[Finding]) -> String {
    let verdict = match round.verdict {
        Verdict::Pass => "PASS",
        Verdict::Fail => "FAIL",
    };
    let mut out = format!("Review round {}: {verdict}\n", round.round);
    for finding in merged {
        out.push('\n');
        out.push_str(&render_finding(finding));
    }
    out
}

/// How a Run runs its rework loop, settled once at `meta fire`: the meta-agent's plugin
/// supplies the defaults (`[rework]` in its `core/oat-meta.toml`), the repository's
/// `.oat/rework.toml` overrides them (ADR-0009, the ADR-0006 pattern), and a Run keeps the
/// settings it started with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReworkSettings {
    /// A work that still fails its gate at this many review rounds escalates to the human
    /// instead of looping.
    #[serde(default = "ReworkSettings::default_max_rounds")]
    pub max_rounds: u32,
    /// Whether a P2 finding must be fixed to pass the gate. `false` makes P2s advisory: like a
    /// P3, one may be waived with a note instead.
    #[serde(default = "ReworkSettings::default_p2_required")]
    pub p2_required: bool,
}

impl Default for ReworkSettings {
    fn default() -> Self {
        ReworkSettings { max_rounds: 3, p2_required: true }
    }
}

impl ReworkSettings {
    pub fn default_max_rounds() -> u32 {
        3
    }

    pub fn default_p2_required() -> bool {
        true
    }

    pub fn is_default(&self) -> bool {
        *self == ReworkSettings::default()
    }

    /// The repository's `.oat/rework.toml` over `default` (the plugin's); a repository without
    /// one runs the plugin's bounds.
    pub fn resolve(default: ReworkSettings, repo: &std::path::Path) -> Result<Self> {
        let path = repo.join(".oat/rework.toml");
        let contents = match std::fs::read_to_string(&path) {
            Ok(contents) => contents,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(default),
            Err(e) => {
                return Err(err(codes::REWORK_SETTINGS_INVALID, format!("could not read {}: {e}", path.display())))
            }
        };
        let invalid = |message: String| err(codes::REWORK_SETTINGS_INVALID, format!(".oat/rework.toml: {message}"));
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct ReworkToml {
            max_rounds: Option<u32>,
            p2_required: Option<bool>,
        }
        let parsed: ReworkToml = toml::from_str(&contents).map_err(|e| invalid(e.to_string()))?;
        let settings = ReworkSettings {
            max_rounds: parsed.max_rounds.unwrap_or(default.max_rounds),
            p2_required: parsed.p2_required.unwrap_or(default.p2_required),
        };
        if settings.max_rounds == 0 {
            return Err(invalid("max_rounds must be at least 1".to_string()));
        }
        Ok(settings)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_round_parses_from_the_reviewers_json_array() {
        let filed = parse_round(
            r#"[
                {"id": "F1", "severity": "P1", "file": "src/auth/apple.ts", "line": 88,
                 "description": "idToken accepted without verification",
                 "required_fix": "verify the signature and issuer before use"},
                {"id": "F2", "severity": "P3", "file": "README.md",
                 "description": "setup step out of order",
                 "required_fix": "move it after the install step",
                 "status": "waived", "note": "cosmetic, out of scope for this change"}
            ]"#,
        )
        .unwrap();
        assert_eq!(filed.len(), 2);
        assert_eq!(filed[0].severity, Severity::P1);
        assert_eq!(filed[0].status, FindingStatus::Open, "a finding arrives open unless said otherwise");
        assert_eq!(filed[1].status, FindingStatus::Waived);
        assert_eq!(filed[1].note.as_deref(), Some("cosmetic, out of scope for this change"));

        // A clean review is an empty array, not an absent one: it files a round the gate passes.
        assert_eq!(parse_round("[]").unwrap(), Vec::new());
    }

    fn finding(id: &str, severity: Severity, status: FindingStatus) -> Finding {
        Finding {
            id: id.to_string(),
            severity,
            file: "src/kiln.rs".to_string(),
            line: Some(8),
            description: "the glaze cracks".to_string(),
            required_fix: "fire it slower".to_string(),
            status,
            note: (status == FindingStatus::Waived).then(|| "kiln is being replaced".to_string()),
        }
    }

    fn settings(max_rounds: u32) -> ReworkSettings {
        ReworkSettings { max_rounds, ..ReworkSettings::default() }
    }

    #[test]
    fn the_gate_passes_only_what_is_fixed_or_properly_waived() {
        // A P1 must be fixed; it cannot be waived (parse refuses that, and the gate agrees).
        assert_eq!(evaluate(&[finding("F1", Severity::P1, FindingStatus::Open)], true), Verdict::Fail);
        assert_eq!(evaluate(&[finding("F1", Severity::P1, FindingStatus::Waived)], true), Verdict::Fail);
        assert_eq!(evaluate(&[finding("F1", Severity::P1, FindingStatus::Fixed)], true), Verdict::Pass);

        // A P3 may be waived with a note, but an open one blocks like any other.
        assert_eq!(
            evaluate(&[finding("F3", Severity::P3, FindingStatus::Waived)], true),
            Verdict::Pass
        );
        assert_eq!(evaluate(&[finding("F3", Severity::P3, FindingStatus::Open)], true), Verdict::Fail);

        // A P2 must be fixed while the repository requires it; made advisory, a waiver closes
        // it like a P3's — but leaving it open still blocks.
        assert_eq!(evaluate(&[finding("F2", Severity::P2, FindingStatus::Open)], true), Verdict::Fail);
        assert_eq!(evaluate(&[finding("F2", Severity::P2, FindingStatus::Waived)], true), Verdict::Fail);
        assert_eq!(evaluate(&[finding("F2", Severity::P2, FindingStatus::Fixed)], true), Verdict::Pass);
        assert_eq!(evaluate(&[finding("F2", Severity::P2, FindingStatus::Open)], false), Verdict::Fail);
        assert_eq!(evaluate(&[finding("F2", Severity::P2, FindingStatus::Waived)], false), Verdict::Pass);

        // Findings of every grade must stand resolved together.
        let all = vec![
            finding("F1", Severity::P1, FindingStatus::Fixed),
            finding("F2", Severity::P2, FindingStatus::Fixed),
            finding("F3", Severity::P3, FindingStatus::Waived),
        ];
        assert_eq!(evaluate(&all, true), Verdict::Pass);
        let mut one_open = all.clone();
        one_open[1].status = FindingStatus::Open;
        assert_eq!(evaluate(&one_open, true), Verdict::Fail);
    }

    #[test]
    fn rounds_accumulate_under_one_work_name_and_carry_unmentioned_findings_forward() {
        let dir = tempfile::tempdir().unwrap();
        let store = FindingsStore::for_dir(dir.path().to_path_buf());
        assert_eq!(store.resolve("kiln"), None, "nothing has settled yet");

        let p1 = finding("F1", Severity::P1, FindingStatus::Open);
        let p3 = finding("F3", Severity::P3, FindingStatus::Open);
        let first = store
            .append_round("kiln", "dispatch-1", vec![p1.clone(), p3.clone()], Some("cafe1".to_string()), &settings(3))
            .unwrap();
        assert_eq!((first.work.as_str(), first.round.round, first.round.verdict), ("kiln", 1, Verdict::Fail));
        assert!(!first.escalated, "the cap is nowhere near");

        // A settling Dispatch named for a later round of the same work joins its record.
        assert_eq!(store.resolve("kiln-fix1").as_deref(), Some("kiln"));
        let fixed = Finding { status: FindingStatus::Fixed, ..p1.clone() };
        let second = store
            .append_round("kiln-fix1", "dispatch-2", vec![fixed], Some("cafe2".to_string()), &settings(3))
            .unwrap();
        assert_eq!((second.work.as_str(), second.round.round, second.round.verdict), ("kiln", 2, Verdict::Fail));
        assert_eq!(second.round.reviewed_commit.as_deref(), Some("cafe2"));

        let record = store.load("kiln").unwrap().unwrap();
        let merged = record.merged();
        assert_eq!(merged.len(), 2, "F3 was not mentioned in round 2 and keeps its state");
        assert_eq!(merged[0].status, FindingStatus::Fixed, "merged is ordered by id: F1 first");
        assert_eq!(merged[1].status, FindingStatus::Open);
        assert_eq!(record.verdict(), Some(Verdict::Fail));

        let waived = Finding { status: FindingStatus::Waived, ..p3.clone() };
        let third = store
            .append_round("kiln-fix2", "dispatch-3", vec![waived], None, &settings(3))
            .unwrap();
        assert_eq!(third.round.verdict, Verdict::Pass);
        assert_eq!(third.round.reviewed_commit, None);
        assert_eq!(store.load("kiln").unwrap().unwrap().verdict(), Some(Verdict::Pass));
    }

    #[test]
    fn the_round_cap_escalates_only_when_the_cap_round_itself_still_fails() {
        let dir = tempfile::tempdir().unwrap();
        let store = FindingsStore::for_dir(dir.path().to_path_buf());
        let p1 = finding("F1", Severity::P1, FindingStatus::Open);
        let first = store.append_round("kiln", "d1", vec![p1.clone()], None, &settings(2)).unwrap();
        assert!(!first.escalated, "round 1 of 2 still has a round left");
        let second = store.append_round("kiln-fix1", "d2", vec![p1], None, &settings(2)).unwrap();
        assert!(second.escalated, "round 2 of 2, still failing, escalates");
    }

    #[test]
    fn the_repository_overrides_the_plugins_bounds_field_by_field() {
        let repo = tempfile::tempdir().unwrap();
        let plugin = ReworkSettings { max_rounds: 5, p2_required: false };
        assert_eq!(
            ReworkSettings::resolve(plugin.clone(), repo.path()).unwrap(),
            plugin,
            "a repository without .oat/rework.toml runs the plugin's bounds"
        );
        assert_eq!(
            ReworkSettings::resolve(ReworkSettings::default(), repo.path()).unwrap(),
            ReworkSettings::default(),
            "and a plugin that sets nothing runs the core's defaults"
        );

        std::fs::create_dir_all(repo.path().join(".oat")).unwrap();
        std::fs::write(repo.path().join(".oat/rework.toml"), "max_rounds = 2\n").unwrap();
        let tuned = ReworkSettings::resolve(plugin, repo.path()).unwrap();
        assert_eq!(
            (tuned.max_rounds, tuned.p2_required),
            (2, false),
            "a field the repository leaves out keeps the plugin's value"
        );

        for refused in ["max_rounds = 0\n", "max_rounds = \"two\"\n", "p2_required = maybe\n", "rounds = 3\n"] {
            std::fs::write(repo.path().join(".oat/rework.toml"), refused).unwrap();
            let error = ReworkSettings::resolve(ReworkSettings::default(), repo.path()).unwrap_err();
            assert_eq!(
                crate::error::to_failure(&error).code,
                codes::REWORK_SETTINGS_INVALID,
                "{refused}"
            );
        }
    }

    #[test]
    fn a_round_that_cannot_be_read_is_refused_rather_than_read_loosely() {
        for (body, because) in [
            (r#"[{"id": "", "severity": "P1", "file": "a.ts", "description": "d", "required_fix": "f"}]"#, "empty id"),
            (r#"[{"id": "F1", "severity": "P0", "file": "a.ts", "description": "d", "required_fix": "f"}]"#, "unknown severity"),
            (r#"[{"id": "F1", "file": "a.ts", "description": "d", "required_fix": "f"}]"#, "missing severity"),
            (r#"[{"id": "F1", "severity": "P1", "description": "d", "required_fix": "f"}]"#, "missing file"),
            (r#"[{"id": "F1", "severity": "P1", "file": "a.ts", "required_fix": "f"}]"#, "missing description"),
            (r#"[{"id": "F1", "severity": "P1", "file": "a.ts", "description": "d", "required_fix": "f", "status": "maybe"}]"#, "unknown status"),
            (r#"[{"id": "F1", "severity": "P1", "file": "a.ts", "description": "d", "required_fix": "f", "requried_fix": "x"}]"#, "misspelled field"),
            (
                r#"[{"id": "F1", "severity": "P3", "file": "a.ts", "description": "d", "required_fix": "f", "status": "waived"}]"#,
                "a waiver without a note",
            ),
            (
                r#"[{"id": "F1", "severity": "P1", "file": "a.ts", "description": "d", "required_fix": "f", "status": "waived", "note": "later"}]"#,
                "a waived P1",
            ),
            (
                r#"[{"id": "F1", "severity": "P1", "file": "a.ts", "description": "d", "required_fix": "f"},
                   {"id": "F1", "severity": "P2", "file": "b.ts", "description": "d", "required_fix": "f"}]"#,
                "a duplicate id",
            ),
            (r#"{"findings": []}"#, "an object instead of an array"),
        ] {
            let error = parse_round(body).unwrap_err();
            assert_eq!(crate::error::to_failure(&error).code, codes::FINDINGS_INVALID, "{body}");
            assert!(!because.is_empty());
        }
    }
}
