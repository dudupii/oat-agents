// SPDX-License-Identifier: MIT
// Copyright (c) 2026 oat-agents contributors

mod common;

use common::{TempRepo, TestWorld};
use oat_agents::role::memory::InMemoryCatalogBuilder;
use oat_agents::role::{CoreRole, CoreRoleDefinition, RoleDefinition, StartLocation};
use oat_agents::{execute, Cli};
use clap::Parser;
use std::collections::BTreeMap;
use std::path::Path;

fn fixture_catalog() -> oat_agents::role::memory::InMemoryCatalog {
    InMemoryCatalogBuilder::new()
        .with_role(RoleDefinition {
            name: "worker".to_string(),
            instructions: "Write the pottery this task asks for.".to_string(),
            models: BTreeMap::new(),
            skills: Vec::new(),
            start: StartLocation::Fresh,
            exec_environment: false,
            prior_verification: false,
            max_concurrent: None,
        })
        .with_role(RoleDefinition {
            name: "reviewer".to_string(),
            instructions: "Review the pottery someone else made.".to_string(),
            models: BTreeMap::new(),
            skills: Vec::new(),
            start: StartLocation::Existing,
            exec_environment: false,
            prior_verification: false,
            max_concurrent: None,
        })
        .with_core_role(
            CoreRole::Meta,
            CoreRoleDefinition {
                instructions: "Coordinate the pottery workshop.".to_string(),
                models: BTreeMap::new(),
                skills: Vec::new(),
                backend: None,
                rework: Default::default(),
            },
        )
        .with_core_role(
            CoreRole::Console,
            CoreRoleDefinition {
                instructions: "Observe the pottery workshop.".to_string(),
                models: BTreeMap::new(),
                skills: Vec::new(),
                backend: None,
                rework: Default::default(),
            },
        )
        .build()
}

fn parse(args: &[&str]) -> Cli {
    let mut full = vec!["oat-agents"];
    full.extend_from_slice(args);
    Cli::try_parse_from(full).unwrap()
}

fn run_json(cli: Cli, env: &dyn oat_agents::environment::Environment) -> serde_json::Value {
    let catalog = fixture_catalog();
    execute(cli, env, &catalog).unwrap()
}

fn run_err(cli: Cli, env: &dyn oat_agents::environment::Environment) -> oat_agents::error::CliFailure {
    let catalog = fixture_catalog();
    oat_agents::error::to_failure(&execute(cli, env, &catalog).unwrap_err())
}

/// One Run with a worker Dispatch under way: `fire_worker` and `settle` drive the loop's
/// rounds, `commit` stands in for the agent's own work. Owns its repository and scratch
/// space, so both outlive the Run it starts.
struct Loop {
    /// Held, never read: the repository and scratch space live as long as the Loop.
    _temp: TempRepo,
    world: TestWorld,
    run: String,
}

impl Loop {
    /// `rework_toml` lands in the repository before `meta fire`, so the Run starts with it.
    fn start(run: &str, rework_toml: Option<&str>) -> Self {
        Self::start_with(run, rework_toml, None)
    }

    /// The same, with `[rework]` in the plugin's `core/oat-meta.toml` — the plugin-side source
    /// of the Run's bounds (ADR-0009).
    fn start_with(run: &str, rework_toml: Option<&str>, plugin_rework: Option<&str>) -> Self {
        let temp = TempRepo::new();
        let world = TestWorld::new();
        common::install_pottery_plugin_with_rework(&world, &temp.path(), plugin_rework);
        if let Some(contents) = rework_toml {
            std::fs::create_dir_all(temp.path().join(".oat")).unwrap();
            std::fs::write(temp.path().join(".oat/rework.toml"), contents).unwrap();
        }
        let env = world.env();
        run_json(
            parse(&[
                "meta", "fire", "--prompt", "Ship the mugs", "--repo",
                &temp.path().to_string_lossy(), "--name", run, "--agent", "claude",
            ]),
            &env,
        );
        Loop { _temp: temp, world, run: run.to_string() }
    }

