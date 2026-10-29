// SPDX-License-Identifier: MIT
// Copyright (c) 2026 oat-agents contributors

//! `.oat/plugins.toml` (ticket Scope): the ordered, pinned list of plugins a repository
//! declares, and turning that list into resolved, trust-checked plugin directories a `RoleCatalog`
//! can be built from. `plugin::` (singular) is F4's plugin *format* and loader; this module is
//! F5's plugin *sources* — where a pinned plugin comes from, and whether this machine trusts it.

pub mod embedded;
pub mod git;
pub mod snapshot;

use crate::environment::Environment;
use crate::error::{codes, err};
use crate::hashing::hash_dir;
use crate::plugin::format::{parse_toml, PluginManifest};
use crate::trust::{untrusted_plugin_error, TrustKey, TrustStore};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// The user-facing pinned-plugin-list file (ticket Contracts: "a user-facing file format;
/// changing it after this spec is a breaking change").
pub const REPO_PLUGINS_CONFIG: &str = ".oat/plugins.toml";

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    #[serde(default, rename = "plugin")]
    plugins: Vec<RawPin>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RawPin {
    source: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    commit: Option<String>,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    content_hash: Option<String>,
}

/// The `commit` of a git pin that follows the plugin repository's default branch instead of
/// one commit (ADR-0008).
pub const LATEST: &str = "latest";

/// A git pin's `commit`: a full commit id, lowercased, or `latest`.
fn validate_git_commit(commit: &str) -> Result<String, String> {
    if commit == LATEST {
        return Ok(LATEST.to_string());
    }
    if commit.len() != 40 || !commit.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(format!("'commit' must be a full 40-character commit id or '{LATEST}', got '{commit}'"));
    }
    Ok(commit.to_lowercase())
}

/// One validated entry from `.oat/plugins.toml` (ticket Architecture: "Pins").
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginPin {
    Embedded { name: String },
    Git { name: String, url: String, commit: String },
    Path { name: String, path: String, content_hash: String },
}

impl PluginPin {
    fn to_raw(&self) -> RawPin {
        match self {
            PluginPin::Embedded { name } => RawPin {
                source: "embedded".to_string(),
                name: Some(name.clone()),
                url: None,
                commit: None,
                path: None,
                content_hash: None,
            },
            PluginPin::Git { name, url, commit } => RawPin {
                source: "git".to_string(),
                name: Some(name.clone()),
                url: Some(url.clone()),
                commit: Some(commit.clone()),
                path: None,
                content_hash: None,
            },
            PluginPin::Path { name, path, content_hash } => RawPin {
                source: "path".to_string(),
                name: Some(name.clone()),
                url: None,
                commit: None,
                path: Some(path.clone()),
                content_hash: Some(content_hash.clone()),
            },
        }
    }
}

fn parse_pin(index: usize, raw: RawPin) -> Result<PluginPin> {
    let invalid = |message: String| err(codes::PLUGIN_PIN_INVALID, format!("plugins.toml entry {index}: {message}"));
    match raw.source.as_str() {
        "embedded" => {
            if raw.url.is_some() || raw.commit.is_some() || raw.path.is_some() || raw.content_hash.is_some() {
                return Err(invalid("an embedded pin takes only 'name'".to_string()));
            }
            let name = raw.name.unwrap_or_else(|| embedded::EXAMPLE_NAME.to_string());
            embedded::require_known_embedded_plugin(&name)?;
            Ok(PluginPin::Embedded { name })
        }
        "git" => {
            let name = raw.name.ok_or_else(|| invalid("a git pin requires 'name'".to_string()))?;
            let url = raw.url.ok_or_else(|| invalid("a git pin requires 'url'".to_string()))?;
            let commit = raw.commit.ok_or_else(|| invalid("a git pin requires 'commit'".to_string()))?;
            if raw.path.is_some() || raw.content_hash.is_some() {
                return Err(invalid("a git pin takes only 'name', 'url' and 'commit'".to_string()));
            }
            let commit = validate_git_commit(&commit).map_err(invalid)?;
            Ok(PluginPin::Git { name, url, commit })
        }
        "path" => {
            let name = raw.name.ok_or_else(|| invalid("a path pin requires 'name'".to_string()))?;
            let path = raw.path.ok_or_else(|| invalid("a path pin requires 'path'".to_string()))?;
            let content_hash = raw.content_hash.ok_or_else(|| invalid("a path pin requires 'content_hash'".to_string()))?;
            if raw.url.is_some() || raw.commit.is_some() {
                return Err(invalid("a path pin takes only 'name', 'path' and 'content_hash'".to_string()));
            }
            if Path::new(&path).is_absolute() || path.split('/').any(|part| part == "..") {
                return Err(invalid(format!("'path' must be a relative path inside the repository, got '{path}'")));
            }
            validate_content_hash(&content_hash).map_err(invalid)?;
            Ok(PluginPin::Path { name, path, content_hash })
        }
        other => Err(invalid(format!("unknown source '{other}'; expected 'embedded', 'git' or 'path'"))),
    }
}

