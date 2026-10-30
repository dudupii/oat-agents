// SPDX-License-Identifier: MIT
// Copyright (c) 2026 oat-agents contributors

//! The Kubernetes backend.
//!
//! This is the only file that knows Kubernetes exists (ticket Architecture). It never reads
//! the caller's current context: the profile names the context and the namespace, because a
//! command that means one cluster here and another cluster there is not a command anybody can
//! trust.

use super::config::{ImageLoad, ProviderKind, Profile, RunAsUid};
use super::provider::{ExecEnvironment, ExecProvider, WorkspaceStrategy};
use super::{block_on, parse_duration, sanitize, EnvRecord, EnvSpec, ExecOutcome, ExecRequest, RUNNER_PATH};
use crate::error::{codes, err};
use anyhow::Result;
use k8s_openapi::api::core::v1::Pod;
use kube::api::{Api, AttachParams, DeleteParams, ListParams, PostParams};
use kube::config::{KubeConfigOptions, Kubeconfig};
use kube::{Client, Config};
use serde_json::{json, Value};
use std::io::Write;
use std::path::Path;
use std::time::{Duration, Instant};
use tokio::io::{AsyncRead, AsyncReadExt};

const MANAGED_BY: &str = "oat-agents";
const RUNNER: &str = include_str!("run.sh");
const DEFAULT_READY_TIMEOUT_SECS: u64 = 180;

pub struct KubernetesProvider {
    profile_name: String,
    profile: Profile,
    workspace: Box<dyn WorkspaceStrategy>,
}

impl KubernetesProvider {
    pub fn new(profile_name: &str, profile: Profile) -> Self {
        let workspace = super::workspace::for_profile(&profile);
        Self {
            profile_name: profile_name.to_owned(),
            profile,
            workspace,
        }
    }

    fn client(&self) -> Result<Client> {
        install_crypto_provider();
        let options = KubeConfigOptions {
            context: Some(self.profile.context.clone()),
            cluster: None,
            user: None,
        };
        let kubeconfig = self.profile.kubeconfig.clone();
        block_on(async move {
            let config = match kubeconfig {
                Some(path) => {
                    let file = Kubeconfig::read_from(&path)
                        .map_err(|error| backend(format!("failed to read {}: {error}", path.display())))?;
                    Config::from_custom_kubeconfig(file, &options)
                        .await
                        .map_err(|error| backend(format!("kubeconfig is unusable: {error}")))?
                }
                None => Config::from_kubeconfig(&options)
                    .await
                    .map_err(|error| backend(format!("kubeconfig is unusable: {error}")))?,
            };
            Client::try_from(config).map_err(|error| backend(format!("cannot reach the cluster: {error}")))
        })?
    }

    fn pods(&self, client: &Client) -> Api<Pod> {
        Api::namespaced(client.clone(), &self.profile.namespace)
    }

    fn pod_manifest(&self, spec: &EnvSpec) -> Result<Pod> {
        let binding = self.workspace.bind(&spec.worktree)?;
        let security = match (self.profile.run_as_uid, spec.user) {
            (RunAsUid::Host, Some((uid, gid))) => json!({
                "runAsUser": uid,
                "runAsGroup": gid,
                "fsGroup": gid,
            }),
            _ => json!({}),
        };
        // A local image is never pulled: if it is missing from the node the pod must fail
        // loudly rather than quietly fetch something else.
        let pull_policy = match self.profile.image.load {
            ImageLoad::Push => "IfNotPresent",
            _ => "Never",
        };
        let manifest = json!({
            "apiVersion": "v1",
            "kind": "Pod",
            "metadata": {
                "name": spec.env_id,
                "namespace": self.profile.namespace,
                "labels": {
                    "app.kubernetes.io/managed-by": MANAGED_BY,
                    "oat-agents/env": sanitize(&spec.env_id),
                    "oat-agents/role": sanitize(&spec.role),
                    "oat-agents/run-id": sanitize(spec.run_id.as_deref().unwrap_or("norun")),
                },
                "annotations": {
                    "oat-agents/worktree": spec.worktree.to_string_lossy(),
                    "oat-agents/image-id": spec.image.id,
                    "oat-agents/image-ref": spec.image.reference,
                    "oat-agents/profile": self.profile_name,
                }
            },
            "spec": {
                "restartPolicy": "Never",
                "securityContext": security,
                "containers": [{
                    "name": "work",
                    "image": spec.image.reference,
                    "imagePullPolicy": pull_policy,
                    "command": ["sleep", "infinity"],
                    "workingDir": binding.container_path.to_string_lossy(),
                    "volumeMounts": [binding.mount],
                }],
                "volumes": [binding.volume],
            }
        });
        serde_json::from_value(manifest).map_err(|error| backend(format!("invalid pod manifest: {error}")))
    }

