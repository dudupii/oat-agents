// SPDX-License-Identifier: MIT
// Copyright (c) 2026 oat-agents contributors

mod common;

use clap::Parser;
use common::{PluginBuilder, TempRepo, TestWorld};
use oat_agents::error::{codes, to_failure};
use oat_agents::role::memory::InMemoryCatalogBuilder;
use oat_agents::{execute, Cli};
use serde_json::Value;
use std::path::Path;

fn parse(args: &[&str]) -> Cli {
    let mut full = vec!["oat-agents"];
    full.extend_from_slice(args);
    Cli::try_parse_from(full).unwrap()
}

fn run(cli: Cli, env: &dyn oat_agents::environment::Environment) -> anyhow::Result<Value> {
    execute(cli, env, &InMemoryCatalogBuilder::new().build())
}

/// A plugin whose `worker` declares a model on each backend.
fn install_plugin(world: &TestWorld, repo: &Path) {
    PluginBuilder::new(repo, "fixture-plugin", "fixture-plugin")
        .role("worker", "fresh", "Throw the pots.")
        .role_toml(
            "worker",
            "[model.claude]\nmodel = \"plugin-claude\"\n\
             [model.codex]\nmodel = \"plugin-codex\"\nreasoning_effort = \"medium\"\n",
        )
        .core("oat-meta", "Coordinate the pottery workshop.")
        .core("oat-console", "Observe the pottery workshop.")
        .finish(world, repo, "fixture-plugin", "fixture-plugin");
}

fn meta_fire(world: &TestWorld, repo: &Path, extra: &[&str]) -> anyhow::Result<Value> {
    let repo = repo.to_string_lossy();
    let mut args = vec!["meta", "fire", "--prompt", "plan", "--repo", &repo, "--name", "kiln"];
    args.extend_from_slice(extra);
    run(parse(&args), &world.env())
}

fn fire_worker(world: &TestWorld, name: &str, extra: &[&str]) -> Value {
    let env = world.env().with_var("OAT_RUN_ID", "kiln");
    let mut args = vec!["role", "fire", "worker", "--name", name, "--prompt", "throw a pot"];
    args.extend_from_slice(extra);
    run(parse(&args), &env).unwrap()
}

fn launched_with(world: &TestWorld, var: &str) -> bool {
    world.tmux_calls().iter().any(|call| call.contains(var))
}

#[test]
fn the_repository_chooses_a_roles_backend_and_model_over_the_plugin_and_the_run() {
    let repo = TempRepo::new();
    let world = TestWorld::new();
    install_plugin(&world, &repo.path());
    std::fs::write(
        repo.path().join(".oat/roles.toml"),
        "[worker]\nbackend = \"codex\"\n[worker.model.codex]\nreasoning_effort = \"high\"\n",
    )
    .unwrap();
    let fired = meta_fire(&world, &repo.path(), &["--agent", "claude"]).unwrap();
    assert_eq!(fired["backend"], "claude");
    assert_eq!(fired["role_settings"]["worker"]["backend"], "codex", "{fired}");

    let worker = fire_worker(&world, "bowl", &[]);
    assert_eq!(worker["backend"], "codex", "{worker}");
    assert!(launched_with(&world, "OAT_MODEL=plugin-codex"), "the plugin's model where the repository sets none");
    assert!(launched_with(&world, "OAT_EFFORT=high"), "the repository's effort replaces the plugin's");

    let explicit = fire_worker(&world, "vase", &["--agent", "claude"]);
    assert_eq!(explicit["backend"], "claude", "--agent on one launch wins: {explicit}");
    assert!(launched_with(&world, "OAT_MODEL=plugin-claude"));
}

#[test]
fn the_repository_chooses_the_meta_agents_backend_and_model_when_meta_fire_names_none() {
    let repo = TempRepo::new();
    let world = TestWorld::new();
    install_plugin(&world, &repo.path());
    std::fs::write(
        repo.path().join(".oat/roles.toml"),
        "[oat-meta]\nbackend = \"codex\"\n[oat-meta.model.codex]\nmodel = \"repo-codex\"\n",
    )
    .unwrap();
    let fired = meta_fire(&world, &repo.path(), &[]).unwrap();
    assert_eq!(fired["backend"], "codex", "{fired}");
    assert!(launched_with(&world, "OAT_MODEL=repo-codex"));

    let worker = fire_worker(&world, "bowl", &[]);
    assert_eq!(worker["backend"], "codex", "a role the repository leaves alone runs on the Run's backend");
}

#[test]
fn an_unknown_backend_in_roles_toml_refuses_the_run() {
    let repo = TempRepo::new();
    let world = TestWorld::new();
    install_plugin(&world, &repo.path());
    std::fs::write(repo.path().join(".oat/roles.toml"), "[worker]\nbackend = \"gemini\"\n").unwrap();
    let error = meta_fire(&world, &repo.path(), &[]).unwrap_err();
    assert_eq!(to_failure(&error).code, codes::ROLE_SETTINGS_INVALID);
}

#[test]
fn backend_arguments_from_the_environment_reach_the_launched_session() {
    let repo = TempRepo::new();
    let world = TestWorld::new();
    install_plugin(&world, &repo.path());

    // The launch scripts expand ${OAT_CODEX_ARGS:-} unquoted, so one variable carries the flags
    // an unattended backend needs (issue #9: --disable api_key_model_discovery) — but only if
    // the launch forwards it into the session, which until now it did not.
    let env = world
        .env()
        .with_var("OAT_CODEX_ARGS", "--disable api_key_model_discovery")
        .with_var("OAT_CLAUDE_ARGS", "--verbose");
    run(
        parse(&[
            "meta", "fire", "--prompt", "plan", "--repo", &repo.path().to_string_lossy(),
            "--name", "kiln", "--agent", "codex",
        ]),
        &env,
    )
    .unwrap();
    run(
        parse(&["role", "fire", "worker", "--name", "bowl", "--prompt", "throw a pot", "--agent", "codex"]),
        &env.with_var("OAT_RUN_ID", "kiln"),
    )
    .unwrap();

    assert!(launched_with(&world, "OAT_CODEX_ARGS=--disable api_key_model_discovery"));
    assert!(launched_with(&world, "OAT_CLAUDE_ARGS=--verbose"));
    let calls = world.tmux_calls();
    let sessions: Vec<&String> = calls.iter().filter(|call| call.contains("new-session")).collect();
    assert_eq!(sessions.len(), 2, "one session for the meta-agent, one for the member");
    for call in &sessions {
        assert!(
            call.contains("OAT_CODEX_ARGS=--disable api_key_model_discovery")
                && call.contains("OAT_CLAUDE_ARGS=--verbose"),
            "every launched session carries the backend arguments: {call}"
        );
    }
}