fn validate_content_hash(hash: &str) -> Result<(), String> {
    let hex = hash
        .strip_prefix("sha256:")
        .ok_or_else(|| format!("'content_hash' must be 'sha256:<64 hex chars>', got '{hash}'"))?;
    if hex.len() != 64 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(format!("'content_hash' must be 'sha256:<64 hex chars>', got '{hash}'"));
    }
    Ok(())
}

fn parse_config_text(text: &str) -> Result<Vec<PluginPin>> {
    let raw: RawConfig = toml::from_str(text)
        .map_err(|e| err(codes::PLUGIN_PIN_INVALID, format!("{REPO_PLUGINS_CONFIG} is malformed: {e}")))?;
    raw.plugins.into_iter().enumerate().map(|(i, p)| parse_pin(i, p)).collect()
}

pub fn render_config(pins: &[PluginPin]) -> String {
    let raw = RawConfig { plugins: pins.iter().map(PluginPin::to_raw).collect() };
    toml::to_string_pretty(&raw).expect("plugin pins always serialize")
}

/// `$(git rev-parse --git-dir)/oat/plugins.toml` — `init --local`'s file, checked before the
/// repository's own (ticket Architecture: "`meta fire` and the console read the local one first
/// when it exists").
pub fn local_config_path(repo: &Path) -> Option<PathBuf> {
    let output = crate::worktree::run_git(repo, &["rev-parse", "--git-dir"]).ok()?;
    if !output.status.success() {
        return None;
    }
    let git_dir = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let git_dir = if Path::new(&git_dir).is_absolute() {
        PathBuf::from(git_dir)
    } else {
        repo.join(git_dir)
    };
    Some(git_dir.join("oat").join("plugins.toml"))
}

pub fn repo_config_path(repo: &Path) -> PathBuf {
    repo.join(REPO_PLUGINS_CONFIG)
}

/// The repository's declared plugin pins, local override first, or `None` when neither file
/// exists — an uninitialised repository (ticket Scope: "`init`'s never overwriting" and
/// "`meta fire` gates: an uninitialised repository is told to run `init`").
pub fn load_pins(repo: &Path) -> Result<Option<(PathBuf, Vec<PluginPin>)>> {
    if let Some(local) = local_config_path(repo) {
        if local.is_file() {
            let text = std::fs::read_to_string(&local)?;
            return Ok(Some((local, parse_config_text(&text)?)));
        }
    }
    let repo_path = repo_config_path(repo);
    if repo_path.is_file() {
        let text = std::fs::read_to_string(&repo_path)?;
        return Ok(Some((repo_path, parse_config_text(&text)?)));
    }
    Ok(None)
}

/// A pin resolved to a concrete, verified, on-disk plugin directory, ready for `plugin::load_catalog`.
#[derive(Debug, Clone)]
pub struct ResolvedPlugin {
    pub declared_name: String,
    pub dir: PathBuf,
    pub trust_key: TrustKey,
    /// The exact version that was loaded. It is the trust key's version, except for a git pin
    /// that follows `latest`: that is trusted as `latest` and loaded at the commit it resolved to.
    pub version: String,
}

impl ResolvedPlugin {
    pub fn source_label(&self) -> String {
        self.trust_key.source_label()
    }

    pub fn version(&self) -> String {
        self.version.clone()
    }
}

/// `init`'s constructors: each pin an operator can choose non-interactively (ticket Scope:
/// "non-interactive flags exist for every choice"). Unlike `resolve_pin`, these never fetch a
/// git commit — a git pin's declared name is only verified the first time it is resolved — but
/// a path pin's content hash is computed now, from whatever is on disk, since pinning "now" is
/// the whole point of choosing it at `init` time.
pub fn embedded_pin(name: impl Into<String>) -> Result<PluginPin> {
    let name = name.into();
    embedded::require_known_embedded_plugin(&name)?;
    Ok(PluginPin::Embedded { name })
}