    fn ready_timeout(&self) -> Duration {
        Duration::from_secs(self.profile.ready_timeout_secs.unwrap_or(DEFAULT_READY_TIMEOUT_SECS))
    }

    fn await_ready(&self, client: &Client, name: &str) -> Result<()> {
        let api = self.pods(client);
        let deadline = Instant::now() + self.ready_timeout();
        let name = name.to_owned();
        block_on(async move {
            loop {
                let pod = api
                    .get(&name)
                    .await
                    .map_err(|error| backend(format!("cannot read pod {name}: {error}")))?;
                let status = pod.status.clone().unwrap_or_default();
                let phase = status.phase.clone().unwrap_or_default();
                let ready = status
                    .container_statuses
                    .as_ref()
                    .and_then(|statuses| statuses.first().map(|status| status.ready))
                    .unwrap_or(false);
                if phase == "Running" && ready {
                    return Ok(());
                }
                if phase == "Failed" || phase == "Succeeded" {
                    return Err(not_ready(format!(
                        "pod {name} is {phase}: {}",
                        status.reason.unwrap_or_else(|| "no reason reported".to_owned())
                    )));
                }
                if Instant::now() >= deadline {
                    let waiting = status
                        .container_statuses
                        .as_ref()
                        .and_then(|statuses| statuses.first())
                        .and_then(|status| status.state.as_ref())
                        .and_then(|state| state.waiting.as_ref())
                        .map(|waiting| {
                            format!(
                                "{}: {}",
                                waiting.reason.clone().unwrap_or_default(),
                                waiting.message.clone().unwrap_or_default()
                            )
                        })
                        .unwrap_or_else(|| phase.clone());
                    return Err(not_ready(format!("pod {name} did not become ready in time ({waiting})")));
                }
                tokio::time::sleep(Duration::from_millis(400)).await;
            }
        })?
    }

    /// Installs the runner only if it is not already there. A pod outlives many commands, and
    /// anything may have cleaned out its temporary files between two of them.
    fn ensure_runner(&self, client: &Client, name: &str) -> Result<()> {
        let api = self.pods(client);
        let pod = name.to_owned();
        let present = block_on(async move {
            let params = AttachParams::default().stdin(false).stdout(false).stderr(false);
            let Ok(mut process) = api
                .exec(&pod, vec!["/bin/sh", "-c", "test -x /tmp/oat-agents/run.sh"], &params)
                .await
            else {
                return false;
            };
            match process.take_status() {
                Some(status) => exit_code(status.await) == 0,
                None => false,
            }
        })?;
        if present {
            return Ok(());
        }
        self.install_runner(client, name)
    }

