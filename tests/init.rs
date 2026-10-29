// SPDX-License-Identifier: MIT
// Copyright (c) 2026 oat-agents contributors

//! `oat-agents init [--local]` (issue's own acceptance criteria): never overwrites an existing
//! configuration without saying so, `init --local` leaves `git status` clean, and plain `init`
//! writes `.oat/plugins.toml`.

mod common;

use clap::Parser;
use common::TempRepo;
use oat_agents::environment::TestEnvironment;
use oat_agents::error::{codes, to_failure};
use oat_agents::role::memory::InMemoryCatalogBuilder;
use oat_agents::{execute, Cli};

fn parse(args: &[&str]) -> Cli {
    let mut full = vec!["oat-agents"];
    full.extend_from_slice(args);
    Cli::try_parse_from(full).unwrap()
}

fn run(cli: Cli, env: &dyn oat_agents::environment::Environment) -> anyhow::Result<serde_json::Value> {
    let catalog = InMemoryCatalogBuilder::new().build();
    execute(cli, env, &catalog)
}

fn git_status_is_clean(repo: &std::path::Path) -> bool {
    let output = std::process::Command::new("git").current_dir(repo).args(["status", "--porcelain"]).output().unwrap();
    output.status.success() && output.stdout.is_empty()
}

#[test]
fn init_without_local_writes_dot_oat_plugins_toml() {
    let repo = TempRepo::new();
    let home = tempfile::tempdir().unwrap();
    let env = TestEnvironment::new(home.path().to_path_buf());

    let cli = parse(&["init", "--repo", &repo.path().to_string_lossy()]);
    let result = run(cli, &env).unwrap();

    let config_path = repo.path().join(".oat/plugins.toml");
    assert!(config_path.is_file());
    assert_eq!(result["config"], config_path.to_string_lossy().to_string());
    assert_eq!(result["local"], false);

    let contents = std::fs::read_to_string(&config_path).unwrap();
    assert!(contents.contains("source = \"embedded\""), "{contents}");
}

#[test]
fn init_local_leaves_git_status_clean() {
    let repo = TempRepo::new();
    let home = tempfile::tempdir().unwrap();
    let env = TestEnvironment::new(home.path().to_path_buf());

    let cli = parse(&["init", "--local", "--repo", &repo.path().to_string_lossy()]);
    let result = run(cli, &env).unwrap();
    assert_eq!(result["local"], true);

    assert!(git_status_is_clean(&repo.path()), "init --local must not create a tracked or untracked file");

    let config_path = repo.path().join(".git/oat/plugins.toml");
    assert!(config_path.is_file(), "the local config lives under .git/oat/plugins.toml");
}

#[test]
fn init_never_overwrites_an_existing_configuration_without_force() {
    let repo = TempRepo::new();
    let home = tempfile::tempdir().unwrap();
    let env = TestEnvironment::new(home.path().to_path_buf());

    run(parse(&["init", "--repo", &repo.path().to_string_lossy()]), &env).unwrap();

    let error = run(parse(&["init", "--repo", &repo.path().to_string_lossy()]), &env).unwrap_err();
    assert_eq!(to_failure(&error).code, codes::PLUGIN_ALREADY_INITIALISED);

    // --force overwrites it.
    let result = run(
        parse(&["init", "--repo", &repo.path().to_string_lossy(), "--force", "--embedded"]),
        &env,
    )
    .unwrap();
    assert_eq!(result["plugins"].as_array().unwrap().len(), 1);
}

#[test]
fn init_with_a_path_plugin_pins_its_current_content_hash() {
    let repo = TempRepo::new();
    let home = tempfile::tempdir().unwrap();
    let env = TestEnvironment::new(home.path().to_path_buf());

    let plugin_dir = repo.path().join("local-plugin");
    std::fs::create_dir_all(&plugin_dir).unwrap();
    std::fs::write(
        plugin_dir.join("oat-plugin.toml"),
        "format_version = 1\nname = \"local-plugin\"\ndescription = \"d\"\n",
    )
    .unwrap();

    let expected_hash = oat_agents::hashing::hash_dir(&plugin_dir).unwrap();

    let cli = parse(&[
        "init", "--repo", &repo.path().to_string_lossy(), "--path", "local-plugin", "local-plugin",
    ]);
    let result = run(cli, &env).unwrap();
    assert_eq!(result["plugins"][0]["content_hash"], expected_hash);
}

#[test]
fn a_stale_same_version_embedded_copy_is_not_served_to_new_runs() {
    let repo = TempRepo::new();
    let world = common::TestWorld::new();
    let env = world.env();

    // A copy some earlier binary materialized under the version key — same version as this
    // build, content this build no longer embeds. Found in the field: a dev build's plugin
    // changes never reached any Run because this directory kept being served as the plugin.
    let stale = world.home.join(".oat/plugins/embedded/0.1.0");
    std::fs::create_dir_all(stale.join("roles/reviewer")).unwrap();
    std::fs::write(
        stale.join("oat-plugin.toml"),
        "format_version = 1\nname = \"example\"\ndescription = \"stale\"\n",
    )
    .unwrap();
    std::fs::write(stale.join("roles/reviewer/instructions.md"), "# STALE reviewer\n").unwrap();

    run(parse(&["init", "--repo", &repo.path().to_string_lossy()]), &env).unwrap();

    let mut store = oat_agents::trust::TrustStore::open(&env).unwrap();
    store.grant(
        oat_agents::trust::TrustKey::Embedded {
            name: "example".to_string(),
            version: oat_agents::plugins::embedded::example_version().to_string(),
        },
        "2026-01-01T00:00:00Z".to_string(),
    )
    .unwrap();

    run(
        parse(&[
            "meta", "fire", "--prompt", "plan", "--repo", &repo.path().to_string_lossy(),
            "--name", "embedded-fresh", "--agent", "claude",
        ]),
        &env,
    )
    .unwrap();

    let snapshot = std::fs::read_to_string(
        world.scratch.path().join("state/runs/embedded-fresh/plugins/000-example/roles/reviewer/instructions.md"),
    )
    .unwrap();
    assert_eq!(
        snapshot,
        std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("plugins/example/roles/reviewer/instructions.md")
        )
        .unwrap(),
        "the Run's snapshot carries what this build embeds, not the stale copy"
    );
}

#[test]
fn init_also_scaffolds_an_execution_profile_when_asked() {
    let repo = TempRepo::new();
    let home = tempfile::tempdir().unwrap();
    let env = TestEnvironment::new(home.path().to_path_buf());

    let cli = parse(&[
        "init",
        "--repo",
        &repo.path().to_string_lossy(),
        "--exec-profile",
        "local",
        "--exec-context",
        "dev",
        "--exec-namespace",
        "agents",
    ]);
    run(cli, &env).unwrap();

    let repo_exec = std::fs::read_to_string(repo.path().join(".oat/exec.toml")).unwrap();
    assert!(repo_exec.contains("default_profile = \"local\""));
    let machine_exec = std::fs::read_to_string(home.path().join(".oat/exec.toml")).unwrap();
    assert!(machine_exec.contains("context = \"dev\""));
}
