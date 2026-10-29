// SPDX-License-Identifier: MIT
// Copyright (c) 2026 oat-agents contributors

mod common;

use clap::Parser;
use common::{TempRepo, TestWorld};
use oat_agents::plugin::{load, load_catalog};
use oat_agents::role::{Backend, CoreRole, RoleCatalog, StartLocation};
use oat_agents::{execute, Cli};
use std::path::{Path, PathBuf};

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn sample_plugin_dirs() -> Vec<PathBuf> {
    vec![
        fixtures_dir().join("sample-plugin"),
        fixtures_dir().join("sample-plugin-secondary"),
    ]
}

fn parse(args: &[&str]) -> Cli {
    let mut full = vec!["oat-agents"];
    full.extend_from_slice(args);
    Cli::try_parse_from(full).unwrap()
}

fn run_json(
    cli: Cli,
    env: &dyn oat_agents::environment::Environment,
    catalog: &dyn RoleCatalog,
) -> serde_json::Value {
    execute(cli, env, catalog).unwrap()
}

// ---- Loading the fixture plugin proves every kind of customization (spec AC3). ----

#[test]
fn loading_the_fixture_plugins_proves_every_kind_of_customization() {
    let dirs = sample_plugin_dirs();
    let (catalog, refs) = load_catalog(&dirs).unwrap_or_else(|errors| {
        panic!(
            "fixture plugins failed to load: {}",
            errors.iter().map(|e| e.to_string()).collect::<Vec<_>>().join("; ")
        )
    });

    let names: Vec<String> = refs.iter().map(|r| r.name.clone()).collect();
    assert_eq!(names, vec!["sample-plugin".to_string(), "sample-plugin-secondary".to_string()]);

    let mut role_names = catalog.role_names();
    role_names.sort();
    assert_eq!(role_names, vec!["sample-role-existing".to_string(), "sample-role-fresh".to_string()]);

    // Role instructions, per-backend role model, role skills, and both start locations.
    let fresh = catalog.role("sample-role-fresh").unwrap();
    assert!(fresh.instructions.contains("Sample role (fresh)"));
    assert_eq!(fresh.start, StartLocation::Fresh);
    assert!(fresh.exec_environment, "exec_environment reaches the catalog");
    assert!(fresh.prior_verification, "prior_verification reaches the catalog");
    assert_eq!(fresh.skills.len(), 1);
    assert_eq!(fresh.skills[0].name, "sample-skill");
    let claude_model = fresh.models.get(&Backend::Claude).expect("a Claude model was declared");
    assert_eq!(claude_model.model.as_deref(), Some("sample-claude-model"));
    let codex_model = fresh.models.get(&Backend::Codex).expect("a Codex model was declared");
    assert_eq!(codex_model.model.as_deref(), Some("sample-codex-model"));
    assert_eq!(codex_model.reasoning_effort.as_deref(), Some("high"));

    let existing = catalog.role("sample-role-existing").unwrap();
    assert!(existing.instructions.contains("Sample role (existing)"));
    assert_eq!(existing.start, StartLocation::Existing);
    assert!(!existing.exec_environment);
    assert!(!existing.prior_verification);

    // Core-role instructions, skills and model, joined across both plugins in plugin order.
    let meta = catalog.core_role(CoreRole::Meta).unwrap();
    let delegate_at = meta
        .instructions
        .find("Delegate every piece of work")
        .expect("the primary plugin's oat-meta instructions are present");
    let escalate_at = meta
        .instructions
        .find("Escalate anything the primary instructions")
        .expect("the secondary plugin's oat-meta instructions are present");
    assert!(delegate_at < escalate_at, "core-role instructions are joined in plugin order");
    assert_eq!(meta.skills.len(), 1);
    assert_eq!(meta.skills[0].name, "sample-skill");
    assert_eq!(
        meta.models.get(&Backend::Claude).unwrap().model.as_deref(),
        Some("sample-claude-meta-model")
    );

    let console = catalog.core_role(CoreRole::Console).unwrap();
    let answer_at = console
        .instructions
        .find("Answer questions about the state of a Run")
        .expect("the primary plugin's oat-console instructions are present");
    let doubt_at = console
        .instructions
        .find("When in doubt")
        .expect("the secondary plugin's oat-console instructions are present");
    assert!(answer_at < doubt_at, "core-role instructions are joined in plugin order");
    assert_eq!(
        console.models.get(&Backend::Codex).unwrap().model.as_deref(),
        Some("sample-codex-console-model")
    );
}