    /// Installs the runner that lets a command outlive its own exec stream.
    fn install_runner(&self, client: &Client, name: &str) -> Result<()> {
        let api = self.pods(client);
        let name = name.to_owned();
        block_on(async move {
            let params = AttachParams::default().stdin(true).stdout(false).stderr(true);
            let mut process = api
                .exec(
                    &name,
                    vec![
                        "/bin/sh",
                        "-c",
                        "mkdir -p /tmp/oat-agents && cat > /tmp/oat-agents/run.sh && chmod +x /tmp/oat-agents/run.sh",
                    ],
                    &params,
                )
                .await
                .map_err(|error| backend(format!("cannot install the runner: {error}")))?;
            if let Some(mut stdin) = process.stdin() {
                use tokio::io::AsyncWriteExt;
                stdin
                    .write_all(RUNNER.as_bytes())
                    .await
                    .map_err(|error| backend(format!("cannot write the runner: {error}")))?;
                stdin
                    .shutdown()
                    .await
                    .map_err(|error| backend(format!("cannot finish the runner: {error}")))?;
            }
            let code = match process.take_status() {
                Some(status) => exit_code(status.await),
                None => 0,
            };
            if code != 0 {
                return Err(backend(format!("installing the runner failed with exit {code}")));
            }
            Ok(())
        })?
    }

    fn environment(&self, client: Client, record: EnvRecord) -> KubernetesEnvironment {
        KubernetesEnvironment {
            client,
            namespace: self.profile.namespace.clone(),
            record,
        }
    }
}

impl ExecProvider for KubernetesProvider {
    fn ensure(&self, spec: &EnvSpec) -> Result<EnvRecord> {
        let client = self.client()?;
        let api = self.pods(&client);
        let manifest = self.pod_manifest(spec)?;
        let name = spec.env_id.clone();
        let existing = {
            let api = api.clone();
            let lookup = name.clone();
            block_on(async move { api.get_opt(&lookup).await })?
                .map_err(|error| backend(format!("cannot read pod {name}: {error}")))?
        };
        let reusable = existing.as_ref().is_some_and(|pod| {
            pod.metadata
                .annotations
                .as_ref()
                .and_then(|annotations| annotations.get("oat-agents/image-id"))
                .is_some_and(|id| id == &spec.image.id)
        });
        if existing.is_some() && !reusable {
            // A pod built from a different image is not this environment, no matter what it
            // is called.
            self.delete(&api, &spec.env_id)?;
        }
        if existing.is_none() || !reusable {
            let api = api.clone();
            block_on(async move { api.create(&PostParams::default(), &manifest).await })?
                .map_err(|error| backend(format!("cannot create pod {}: {error}", spec.env_id)))?;
        }
        self.await_ready(&client, &spec.env_id)?;
        self.ensure_runner(&client, &spec.env_id)?;

        let binding = self.workspace.bind(&spec.worktree)?;
        let record = EnvRecord {
            env_id: spec.env_id.clone(),
            run_id: spec.run_id.clone(),
            role: spec.role.clone(),
            profile: self.profile_name.clone(),
            kind: ProviderKind::Kubernetes,
            worktree: spec.worktree.clone(),
            container_path: binding.container_path,
            image_ref: spec.image.reference.clone(),
            image_id: spec.image.id.clone(),
            pod: spec.env_id.clone(),
            namespace: self.profile.namespace.clone(),
            context: self.profile.context.clone(),
            created_ms: super::now_ms(),
        };
        drop(client);
        Ok(record)
    }

    fn attach(&self, record: &EnvRecord) -> Result<Box<dyn ExecEnvironment>> {
        let client = self.client()?;
        let api = self.pods(&client);
        let name = record.pod.clone();
        let pod = block_on(async move { api.get_opt(&name).await })?
            .map_err(|error| backend(format!("cannot read pod {}: {error}", record.pod)))?;
        let pod = pod.ok_or_else(|| not_ready(format!("pod {} is gone; the environment has to be created again", record.pod)))?;
        let phase = pod.status.as_ref().and_then(|status| status.phase.clone()).unwrap_or_default();
        if phase != "Running" {
            return Err(not_ready(format!("pod {} is {phase}, not Running", record.pod)));
        }
        self.ensure_runner(&client, &record.pod)?;
        Ok(Box::new(self.environment(client, record.clone())))
    }

    fn collect(&self, record: &EnvRecord) -> Result<()> {
        let client = self.client()?;
        let environment = self.environment(client, record.clone());
        self.workspace.collect(&environment, &record.worktree)
    }

    fn destroy(&self, record: &EnvRecord) -> Result<()> {
        let client = self.client()?;
        let api = self.pods(&client);
        self.delete(&api, &record.pod)
    }