    fn env(&self) -> oat_agents::environment::TestEnvironment {
        self.world.env().with_var("OAT_RUN_ID", self.run.clone())
    }

    fn fire_worker(&self, name: &str, prompt: &str) -> serde_json::Value {
        run_json(
            parse(&["role", "fire", "worker", "--name", name, "--prompt", prompt]),
            &self.env(),
        )
    }

    fn fire_reviewer(&self, from: &Path, name: &str, prompt: &str) -> serde_json::Value {
        run_json(
            parse(&[
                "role", "fire", "reviewer", "--from", &from.to_string_lossy(), "--name", name,
                "--prompt", prompt,
            ]),
            &self.env(),
        )
    }

    fn settle(&self, dispatch: &str, report: &str) -> serde_json::Value {
        run_json(
            parse(&["dispatch", "done", "--dispatch", dispatch, "--report", report]),
            &self.env(),
        )
    }

    fn settle_with_findings(&self, dispatch: &str, report: &str, findings: &str) -> serde_json::Value {
        let path = self.world.scratch.path().join("findings.json");
        std::fs::write(&path, findings).unwrap();
        run_json(
            parse(&[
                "dispatch", "done", "--dispatch", dispatch, "--report", report,
                "--findings-file", &path.to_string_lossy(),
            ]),
            &self.env(),
        )
    }

    fn commit(&self, worktree: &Path, name: &str, contents: &str) {
        std::fs::write(worktree.join(name), contents).unwrap();
        for args in [
            vec!["add", name],
            vec!["commit", "-q", "-m", &format!("add {name}")],
        ] {
            let status = std::process::Command::new("git")
                .current_dir(worktree)
                .args(&args)
                .status()
                .unwrap();
            assert!(status.success(), "git {args:?} failed");
        }
    }

    fn log(&self, event: &str) -> Vec<serde_json::Value> {
        let shown = run_json(
            parse(&["log", "show", "--run", &self.run, "--event", event, "--limit", "500"]),
            &self.env(),
        );
        shown["entries"].as_array().unwrap().clone()
    }

    fn state(&self) -> std::path::PathBuf {
        self.world.scratch.path().join("state").join("runs").join(&self.run)
    }
}

/// The finding that fails the gate: an unverified idToken, the shape of the B-line's catch.
const P1_OPEN: &str = r#"[
    {"id": "F1", "severity": "P1", "file": "src/mugs.ts", "line": 8,
     "description": "the handle is attached before the clay rests",
     "required_fix": "let the clay rest a day before attaching the handle"},
    {"id": "F2", "severity": "P3", "file": "README.md",
     "description": "the firing schedule omits the bisque step",
     "required_fix": "add the bisque step to the schedule",
     "status": "waived", "note": "documentation, not this change"}
]"#;

