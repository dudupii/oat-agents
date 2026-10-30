// SPDX-License-Identifier: MIT
// Copyright (c) 2026 oat-agents contributors

mod common;

use common::{TempRepo, TestWorld};
use clap::Parser;
use oat_agents::env::integration::ExecEnvironments;
use oat_agents::env::EnvRecord;
use oat_agents::environment::Environment;
use oat_agents::event_log::EventLog;
use oat_agents::role::memory::InMemoryCatalogBuilder;
use oat_agents::role::{CoreRole, CoreRoleDefinition, RoleDefinition, StartLocation};
use oat_agents::{execute_with_exec, Cli};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// A stand-in for a real cluster: hands back a fixed environment record and a fixed ledger
/// report, so `role fire`'s wiring can be tested without Kubernetes.
struct FakeExec {
    record: EnvRecord,
    reusable: serde_json::Value,
    calls: Mutex<Vec<String>>,
}

impl FakeExec {
    fn new() -> Self {
        Self {
            record: EnvRecord {
                env_id: "oat-fake-worker-1".to_string(),
                run_id: Some("run".to_string()),
                role: "worker".to_string(),
                profile: "local".to_string(),
                kind: oat_agents::env::config::ProviderKind::Kubernetes,
                worktree: PathBuf::from("/w"),
                container_path: PathBuf::from("/w"),
                image_ref: "oat-w:abc".to_string(),
                image_id: "sha256:fakeimage".to_string(),
                pod: "oat-fake-worker-1".to_string(),
                namespace: "agents".to_string(),
                context: "local".to_string(),
                created_ms: 1,
            },
            reusable: serde_json::json!({"entries": [], "note": "fake"}),
            calls: Mutex::new(Vec::new()),
        }
    }
}

impl ExecEnvironments for FakeExec {
    fn ensure(
        &self,
        _environment: &dyn Environment,
        _log: &EventLog,
        _run_id: &str,
        _profile: &str,
        role: &str,
        _worktree: &Path,
    ) -> anyhow::Result<EnvRecord> {
        self.calls.lock().unwrap().push(format!("ensure:{role}"));
        Ok(self.record.clone())
    }

    fn reusable(&self, _environment: &dyn Environment, _worktree: &Path, _against: &EnvRecord) -> serde_json::Value {
        self.reusable.clone()
    }

    fn remove_run(&self, _environment: &dyn Environment, _log: &EventLog, run_id: &str) -> serde_json::Value {
        self.calls.lock().unwrap().push(format!("remove_run:{run_id}"));
        serde_json::json!({"removed": [self.record.env_id], "failed": []})
    }
}

fn write_exec_profile(repo: &Path, home: &Path) {
    std::fs::create_dir_all(repo.join(".oat")).unwrap();
    std::fs::write(repo.join(".oat/exec.toml"), "default_profile = \"local\"\n").unwrap();
    std::fs::create_dir_all(home.join(".oat")).unwrap();
    std::fs::write(
        home.join(".oat/exec.toml"),
        "[profile.local]\ncontext = \"test-context\"\nnamespace = \"test-namespace\"\n",
    )
    .unwrap();
}

fn write_exec_profile_with_kubeconfig(repo: &Path, home: &Path, kubeconfig: &Path) {
    std::fs::create_dir_all(repo.join(".oat")).unwrap();
    std::fs::write(repo.join(".oat/exec.toml"), "default_profile = \"local\"\n").unwrap();
    std::fs::create_dir_all(home.join(".oat")).unwrap();
    std::fs::write(
        home.join(".oat/exec.toml"),
        format!(
            "[profile.local]\ncontext = \"test-context\"\nnamespace = \"test-namespace\"\nkubeconfig = {:?}\n",
            kubeconfig
        ),
    )
    .unwrap();
}