    fn reap(&self, run_id: Option<&str>, all: bool) -> Result<Vec<String>> {
        let client = self.client()?;
        let api = self.pods(&client);
        let mut selector = format!("app.kubernetes.io/managed-by={MANAGED_BY}");
        if let Some(run_id) = run_id {
            selector.push_str(&format!(",oat-agents/run-id={}", sanitize(run_id)));
        }
        let listed = {
            let api = api.clone();
            let params = ListParams::default().labels(&selector);
            block_on(async move { api.list(&params).await })?
                .map_err(|error| backend(format!("cannot list managed pods: {error}")))?
        };
        let ttl = self.idle_ttl();
        let mut removed = Vec::new();
        for pod in listed {
            let Some(name) = pod.metadata.name.clone() else {
                continue;
            };
            let stale = all || run_id.is_some() || past_ttl(&pod, ttl);
            if !stale {
                continue;
            }
            self.delete(&api, &name)?;
            removed.push(name);
        }
        Ok(removed)
    }

    fn doctor(&self) -> Result<Value> {
        let mut report = json!({
            "profile": self.profile_name,
            "kind": "kubernetes",
            "context": self.profile.context,
            "namespace": self.profile.namespace,
            "workspace": format!("{:?}", self.profile.workspace).to_lowercase(),
            "image_builder": format!("{:?}", self.profile.image.builder).to_lowercase(),
            "image_load": format!("{:?}", self.profile.image.load).to_lowercase(),
        });
        let object = report.as_object_mut().expect("object");
        match self.client() {
            Ok(client) => {
                // Building a client only parses the kubeconfig and constructs a handle — no
                // network request happens yet. `namespace_readable`, from the `list` call below,
                // is the field that actually reflects the cluster answering.
                object.insert("kubeconfig_valid".into(), json!(true));
                let api = self.pods(&client);
                let listed = {
                    let api = api.clone();
                    block_on(async move { api.list(&ListParams::default().limit(1)).await })
                };
                match listed {
                    Ok(Ok(_)) => {
                        object.insert("namespace_readable".into(), json!(true));
                    }
                    Ok(Err(error)) => {
                        object.insert("namespace_readable".into(), json!(false));
                        object.insert("namespace_error".into(), json!(error.to_string()));
                    }
                    Err(error) => {
                        object.insert("namespace_readable".into(), json!(false));
                        object.insert("namespace_error".into(), json!(error.to_string()));
                    }
                }
                object.insert("network_policies".into(), self.network_policies(&client));
            }
            Err(error) => {
                object.insert("kubeconfig_valid".into(), json!(false));
                object.insert("cluster_error".into(), json!(error.to_string()));
            }
        }
        object.insert("tools".into(), tool_report());
        Ok(report)
    }
}

impl KubernetesProvider {
    fn delete(&self, api: &Api<Pod>, name: &str) -> Result<()> {
        let api = api.clone();
        let name = name.to_owned();
        let deleted = block_on(async move { api.delete(&name, &DeleteParams::default().grace_period(0)).await })?;
        match deleted {
            Ok(_) => Ok(()),
            Err(kube::Error::Api(response)) if response.code == 404 => Ok(()),
            Err(error) => Err(backend(format!("cannot delete a pod: {error}"))),
        }
    }

    fn idle_ttl(&self) -> Option<Duration> {
        self.profile.idle_ttl.as_deref().and_then(parse_duration)
    }

    /// An unrestricted network is the point of this environment, so the count is reported
    /// rather than enforced: a person decides what it means.
    fn network_policies(&self, client: &Client) -> Value {
        use k8s_openapi::api::networking::v1::NetworkPolicy;
        let api: Api<NetworkPolicy> = Api::namespaced(client.clone(), &self.profile.namespace);
        match block_on(async move { api.list(&ListParams::default()).await }) {
            Ok(Ok(list)) => json!(list.items.len()),
            _ => json!("unknown"),
        }
    }
}

pub struct KubernetesEnvironment {
    client: Client,
    namespace: String,
    record: EnvRecord,
}