/// End to end: a real `meta fire` / `role fire` against the fixture plugin's catalog proves
/// every declared property actually reaches a launch, not just the in-memory `RoleDefinition`.
#[test]
fn the_fixture_plugin_s_properties_reach_a_real_launch() {
    let repo = TempRepo::new();
    let world = TestWorld::new();
    let env = world.env();
    let (catalog, _) = load_catalog(&sample_plugin_dirs()).unwrap();

    // F5's gate: `meta fire`/`role fire` now build their real catalog from `.oat/plugins.toml`,
    // not from the `catalog` fixture above (kept only because `execute` still takes one). Both
    // fixture plugins are copied into the repository, since a path pin only ever points inside
    // it, then pinned and trusted.
    let mut pins = Vec::new();
    for (dest, source) in [
        ("sample-plugin", fixtures_dir().join("sample-plugin")),
        ("sample-plugin-secondary", fixtures_dir().join("sample-plugin-secondary")),
    ] {
        common::copy_dir_all(&source, &repo.path().join(dest));
    }
    pins.push(oat_agents::plugins::path_pin_from_current_content(&repo.path(), "sample-plugin", "sample-plugin").unwrap());
    pins.push(
        oat_agents::plugins::path_pin_from_current_content(&repo.path(), "sample-plugin-secondary", "sample-plugin-secondary")
            .unwrap(),
    );
    common::write_plugins_config(&repo.path(), &pins);
    common::trust_pins(&world, &pins);

    let fire = parse(&[
        "meta", "fire", "--prompt", "plan", "--repo", &repo.path().to_string_lossy(),
        "--name", "fixture-run", "--agent", "claude",
    ]);
    let meta_result = run_json(fire, &env, &catalog);
    let meta_worktree = meta_result["worktree"].as_str().unwrap().to_string();
    let env_with_run = world.env().with_var("OAT_RUN_ID", "fixture-run");

    // Claude: the declared model reaches the launch, the skill lands with its script
    // executable, and `$sample-skill` is rewritten to a plain name in both the prompt and the
    // skill's own SKILL.md — never in its supporting script.
    let claude_fire = parse(&[
        "role", "fire", "sample-role-fresh", "--name", "fresh-claude", "--agent", "claude",
        "--prompt", "do work",
    ]);
    let claude_result = run_json(claude_fire, &env_with_run, &catalog);
    let claude_worktree = PathBuf::from(claude_result["worktree"].as_str().unwrap());
    let claude_dispatch_dir = PathBuf::from(claude_result["dispatch_dir"].as_str().unwrap());

    let claude_prompt = std::fs::read_to_string(claude_dispatch_dir.join("prompt.md")).unwrap();
    assert!(
        claude_prompt.contains("Load `sample-skill` before you begin"),
        "the skill cross-reference is rewritten to a plain name for Claude Code: {claude_prompt}"
    );
    assert!(!claude_prompt.contains("$sample-skill"));

    let claude_skill_md =
        std::fs::read_to_string(claude_worktree.join(".claude/skills/sample-skill/SKILL.md")).unwrap();
    assert!(!claude_skill_md.contains("$sample-skill"), "SKILL.md is rewritten for Claude Code");
    assert!(claude_skill_md.contains("Load `sample-skill` again"));

    let claude_script_path = claude_worktree.join(".claude/skills/sample-skill/check.sh");
    let claude_script = std::fs::read_to_string(&claude_script_path).unwrap();
    assert!(
        claude_script.contains("$sample-skill"),
        "a skill's supporting script is never rewritten, even on Claude Code: {claude_script}"
    );
    assert_executable(&claude_script_path);

    assert!(
        world.tmux_calls().iter().any(|c| c.contains("OAT_MODEL=sample-claude-model")),
        "the role's declared Claude model reaches the launch"
    );

    // Codex: the reference is left as written, and the other declared backend model reaches
    // the launch too.
    let codex_fire = parse(&[
        "role", "fire", "sample-role-fresh", "--name", "fresh-codex", "--agent", "codex",
        "--prompt", "do work",
    ]);
    let codex_result = run_json(codex_fire, &env_with_run, &catalog);
    let codex_worktree = PathBuf::from(codex_result["worktree"].as_str().unwrap());
    let codex_dispatch_dir = PathBuf::from(codex_result["dispatch_dir"].as_str().unwrap());

    let codex_prompt = std::fs::read_to_string(codex_dispatch_dir.join("prompt.md")).unwrap();
    assert!(
        codex_prompt.contains("Load `$sample-skill` before you begin"),
        "the skill cross-reference is left as written for Codex: {codex_prompt}"
    );

    let codex_skill_md =
        std::fs::read_to_string(codex_worktree.join(".agents/skills/sample-skill/SKILL.md")).unwrap();
    assert!(codex_skill_md.contains("$sample-skill"), "SKILL.md is left as written for Codex");

    assert!(
        world.tmux_calls().iter().any(|c| c.contains("OAT_MODEL=sample-codex-model")),
        "the role's declared Codex model reaches the launch"
    );
    assert!(
        world.tmux_calls().iter().any(|c| c.contains("OAT_EFFORT=high")),
        "the role's declared Codex reasoning effort reaches the launch"
    );

    // Spec acceptance criterion 5: the preamble and the role protocol precede the plugin's own
    // instructions in the assembled prompt.
    let preamble_at = claude_prompt.find("# Launch").expect("the preamble is present");
    let protocol_at = claude_prompt.find("# Role protocol").expect("the role protocol is present");
    let instructions_at = claude_prompt
        .find("# Sample role (fresh)")
        .expect("the plugin's own role instructions are present");
    assert!(preamble_at < protocol_at && protocol_at < instructions_at);

    // Each start location is enforced at launch: `sample-role-fresh` starts fresh and refuses
    // `--from`; `sample-role-existing` starts in a named worktree and requires it.
    let fresh_with_from = parse(&[
        "role", "fire", "sample-role-fresh", "--from", &meta_worktree, "--prompt", "do work",
    ]);
    let err = execute(fresh_with_from, &env_with_run, &catalog).unwrap_err();
    assert_eq!(oat_agents::error::to_failure(&err).code, oat_agents::error::codes::INVALID_ROLE_OPTION);

    let existing_without_from = parse(&["role", "fire", "sample-role-existing", "--prompt", "do work"]);
    let err = execute(existing_without_from, &env_with_run, &catalog).unwrap_err();
    assert_eq!(oat_agents::error::to_failure(&err).code, oat_agents::error::codes::ROLE_SOURCE_REQUIRED);

    let existing_with_from = parse(&[
        "role", "fire", "sample-role-existing", "--from", &meta_worktree, "--prompt", "do work",
    ]);
    let existing_result = run_json(existing_with_from, &env_with_run, &catalog);
    assert_eq!(existing_result["worktree"].as_str().unwrap(), meta_worktree);
}