pub fn git_pin(name: impl Into<String>, url: impl Into<String>, commit: impl Into<String>) -> Result<PluginPin> {
    let (name, url, commit) = (name.into(), url.into(), commit.into());
    let commit = validate_git_commit(&commit).map_err(|message| err(codes::PLUGIN_PIN_INVALID, message))?;
    Ok(PluginPin::Git { name, url, commit })
}

pub fn path_pin_from_current_content(repo: &Path, path: impl Into<String>, name: impl Into<String>) -> Result<PluginPin> {
    let path = path.into();
    let dir = repo.join(&path);
    if !dir.is_dir() {
        return Err(err(
            codes::PLUGIN_LOAD_FAILED,
            format!("'{path}' does not exist inside the repository"),
        ));
    }
    let content_hash = hash_dir(&dir).map_err(|e| err(codes::PLUGIN_LOAD_FAILED, format!("could not hash '{path}': {e}")))?;
    Ok(PluginPin::Path { name: name.into(), path, content_hash })
}

fn read_declared_name(dir: &Path) -> Result<String> {
    let path = dir.join("oat-plugin.toml");
    let text = std::fs::read_to_string(&path)
        .map_err(|e| err(codes::PLUGIN_LOAD_FAILED, format!("could not read {}: {e}", path.display())))?;
    let manifest: PluginManifest = parse_toml("oat-plugin.toml", &text).map_err(|m| err(codes::PLUGIN_LOAD_FAILED, m))?;
    Ok(manifest.name)
}

fn check_declared_name(dir: &Path, expected: &str, context: &str) -> Result<String> {
    let declared = read_declared_name(dir)?;
    if declared != expected {
        return Err(err(
            codes::PLUGIN_NAME_MISMATCH,
            format!(
                "{context} declares name '{expected}' but the plugin at '{}' declares '{declared}'",
                dir.display()
            ),
        ));
    }
    Ok(declared)
}

/// Resolves one pin to a verified plugin directory: fetches and re-verifies a git commit,
/// re-hashes and compares an in-repository path, or materializes the embedded example — then
/// checks the plugin's own declared name against the pin's `name` in every case.
pub fn resolve_pin(repo: &Path, home: &Path, pin: &PluginPin) -> Result<ResolvedPlugin> {
    match pin {
        PluginPin::Embedded { name } => {
            embedded::require_known_embedded_plugin(name)?;
            if embedded::is_empty() {
                return Err(err(
                    codes::PLUGIN_LOAD_FAILED,
                    "the embedded example plugin has no content in this build yet",
                ));
            }
            let version = embedded::example_version().to_string();
            // The materialization is keyed by content hash, not the binary version: two
            // builds of the same version can embed different content (a dev build between
            // releases), and a copy an earlier binary left behind must not keep serving as
            // this one's plugin. A differently-keyed directory is simply never read again.
            let content_hash = embedded::example_content_hash();
            let content_segment = content_hash.rsplit(':').next().unwrap_or(&content_hash);
            let dir = home.join(".oat").join("plugins").join("embedded").join(content_segment);
            if !dir.join("oat-plugin.toml").is_file() {
                embedded::materialize_example(&dir)?;
            }
            let declared = check_declared_name(&dir, name, "the embedded pin")?;
            Ok(ResolvedPlugin {
                declared_name: declared,
                dir,
                trust_key: TrustKey::Embedded { name: name.clone(), version: version.clone() },
                version,
            })
        }
        PluginPin::Git { name, url, commit } => {
            let resolved_commit = if commit == LATEST { git::latest_commit(url)? } else { commit.clone() };
            let dir = git::ensure_commit(home, url, &resolved_commit)?;
            let declared = check_declared_name(&dir, name, &format!("the git pin for '{url}'"))?;
            Ok(ResolvedPlugin {
                declared_name: declared,
                dir,
                trust_key: TrustKey::Git { url: url.clone(), commit: commit.clone() },
                version: resolved_commit,
            })
        }
        PluginPin::Path { name, path, content_hash } => {
            let dir = repo.join(path);
            if !dir.is_dir() {
                return Err(err(
                    codes::PLUGIN_LOAD_FAILED,
                    format!("the path pin '{path}' does not exist inside the repository"),
                ));
            }
            let canonical_dir = dir
                .canonicalize()
                .map_err(|e| err(codes::PLUGIN_LOAD_FAILED, format!("could not resolve path pin '{path}': {e}")))?;
            let canonical_repo = repo
                .canonicalize()
                .map_err(|e| err(codes::PLUGIN_LOAD_FAILED, format!("could not resolve repository path: {e}")))?;
            if !canonical_dir.starts_with(&canonical_repo) {
                return Err(err(
                    codes::PLUGIN_PIN_INVALID,
                    format!("path pin '{path}' escapes the repository"),
                ));
            }
            let actual_hash = hash_dir(&dir)
                .map_err(|e| err(codes::PLUGIN_LOAD_FAILED, format!("could not hash path pin '{path}': {e}")))?;
            if &actual_hash != content_hash {
                return Err(err(
                    codes::PLUGIN_PIN_MISMATCH,
                    format!(
                        "path pin '{path}' is pinned to {content_hash} but its current content hashes to {actual_hash}; \
                         re-pin it (or restore its original content) to continue"
                    ),
                ));
            }
            let declared = check_declared_name(&dir, name, &format!("the path pin '{path}'"))?;
            Ok(ResolvedPlugin {
                declared_name: declared,
                dir,
                trust_key: TrustKey::Path { content_hash: content_hash.clone() },
                version: content_hash.clone(),
            })
        }
    }
}