#[test]
fn a_review_settling_with_findings_derives_its_verdict_from_the_gate() {
    let rework = Loop::start("kiln", None);
    let worker = rework.fire_worker("kiln", "Glaze the mugs.");
    let worktree = std::path::PathBuf::from(worker["worktree"].as_str().unwrap());
    rework.commit(&worktree, "mugs.txt", "six mugs\n");
    rework.settle(worker["dispatch_id"].as_str().unwrap(), "glazed and committed");

    let reviewer = rework.fire_reviewer(&worktree, "kiln", "Review the glaze.");
    assert_eq!(reviewer["worktree"].as_str().unwrap(), worktree.to_string_lossy());

    let settled = rework.settle_with_findings(
        reviewer["dispatch_id"].as_str().unwrap(),
        "re-ran the glaze checks",
        P1_OPEN,
    );
    assert_eq!(settled["settled"], "succeeded", "a failing review still settles succeeded");
    assert_eq!(settled["review"]["verdict"], "fail");
    assert_eq!(settled["review"]["round"], 1);
    assert_eq!(settled["review"]["escalated"], false);

    // The meta-agent reads the round in its inbox message, findings and all.
    let inbox = rework.state().join("inbox");
    let settled_message: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(inbox.join("2.json")).unwrap(),
    )
    .unwrap();
    let message = settled_message["message"].as_str().unwrap();
    assert!(message.contains("re-ran the glaze checks"), "the prose report leads: {message}");
    assert!(message.contains("Review round 1: FAIL"), "{message}");
    assert!(message.contains("[P1] F1 src/mugs.ts:8"), "{message}");
    assert!(message.contains("let the clay rest"), "{message}");
    assert!(message.contains("(waived: documentation, not this change)"), "{message}");

    // The workflow log carries the round for observers, and the state holds it as structured
    // data the next round is built from.
    let rounds = rework.log("review-result");
    assert_eq!(rounds.len(), 1);
    assert_eq!(rounds[0]["details"]["verdict"], "fail");
    assert_eq!(rounds[0]["details"]["open"]["P1"], 1);
    let record: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(rework.state().join("findings").join("kiln.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(record["rounds"].as_array().unwrap().len(), 1);
    assert_eq!(record["rounds"][0]["verdict"], "fail");

    // The task as given survives verbatim, for the briefs to quote.
    let task = std::fs::read_to_string(
        std::path::PathBuf::from(worker["dispatch_dir"].as_str().unwrap()).join("task.md"),
    )
    .unwrap();
    assert_eq!(task, "Glaze the mugs.");
}

#[test]
fn rework_brief_quotes_the_original_task_hands_over_the_findings_and_fires() {
    let rework = Loop::start("kiln", None);
    let worker = rework.fire_worker("kiln", "Glaze the mugs.");
    let worktree = std::path::PathBuf::from(worker["worktree"].as_str().unwrap());
    rework.commit(&worktree, "mugs.txt", "six mugs\n");
    rework.settle(worker["dispatch_id"].as_str().unwrap(), "glazed and committed");
    let reviewer = rework.fire_reviewer(&worktree, "kiln", "Review the glaze.");
    rework.settle_with_findings(reviewer["dispatch_id"].as_str().unwrap(), "checked", P1_OPEN);

    let brief = run_json(
        parse(&["rework", "brief", "--run", "kiln", "--name", "kiln"]),
        &rework.env(),
    );
    assert_eq!(brief["work"], "kiln");
    assert_eq!(brief["round"], 2);
    assert_eq!(brief["next_name"], "kiln-fix1");
    assert_eq!(brief["branch"], "oat/kiln/kiln");
    let text = brief["brief"].as_str().unwrap();
    assert!(text.contains("Glaze the mugs."), "the original brief verbatim: {text}");
    assert!(text.contains("[P1] F1 src/mugs.ts:8"), "{text}");
    assert!(text.contains("let the clay rest"), "{text}");
    assert!(text.contains("`oat/kiln/kiln`"), "names the branch to continue: {text}");

    // The brief fires as-is: no surgery between the failed review and the fix round.
    let fix = rework.fire_worker(brief["next_name"].as_str().unwrap(), text);
    assert_eq!(fix["branch"], "oat/kiln/kiln-fix1");
}

#[test]
fn rework_brief_refuses_work_that_never_settled_or_already_passed() {
    let rework = Loop::start("kiln", None);
    let worker = rework.fire_worker("kiln", "Glaze the mugs.");
    let worktree = std::path::PathBuf::from(worker["worktree"].as_str().unwrap());
    rework.commit(&worktree, "mugs.txt", "six mugs\n");
    rework.settle(worker["dispatch_id"].as_str().unwrap(), "glazed and committed");
    let reviewer = rework.fire_reviewer(&worktree, "kiln", "Review the glaze.");

    let none = run_err(
        parse(&["rework", "brief", "--run", "kiln", "--name", "kiln"]),
        &rework.env(),
    );
    assert_eq!(none.code, "invalid_input");
    assert!(none.message.contains("no review has settled"), "{}", none.message);

    rework.settle_with_findings(reviewer["dispatch_id"].as_str().unwrap(), "checked", P1_OPEN);

    // A second round, same work: the fix lands, everything resolves.
    let fix = rework.fire_worker("kiln-fix1", "fix it");
    let fix_worktree = std::path::PathBuf::from(fix["worktree"].as_str().unwrap());
    rework.commit(&fix_worktree, "handles.txt", "rested handles\n");
    rework.settle(fix["dispatch_id"].as_str().unwrap(), "fixed");
    let re_reviewer = rework.fire_reviewer(&fix_worktree, "kiln-fix1", "Re-review the fix.");
    rework.settle_with_findings(
        re_reviewer["dispatch_id"].as_str().unwrap(),
        "all better",
        r#"[{"id": "F1", "severity": "P1", "file": "src/mugs.ts", "line": 8,
            "description": "the handle is attached before the clay rests",
            "required_fix": "let the clay rest a day before attaching the handle",
            "status": "fixed"},
           {"id": "F2", "severity": "P3", "file": "README.md",
            "description": "the firing schedule omits the bisque step",
            "required_fix": "add the bisque step",
            "status": "waived", "note": "documentation, not this change"}]"#,
    );
    let passed = run_err(
        parse(&["rework", "brief", "--run", "kiln", "--name", "kiln"]),
        &rework.env(),
    );
    assert_eq!(passed.code, "invalid_input");
    assert!(passed.message.contains("passed its gate"), "{}", passed.message);
}