#[cfg(unix)]
fn assert_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(path).unwrap().permissions().mode();
    assert_eq!(mode & 0o111, 0o111, "the skill's script keeps its executable bit: {path:?}");
}

#[cfg(not(unix))]
fn assert_executable(_path: &Path) {}

// ---- Validation errors: every one names the plugin, the file and the problem. ----

fn write_manifest(dir: &Path, name: &str) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(
        dir.join("oat-plugin.toml"),
        format!("format_version = 1\nname = \"{name}\"\ndescription = \"a test plugin\"\n"),
    )
    .unwrap();
}

fn write_role(dir: &Path, role: &str, start: &str, instructions: &str) {
    let role_dir = dir.join("roles").join(role);
    std::fs::create_dir_all(&role_dir).unwrap();
    std::fs::write(role_dir.join("role.toml"), format!("description = \"a test role\"\nstart = \"{start}\"\n")).unwrap();
    std::fs::write(role_dir.join("instructions.md"), instructions).unwrap();
}

fn write_core_instruction(dir: &Path, slug: &str, text: &str) {
    let core_dir = dir.join("core");
    std::fs::create_dir_all(&core_dir).unwrap();
    std::fs::write(core_dir.join(format!("{slug}-instruction.md")), text).unwrap();
}

fn errors_text(errors: &[oat_agents::plugin::PluginError]) -> String {
    errors.iter().map(|e| e.to_string()).collect::<Vec<_>>().join("; ")
}

#[test]
fn two_plugins_defining_the_same_role_name_fail_naming_both() {
    let tmp = tempfile::tempdir().unwrap();
    let a = tmp.path().join("plugin-a");
    let b = tmp.path().join("plugin-b");

    write_manifest(&a, "plugin-a");
    write_role(&a, "dup", "fresh", "Do the work.");
    write_core_instruction(&a, "oat-meta", "Coordinate.");
    write_core_instruction(&a, "oat-console", "Observe.");

    write_manifest(&b, "plugin-b");
    write_role(&b, "dup", "fresh", "Do the work differently.");

    let errors = load_catalog(&[a, b]).unwrap_err();
    let text = errors_text(&errors);
    assert!(text.contains("dup"), "{text}");
    assert!(text.contains("plugin-a"), "{text}");
    assert!(text.contains("plugin-b"), "{text}");
}

#[test]
fn a_plugin_set_supplying_no_core_instructions_fails_naming_the_missing_files() {
    let tmp = tempfile::tempdir().unwrap();
    let a = tmp.path().join("plugin-a");
    write_manifest(&a, "plugin-a");
    write_role(&a, "worker", "fresh", "Do the work.");

    let errors = load_catalog(&[a]).unwrap_err();
    let text = errors_text(&errors);
    assert!(text.contains("core/oat-meta-instruction.md"), "{text}");
    assert!(text.contains("core/oat-console-instruction.md"), "{text}");
}

