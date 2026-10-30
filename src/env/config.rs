// SPDX-License-Identifier: MIT
// Copyright (c) 2026 oat-agents contributors

//! Execution profiles.
//!
//! Nothing in this crate is allowed to assume the cluster is the local one. Every fact that
//! changes with the machine — context, namespace, how the workspace gets in, how the image
//! gets to the nodes — is a field here, and a second machine is a second `[profile.*]` table.

use crate::environment::Environment;
use crate::error::{codes, err};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The repository names the profile it expects; the machine says how it provides it
/// (`.dev_docs/CONTEXT.md`, *Execution profile*). Both files share this relative path.
pub const REPO_CONFIG: &str = ".oat/exec.toml";
pub const MACHINE_CONFIG: &str = ".oat/exec.toml";

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecConfig {
    pub default_profile: Option<String>,
    #[serde(default)]
    pub profile: BTreeMap<String, Profile>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    #[serde(default)]
    pub kind: ProviderKind,
    /// Always written out. Reading the caller's current context would make the same command
    /// mean different things on different machines.
    pub context: String,
    pub namespace: String,
    #[serde(default)]
    pub workspace: WorkspaceKind,
    #[serde(default)]
    pub run_as_uid: RunAsUid,
    #[serde(default)]
    pub idle_ttl: Option<String>,
    #[serde(default)]
    pub kubeconfig: Option<PathBuf>,
    #[serde(default)]
    pub ready_timeout_secs: Option<u64>,
    #[serde(default)]
    pub image: ImageConfig,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProviderKind {
    #[default]
    Kubernetes,
    Docker,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum WorkspaceKind {
    /// The worktree is mounted from the node's filesystem: only correct when the pod is
    /// scheduled on the machine holding it.
    #[default]
    Hostpath,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum RunAsUid {
    /// Match the worktree's owner, so files written inside the pod stay readable and removable
    /// outside it.
    #[default]
    Host,
    /// Whatever the image declares.
    Image,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImageConfig {
    #[serde(default)]
    pub builder: ImageBuilder,
    /// How a freshly built image reaches the cluster's nodes. There is no safe default to
    /// infer here: guessing wrong fails at pod start, not at build.
    #[serde(default)]
    pub load: ImageLoad,
    /// The address the cluster pulls by.
    #[serde(default)]
    pub registry: Option<String>,
    /// The address this machine can push to, when it is not the one the cluster pulls by. An
    /// in-cluster registry is the ordinary case: the nodes reach it on a service address that
    /// the host cannot use.
    #[serde(default)]
    pub push_to: Option<String>,
    /// A command that makes `push_to` reachable, run for the push and stopped afterwards.
    /// Whatever the machine needs — a port-forward, a tunnel — stated here rather than assumed
    /// anywhere in the code.
    #[serde(default)]
    pub push_tunnel: Option<Vec<String>>,
    #[serde(default)]
    pub cluster: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum ImageBuilder {
    #[default]
    Devcontainer,
    Dockerfile,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum ImageLoad {
    /// The cluster already sees the builder's image store.
    #[default]
    None,
    Kind,
    Minikube,
    Push,
}

/// The repository's file states what the project needs; the machine's file states how this
/// machine provides it, and wins whole profiles.
pub fn load(environment: &dyn Environment, repo: Option<&Path>) -> Result<ExecConfig> {
    let mut merged = ExecConfig::default();
    if let Some(repo) = repo {
        merge(&mut merged, read_file(&repo.join(REPO_CONFIG))?);
    }
    if let Some(home) = environment.home_dir() {
        merge(&mut merged, read_file(&home.join(MACHINE_CONFIG))?);
    }
    Ok(merged)
}

fn merge(into: &mut ExecConfig, from: Option<ExecConfig>) {
    let Some(from) = from else { return };
    if from.default_profile.is_some() {
        into.default_profile = from.default_profile;
    }
    for (name, profile) in from.profile {
        into.profile.insert(name, profile);
    }
}

fn read_file(path: &Path) -> Result<Option<ExecConfig>> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Ok(None);
    };
    let config: ExecConfig = toml::from_str(&text).map_err(|error| {
        err(
            codes::ENV_PROFILE_UNRESOLVED,
            format!("{} is not a valid exec config: {error}", path.display()),
        )
    })?;
    Ok(Some(config))
}

/// Resolve one profile by name, or the configured default. A missing profile is a hard
/// failure: silently running somewhere else is the one outcome this whole feature exists to
/// prevent.
pub fn resolve(config: &ExecConfig, requested: Option<&str>) -> Result<(String, Profile)> {
    let name = requested
        .map(str::to_owned)
        .or_else(|| config.default_profile.clone())
        .ok_or_else(|| {
            err(
                codes::ENV_PROFILE_UNRESOLVED,
                "no exec profile was given and no default_profile is configured; \
                 add one to .oat/exec.toml or pass --profile",
            )
        })?;
    let profile = config.profile.get(&name).cloned().ok_or_else(|| {
        let known: Vec<&str> = config.profile.keys().map(String::as_str).collect();
        err(
            codes::ENV_PROFILE_UNRESOLVED,
            format!(
                "exec profile '{name}' is not defined; known profiles: {}",
                if known.is_empty() {
                    "none".to_owned()
                } else {
                    known.join(", ")
                }
            ),
        )
    })?;
    Ok((name, profile))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::environment::TestEnvironment;

    fn write(path: &Path, body: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    #[test]
    fn the_machine_states_how_a_profile_is_provided_and_wins() {
        let directory = tempfile::tempdir().unwrap();
        let repo = directory.path().join("repo");
        let home = directory.path().join("home");
        write(
            &repo.join(REPO_CONFIG),
            "default_profile = \"local\"\n\n[profile.local]\ncontext = \"from-repo\"\nnamespace = \"agents\"\n",
        );
        write(
            &home.join(MACHINE_CONFIG),
            "[profile.local]\ncontext = \"from-machine\"\nnamespace = \"agents\"\n\n[profile.lab]\ncontext = \"lab\"\nnamespace = \"agents\"\n",
        );
        let environment = TestEnvironment::new(home);

        let config = load(&environment, Some(&repo)).unwrap();
        let (name, profile) = resolve(&config, None).unwrap();
        assert_eq!(name, "local");
        assert_eq!(
            profile.context, "from-machine",
            "the machine owns how a profile is provided here"
        );
        assert_eq!(
            resolve(&config, Some("lab")).unwrap().1.context,
            "lab",
            "a profile only the machine defines is still reachable"
        );
    }

    /// An in-cluster registry is reachable from the nodes and not from here. The pod must
    /// still be told the address the nodes use.
    #[test]
    fn the_address_a_machine_pushes_to_is_not_always_the_one_pods_pull_by() {
        let directory = tempfile::tempdir().unwrap();
        let repo = directory.path().join("repo");
        write(
            &repo.join(REPO_CONFIG),
            "default_profile = \"local\"\n\n[profile.local]\ncontext = \"c\"\nnamespace = \"n\"\n\n[profile.local.image]\nload = \"push\"\nregistry = \"10.43.0.100:5000\"\npush_to = \"127.0.0.1:5000\"\npush_tunnel = [\"kubectl\", \"port-forward\", \"svc/registry\", \"5000:5000\"]\n",
        );
        let config = load(&TestEnvironment::new(directory.path().join("home")), Some(&repo)).unwrap();

        let (_, profile) = resolve(&config, None).unwrap();

        assert_eq!(profile.image.load, ImageLoad::Push);
        assert_eq!(profile.image.registry.as_deref(), Some("10.43.0.100:5000"));
        assert_eq!(profile.image.push_to.as_deref(), Some("127.0.0.1:5000"));
        assert_eq!(
            profile.image.push_tunnel.as_deref().map(<[String]>::len),
            Some(4),
            "whatever makes the push address reachable is stated, never assumed"
        );
    }

    #[test]
    fn an_unknown_profile_fails_instead_of_running_somewhere_else() {
        let directory = tempfile::tempdir().unwrap();
        let repo = directory.path().join("repo");
        write(&repo.join(REPO_CONFIG), "[profile.local]\ncontext = \"c\"\nnamespace = \"n\"\n");
        let config = load(&TestEnvironment::new(directory.path().join("home")), Some(&repo)).unwrap();

        let error = resolve(&config, Some("nope")).unwrap_err();
        assert_eq!(
            error
                .downcast_ref::<crate::error::CliFailure>()
                .map(|failure| failure.code.as_str()),
            Some(codes::ENV_PROFILE_UNRESOLVED)
        );
        assert!(error.to_string().contains("local"), "{error}");

        let missing_default = resolve(&config, None).unwrap_err();
        assert_eq!(
            missing_default
                .downcast_ref::<crate::error::CliFailure>()
                .map(|failure| failure.code.as_str()),
            Some(codes::ENV_PROFILE_UNRESOLVED)
        );
    }

    #[test]
    fn a_profile_never_falls_back_to_the_callers_current_context() {
        // `context` has no default: a profile that does not say which cluster it means would
        // mean a different one on every machine.
        let error = toml::from_str::<ExecConfig>("[profile.local]\nnamespace = \"n\"\n")
            .expect_err("context is required");
        assert!(error.to_string().contains("context"), "{error}");
    }

    #[test]
    fn a_profile_names_the_backend_that_provides_it() {
        let config: ExecConfig = toml::from_str(
            "[profile.local]\nkind = \"docker\"\ncontext = \"c\"\nnamespace = \"n\"\n",
        )
        .unwrap();
        assert_eq!(config.profile["local"].kind, ProviderKind::Docker);

        let config: ExecConfig =
            toml::from_str("[profile.local]\ncontext = \"c\"\nnamespace = \"n\"\n").unwrap();
        assert_eq!(
            config.profile["local"].kind,
            ProviderKind::Kubernetes,
            "a profile that says nothing is the shipped backend, as before"
        );
    }

    #[test]
    fn no_config_anywhere_is_not_an_error_it_is_just_no_environment() {
        let directory = tempfile::tempdir().unwrap();
        let config = load(
            &TestEnvironment::new(directory.path().join("home")),
            Some(directory.path()),
        )
        .unwrap();
        assert!(config.profile.is_empty());
        assert!(config.default_profile.is_none());
    }
}