#[test]
fn rework_delta_hands_the_reviewer_the_findings_and_only_the_fix_diff() {
    let rework = Loop::start("kiln", None);
    let worker = rework.fire_worker("kiln", "Glaze the mugs.");
    let worktree = std::path::PathBuf::from(worker["worktree"].as_str().unwrap());
    rework.commit(&worktree, "mugs.txt", "six mugs\n");
    rework.settle(worker["dispatch_id"].as_str().unwrap(), "glazed and committed");
    let reviewer = rework.fire_reviewer(&worktree, "kiln", "Review the glaze.");
    rework.settle_with_findings(reviewer["dispatch_id"].as_str().unwrap(), "checked", P1_OPEN);

    let brief = run_json(
        parse(&["rework", "brief", "--run", "kiln", "--name", "kiln"]),
        &rework.env(),
    );
    let fix = rework.fire_worker(brief["next_name"].as_str().unwrap(), brief["brief"].as_str().unwrap());
    let fix_worktree = std::path::PathBuf::from(fix["worktree"].as_str().unwrap());
    // The fix round does what its brief says: merge the attempt, then fix on top of it.
    let merged = std::process::Command::new("git")
        .current_dir(&fix_worktree)
        .args(["merge", "-q", brief["branch"].as_str().unwrap()])
        .status()
        .unwrap();
    assert!(merged.success(), "merging the attempt into the fix worktree failed");
    rework.commit(&fix_worktree, "handles.txt", "handles attached after a day of rest\n");

    let delta = run_json(
        parse(&["rework", "delta", "--run", "kiln", "--name", "kiln"]),
        &rework.env(),
    );
    assert_eq!(delta["work"], "kiln");
    assert_eq!(delta["round"], 2);
    assert_eq!(delta["next_name"], "kiln-fix1");
    // The diff runs from what round 1 reviewed to the fix's head.
    let record: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(rework.state().join("findings").join("kiln.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(delta["base"], record["rounds"][0]["reviewed_commit"]);
    assert_ne!(delta["head"], delta["base"]);
    let text = delta["brief"].as_str().unwrap();
    assert!(text.contains("Glaze the mugs."), "the original brief verbatim: {text}");
    assert!(text.contains("[P1] F1 src/mugs.ts:8"), "{text}");
    assert!(text.contains("## The fix diff"), "{text}");
    assert!(text.contains("handles attached after a day of rest"), "the fix, in the diff: {text}");
    assert!(!text.contains("six mugs"), "only the fix, not round 1's work: {text}");
    assert!(text.contains("dispatch done --findings-file"), "how to settle: {text}");

    // The reviewer's round fires as-is, from the fix's worktree under the fix's name.
    let re_reviewer = rework.fire_reviewer(
        &fix_worktree,
        delta["next_name"].as_str().unwrap(),
        delta["brief"].as_str().unwrap(),
    );
    assert_eq!(
        re_reviewer["worktree"].as_str().unwrap(),
        fix_worktree.to_string_lossy()
    );
}

#[test]
fn at_the_round_cap_the_work_escalates_to_the_human_instead_of_looping() {
    let rework = Loop::start("kiln", Some("max_rounds = 2\n"));
    let worker = rework.fire_worker("kiln", "Glaze the mugs.");
    let worktree = std::path::PathBuf::from(worker["worktree"].as_str().unwrap());
    rework.commit(&worktree, "mugs.txt", "six mugs\n");
    rework.settle(worker["dispatch_id"].as_str().unwrap(), "glazed and committed");
    let reviewer = rework.fire_reviewer(&worktree, "kiln", "Review the glaze.");
    let first = rework.settle_with_findings(reviewer["dispatch_id"].as_str().unwrap(), "checked", P1_OPEN);
    assert_eq!(first["review"]["verdict"], "fail");
    assert_eq!(first["review"]["escalated"], false, "one failed round is not yet the cap");

    // Round 2 of 2: the fix lands but the finding stays open.
    let brief = run_json(
        parse(&["rework", "brief", "--run", "kiln", "--name", "kiln"]),
        &rework.env(),
    );
    let fix = rework.fire_worker(brief["next_name"].as_str().unwrap(), brief["brief"].as_str().unwrap());
    let fix_worktree = std::path::PathBuf::from(fix["worktree"].as_str().unwrap());
    rework.commit(&fix_worktree, "handles.txt", "rushed handles\n");
    rework.settle(fix["dispatch_id"].as_str().unwrap(), "fixed, allegedly");
    let re_reviewer = rework.fire_reviewer(&fix_worktree, "kiln-fix1", "Re-review the fix.");
    let second = rework.settle_with_findings(
        re_reviewer["dispatch_id"].as_str().unwrap(),
        "still not resting the clay",
        r#"[{"id": "F1", "severity": "P1", "file": "src/mugs.ts", "line": 8,
            "description": "the handle is attached before the clay rests",
            "required_fix": "let the clay rest a day before attaching the handle"}]"#,
    );
    assert_eq!(second["review"]["round"], 2);
    assert_eq!(second["review"]["verdict"], "fail");
    assert_eq!(second["review"]["escalated"], true, "the cap turns a fail into an escalation");

    let escalations = rework.log("needs_human");
    assert_eq!(escalations.len(), 1, "exactly one escalation");
    assert_eq!(escalations[0]["details"]["reason"], "round_cap");
    assert_eq!(escalations[0]["details"]["work"], "kiln");
    assert_eq!(escalations[0]["details"]["rounds"], 2);
    assert_eq!(escalations[0]["details"]["max_rounds"], 2);

    // The stop order reaches the meta-agent in the one channel it reads, not only the log.
    let settled_message: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(rework.state().join("inbox").join("4.json")).unwrap(),
    )
    .unwrap();
    let message = settled_message["message"].as_str().unwrap();
    assert!(message.contains("Round cap reached"), "{message}");
    assert!(message.contains("Do not fire another round"), "{message}");

    // The loop stops: the next brief refuses, pointing at the escalation.
    let refused = run_err(
        parse(&["rework", "brief", "--run", "kiln", "--name", "kiln"]),
        &rework.env(),
    );
    assert_eq!(refused.code, "invalid_input");
    assert!(refused.message.contains("round cap"), "{}", refused.message);
}

