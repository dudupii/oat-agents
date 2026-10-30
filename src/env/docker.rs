// SPDX-License-Identifier: MIT
// Copyright (c) 2026 oat-agents contributors

//! The Docker backend.
//!
//! This is the only file that knows Docker exists. One environment is one plain long-running
//! container — `docker run -d`, a bind mount for the worktree the way the hostpath strategy
//! mounts it, `docker exec` for commands — with no scheduler and no manifest to compose. The
//! profile names everything that changes with the machine; `OAT_DOCKER_COMMAND` names the
//! binary when it is not `docker` on `PATH` (a test stand-in, or a docker somewhere else).

use super::config::{ProviderKind, Profile, RunAsUid};
use super::provider::{ExecEnvironment, ExecProvider, WorkspaceBinding, WorkspaceStrategy};
use super::{parse_duration, sanitize, EnvRecord, EnvSpec, ExecOutcome, ExecRequest, RUNNER_PATH};
use crate::environment::Environment;
use crate::error::{codes, err};
use anyhow::Result;
use serde_json::{json, Value};
use std::io::Write;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const MANAGED_BY: &str = "oat-agents";
const LABEL_MANAGED_BY: &str = "oat-agents.managed-by";
const LABEL_ENV: &str = "oat-agents.env";
const LABEL_ROLE: &str = "oat-agents.role";
const LABEL_RUN_ID: &str = "oat-agents.run-id";
const RUNNER: &str = include_str!("run.sh");
const DEFAULT_READY_TIMEOUT_SECS: u64 = 180;

/// What `docker inspect` says about one container, in the shape this backend asks for it.
#[derive(Debug, Clone)]
struct ContainerState {
    image: String,
    running: bool,
    status: String,
}

pub struct DockerProvider {
    profile_name: String,
    profile: Profile,
    workspace: Box<dyn WorkspaceStrategy>,
    docker: String,
}

impl DockerProvider {
    pub fn new(environment: &dyn Environment, profile_name: &str, profile: Profile) -> Self {
        let workspace = super::workspace::for_profile(&profile);
        Self {
            profile_name: profile_name.to_owned(),
            profile,
            workspace,
            docker: environment.var_or("OAT_DOCKER_COMMAND", "docker"),
        }
    }

    fn command(&self, args: &[String]) -> Command {
        docker_command(&self.docker, args)
    }