/// A profile whose `kind` is docker. `context`/`namespace` stay in the table — the shared
/// shape requires them — but nothing docker-side reads them.
fn write_docker_exec_profile(repo: &Path, home: &Path) {
    std::fs::create_dir_all(repo.join(".oat")).unwrap();
    std::fs::write(repo.join(".oat/exec.toml"), "default_profile = \"local\"\n").unwrap();
    std::fs::create_dir_all(home.join(".oat")).unwrap();
    std::fs::write(
        home.join(".oat/exec.toml"),
        "[profile.local]\nkind = \"docker\"\ncontext = \"docker\"\nnamespace = \"docker\"\n",
    )
    .unwrap();
}

/// A registered environment the docker backend placed. Docker itself is absent on purpose in
/// every test that uses it: what is under test is which backend a record reaches, and what it
/// says when docker cannot be asked.
fn docker_record() -> EnvRecord {
    EnvRecord {
        env_id: "oat-docker-worker-1".to_string(),
        run_id: Some("run".to_string()),
        role: "worker".to_string(),
        profile: "local".to_string(),
        kind: oat_agents::env::config::ProviderKind::Docker,
        worktree: PathBuf::from("/"),
        container_path: PathBuf::from("/"),
        image_ref: "oat-w:abc".to_string(),
        image_id: "sha256:fakeimage".to_string(),
        pod: "oat-docker-worker-1".to_string(),
        namespace: "docker".to_string(),
        context: "docker".to_string(),
        created_ms: 1,
    }
}