#[test]
fn rework_show_reports_the_rounds_and_where_the_gate_stands() {
    let rework = Loop::start("kiln", Some("max_rounds = 2\n"));
    let worker = rework.fire_worker("kiln", "Glaze the mugs.");
    let worktree = std::path::PathBuf::from(worker["worktree"].as_str().unwrap());
    rework.commit(&worktree, "mugs.txt", "six mugs\n");
    rework.settle(worker["dispatch_id"].as_str().unwrap(), "glazed and committed");
    let reviewer = rework.fire_reviewer(&worktree, "kiln", "Review the glaze.");
    rework.settle_with_findings(reviewer["dispatch_id"].as_str().unwrap(), "checked", P1_OPEN);

    let shown = run_json(
        parse(&["rework", "show", "--run", "kiln", "--name", "kiln"]),
        &rework.env(),
    );
    assert_eq!(shown["work"], "kiln");
    assert_eq!(shown["rounds"].as_array().unwrap().len(), 1);
    assert_eq!(shown["gate"]["verdict"], "fail");
    assert_eq!(shown["gate"]["max_rounds"], 2, "the settings the Run started with");
    assert_eq!(shown["gate"]["p2_required"], true);
    let findings = shown["findings"].as_array().unwrap();
    assert_eq!(findings.len(), 2, "the merged record across rounds");
    assert_eq!(findings[0]["id"], "F1");
    assert_eq!(findings[0]["status"], "open");
}