#[test]
fn a_bogus_core_file_for_a_role_that_does_not_exist_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let a = tmp.path().join("plugin-a");
    write_manifest(&a, "plugin-a");
    write_core_instruction(&a, "oat-meta", "Coordinate.");
    write_core_instruction(&a, "oat-console", "Observe.");
    std::fs::write(a.join("core").join("oat-planner-instruction.md"), "Plan.").unwrap();

    let errors = load::load_plugin(&a).unwrap_err();
    let text = errors_text(&errors);
    assert!(
        text.contains("oat-planner-instruction.md"),
        "the bogus core/ file is named: {text}"
    );
}

#[test]
fn a_malformed_manifest_names_the_file() {
    let tmp = tempfile::tempdir().unwrap();
    let a = tmp.path().join("plugin-a");
    std::fs::create_dir_all(&a).unwrap();
    std::fs::write(a.join("oat-plugin.toml"), "this is not valid toml =====").unwrap();

    let errors = load::load_plugin(&a).unwrap_err();
    let text = errors_text(&errors);
    assert!(text.contains("oat-plugin.toml"), "{text}");
}

#[test]
fn a_missing_instruction_file_names_the_file() {
    let tmp = tempfile::tempdir().unwrap();
    let a = tmp.path().join("plugin-a");
    write_manifest(&a, "plugin-a");
    let role_dir = a.join("roles/lonely");
    std::fs::create_dir_all(&role_dir).unwrap();
    std::fs::write(role_dir.join("role.toml"), "description = \"x\"\nstart = \"fresh\"\n").unwrap();
    // instructions.md is deliberately missing.

    let errors = load::load_plugin(&a).unwrap_err();
    let text = errors_text(&errors);
    assert!(text.contains("roles/lonely/instructions.md"), "{text}");
}

#[test]
fn an_unknown_field_names_the_file() {
    let tmp = tempfile::tempdir().unwrap();
    let a = tmp.path().join("plugin-a");
    std::fs::create_dir_all(&a).unwrap();
    std::fs::write(
        a.join("oat-plugin.toml"),
        "format_version = 1\nname = \"plugin-a\"\ndescription = \"x\"\nnickname = \"nope\"\n",
    )
    .unwrap();

    let errors = load::load_plugin(&a).unwrap_err();
    let text = errors_text(&errors);
    assert!(text.contains("oat-plugin.toml"), "{text}");
    assert!(text.contains("nickname"), "the unknown field is named: {text}");
}

#[test]
fn only_the_console_may_choose_its_backend_and_only_a_known_one() {
    let tmp = tempfile::tempdir().unwrap();
    let a = tmp.path().join("plugin-a");
    write_manifest(&a, "plugin-a");
    write_core_instruction(&a, "oat-meta", "Coordinate.");
    write_core_instruction(&a, "oat-console", "Observe.");
    std::fs::write(a.join("core/oat-meta.toml"), "backend = \"codex\"\n").unwrap();
    std::fs::write(a.join("core/oat-console.toml"), "backend = \"gemini\"\n").unwrap();

    let text = errors_text(&load::load_plugin(&a).unwrap_err());
    assert!(text.contains("core/oat-meta.toml sets `backend`"), "{text}");
    assert!(text.contains("unknown backend 'gemini'"), "{text}");

    std::fs::write(a.join("core/oat-meta.toml"), "").unwrap();
    std::fs::write(a.join("core/oat-console.toml"), "backend = \"codex\"\n").unwrap();
    let loaded = load::load_plugin(&a).unwrap();
    assert_eq!(
        loaded.console.unwrap().backend,
        Some(oat_agents::role::Backend::Codex)
    );
}

#[test]
fn only_oat_meta_may_set_the_rework_bounds_and_only_sane_ones() {
    let tmp = tempfile::tempdir().unwrap();
    let a = tmp.path().join("plugin-a");
    write_manifest(&a, "plugin-a");
    write_core_instruction(&a, "oat-meta", "Coordinate.");
    write_core_instruction(&a, "oat-console", "Observe.");
    std::fs::write(a.join("core/oat-console.toml"), "[rework]\nmax_rounds = 2\n").unwrap();
    std::fs::write(a.join("core/oat-meta.toml"), "[rework]\nmax_rounds = 0\n").unwrap();

    let text = errors_text(&load::load_plugin(&a).unwrap_err());
    assert!(text.contains("core/oat-console.toml sets `[rework]`"), "{text}");
    assert!(text.contains("max_rounds = 0; a work gets at least one review round"), "{text}");

    std::fs::write(a.join("core/oat-console.toml"), "").unwrap();
    std::fs::write(a.join("core/oat-meta.toml"), "[rework]\nmax_rounds = 2\n").unwrap();
    let loaded = load::load_plugin(&a).unwrap();
    assert_eq!(
        loaded.meta.unwrap().rework,
        oat_agents::findings::ReworkSettings { max_rounds: 2, ..Default::default() }
    );
}
