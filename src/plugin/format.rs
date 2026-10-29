// SPDX-License-Identifier: MIT
// Copyright (c) 2026 oat-agents contributors

//! The plugin format's on-disk shape (ADR-0002): the TOML types every plugin file
//! deserializes into, and the directory walk `load` uses to find them. Nothing here validates
//! the *content* of a plugin — only how its files parse. `load` turns a parsed value into a
//! `role::RoleDefinition`-shaped thing or an error; `catalog` combines several plugins.

use serde::Deserialize;
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// The only format version this crate understands. A plugin naming a different version is
/// refused before any of its other files are read (ADR-0002: "never half-parsed").
pub const SUPPORTED_FORMAT_VERSION: u32 = 1;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginManifest {
    pub format_version: u32,
    pub name: String,
    pub description: String,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StartLocationToml {
    Fresh,
    Existing,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelToml {
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub reasoning_effort: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoleToml {
    pub description: String,
    pub start: StartLocationToml,
    #[serde(default)]
    pub exec_environment: bool,
    #[serde(default)]
    pub prior_verification: bool,
    /// How many of this role's Dispatches may run at once in one Run; absent means no limit.
    /// A repository lowers or raises it in `.oat/roles.toml`.
    #[serde(default)]
    pub max_concurrent: Option<u32>,
    #[serde(default)]
    pub skills: Vec<String>,
    #[serde(default)]
    pub model: BTreeMap<String, ModelToml>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoreRoleToml {
    /// The backend the core role runs on. Only `core/oat-console.toml` may set it: the console
    /// is launched from no command that could choose one, while `oat-meta`'s backend is the
    /// Run's, chosen by `meta fire --agent`.
    #[serde(default)]
    pub backend: Option<String>,
    #[serde(default)]
    pub skills: Vec<String>,
    #[serde(default)]
    pub model: BTreeMap<String, ModelToml>,
    /// The rework loop's bounds (ADR-0009). Only `core/oat-meta.toml` may set them: the loop is
    /// the meta-agent's to run.
    #[serde(default)]
    pub rework: Option<ReworkToml>,
}

/// One plugin's `[rework]` table in `core/oat-meta.toml`: each field the plugin leaves unset
/// keeps the core's default, and the repository's `.oat/rework.toml` overrides the result.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReworkToml {
    #[serde(default)]
    pub max_rounds: Option<u32>,
    #[serde(default)]
    pub p2_required: Option<bool>,
}

/// Parses `contents` as one of this module's TOML types, turning a syntax error, a type
/// mismatch or an unknown field into a message naming the file (the caller supplies `file`,
/// a plugin-relative path, since `toml`'s own error does not know it).
pub fn parse_toml<T: for<'de> Deserialize<'de>>(file: &str, contents: &str) -> Result<T, String> {
    toml::from_str(contents).map_err(|e| format!("{file} is malformed: {e}"))
}

/// The immediate subdirectories of `dir`, in name order, or `None` when `dir` does not exist —
/// a plugin with no `skills/` or no `roles/` is not malformed, it simply supplies none.
pub fn list_subdirectories(dir: &Path) -> io::Result<Option<Vec<(String, PathBuf)>>> {
    if !dir.is_dir() {
        return Ok(None);
    }
    let mut out = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            let name = entry.file_name().to_string_lossy().to_string();
            out.push((name, path));
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(Some(out))
}

/// Every file under `dir`, recursively, as a path relative to `dir` — used to collect a
/// skill's supporting files (ticket: "a skill's files with their modes, so F1's materialization
/// can keep scripts executable without re-reading the plugin").
pub fn list_files_recursive(dir: &Path) -> io::Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    collect_files(dir, dir, &mut out)?;
    out.sort();
    Ok(out)
}

fn collect_files(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_files(root, &path, out)?;
        } else if path.is_file() {
            out.push(path.strip_prefix(root).unwrap().to_path_buf());
        }
    }
    Ok(())
}

#[cfg(unix)]
pub fn is_executable(path: &Path) -> io::Result<bool> {
    use std::os::unix::fs::PermissionsExt;
    Ok(fs::metadata(path)?.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
pub fn is_executable(_path: &Path) -> io::Result<bool> {
    Ok(false)
}