pub fn resolve_all(repo: &Path, home: &Path, pins: &[PluginPin]) -> Result<Vec<ResolvedPlugin>> {
    pins.iter().map(|pin| resolve_pin(repo, home, pin)).collect()
}

/// The full gate `meta fire` and the console launch run before anything else starts (ticket
/// Scope): an uninitialised repository, a resolution failure (a mismatched git tree, a
/// mismatched content hash, a name that does not match its entry, two plugins defining the same
/// role), and an untrusted plugin are all reported here, before a worktree or a session exists.
/// Returns the resolved plugins and the catalog built from them.
pub fn gate(repo: &Path, env: &dyn Environment) -> Result<(Vec<ResolvedPlugin>, crate::plugin::PluginCatalog)> {
    let Some((config_path, pins)) = load_pins(repo)? else {
        return Err(err(
            codes::PLUGIN_UNINITIALISED,
            format!(
                "'{}' has not been initialised for oat-agents; run `oat-agents init` (or `oat-agents init --local`) first",
                repo.display()
            ),
        ));
    };
    if pins.is_empty() {
        return Err(err(
            codes::PLUGIN_UNINITIALISED,
            format!("{} names no plugins; run `oat-agents init` to choose one", config_path.display()),
        ));
    }

    let home = env
        .home_dir()
        .ok_or_else(|| err(codes::INTERNAL_ERROR, "no home directory available"))?;
    let resolved = resolve_all(repo, &home, &pins)?;

    let dirs: Vec<PathBuf> = resolved.iter().map(|r| r.dir.clone()).collect();
    let (catalog, _refs) =
        crate::plugin::load_catalog(&dirs).map_err(|errors| crate::plugin::errors_to_cli_failure(&errors))?;

    let trust = TrustStore::open(env)?;
    for plugin in &resolved {
        if !trust.is_trusted(&plugin.trust_key) {
            return Err(untrusted_plugin_error(&plugin.declared_name, &plugin.trust_key));
        }
    }

    Ok((resolved, catalog))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::environment::TestEnvironment;

    fn write(path: &Path, body: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    fn init_repo(dir: &Path) {
        std::fs::create_dir_all(dir).unwrap();
        let status = std::process::Command::new("git").current_dir(dir).args(["init", "--quiet"]).status().unwrap();
        assert!(status.success());
    }

    #[test]
    fn a_repository_with_no_plugins_toml_is_uninitialised() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("repo");
        init_repo(&repo);
        let env = TestEnvironment::new(temp.path().join("home"));
        let error = gate(&repo, &env).unwrap_err();
        assert_eq!(
            error.downcast_ref::<crate::error::CliFailure>().map(|f| f.code.as_str()),
            Some(codes::PLUGIN_UNINITIALISED)
        );
        assert!(error.to_string().to_lowercase().contains("init"), "{error}");
    }

    #[test]
    fn a_path_pin_verifies_its_content_hash_and_declared_name() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("repo");
        init_repo(&repo);
        let plugin_dir = repo.join("local-plugin");
        write(&plugin_dir.join("oat-plugin.toml"), "format_version = 1\nname = \"local-plugin\"\ndescription = \"d\"\n");

        let hash = hash_dir(&plugin_dir).unwrap();
        let pin = PluginPin::Path {
            name: "local-plugin".to_string(),
            path: "local-plugin".to_string(),
            content_hash: hash.clone(),
        };
        let home = temp.path().join("home");
        let resolved = resolve_pin(&repo, &home, &pin).unwrap();
        assert_eq!(resolved.declared_name, "local-plugin");
        assert_eq!(resolved.version(), hash);

        // Editing the plugin after it was pinned voids the pin.
        write(&plugin_dir.join("oat-plugin.toml"), "format_version = 1\nname = \"local-plugin\"\ndescription = \"changed\"\n");
        let error = resolve_pin(&repo, &home, &pin).unwrap_err();
        assert_eq!(
            error.downcast_ref::<crate::error::CliFailure>().map(|f| f.code.as_str()),
            Some(codes::PLUGIN_PIN_MISMATCH)
        );
    }

    #[test]
    fn a_path_pin_whose_declared_name_differs_from_its_entry_fails_naming_both() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("repo");
        init_repo(&repo);
        let plugin_dir = repo.join("local-plugin");
        write(&plugin_dir.join("oat-plugin.toml"), "format_version = 1\nname = \"actual-name\"\ndescription = \"d\"\n");
        let hash = hash_dir(&plugin_dir).unwrap();
        let pin = PluginPin::Path {
            name: "expected-name".to_string(),
            path: "local-plugin".to_string(),
            content_hash: hash,
        };
        let home = temp.path().join("home");
        let error = resolve_pin(&repo, &home, &pin).unwrap_err();
        assert_eq!(
            error.downcast_ref::<crate::error::CliFailure>().map(|f| f.code.as_str()),
            Some(codes::PLUGIN_NAME_MISMATCH)
        );
        assert!(error.to_string().contains("expected-name"));
        assert!(error.to_string().contains("actual-name"));
    }

    #[test]
    fn config_round_trips_through_render_and_parse() {
        let pins = vec![
            PluginPin::Embedded { name: "example".to_string() },
            PluginPin::Git { name: "team".to_string(), url: "https://example.com/p.git".to_string(), commit: "a".repeat(40) },
            PluginPin::Path { name: "local".to_string(), path: "plugins/local".to_string(), content_hash: format!("sha256:{}", "b".repeat(64)) },
        ];
        let rendered = render_config(&pins);
        let parsed = parse_config_text(&rendered).unwrap();
        assert_eq!(parsed, pins);
    }

    fn git(dir: &Path, args: &[&str]) -> String {
        let output = std::process::Command::new("git").current_dir(dir).args(args).output().unwrap();
        assert!(output.status.success(), "git {args:?} failed");
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    fn commit_plugin(source: &Path, description: &str) -> String {
        write(&source.join("oat-plugin.toml"), &format!("format_version = 1\nname = \"team\"\ndescription = \"{description}\"\n"));
        git(source, &["add", "."]);
        git(source, &["commit", "--quiet", "-m", description]);
        git(source, &["rev-parse", "HEAD"])
    }

    #[test]
    fn a_latest_pin_is_trusted_as_latest_and_loads_the_commit_the_remote_is_at_now() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("repo");
        init_repo(&repo);
        let source = temp.path().join("source");
        init_repo(&source);
        git(&source, &["config", "user.email", "test@example.com"]);
        git(&source, &["config", "user.name", "Test"]);
        let first = commit_plugin(&source, "first");

        let url = source.to_string_lossy().to_string();
        let pin = parse_config_text(&format!(
            "[[plugin]]\nsource = \"git\"\nname = \"team\"\nurl = \"{url}\"\ncommit = \"latest\"\n"
        ))
        .unwrap()
        .remove(0);
        let home = temp.path().join("home");

        let resolved = resolve_pin(&repo, &home, &pin).unwrap();
        assert_eq!(resolved.trust_key, TrustKey::Git { url: url.clone(), commit: LATEST.to_string() });
        assert_eq!(resolved.version(), first, "a Run records the commit it actually loaded");

        // The plugin moves on; the same pin, and the same trust, follow it.
        let second = commit_plugin(&source, "second");
        let resolved = resolve_pin(&repo, &home, &pin).unwrap();
        assert_eq!(resolved.version(), second);
        assert_eq!(resolved.trust_key.version(), LATEST);
        assert!(std::fs::read_to_string(resolved.dir.join("oat-plugin.toml")).unwrap().contains("second"));
    }

    #[test]
    fn an_invalid_commit_length_is_refused_at_parse_time() {
        let text = "[[plugin]]\nsource = \"git\"\nname = \"team\"\nurl = \"https://example.com/p.git\"\ncommit = \"deadbeef\"\n";
        let error = parse_config_text(text).unwrap_err();
        assert_eq!(
            error.downcast_ref::<crate::error::CliFailure>().map(|f| f.code.as_str()),
            Some(codes::PLUGIN_PIN_INVALID)
        );
    }
}