fn catalog_with(exec_environment: bool, prior_verification: bool) -> oat_agents::role::memory::InMemoryCatalog {
    InMemoryCatalogBuilder::new()
        .with_role(RoleDefinition {
            name: "worker".to_string(),
            instructions: "Write the pottery this task asks for.".to_string(),
            models: BTreeMap::new(),
            skills: Vec::new(),
            start: StartLocation::Fresh,
            exec_environment,
            prior_verification,
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

/// The Run's plugin (F5's gate) must itself carry the `exec_environment`/`prior_verification`
/// flags each test exercises: `role fire` now rebuilds its catalog from the Run's plugin
/// snapshot, not from the `catalog_with(...)` fixture passed alongside it.
fn fire_run(world: &TestWorld, repo: &Path, exec_environment: bool, prior_verification: bool) -> String {
    fire_run_reporting(world, repo, exec_environment, prior_verification);
    "run-a".to_string()
}

fn fire_run_reporting(world: &TestWorld, repo: &Path, exec_environment: bool, prior_verification: bool) -> serde_json::Value {
    common::PluginBuilder::new(repo, "fixture-plugin", "fixture-plugin")
        .role_with(
            "worker",
            "fresh",
            "Write the pottery this task asks for.",
            &[],
            exec_environment,
            prior_verification,
        )
        .with_default_core()
        .finish(world, repo, "fixture-plugin", "fixture-plugin");
    let env = world.env();
    let catalog = catalog_with(exec_environment, prior_verification);
    let fake = FakeExec::new();
    let cli = parse(&[
        "meta", "fire", "--prompt", "plan", "--repo", &repo.to_string_lossy(), "--name", "run-a", "--agent", "claude",
    ]);
    execute_with_exec(cli, &env, &catalog, &fake).unwrap()
}

fn run_events(world: &TestWorld, run_id: &str) -> Vec<oat_agents::event_log::LogEntry> {
    EventLog::open(&world.env()).read_run(run_id).unwrap()
}

#[test]
fn exec_environment_false_gets_no_environment_even_when_the_run_has_a_profile() {
    let repo = TempRepo::new();
    let world = TestWorld::new();
    write_exec_profile(&repo.path(), &world.home);
    let run_id = fire_run(&world, &repo.path(), false, false);

    let catalog = catalog_with(false, false);
    let fake = FakeExec::new();
    let env_with_run = world.env().with_var("OAT_RUN_ID", &run_id);
    let role_fire = parse(&["role", "fire", "worker", "--name", "w1", "--prompt", "do work"]);
    let result = execute_with_exec(role_fire, &env_with_run, &catalog, &fake).unwrap();

    assert!(fake.calls.lock().unwrap().is_empty(), "an opted-out role never reaches the provider");
    let dispatch_dir = PathBuf::from(result["dispatch_dir"].as_str().unwrap());
    let prompt = std::fs::read_to_string(dispatch_dir.join("prompt.md")).unwrap();
    assert!(!prompt.contains("<execution-environment>"), "no environment was bound: {prompt}");
}

#[test]
fn exec_environment_true_gets_one_when_the_run_has_a_profile() {
    let repo = TempRepo::new();
    let world = TestWorld::new();
    write_exec_profile(&repo.path(), &world.home);
    let run_id = fire_run(&world, &repo.path(), true, false);

    let catalog = catalog_with(true, false);
    let fake = FakeExec::new();
    let env_with_run = world.env().with_var("OAT_RUN_ID", &run_id);
    let role_fire = parse(&["role", "fire", "worker", "--name", "w1", "--prompt", "do work"]);
    let result = execute_with_exec(role_fire, &env_with_run, &catalog, &fake).unwrap();

    assert_eq!(fake.calls.lock().unwrap().as_slice(), ["ensure:worker"]);
    let dispatch_dir = PathBuf::from(result["dispatch_dir"].as_str().unwrap());
    let prompt = std::fs::read_to_string(dispatch_dir.join("prompt.md")).unwrap();
    assert!(prompt.contains("<execution-environment>"), "{prompt}");
    assert!(prompt.contains("sha256:fakeimage"), "{prompt}");
    assert!(!prompt.contains("Prior verification"), "prior_verification is false: {prompt}");
}

#[test]
fn prior_verification_true_carries_the_ledgers_reusable_entries_and_false_does_not() {
    let repo = TempRepo::new();
    let world = TestWorld::new();
    write_exec_profile(&repo.path(), &world.home);
    let run_id = fire_run(&world, &repo.path(), true, true);

    let catalog = catalog_with(true, true);
    let fake = FakeExec::new();
    let env_with_run = world.env().with_var("OAT_RUN_ID", &run_id);
    let role_fire = parse(&["role", "fire", "worker", "--name", "w1", "--prompt", "do work"]);
    let result = execute_with_exec(role_fire, &env_with_run, &catalog, &fake).unwrap();
    let dispatch_dir = PathBuf::from(result["dispatch_dir"].as_str().unwrap());
    let prompt = std::fs::read_to_string(dispatch_dir.join("prompt.md")).unwrap();
    assert!(prompt.contains("Prior verification"), "{prompt}");
    assert!(prompt.contains("\"note\": \"fake\""), "the ledger's reusable entries are in the prompt: {prompt}");
}

#[test]
fn exec_environment_false_never_gets_prior_verification_either() {
    let repo = TempRepo::new();
    let world = TestWorld::new();
    write_exec_profile(&repo.path(), &world.home);
    let run_id = fire_run(&world, &repo.path(), false, true);

    let catalog = catalog_with(false, true);
    let fake = FakeExec::new();
    let env_with_run = world.env().with_var("OAT_RUN_ID", &run_id);
    let role_fire = parse(&["role", "fire", "worker", "--name", "w1", "--prompt", "do work"]);
    let result = execute_with_exec(role_fire, &env_with_run, &catalog, &fake).unwrap();
    let dispatch_dir = PathBuf::from(result["dispatch_dir"].as_str().unwrap());
    let prompt = std::fs::read_to_string(dispatch_dir.join("prompt.md")).unwrap();
    assert!(!prompt.contains("Prior verification"), "{prompt}");
    assert!(fake.calls.lock().unwrap().is_empty());
}

#[test]
fn meta_finish_removes_the_runs_environments_and_reports_them() {
    let repo = TempRepo::new();
    let world = TestWorld::new();
    write_exec_profile(&repo.path(), &world.home);
    let run_id = fire_run(&world, &repo.path(), true, false);

    let fake = FakeExec::new();
    let finish = parse(&["meta", "finish", "--run", &run_id]);
    let receipt = execute_with_exec(finish, &world.env(), &catalog_with(true, false), &fake).unwrap();

    assert_eq!(fake.calls.lock().unwrap().as_slice(), [format!("remove_run:{run_id}")]);
    assert_eq!(receipt["environments"]["removed"], serde_json::json!(["oat-fake-worker-1"]), "{receipt}");
}

#[test]
fn meta_fire_reports_the_profile_it_selected_and_logs_it() {
    let repo = TempRepo::new();
    let world = TestWorld::new();
    write_exec_profile(&repo.path(), &world.home);
    let result = fire_run_reporting(&world, &repo.path(), true, false);

    assert_eq!(result["exec"]["profile"], "local", "{result}");
    assert_eq!(result["exec"]["source"], "default_profile", "{result}");
    assert!(result["exec"]["warning"].is_null(), "{result}");
    let events = run_events(&world, "run-a");
    let selected = events.iter().find(|e| e.event == "exec_profile_selected").expect("logged");
    assert_eq!(selected.details.as_ref().unwrap()["profile"], "local");
}

#[test]
fn a_run_with_no_profile_says_so_at_meta_fire_and_at_every_role_that_wanted_one() {
    let repo = TempRepo::new();
    let world = TestWorld::new();
    let result = fire_run_reporting(&world, &repo.path(), true, false);

    assert!(result["exec"]["profile"].is_null(), "{result}");
    assert_eq!(result["exec"]["roles_wanting_environment"], serde_json::json!(["worker"]));
    let warning = result["exec"]["warning"].as_str().expect("a warning when a role wants a pod");
    assert!(warning.contains("worker") && warning.contains("--exec-profile"), "{warning}");
    assert!(run_events(&world, "run-a").iter().any(|e| e.event == "exec_profile_none"));

    let catalog = catalog_with(true, false);
    let fake = FakeExec::new();
    let env_with_run = world.env().with_var("OAT_RUN_ID", "run-a");
    let role_fire = parse(&["role", "fire", "worker", "--name", "w1", "--prompt", "do work"]);
    let fired = execute_with_exec(role_fire, &env_with_run, &catalog, &fake).unwrap();

    assert!(fake.calls.lock().unwrap().is_empty());
    assert_eq!(fired["exec"]["environment"], "host", "{fired}");
    assert!(fired["exec"]["warning"].as_str().is_some_and(|w| w.contains("no execution profile")), "{fired}");
    let prompt = std::fs::read_to_string(PathBuf::from(fired["dispatch_dir"].as_str().unwrap()).join("prompt.md")).unwrap();
    assert!(prompt.contains("<execution-environment>\nnone:"), "the role is told it has no pod: {prompt}");
    let skipped = run_events(&world, "run-a").into_iter().find(|e| e.event == "env_skipped").expect("logged");
    assert_eq!(skipped.dispatch_id.as_deref(), fired["dispatch_id"].as_str());
    let store = oat_agents::store::Store::open(&world.env()).unwrap();
    let dispatch = store.load_dispatch("run-a", fired["dispatch_id"].as_str().unwrap()).unwrap();
    assert!(dispatch.env_skipped.is_some());
}

#[test]
fn no_exec_is_deliberate_and_carries_no_warning() {
    let repo = TempRepo::new();
    let world = TestWorld::new();
    write_exec_profile(&repo.path(), &world.home);
    common::PluginBuilder::new(&repo.path(), "fixture-plugin", "fixture-plugin")
        .role_with("worker", "fresh", "Work.", &[], true, false)
        .with_default_core()
        .finish(&world, &repo.path(), "fixture-plugin", "fixture-plugin");
    let cli = parse(&[
        "meta", "fire", "--prompt", "plan", "--repo", &repo.path().to_string_lossy(), "--name", "run-a", "--no-exec",
    ]);
    let result = execute_with_exec(cli, &world.env(), &catalog_with(true, false), &FakeExec::new()).unwrap();
    assert!(result["exec"]["profile"].is_null(), "{result}");
    assert_eq!(result["exec"]["source"], "--no-exec");
    assert!(result["exec"]["warning"].is_null(), "{result}");
}

#[test]
fn role_fire_reports_the_pod_it_bound() {
    let repo = TempRepo::new();
    let world = TestWorld::new();
    write_exec_profile(&repo.path(), &world.home);
    let run_id = fire_run(&world, &repo.path(), true, false);
    let env_with_run = world.env().with_var("OAT_RUN_ID", &run_id);
    let role_fire = parse(&["role", "fire", "worker", "--name", "w1", "--prompt", "do work"]);
    let fired = execute_with_exec(role_fire, &env_with_run, &catalog_with(true, false), &FakeExec::new()).unwrap();
    assert_eq!(fired["exec"]["environment"], "pod", "{fired}");
    assert_eq!(fired["exec"]["env_id"], "oat-fake-worker-1");
    assert_eq!(fired["exec"]["image_id"], "sha256:fakeimage");
}

#[test]
fn env_doctor_reports_what_is_missing_on_a_machine_with_no_cluster() {
    // No real kubeconfig can be reached from a test process: point the profile's own
    // `kubeconfig` field at a path that does not exist, so the report is deterministic wherever
    // this runs. Mutating the process-wide `KUBECONFIG` variable would race other tests running
    // in parallel threads in this same binary.
    let directory = tempfile::tempdir().unwrap();
    let missing_kubeconfig = directory.path().join("no-such-kubeconfig");

    let repo = directory.path().join("repo");
    let home = directory.path().join("home");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    write_exec_profile_with_kubeconfig(&repo, &home, &missing_kubeconfig);
    let environment = oat_agents::environment::TestEnvironment::new(home);
    let log = EventLog::for_dir(None);

    let result = oat_agents::env::commands::execute(
        &environment,
        &log,
        oat_agents::env::commands::EnvSubcommand::Doctor(oat_agents::env::commands::EnvDoctorArgs {
            profile: None,
            repo,
        }),
    );

    let report = result.expect("doctor reports rather than failing when the cluster is unreachable");
    assert_eq!(report["kubeconfig_valid"], false);
    assert!(report["cluster_error"].as_str().is_some_and(|s| !s.is_empty()), "{report}");
    assert_eq!(report["profile"], "local");
    assert!(report.get("tools").is_some(), "{report}");
}

#[test]
fn a_docker_profile_reaches_the_docker_backend_and_a_missing_docker_is_reported() {
    // `OAT_DOCKER_COMMAND` is the docker backend's tmux stub: pointing it at a path that does
    // not exist makes "no docker on this machine" deterministic wherever the test runs.
    let directory = tempfile::tempdir().unwrap();
    let missing_docker = directory.path().join("no-such-docker");
    let repo = directory.path().join("repo");
    let home = directory.path().join("home");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    write_docker_exec_profile(&repo, &home);
    let environment = oat_agents::environment::TestEnvironment::new(home)
        .with_var("OAT_DOCKER_COMMAND", missing_docker.to_string_lossy().to_string());
    let log = EventLog::for_dir(None);

    let result = oat_agents::env::commands::execute(
        &environment,
        &log,
        oat_agents::env::commands::EnvSubcommand::Doctor(oat_agents::env::commands::EnvDoctorArgs {
            profile: None,
            repo,
        }),
    );

    let report = result.expect("doctor reports rather than failing when docker is missing");
    assert_eq!(report["kind"], "docker", "the profile's kind picked the docker provider: {report}");
    assert_eq!(report["profile"], "local");
    assert_eq!(report["docker_cli"], false, "a docker that is not there is said so, never passed: {report}");
    assert!(report["docker_error"].as_str().is_some_and(|s| !s.is_empty()), "{report}");
}

#[test]
fn a_registered_docker_environment_reaches_the_docker_backend_again() {
    // A record carries its backend's kind, so a later process — `env exec` here — goes to the
    // docker provider again instead of reading for a cluster. Docker absent on purpose: the
    // honest outcome is a named failure, never a command that quietly "succeeded".
    let directory = tempfile::tempdir().unwrap();
    let missing_docker = directory.path().join("no-such-docker");
    let home = directory.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let environment = oat_agents::environment::TestEnvironment::new(home)
        .with_var("OAT_DOCKER_COMMAND", missing_docker.to_string_lossy().to_string());
    oat_agents::env::state::EnvStore::open(&environment).unwrap().save(&docker_record()).unwrap();
    let log = EventLog::for_dir(None);

    let result = oat_agents::env::commands::execute(
        &environment,
        &log,
        oat_agents::env::commands::EnvSubcommand::Exec(oat_agents::env::commands::EnvExecArgs {
            env: Some("oat-docker-worker-1".to_string()),
            role: None,
            timeout: None,
            attach: None,
            command: vec!["true".to_string()],
        }),
    );

    let error = result.expect_err("a docker that is not there fails the command");
    assert_eq!(
        error.downcast_ref::<oat_agents::error::CliFailure>().map(|failure| failure.code.as_str()),
        Some("env_backend_error"),
        "{error:#}"
    );
    assert!(error.to_string().contains("docker"), "{error:#}");
}

#[test]
fn an_unreachable_docker_daemon_fails_env_down_rather_than_reporting_removal() {
    // A stand-in docker that is there but cannot reach any daemon: it fails the way a real
    // CLI does when the daemon is down. `env down` must report that failure — a container
    // docker could not be asked about is not a container that was removed.
    let directory = tempfile::tempdir().unwrap();
    let docker = directory.path().join("docker");
    std::fs::write(
        &docker,
        concat!(
            "#!/bin/sh\n",
            "echo 'Cannot connect to the Docker daemon at unix:///var/run/docker.sock. ",
            "Is the docker daemon running?' >&2\n",
            "exit 1\n",
        ),
    )
    .unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&docker, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let home = directory.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let environment = oat_agents::environment::TestEnvironment::new(home)
        .with_var("OAT_DOCKER_COMMAND", docker.to_string_lossy().to_string());
    oat_agents::env::state::EnvStore::open(&environment).unwrap().save(&docker_record()).unwrap();
    let log = EventLog::for_dir(None);

    let result = oat_agents::env::commands::execute(
        &environment,
        &log,
        oat_agents::env::commands::EnvSubcommand::Down(oat_agents::env::commands::EnvDownArgs {
            env: Some("oat-docker-worker-1".to_string()),
            run: None,
        }),
    );

    let error = result.expect_err("a daemon that cannot be asked about is not a container removed");
    assert_eq!(
        error.downcast_ref::<oat_agents::error::CliFailure>().map(|failure| failure.code.as_str()),
        Some("env_backend_error"),
        "{error:#}"
    );
    assert!(
        error.to_string().contains("daemon"),
        "the failure names the daemon, so it cannot be read as removal: {error:#}"
    );
}

#[test]
fn env_rebuild_refuses_when_the_profile_now_selects_another_backend() {
    // The record says the docker backend placed this environment; the machine's profile of
    // the same name now selects kubernetes. Rebuilding must refuse rather than create the
    // replacement as a pod — a record's backend never changes out from under it.
    let directory = tempfile::tempdir().unwrap();
    let repo = directory.path().join("repo");
    let home = directory.path().join("home");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    write_exec_profile(&repo, &home);
    let environment = oat_agents::environment::TestEnvironment::new(home);
    oat_agents::env::state::EnvStore::open(&environment).unwrap().save(&docker_record()).unwrap();
    let log = EventLog::for_dir(None);

    let result = oat_agents::env::commands::execute(
        &environment,
        &log,
        oat_agents::env::commands::EnvSubcommand::Rebuild(oat_agents::env::commands::EnvRebuildArgs {
            env: Some("oat-docker-worker-1".to_string()),
            role: None,
        }),
    );

    let error = result.expect_err("rebuilding an environment under another backend is refused");
    assert_eq!(
        error.downcast_ref::<oat_agents::error::CliFailure>().map(|failure| failure.code.as_str()),
        Some("env_backend_error"),
        "{error:#}"
    );
    let message = format!("{error:#}");
    assert!(
        message.contains("docker") && message.contains("kubernetes"),
        "the refusal names both backends, so it cannot be mistaken for a docker failure: {message}"
    );
}