#[test]
fn the_plugins_bounds_reach_the_gate_and_the_repository_still_overrides_them() {
    // The plugin allows one round: a single failing review is already the cap.
    let capped = Loop::start_with("kiln", None, Some("[rework]\nmax_rounds = 1\n"));
    let worker = capped.fire_worker("kiln", "Glaze the mugs.");
    let worktree = std::path::PathBuf::from(worker["worktree"].as_str().unwrap());
    capped.commit(&worktree, "mugs.txt", "six mugs\n");
    capped.settle(worker["dispatch_id"].as_str().unwrap(), "glazed and committed");
    let reviewer = capped.fire_reviewer(&worktree, "kiln", "Review the glaze.");
    let settled = capped.settle_with_findings(reviewer["dispatch_id"].as_str().unwrap(), "checked", P1_OPEN);
    assert_eq!(settled["review"]["escalated"], true, "round 1 of 1 escalates");
    let shown = run_json(
        parse(&["rework", "show", "--run", "kiln", "--name", "kiln"]),
        &capped.env(),
    );
    assert_eq!(shown["gate"]["max_rounds"], 1, "the plugin's bounds, as the Run settled them");

    // The repository's override beats the plugin's bounds (ADR-0009, the ADR-0006 pattern).
    let eased = Loop::start_with("kiln", Some("max_rounds = 5\n"), Some("[rework]\nmax_rounds = 1\n"));
    let worker = eased.fire_worker("kiln", "Glaze the mugs.");
    let worktree = std::path::PathBuf::from(worker["worktree"].as_str().unwrap());
    eased.commit(&worktree, "mugs.txt", "six mugs\n");
    eased.settle(worker["dispatch_id"].as_str().unwrap(), "glazed and committed");
    let reviewer = eased.fire_reviewer(&worktree, "kiln", "Review the glaze.");
    let settled = eased.settle_with_findings(reviewer["dispatch_id"].as_str().unwrap(), "checked", P1_OPEN);
    assert_eq!(settled["review"]["escalated"], false, "the repository eased the cap to 5");
}

#[test]
fn a_clean_review_files_an_empty_round_and_the_gate_passes_it() {
    let rework = Loop::start("kiln", None);
    let worker = rework.fire_worker("kiln", "Glaze the mugs.");
    let worktree = std::path::PathBuf::from(worker["worktree"].as_str().unwrap());
    rework.commit(&worktree, "mugs.txt", "six mugs\n");
    rework.settle(worker["dispatch_id"].as_str().unwrap(), "glazed and committed");
    let reviewer = rework.fire_reviewer(&worktree, "kiln", "Review the glaze.");
    let settled = rework.settle_with_findings(reviewer["dispatch_id"].as_str().unwrap(), "nothing to fix", "[]");
    assert_eq!(settled["review"]["verdict"], "pass");
    assert_eq!(settled["review"]["round"], 1);
    assert_eq!(settled["settled"], "succeeded");

    let settled_message: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(rework.state().join("inbox").join("2.json")).unwrap(),
    )
    .unwrap();
    let message = settled_message["message"].as_str().unwrap();
    assert!(message.contains("Review round 1: PASS"), "{message}");

    let refused = run_err(
        parse(&["rework", "brief", "--run", "kiln", "--name", "kiln"]),
        &rework.env(),
    );
    assert_eq!(refused.code, "invalid_input");
    assert!(refused.message.contains("passed its gate"), "{}", refused.message);
}