impl ExecEnvironment for KubernetesEnvironment {
    fn exec(&self, request: &ExecRequest) -> Result<ExecOutcome> {
        let api: Api<Pod> = Api::namespaced(self.client.clone(), &self.namespace);
        let mut command: Vec<String> = vec!["/bin/sh".into(), RUNNER_PATH.into()];
        if request.attach {
            command.push("attach".into());
            command.push(request.exec_id.clone());
        } else {
            command.push("start".into());
            command.push(request.exec_id.clone());
            command.push(request.cwd.to_string_lossy().to_string());
            command.extend(request.argv.iter().cloned());
        }
        let pod = self.record.pod.clone();
        let timeout = request.timeout;
        let exec_id = request.exec_id.clone();
        let started = Instant::now();
        let code = block_on(async move {
            let params = AttachParams::default().stdin(false).stdout(true).stderr(true);
            let mut process = api
                .exec(&pod, command, &params)
                .await
                .map_err(|error| backend(format!("cannot execute in pod {pod}: {error}")))?;
            let stdout = process.stdout();
            let stderr = process.stderr();
            let pump = async {
                tokio::join!(pump(stdout, Sink::Out), pump(stderr, Sink::Err));
            };
            match timeout {
                Some(limit) => {
                    if tokio::time::timeout(limit, pump).await.is_err() {
                        return Err(timed_out(format!(
                            "the command passed its {}s limit and is still running in the environment; \
                             follow it with `oat-agents env exec --attach {exec_id}`",
                            limit.as_secs()
                        )));
                    }
                }
                None => pump.await,
            }
            Ok(match process.take_status() {
                Some(status) => exit_code(status.await),
                None => 0,
            })
        })??;
        Ok(ExecOutcome {
            exit_code: code,
            duration_ms: started.elapsed().as_millis() as u64,
        })
    }
}

enum Sink {
    Out,
    Err,
}

/// Streams one channel through to this process untouched. The two channels stay separate all
/// the way out, because a caller that cannot tell output from diagnostics cannot judge what
/// happened.
async fn pump<R: AsyncRead + Unpin>(reader: Option<R>, sink: Sink) {
    let Some(mut reader) = reader else { return };
    let mut buffer = [0_u8; 8192];
    loop {
        match reader.read(&mut buffer).await {
            Ok(0) | Err(_) => return,
            Ok(read) => {
                let chunk = &buffer[..read];
                let _ = match sink {
                    Sink::Out => std::io::stdout().write_all(chunk).and_then(|()| std::io::stdout().flush()),
                    Sink::Err => std::io::stderr().write_all(chunk).and_then(|()| std::io::stderr().flush()),
                };
            }
        }
    }
}

/// Kubernetes reports a non-zero exit as a `Failure` status carrying an `ExitCode` cause.
/// Reading it generically keeps this working across client versions that model the status
/// slightly differently.
fn exit_code(status: Option<k8s_openapi::apimachinery::pkg::apis::meta::v1::Status>) -> i32 {
    let Some(status) = status else {
        // No status at all means the stream ended without the API server saying how; treating
        // that as success would invent a result.
        return 1;
    };
    let value = serde_json::to_value(&status).unwrap_or(Value::Null);
    if value.get("status").and_then(Value::as_str) == Some("Success") {
        return 0;
    }
    value
        .get("details")
        .and_then(|details| details.get("causes"))
        .and_then(Value::as_array)
        .and_then(|causes| {
            causes
                .iter()
                .find(|cause| cause.get("reason").and_then(Value::as_str) == Some("ExitCode"))
                .and_then(|cause| cause.get("message"))
                .and_then(Value::as_str)
                .and_then(|message| message.parse::<i32>().ok())
        })
        .unwrap_or(1)
}

fn past_ttl(pod: &Pod, ttl: Option<Duration>) -> bool {
    let Some(ttl) = ttl else { return false };
    let Some(created) = pod.metadata.creation_timestamp.as_ref() else {
        return false;
    };
    let age_ms = super::now_ms() as i64 - created.0.timestamp_millis();
    age_ms > ttl.as_millis() as i64
}

