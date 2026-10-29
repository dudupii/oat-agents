// SPDX-License-Identifier: MIT
// Copyright (c) 2026 oat-agents contributors

pub mod memory;
pub mod repository;

use crate::error::{codes, err};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Backend {
    Claude,
    Codex,
}

impl Backend {
    pub fn label(&self) -> &'static str {
        match self {
            Backend::Claude => "Claude Code",
            Backend::Codex => "Codex",
        }
    }
}

impl std::str::FromStr for Backend {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        match s {
            "claude" => Ok(Backend::Claude),
            "codex" => Ok(Backend::Codex),
            other => Err(err(
                codes::INVALID_CLI_ARGUMENTS,
                format!("unknown backend: {other}"),
            )),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartLocation {
    Fresh,
    Existing,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelSetting {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SkillFile {
    pub relative_path: PathBuf,
    pub contents: Vec<u8>,
    pub executable: bool,
}

#[derive(Debug, Clone)]
pub struct SkillRef {
    pub name: String,
    pub files: Vec<SkillFile>,
}

#[derive(Debug, Clone)]
pub struct RoleDefinition {
    pub name: String,
    pub instructions: String,
    pub models: BTreeMap<Backend, ModelSetting>,
    pub skills: Vec<SkillRef>,
    pub start: StartLocation,
    pub exec_environment: bool,
    pub prior_verification: bool,
    /// The plugin's default for how many Dispatches of this role one Run may run at once;
    /// `None` is no limit. The Run's effective limit is in its record.
    pub max_concurrent: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum CoreRole {
    Meta,
    Console,
}

impl CoreRole {
    pub fn name(&self) -> &'static str {
        match self {
            CoreRole::Meta => "oat-meta",
            CoreRole::Console => "oat-console",
        }
    }
}

#[derive(Debug, Clone)]
pub struct CoreRoleDefinition {
    pub instructions: String,
    pub models: BTreeMap<Backend, ModelSetting>,
    pub skills: Vec<SkillRef>,
    /// The backend a plugin chose for this core role; only the console takes one.
    pub backend: Option<Backend>,
    /// The rework loop's bounds this plugin's teams run under (ADR-0009); only `oat-meta`
    /// takes them, and a repository overrides them in `.oat/rework.toml`.
    pub rework: crate::findings::ReworkSettings,
}

/// Given a role name, its definition; nothing else in the crate knows where roles come from
/// (ticket Architecture). `role::memory::InMemoryCatalog` is for tests;
/// `plugin::catalog::PluginCatalog` builds one from a set of loaded plugins.
pub trait RoleCatalog {
    fn role_names(&self) -> Vec<String>;
    fn role(&self, name: &str) -> Result<&RoleDefinition>;
    fn core_role(&self, role: CoreRole) -> Result<&CoreRoleDefinition>;
}

pub fn unknown_role_error(catalog: &dyn RoleCatalog, name: &str) -> anyhow::Error {
    let mut known = catalog.role_names();
    known.sort();
    err(
        codes::UNKNOWN_ROLE,
        format!("unknown role '{name}'; known roles: {}", known.join(", ")),
    )
}

pub fn missing_core_instructions_error(role: CoreRole) -> anyhow::Error {
    err(
        codes::CORE_ROLE_INSTRUCTIONS_MISSING,
        format!("no instructions supplied for core role '{}'", role.name()),
    )
}