    /// Runs the docker CLI and captures its stdout. A docker that is not there is a named
    /// failure, never a quiet success — the whole backend's contract with its caller.
    fn docker_output(&self, args: &[String]) -> Result<String> {
        let output = self.command(args).output().map_err(|error| {
            backend(format!("docker ({}) could not be executed: {error}", self.docker))
        })?;
        if !output.status.success() {
            return Err(backend(format!(
                "docker {} failed (exit {}): {}",
                args.join(" "),
                output.status.code().unwrap_or(-1),
                detail(&output.stderr, &output.stdout)
            )));
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    }

    /// The same, but a non-zero exit is `None` rather than an error: it is how "no such
    /// container" is told apart from "could not ask docker at all".
    fn docker_try(&self, args: &[String]) -> Result<Option<String>> {
        let output = self.command(args).output().map_err(|error| {
            backend(format!("docker ({}) could not be executed: {error}", self.docker))
        })?;
        if !output.status.success() {
            return Ok(None);
        }
        Ok(Some(String::from_utf8_lossy(&output.stdout).trim().to_owned()))
    }

    /// The command line that creates one environment: a plain long-running container. The
    /// worktree is bind-mounted at the path it already has, so a command's cwd needs no
    /// translation and its output names files the operator can open.
    fn run_command(&self, spec: &EnvSpec, binding: &WorkspaceBinding) -> Vec<String> {
        let mut argv = vec!["run".to_owned(), "-d".to_owned(), "--name".to_owned(), spec.env_id.clone()];
        for (key, value) in [
            (LABEL_MANAGED_BY, MANAGED_BY.to_owned()),
            (LABEL_ENV, spec.env_id.clone()),
            (LABEL_ROLE, sanitize(&spec.role)),
            (LABEL_RUN_ID, sanitize(spec.run_id.as_deref().unwrap_or("norun"))),
        ] {
            argv.push("--label".to_owned());
            argv.push(format!("{key}={value}"));
        }
        // A locally built image is never pulled: like the Kubernetes backend's `Never` policy,
        // a missing image must fail loudly rather than quietly fetch something else.
        argv.push("--pull".to_owned());
        argv.push("never".to_owned());
        if let (RunAsUid::Host, Some((uid, gid))) = (self.profile.run_as_uid, spec.user) {
            argv.push("--user".to_owned());
            argv.push(format!("{uid}:{gid}"));
        }
        argv.push("--volume".to_owned());
        argv.push(format!(
            "{}:{}",
            spec.worktree.to_string_lossy(),
            binding.container_path.to_string_lossy()
        ));
        argv.push("--workdir".to_owned());
        argv.push(binding.container_path.to_string_lossy().to_string());
        argv.push(spec.image.reference.clone());
        argv.push("sleep".to_owned());
        argv.push("infinity".to_owned());
        argv
    }

    /// The image a container runs, whether it is running and what its status is, or `None`
    /// when no container has this name. Several facts in one inspect, because every caller
    /// needs them to agree.
    fn container_state(&self, name: &str) -> Result<Option<ContainerState>> {
        let inspected = self.docker_try(&[
            "inspect".to_owned(),
            "--format".to_owned(),
            "{{.Image}} {{.State.Running}} {{.State.Status}}".to_owned(),
            name.to_owned(),
        ])?;
        Ok(inspected.map(|text| {
            let mut fields = text.split(' ');
            ContainerState {
                image: fields.next().unwrap_or_default().to_owned(),
                running: fields.next() == Some("true"),
                status: fields.next().unwrap_or_default().to_owned(),
            }
        }))
    }

    fn await_running(&self, name: &str) -> Result<()> {
        let deadline = Instant::now()
            + Duration::from_secs(self.profile.ready_timeout_secs.unwrap_or(DEFAULT_READY_TIMEOUT_SECS));
        while Instant::now() < deadline {
            match self.container_state(name)? {
                Some(state) if state.running => return Ok(()),
                // A container that has already exited never will: waiting out the timeout
                // would only delay the report.
                Some(state) if state.status == "exited" || state.status == "dead" => {
                    return Err(not_ready(format!(
                        "container {name} is {status}: it did not stay up",
                        status = state.status
                    )));
                }
                _ => std::thread::sleep(Duration::from_millis(400)),
            }
        }
        Err(not_ready(format!("container {name} did not become ready in time")))
    }

    /// Installs the runner only if it is not already there. A container outlives many
    /// commands, and anything may have cleaned out its temporary files between two of them.
    fn ensure_runner(&self, name: &str) -> Result<()> {
        let present = self.docker_try(&[
            "exec".to_owned(),
            name.to_owned(),
            "/bin/sh".to_owned(),
            "-c".to_owned(),
            format!("test -x {RUNNER_PATH}"),
        ])?;
        if present.is_some() {
            return Ok(());
        }
        self.install_runner(name)
    }

    /// Installs the runner that lets a command outlive its own exec stream — the same runner
    /// and the same protocol as the Kubernetes backend, so `--attach` means the same thing
    /// whichever provides the environment.
    fn install_runner(&self, name: &str) -> Result<()> {
        let mut child = self
            .command(&[
                "exec".to_owned(),
                "-i".to_owned(),
                name.to_owned(),
                "/bin/sh".to_owned(),
                "-c".to_owned(),
                format!("mkdir -p /tmp/oat-agents && cat > {RUNNER_PATH} && chmod +x {RUNNER_PATH}"),
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| backend(format!("cannot install the runner: {error}")))?;
        if let Some(stdin) = child.stdin.as_mut() {
            stdin
                .write_all(RUNNER.as_bytes())
                .map_err(|error| backend(format!("cannot write the runner: {error}")))?;
        }
        drop(child.stdin.take());
        let output = child
            .wait_with_output()
            .map_err(|error| backend(format!("cannot finish installing the runner: {error}")))?;
        if !output.status.success() {
            return Err(backend(format!(
                "installing the runner failed (exit {}): {}",
                output.status.code().unwrap_or(-1),
                detail(&output.stderr, &output.stdout)
            )));
        }
        Ok(())
    }

    /// Removes a container, treating one that is already gone as removed — the Kubernetes
    /// backend treats a 404 the same way.
    fn remove(&self, name: &str) -> Result<()> {
        if self.container_state(name)?.is_none() {
            return Ok(());
        }
        self.docker_output(&["rm".to_owned(), "-f".to_owned(), name.to_owned()]).map(|_| ())
    }

    fn past_ttl(&self, name: &str, ttl: Option<Duration>) -> Result<bool> {
        let Some(ttl) = ttl else { return Ok(false) };
        let created = self.docker_try(&[
            "inspect".to_owned(),
            "--format".to_owned(),
            "{{.Created}}".to_owned(),
            name.to_owned(),
        ])?;
        let Some(created_ms) = created.as_deref().and_then(crate::event_log::epoch_millis) else {
            return Ok(false);
        };
        Ok(super::now_ms() as i64 - created_ms > ttl.as_millis() as i64)
    }

    fn environment(&self, record: EnvRecord) -> DockerEnvironment {
        DockerEnvironment {
            docker: self.docker.clone(),
            record,
        }
    }
}

impl ExecProvider for DockerProvider {
    fn ensure(&self, spec: &EnvSpec) -> Result<EnvRecord> {
        let binding = self.workspace.bind(&spec.worktree)?;
        let existing = self.container_state(&spec.env_id)?;
        let create = match &existing {
            None => true,
            // A container built from a different image is not this environment, no matter
            // what it is called.
            Some(state) if state.image != spec.image.id => {
                self.remove(&spec.env_id)?;
                true
            }
            // One that outlived a reboot is stopped, not gone: still this environment, and
            // only needs starting again.
            Some(state) if !state.running => {
                self.docker_output(&["start".to_owned(), spec.env_id.clone()])?;
                false
            }
            Some(_) => false,
        };
        if create {
            let argv = self.run_command(spec, &binding);
            let container = self.docker_output(&argv)?;
            if container.is_empty() {
                return Err(backend(format!(
                    "docker started {} but reported no container",
                    spec.env_id
                )));
            }
        }
        self.await_running(&spec.env_id)?;
        self.ensure_runner(&spec.env_id)?;

        Ok(EnvRecord {
            env_id: spec.env_id.clone(),
            run_id: spec.run_id.clone(),
            role: spec.role.clone(),
            profile: self.profile_name.clone(),
            kind: ProviderKind::Docker,
            worktree: spec.worktree.clone(),
            container_path: binding.container_path,
            image_ref: spec.image.reference.clone(),
            image_id: spec.image.id.clone(),
            pod: spec.env_id.clone(),
            // One daemon, no clusters: these say which backend placed the environment, so a
            // ledger entry still says what "elsewhere" would mean.
            namespace: "docker".to_owned(),
            context: "docker".to_owned(),
            created_ms: super::now_ms(),
        })
    }

    fn attach(&self, record: &EnvRecord) -> Result<Box<dyn ExecEnvironment>> {
        match self.container_state(&record.pod)? {
            None => Err(not_ready(format!(
                "container {} is gone; the environment has to be created again",
                record.pod
            ))),
            Some(state) if !state.running => Err(not_ready(format!(
                "container {} is {}, not running",
                record.pod, state.status
            ))),
            Some(_) => {
                self.ensure_runner(&record.pod)?;
                Ok(Box::new(self.environment(record.clone())))
            }
        }
    }

    fn collect(&self, record: &EnvRecord) -> Result<()> {
        let environment = self.environment(record.clone());
        self.workspace.collect(&environment, &record.worktree)
    }

    fn destroy(&self, record: &EnvRecord) -> Result<()> {
        self.remove(&record.pod)
    }

    fn reap(&self, run_id: Option<&str>, all: bool) -> Result<Vec<String>> {
        let mut args = vec![
            "ps".to_owned(),
            "-a".to_owned(),
            "--filter".to_owned(),
            format!("label={LABEL_MANAGED_BY}={MANAGED_BY}"),
        ];
        if let Some(run_id) = run_id {
            args.push("--filter".to_owned());
            args.push(format!("label={LABEL_RUN_ID}={}", sanitize(run_id)));
        }
        args.push("--format".to_owned());
        args.push("{{.Names}}".to_owned());
        let listed = self.docker_output(&args)?;
        let ttl = self.profile.idle_ttl.as_deref().and_then(parse_duration);
        let mut removed = Vec::new();
        for name in listed.lines().map(str::trim).filter(|name| !name.is_empty()) {
            let stale = all || run_id.is_some() || self.past_ttl(name, ttl)?;
            if !stale {
                continue;
            }
            self.remove(name)?;
            removed.push(name.to_owned());
        }
        Ok(removed)
    }

    fn doctor(&self) -> Result<Value> {
        let mut report = json!({
            "profile": self.profile_name,
            "kind": "docker",
            "workspace": format!("{:?}", self.profile.workspace).to_lowercase(),
            "image_builder": format!("{:?}", self.profile.image.builder).to_lowercase(),
            "image_load": format!("{:?}", self.profile.image.load).to_lowercase(),
        });
        let object = report.as_object_mut().expect("object");
        match self.command(&["--version".to_owned()]).output() {
            Err(error) => {
                object.insert("docker_cli".to_owned(), json!(false));
                object.insert(
                    "docker_error".to_owned(),
                    json!(format!("docker ({}) could not be executed: {error}", self.docker)),
                );
            }
            Ok(output) if !output.status.success() => {
                object.insert("docker_cli".to_owned(), json!(false));
                object.insert(
                    "docker_error".to_owned(),
                    json!(format!(
                        "docker --version failed (exit {}): {}",
                        output.status.code().unwrap_or(-1),
                        detail(&output.stderr, &output.stdout)
                    )),
                );
            }
            Ok(_) => {
                object.insert("docker_cli".to_owned(), json!(true));
                // `docker version` answers only when the CLI can also reach a daemon; the
                // binary alone proves nothing about where commands would go.
                match self.command(&["version".to_owned()]).output() {
                    Ok(output) if output.status.success() => {
                        object.insert("docker_daemon".to_owned(), json!(true));
                    }
                    Ok(output) => {
                        object.insert("docker_daemon".to_owned(), json!(false));
                        object.insert(
                            "docker_error".to_owned(),
                            json!(format!(
                                "docker version failed (exit {}): {}",
                                output.status.code().unwrap_or(-1),
                                detail(&output.stderr, &output.stdout)
                            )),
                        );
                    }
                    Err(error) => {
                        object.insert("docker_daemon".to_owned(), json!(false));
                        object.insert(
                            "docker_error".to_owned(),
                            json!(format!("docker version could not be executed: {error}")),
                        );
                    }
                }
            }
        }
        Ok(report)
    }
}

pub struct DockerEnvironment {
    docker: String,
    record: EnvRecord,
}

impl ExecEnvironment for DockerEnvironment {
    fn exec(&self, request: &ExecRequest) -> Result<ExecOutcome> {
        let argv = exec_argv(&self.record.pod, request);
        let started = Instant::now();
        // Inherited stdio: the runner streams the command's own output and diagnostics
        // through, the two channels staying separate all the way out.
        let mut child = self
            .command(&argv)
            .spawn()
            .map_err(|error| backend(format!("cannot execute in container {}: {error}", self.record.pod)))?;
        let code = match request.timeout {
            Some(limit) => wait_within(&mut child, limit, &request.exec_id)?,
            None => child
                .wait()
                // A stream that ended without a status proves nothing succeeded.
                .map(|status| status.code().unwrap_or(1))
                .map_err(|error| backend(format!("the command's stream broke: {error}")))?,
        };
        Ok(ExecOutcome {
            exit_code: code,
            duration_ms: started.elapsed().as_millis() as u64,
        })
    }
}

impl DockerEnvironment {
    fn command(&self, args: &[String]) -> Command {
        docker_command(&self.docker, args)
    }
}

/// One docker CLI invocation: the binary the environment named, with the caller's
/// arguments. Its environment is left alone on purpose — `DOCKER_HOST` and friends are how a
/// machine points docker at one daemon, and this backend must not second-guess that.
fn docker_command(docker: &str, args: &[String]) -> Command {
    let mut command = Command::new(docker);
    command.args(args);
    command
}

/// The command line that runs one request through the runner. The same shape as the
/// Kubernetes backend's, so nothing above the provider seam knows which one carried it.
fn exec_argv(pod: &str, request: &ExecRequest) -> Vec<String> {
    let mut argv = vec![
        "exec".to_owned(),
        pod.to_owned(),
        "/bin/sh".to_owned(),
        RUNNER_PATH.to_owned(),
    ];
    if request.attach {
        argv.push("attach".to_owned());
        argv.push(request.exec_id.clone());
    } else {
        argv.push("start".to_owned());
        argv.push(request.exec_id.clone());
        argv.push(request.cwd.to_string_lossy().to_string());
        argv.extend(request.argv.iter().cloned());
    }
    argv
}

/// Waits for the docker CLI to finish, up to the limit. Passing the limit kills only the
/// stream: the work runs detached inside the container, so it keeps going and `--attach` can
/// pick the same execution back up.
fn wait_within(child: &mut Child, limit: Duration, exec_id: &str) -> Result<i32> {
    let deadline = Instant::now() + limit;
    loop {
        match child
            .try_wait()
            .map_err(|error| backend(format!("the command's stream broke: {error}")))?
        {
            Some(status) => return Ok(status.code().unwrap_or(1)),
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(timed_out(format!(
                    "the command passed its {}s limit and is still running in the environment; \
                     follow it with `oat-agents env exec --attach {exec_id}`",
                    limit.as_secs()
                )));
            }
            None => std::thread::sleep(Duration::from_millis(200)),
        }
    }
}

fn detail(stderr: &[u8], stdout: &[u8]) -> String {
    let stderr = String::from_utf8_lossy(stderr);
    let stdout = String::from_utf8_lossy(stdout);
    let text = if stderr.trim().is_empty() { stdout } else { stderr };
    text.trim().to_owned()
}

fn backend(message: String) -> anyhow::Error {
    err(codes::ENV_BACKEND_ERROR, message)
}

fn timed_out(message: String) -> anyhow::Error {
    err(codes::ENV_EXEC_TIMEOUT, message)
}

fn not_ready(message: String) -> anyhow::Error {
    err(codes::ENV_NOT_READY, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env::config::{ImageConfig, WorkspaceKind};
    use crate::env::image::ImageRef;
    use crate::environment::TestEnvironment;
    use crate::error::CliFailure;
    use std::path::PathBuf;

    fn profile() -> Profile {
        Profile {
            kind: ProviderKind::Docker,
            context: "docker".to_owned(),
            namespace: "docker".to_owned(),
            workspace: WorkspaceKind::Hostpath,
            run_as_uid: RunAsUid::Host,
            idle_ttl: Some("4h".to_owned()),
            kubeconfig: None,
            ready_timeout_secs: None,
            image: ImageConfig::default(),
        }
    }

    fn provider() -> DockerProvider {
        let home = tempfile::tempdir().unwrap();
        DockerProvider::new(&TestEnvironment::new(home.path().to_path_buf()), "local", profile())
    }

    fn spec(worktree: &str) -> EnvSpec {
        EnvSpec {
            env_id: "oat-abc123-worker-0f0f0f0f".to_owned(),
            run_id: Some("run-2026-09-22-abc123".to_owned()),
            role: "worker".to_owned(),
            worktree: PathBuf::from(worktree),
            image: ImageRef {
                reference: "oat-w:0123456789ab".to_owned(),
                id: "sha256:cafe".to_owned(),
                source_hash: "0123456789ab".to_owned(),
                rebuilt: false,
            },
            user: Some((1000, 1000)),
        }
    }

    /// Everything that changes with the machine has to come out of the profile, or a second
    /// daemon becomes a code change — the same contract the manifest test holds the
    /// Kubernetes backend to.
    #[test]
    fn the_run_command_carries_no_value_the_profile_did_not_give_it() {
        let provider = provider();
        let work_spec = spec("/home/a/worktree");
        let binding = provider.workspace.bind(&work_spec.worktree).unwrap();
        let line = provider.run_command(&work_spec, &binding).join(" ");

        assert!(line.contains("--name oat-abc123-worker-0f0f0f0f"), "{line}");
        assert!(line.contains("--label oat-agents.managed-by=oat-agents"), "{line}");
        assert!(line.contains("--label oat-agents.run-id=run-2026-09-22-abc123"), "{line}");
        assert!(line.contains("--label oat-agents.role=worker"), "{line}");
        assert!(
            line.contains("--volume /home/a/worktree:/home/a/worktree"),
            "the workspace keeps the path it has outside: {line}"
        );
        assert!(line.contains("--workdir /home/a/worktree"), "{line}");
        assert!(line.contains("--user 1000:1000"), "run_as_uid = host keeps files owned by the operator: {line}");
        assert!(line.contains("--pull never"), "a locally built image is never quietly replaced by a pulled one: {line}");
        assert!(line.ends_with("oat-w:0123456789ab sleep infinity"), "{line}");
        assert!(
            !line.contains("kubeconfig") && !line.contains("kubectl"),
            "nothing about a cluster may be in a docker command line: {line}"
        );

        let mut as_image = profile();
        as_image.run_as_uid = RunAsUid::Image;
        let mut image_spec = spec("/home/a/worktree");
        image_spec.user = None;
        let provider = DockerProvider::new(&TestEnvironment::new(PathBuf::from("/nowhere")), "local", as_image);
        let line = provider.run_command(&image_spec, &provider.workspace.bind(&image_spec.worktree).unwrap()).join(" ");
        assert!(!line.contains("--user"), "run_as_uid = image leaves the user to the image: {line}");
    }

    #[test]
    fn commands_run_through_the_runner_so_a_dropped_stream_can_be_followed() {
        let request = ExecRequest {
            exec_id: "e1".to_owned(),
            argv: vec!["cargo".to_owned(), "test".to_owned()],
            cwd: PathBuf::from("/w"),
            timeout: None,
            attach: false,
        };
        assert_eq!(
            exec_argv("oat-c", &request),
            vec!["exec", "oat-c", "/bin/sh", RUNNER_PATH, "start", "e1", "/w", "cargo", "test"]
        );

        let attach = ExecRequest {
            exec_id: "e1".to_owned(),
            argv: Vec::new(),
            cwd: PathBuf::from("/w"),
            timeout: None,
            attach: true,
        };
        assert_eq!(
            exec_argv("oat-c", &attach),
            vec!["exec", "oat-c", "/bin/sh", RUNNER_PATH, "attach", "e1"]
        );
    }

    #[test]
    fn a_docker_that_is_not_there_is_a_reported_failure_never_a_success() {
        let directory = tempfile::tempdir().unwrap();
        let environment = TestEnvironment::new(directory.path().join("home")).with_var(
            "OAT_DOCKER_COMMAND",
            directory.path().join("no-such-docker").to_string_lossy().to_string(),
        );
        let provider = DockerProvider::new(&environment, "local", profile());

        let error = provider.ensure(&spec("/home/a/worktree")).unwrap_err();
        assert_eq!(
            error.downcast_ref::<CliFailure>().map(|failure| failure.code.as_str()),
            Some(codes::ENV_BACKEND_ERROR),
            "a missing docker is a failure, not an environment that claims to exist: {error:#}"
        );

        // `env doctor` is the surface that must say so in a report rather than by failing.
        let report = provider.doctor().unwrap();
        assert_eq!(report["kind"], "docker");
        assert_eq!(report["docker_cli"], false);
        assert!(report["docker_error"].as_str().is_some_and(|text| !text.is_empty()), "{report}");
        assert!(report.get("docker_daemon").is_none(), "nothing may read as the daemon answering: {report}");
    }
}