fn tool_report() -> Value {
    let mut tools = serde_json::Map::new();
    for tool in ["docker", "devcontainer", "kind", "minikube"] {
        let found = std::process::Command::new(tool)
            .arg("--version")
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false);
        tools.insert(tool.to_owned(), json!(found));
    }
    Value::Object(tools)
}

pub fn worktree_owner(worktree: &Path) -> Option<(u32, u32)> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(worktree).ok().map(|metadata| (metadata.uid(), metadata.gid()))
}

/// rustls refuses to guess which cryptography to use when more than one provider could be
/// linked in, and the guess it will not make is a panic on the first request. Making the
/// choice once, here, keeps that out of every call site.
fn install_crypto_provider() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
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
    use crate::env::config::{ImageConfig, ProviderKind, WorkspaceKind};
    use crate::env::image::ImageRef;
    use std::path::PathBuf;

    fn profile(namespace: &str, context: &str) -> Profile {
        Profile {
            kind: ProviderKind::Kubernetes,
            context: context.to_owned(),
            namespace: namespace.to_owned(),
            workspace: WorkspaceKind::Hostpath,
            run_as_uid: RunAsUid::Host,
            idle_ttl: Some("4h".to_owned()),
            kubeconfig: None,
            ready_timeout_secs: None,
            image: ImageConfig::default(),
        }
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

    /// Two profiles, one manifest builder: everything that changes with the machine has to
    /// come out of the profile, or a second cluster becomes a code change.
    #[test]
    fn the_manifest_carries_no_value_the_profile_did_not_give_it() {
        for (namespace, context) in [("agents-a", "cluster-a"), ("agents-b", "cluster-b")] {
            let provider = KubernetesProvider::new("local", profile(namespace, context));
            let pod = provider.pod_manifest(&spec("/home/a/worktree")).unwrap();
            let value = serde_json::to_value(&pod).unwrap();

            assert_eq!(value["metadata"]["namespace"], namespace);
            assert_eq!(value["metadata"]["name"], "oat-abc123-worker-0f0f0f0f");
            assert_eq!(value["metadata"]["labels"]["app.kubernetes.io/managed-by"], "oat-agents");
            assert_eq!(value["metadata"]["labels"]["oat-agents/role"], "worker");
            assert_eq!(value["metadata"]["annotations"]["oat-agents/image-id"], "sha256:cafe");
            assert_eq!(value["spec"]["containers"][0]["image"], "oat-w:0123456789ab");
            assert_eq!(value["spec"]["containers"][0]["workingDir"], "/home/a/worktree");
            assert_eq!(
                value["spec"]["containers"][0]["volumeMounts"][0]["mountPath"], "/home/a/worktree",
                "the workspace keeps its path, so a command's cwd needs no translation"
            );
            assert_eq!(value["spec"]["volumes"][0]["hostPath"]["path"], "/home/a/worktree");
            assert_eq!(value["spec"]["securityContext"]["runAsUser"], 1000);
            assert_eq!(
                value["spec"]["containers"][0]["imagePullPolicy"], "Never",
                "a locally built image is never quietly replaced by a pulled one"
            );
            let rendered = value.to_string();
            assert!(
                !rendered.contains("localhost") && !rendered.contains("current-context"),
                "nothing about the local machine may be written into the manifest"
            );
        }
    }

    #[test]
    fn a_failure_reports_the_exit_code_the_command_really_had() {
        let failure = serde_json::from_value(serde_json::json!({
            "status": "Failure",
            "reason": "NonZeroExitCode",
            "details": { "causes": [{ "reason": "ExitCode", "message": "101" }] }
        }))
        .unwrap();
        assert_eq!(exit_code(Some(failure)), 101);

        let success = serde_json::from_value(serde_json::json!({ "status": "Success" })).unwrap();
        assert_eq!(exit_code(Some(success)), 0);

        assert_eq!(exit_code(None), 1, "a stream that ended without a status proves nothing succeeded");
    }
}
