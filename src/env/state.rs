// SPDX-License-Identifier: MIT
// Copyright (c) 2026 oat-agents contributors

//! The environment registry and the execution ledger.
//!
//! The registry is how a command run anywhere inside a worktree finds the environment bound to
//! it, with no environment variable to inherit and no agent state to trust. The ledger is how
//! anyone can tell afterwards whether the work actually happened in there — and, since each
//! line carries the fingerprint of the code it ran against, whether that work still describes
//! the code as it stands.

use super::fingerprint::Fingerprint;
use super::{EnvRecord, ExecOutcome, ExecRequest, LedgerEntry};
use crate::environment::Environment;
use crate::error::{codes, err};
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

/// `OAT_AGENTS_ENV_DIR`, else `$XDG_STATE_HOME/oat-agents/env`, else
/// `~/.local/state/oat-agents/env`; the same ladder the workflow log climbs, so both land under
/// one state directory.
pub fn resolve_state_dir(environment: &dyn Environment) -> Option<PathBuf> {
    if let Some(explicit) = environment.var("OAT_AGENTS_ENV_DIR") {
        return Some(PathBuf::from(explicit));
    }
    if let Some(state) = environment.var("XDG_STATE_HOME") {
        return Some(PathBuf::from(state).join("oat-agents").join("env"));
    }
    environment.home_dir().map(|home| home.join(".local/state/oat-agents/env"))
}

pub struct EnvStore {
    dir: PathBuf,
}

impl EnvStore {
    /// Unlike the workflow log, missing state is fatal here: without it a command cannot know
    /// where it is supposed to run, and guessing is the failure this feature exists to prevent.
    pub fn open(environment: &dyn Environment) -> Result<Self> {
        let dir = resolve_state_dir(environment).ok_or_else(|| {
            err(
                codes::ENV_STATE_UNAVAILABLE,
                "no state directory is available; set OAT_AGENTS_ENV_DIR or HOME",
            )
        })?;
        Ok(Self { dir })
    }

    #[cfg(test)]
    pub fn at(dir: PathBuf) -> Self {
        Self { dir }
    }

    fn record_path(&self, env_id: &str) -> PathBuf {
        self.dir.join(format!("{}.json", super::sanitize(env_id)))
    }

    pub fn ledger_path(&self, env_id: &str) -> PathBuf {
        self.dir.join(format!("{}.jsonl", super::sanitize(env_id)))
    }

    fn run_path(&self, run_id: &str) -> PathBuf {
        self.dir.join(format!("run-{}.json", super::sanitize(run_id)))
    }

    /// The profile a Run was launched with. Role launches inherit it, so a meta-agent cannot
    /// accidentally put one role somewhere else by forgetting a flag.
    pub fn save_run_profile(&self, run_id: &str, profile: &str) -> Result<()> {
        fs::create_dir_all(&self.dir).with_context(|| format!("failed to create {}", self.dir.display()))?;
        let path = self.run_path(run_id);
        fs::write(&path, json!({ "profile": profile }).to_string())
            .with_context(|| format!("failed to write {}", path.display()))
    }

    pub fn run_profile(&self, run_id: &str) -> Option<String> {
        let body = fs::read_to_string(self.run_path(run_id)).ok()?;
        serde_json::from_str::<Value>(&body)
            .ok()?
            .get("profile")
            .and_then(Value::as_str)
            .map(str::to_owned)
    }

    pub fn save(&self, record: &EnvRecord) -> Result<()> {
        fs::create_dir_all(&self.dir).with_context(|| format!("failed to create {}", self.dir.display()))?;
        let path = self.record_path(&record.env_id);
        let body = serde_json::to_string_pretty(record).expect("env records always serialize");
        fs::write(&path, body).with_context(|| format!("failed to write {}", path.display()))
    }

    pub fn load(&self, env_id: &str) -> Result<EnvRecord> {
        let path = self.record_path(env_id);
        let body = fs::read_to_string(&path).map_err(|_| {
            err(
                codes::ENV_NOT_READY,
                format!("no execution environment is registered as '{env_id}'"),
            )
        })?;
        serde_json::from_str(&body).with_context(|| format!("failed to parse {}", path.display()))
    }

    pub fn remove(&self, env_id: &str) -> Result<()> {
        let path = self.record_path(env_id);
        if path.exists() {
            fs::remove_file(&path).with_context(|| format!("failed to remove {}", path.display()))?;
        }
        Ok(())
    }

    pub fn list(&self) -> Vec<EnvRecord> {
        let Ok(entries) = fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        let mut records: Vec<EnvRecord> = entries
            .filter_map(Result::ok)
            .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
            .filter_map(|entry| fs::read_to_string(entry.path()).ok())
            .filter_map(|body| serde_json::from_str::<EnvRecord>(&body).ok())
            .collect();
        records.sort_by_key(|record| record.created_ms);
        records
    }

    /// The environment a command in `cwd` belongs to: the most recently created one whose
    /// worktree contains it. Roles run one after another in the same worktree, so the newest
    /// binding is the live one; naming a role reaches past it when a caller needs to.
    pub fn find_for_cwd(&self, cwd: &Path, role: Option<&str>) -> Result<EnvRecord> {
        let here: Vec<EnvRecord> = self.list().into_iter().filter(|record| cwd.starts_with(&record.worktree)).collect();
        let candidate = here
            .iter()
            .rfind(|record| role.is_none_or(|role| record.role == role))
            .cloned();
        // An environment that is here but belongs to another role is a different mistake from
        // no environment at all, and saying which one it is saves the caller from looking for
        // the wrong thing.
        if let (None, Some(role), Some(present)) = (&candidate, role, here.last()) {
            return Err(err(
                codes::ENV_ROLE_MISMATCH,
                format!(
                    "{} is bound to the {} role's environment {}, not to {role}",
                    cwd.display(),
                    present.role,
                    present.env_id
                ),
            ));
        }
        candidate.ok_or_else(|| {
            err(
                codes::ENV_NOT_READY,
                format!(
                    "no execution environment is bound to {}; the launch that owns this worktree \
                     creates one, and `oat-agents env doctor` says why it did not",
                    cwd.display()
                ),
            )
        })
    }

    /// One line when a command starts, one when it comes back.
    ///
    /// Append-only, and never read back while writing, so two roles executing at once cannot
    /// cost each other anything.
    pub fn append_exec_start(&self, record: &EnvRecord, request: &ExecRequest, tree: &Fingerprint) -> Result<()> {
        // Following a dropped stream rejoins an execution that already has a start line; a
        // second one would double-count the command.
        if request.attach {
            return Ok(());
        }
        self.append_line(
            record,
            json!({
                "phase": "start",
                "ts": crate::event_log::now_iso(),
                "epoch_ms": super::now_ms(),
                "exec_id": request.exec_id,
                "argv": request.argv,
                "cwd": request.cwd.to_string_lossy(),
                "worktree": record.worktree.to_string_lossy(),
                "env_id": record.env_id,
                "role": record.role,
                "image_id": record.image_id,
                "profile": record.profile,
                "context": record.context,
                "namespace": record.namespace,
                "timeout_secs": request.timeout.map(|limit| limit.as_secs()),
                "tree": tree,
            }),
        )
    }

    /// `outcome` is `None` when the command did not come back: the timeout fired, or the
    /// stream broke. That is recorded rather than dropped, and it is why an entry can exist
    /// with no exit status.
    pub fn append_exec_end(
        &self,
        record: &EnvRecord,
        request: &ExecRequest,
        outcome: Option<&ExecOutcome>,
        tree: &Fingerprint,
    ) -> Result<()> {
        let Some(outcome) = outcome else {
            return Ok(());
        };
        self.append_line(
            record,
            json!({
                "phase": "end",
                "ts": crate::event_log::now_iso(),
                "epoch_ms": super::now_ms(),
                "exec_id": request.exec_id,
                "exit": outcome.exit_code,
                "ms": outcome.duration_ms,
                "attach": request.attach,
                "env_id": record.env_id,
                "tree": tree,
            }),
        )
    }

    fn append_line(&self, record: &EnvRecord, entry: Value) -> Result<()> {
        fs::create_dir_all(&self.dir).with_context(|| format!("failed to create {}", self.dir.display()))?;
        let mut line = serde_json::to_string(&entry).expect("ledger entries always serialize");
        line.push('\n');
        let path = self.ledger_path(&record.env_id);
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .with_context(|| format!("failed to open {}", path.display()))?;
        file.write_all(line.as_bytes())
            .with_context(|| format!("failed to append to {}", path.display()))
    }

    /// The commands one environment ran, with each one's start and end folded back together. A
    /// command still outstanding keeps its entry, with no exit status, because that is a fact
    /// about the dispatch too.
    pub fn read_ledger(&self, env_id: &str) -> Vec<LedgerEntry> {
        fold(&read_lines(&self.ledger_path(env_id)))
    }

    /// Every command recorded against this worktree, whichever environment ran it.
    pub fn ledgers_for_worktree(&self, worktree: &Path, since_ms: Option<u64>) -> Vec<LedgerEntry> {
        let worktree = worktree.canonicalize().unwrap_or_else(|_| worktree.to_path_buf());
        let Ok(entries) = fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        let mut found: Vec<LedgerEntry> = entries
            .filter_map(Result::ok)
            .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "jsonl"))
            .flat_map(|entry| fold(&read_lines(&entry.path())))
            .filter(|entry| entry.worktree == worktree)
            .filter(|entry| since_ms.is_none_or(|since| entry.started_ms >= since))
            .collect();
        // `started_ms` alone is not enough to order two entries: it comes from wall-clock time
        // at write time, and two writes landing in the same millisecond tie. Break the tie on
        // `env_id` then `exec_id` so the order never depends on `fs::read_dir`'s enumeration.
        found.sort_by(|a, b| {
            a.started_ms.cmp(&b.started_ms).then_with(|| a.env_id.cmp(&b.env_id)).then_with(|| a.exec_id.cmp(&b.exec_id))
        });
        found
    }

    /// What a dispatch did inside its environment, in the numbers worth carrying into the
    /// workflow log.
    pub fn ledger_summary(&self, env_id: &str) -> Value {
        let entries = self.read_ledger(env_id);
        let failed = entries.iter().filter(|entry| entry.exit.is_some_and(|exit| exit != 0)).count();
        let unfinished = entries.iter().filter(|entry| !entry.finished()).count();
        let total_ms: u64 = entries.iter().filter_map(|entry| entry.ms).sum();
        json!({
            "commands": entries.len(),
            "failed": failed,
            "unfinished": unfinished,
            "total_ms": total_ms,
        })
    }
}

fn read_lines(path: &Path) -> Vec<Value> {
    let Ok(body) = fs::read_to_string(path) else {
        return Vec::new();
    };
    body.lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

/// Folds start and end lines back into one entry each, in the order the commands started.
fn fold(lines: &[Value]) -> Vec<LedgerEntry> {
    let mut order: Vec<String> = Vec::new();
    let mut entries: HashMap<String, LedgerEntry> = HashMap::new();
    for line in lines {
        let exec_id = string(line, "exec_id").unwrap_or_default();
        match line.get("phase").and_then(Value::as_str) {
            Some("end") => {
                if let Some(entry) = entries.get_mut(&exec_id) {
                    entry.exit = line.get("exit").and_then(Value::as_i64).map(|exit| exit as i32);
                    entry.ms = line.get("ms").and_then(Value::as_u64);
                    entry.attached = line.get("attach").and_then(Value::as_bool).unwrap_or(false);
                    entry.tree_end = tree(line);
                }
                // An end with no start belongs to a ledger that was truncated. There is no
                // argv to attach it to, so there is nothing honest to report about it.
            }
            _ => {
                let legacy = line.get("phase").is_none();
                let entry = LedgerEntry {
                    exec_id: exec_id.clone(),
                    argv: line
                        .get("argv")
                        .and_then(Value::as_array)
                        .map(|argv| argv.iter().filter_map(Value::as_str).map(str::to_owned).collect())
                        .unwrap_or_default(),
                    cwd: path_field(line, "cwd"),
                    worktree: path_field(line, "worktree"),
                    env_id: string(line, "env_id").unwrap_or_default(),
                    role: string(line, "role").unwrap_or_default(),
                    image_id: string(line, "image_id").unwrap_or_default(),
                    profile: string(line, "profile").unwrap_or_default(),
                    context: string(line, "context").unwrap_or_default(),
                    namespace: string(line, "namespace").unwrap_or_default(),
                    started_ms: line.get("epoch_ms").and_then(Value::as_u64).unwrap_or(0),
                    ts: string(line, "ts").unwrap_or_default(),
                    tree_start: tree(line).unwrap_or(Fingerprint::Unknown {
                        reason: super::fingerprint::UnknownReason::GitFailed { status: None },
                    }),
                    tree_end: None,
                    exit: legacy.then(|| line.get("exit").and_then(Value::as_i64)).flatten().map(|exit| exit as i32),
                    ms: legacy.then(|| line.get("ms").and_then(Value::as_u64)).flatten(),
                    timeout_secs: line.get("timeout_secs").and_then(Value::as_u64),
                    attached: line.get("attach").and_then(Value::as_bool).unwrap_or(false),
                };
                if entries.insert(exec_id.clone(), entry).is_none() {
                    order.push(exec_id);
                }
            }
        }
    }
    order.into_iter().filter_map(|exec_id| entries.remove(&exec_id)).collect()
}

fn string(line: &Value, key: &str) -> Option<String> {
    line.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn path_field(line: &Value, key: &str) -> PathBuf {
    PathBuf::from(string(line, key).unwrap_or_default())
}

fn tree(line: &Value) -> Option<Fingerprint> {
    serde_json::from_value(line.get("tree")?.clone()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env::{EnvRecord, ExecRequest};

    fn record(env_id: &str, role: &str, worktree: &Path, created_ms: u64) -> EnvRecord {
        EnvRecord {
            env_id: env_id.to_owned(),
            run_id: Some("run-1".to_owned()),
            role: role.to_owned(),
            profile: "local".to_owned(),
            kind: crate::env::config::ProviderKind::Kubernetes,
            worktree: worktree.to_path_buf(),
            container_path: worktree.to_path_buf(),
            image_ref: "oat-x:abc".to_owned(),
            image_id: "sha256:0123456789abcdef".to_owned(),
            pod: env_id.to_owned(),
            namespace: "agents".to_owned(),
            context: "local".to_owned(),
            created_ms,
        }
    }

    #[test]
    fn a_command_finds_the_environment_bound_to_the_worktree_it_is_in() {
        let directory = tempfile::tempdir().unwrap();
        let store = EnvStore::at(directory.path().join("env"));
        let worktree = directory.path().join("worktree");
        std::fs::create_dir_all(worktree.join("src")).unwrap();
        store.save(&record("oat-a-worker-1", "worker", &worktree, 1)).unwrap();

        let found = store.find_for_cwd(&worktree.join("src"), None).unwrap();
        assert_eq!(found.env_id, "oat-a-worker-1");

        let elsewhere = store.find_for_cwd(directory.path(), None).unwrap_err();
        assert_eq!(
            elsewhere.downcast_ref::<crate::error::CliFailure>().map(|failure| failure.code.as_str()),
            Some(codes::ENV_NOT_READY),
            "a directory with no environment is never quietly given one"
        );
    }

    #[test]
    fn roles_share_a_worktree_in_turn_so_the_newest_binding_is_the_live_one() {
        let directory = tempfile::tempdir().unwrap();
        let store = EnvStore::at(directory.path().join("env"));
        let worktree = directory.path().join("worktree");
        std::fs::create_dir_all(&worktree).unwrap();
        store.save(&record("oat-a-worker-1", "worker", &worktree, 10)).unwrap();
        store.save(&record("oat-a-reviewer-1", "reviewer", &worktree, 20)).unwrap();

        assert_eq!(store.find_for_cwd(&worktree, None).unwrap().env_id, "oat-a-reviewer-1");
        assert_eq!(
            store.find_for_cwd(&worktree, Some("worker")).unwrap().env_id,
            "oat-a-worker-1",
            "naming the role reaches past the newest binding"
        );
    }

    #[test]
    fn an_environment_that_belongs_to_another_role_says_so() {
        let directory = tempfile::tempdir().unwrap();
        let store = EnvStore::at(directory.path().join("env"));
        let worktree = directory.path().join("worktree");
        std::fs::create_dir_all(&worktree).unwrap();
        store.save(&record("oat-a-worker-1", "worker", &worktree, 10)).unwrap();

        let error = store.find_for_cwd(&worktree, Some("reviewer")).unwrap_err();

        assert_eq!(
            error.downcast_ref::<crate::error::CliFailure>().map(|failure| failure.code.as_str()),
            Some(codes::ENV_ROLE_MISMATCH),
            "being in the wrong role's worktree is not the same as having no environment"
        );
        assert!(error.to_string().contains("worker"), "{error}");
    }

    fn known(digest: &str) -> Fingerprint {
        Fingerprint::Known {
            digest: digest.to_owned(),
            head: Some("0".repeat(40)),
            dirty: false,
            untracked: 0,
        }
    }

    fn request(exec_id: &str, argv: &[&str], cwd: &Path) -> ExecRequest {
        ExecRequest {
            exec_id: exec_id.to_owned(),
            argv: argv.iter().map(|value| (*value).to_owned()).collect(),
            cwd: cwd.to_path_buf(),
            timeout: None,
            attach: false,
        }
    }

    fn ran(store: &EnvStore, record: &EnvRecord, exec_id: &str, argv: &[&str], exit: i32, ms: u64, tree: &Fingerprint) {
        let request = request(exec_id, argv, &record.worktree);
        store.append_exec_start(record, &request, tree).unwrap();
        store
            .append_exec_end(record, &request, Some(&ExecOutcome { exit_code: exit, duration_ms: ms }), tree)
            .unwrap();
    }

    #[test]
    fn the_ledger_counts_what_ran_and_what_failed() {
        let directory = tempfile::tempdir().unwrap();
        let store = EnvStore::at(directory.path().join("env"));
        let worktree = directory.path().join("worktree");
        let record = record("oat-a-worker-1", "worker", &worktree, 1);
        let tree = known("aaaa");

        assert_eq!(store.ledger_summary(&record.env_id)["commands"], 0);
        ran(&store, &record, "e1", &["cargo", "test"], 0, 120, &tree);
        ran(&store, &record, "e2", &["cargo", "clippy"], 101, 30, &tree);

        let summary = store.ledger_summary(&record.env_id);
        assert_eq!(summary["commands"], 2);
        assert_eq!(summary["failed"], 1);
        assert_eq!(summary["unfinished"], 0);
        assert_eq!(summary["total_ms"], 150);

        let rows = store.read_ledger(&record.env_id);
        assert_eq!(rows[0].argv, ["cargo", "test"]);
        assert_eq!(rows[1].exit, Some(101));
        assert_eq!(rows[1].image_id, "sha256:0123456789abcdef");
        assert_eq!(rows[0].worktree, worktree);
        assert_eq!(rows[0].role, "worker");
        assert!(rows[0].settled_tree().is_some(), "the tree held still");
    }

    #[test]
    fn a_command_that_never_came_back_is_still_in_the_ledger() {
        let directory = tempfile::tempdir().unwrap();
        let store = EnvStore::at(directory.path().join("env"));
        let worktree = directory.path().join("worktree");
        let record = record("oat-a-worker-1", "worker", &worktree, 1);
        let tree = known("aaaa");
        let request = request("e1", &["cargo", "test"], &worktree);

        store.append_exec_start(&record, &request, &tree).unwrap();
        store.append_exec_end(&record, &request, None, &tree).unwrap();

        let rows = store.read_ledger(&record.env_id);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].argv, ["cargo", "test"]);
        assert_eq!(rows[0].exit, None);
        assert!(!rows[0].finished());
        let summary = store.ledger_summary(&record.env_id);
        assert_eq!(summary["commands"], 1);
        assert_eq!(summary["unfinished"], 1);
        assert_eq!(summary["failed"], 0, "not coming back is not a failure");
    }

    #[test]
    fn following_a_dropped_stream_closes_the_command_it_rejoined() {
        let directory = tempfile::tempdir().unwrap();
        let store = EnvStore::at(directory.path().join("env"));
        let worktree = directory.path().join("worktree");
        let record = record("oat-a-worker-1", "worker", &worktree, 1);
        let tree = known("aaaa");

        let started = request("e1", &["cargo", "test", "--offline"], &worktree);
        store.append_exec_start(&record, &started, &tree).unwrap();

        let rejoined = ExecRequest {
            exec_id: "e1".to_owned(),
            argv: Vec::new(),
            cwd: worktree.clone(),
            timeout: None,
            attach: true,
        };
        store.append_exec_start(&record, &rejoined, &tree).unwrap();
        store
            .append_exec_end(&record, &rejoined, Some(&ExecOutcome { exit_code: 0, duration_ms: 431_200 }), &tree)
            .unwrap();

        let rows = store.read_ledger(&record.env_id);
        assert_eq!(rows.len(), 1, "one command, however many streams watched it");
        assert_eq!(rows[0].argv, ["cargo", "test", "--offline"]);
        assert_eq!(rows[0].exit, Some(0));
        assert!(rows[0].attached);
    }

    #[test]
    fn a_ledger_written_before_fingerprints_still_counts() {
        let directory = tempfile::tempdir().unwrap();
        let dir = directory.path().join("env");
        let store = EnvStore::at(dir.clone());
        let record = record("oat-a-worker-1", "worker", directory.path(), 1);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            store.ledger_path(&record.env_id),
            "{\"ts\":\"t\",\"epoch_ms\":5,\"exec_id\":\"old\",\"argv\":[\"cargo\",\"test\"],\
             \"cwd\":\"/w\",\"exit\":0,\"ms\":9,\"env_id\":\"oat-a-worker-1\",\
             \"image_id\":\"sha256:0123456789abcdef\",\"attach\":false}\n",
        )
        .unwrap();

        let summary = store.ledger_summary(&record.env_id);
        assert_eq!(summary["commands"], 1, "an old line is still a command");
        assert_eq!(summary["total_ms"], 9);
        assert_eq!(summary["unfinished"], 0);

        let rows = store.read_ledger(&record.env_id);
        assert_eq!(rows[0].exit, Some(0));
        assert!(!rows[0].tree_start.is_known());
        assert!(rows[0].settled_tree().is_none());
    }

    #[test]
    fn a_worktree_finds_what_every_role_ran_in_it() {
        let directory = tempfile::tempdir().unwrap();
        let store = EnvStore::at(directory.path().join("env"));
        let worktree = directory.path().join("worktree");
        std::fs::create_dir_all(&worktree).unwrap();
        let elsewhere = directory.path().join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        let tree = known("aaaa");

        let worker = record("oat-a-worker-1", "worker", &worktree, 1);
        let reviewer = record("oat-a-reviewer-1", "reviewer", &worktree, 2);
        let other = record("oat-a-worker-2", "worker", &elsewhere, 3);
        // `started_ms` comes from wall-clock time, so two writes without a gap between them can
        // land in the same millisecond; sleeping between them makes the chronological order the
        // test asserts on an actual fact instead of a race against `ledgers_for_worktree`'s tie
        // break (env_id, then exec_id).
        ran(&store, &worker, "e1", &["cargo", "test"], 0, 10, &tree);
        std::thread::sleep(std::time::Duration::from_millis(5));
        ran(&store, &reviewer, "e2", &["cargo", "clippy"], 0, 20, &tree);
        std::thread::sleep(std::time::Duration::from_millis(5));
        ran(&store, &other, "e3", &["cargo", "build"], 0, 30, &tree);

        let found = store.ledgers_for_worktree(&worktree, None);
        let roles: Vec<&str> = found.iter().map(|entry| entry.role.as_str()).collect();
        assert_eq!(roles, ["worker", "reviewer"]);
        assert!(
            found.iter().all(|entry| entry.worktree == worktree),
            "another worktree's commands are not this worktree's evidence"
        );
    }

    #[test]
    fn evidence_older_than_the_window_is_left_out() {
        let directory = tempfile::tempdir().unwrap();
        let store = EnvStore::at(directory.path().join("env"));
        let worktree = directory.path().join("worktree");
        std::fs::create_dir_all(&worktree).unwrap();
        let record = record("oat-a-worker-1", "worker", &worktree, 1);
        ran(&store, &record, "e1", &["cargo", "test"], 0, 10, &known("aaaa"));

        assert_eq!(store.ledgers_for_worktree(&worktree, Some(0)).len(), 1);
        let after_everything = super::super::now_ms() + 60_000;
        assert!(store.ledgers_for_worktree(&worktree, Some(after_everything)).is_empty());
    }

    #[test]
    fn a_run_remembers_the_profile_its_roles_inherit() {
        let directory = tempfile::tempdir().unwrap();
        let store = EnvStore::at(directory.path().join("env"));
        assert_eq!(store.run_profile("run-1"), None);
        store.save_run_profile("run-1", "local").unwrap();
        assert_eq!(store.run_profile("run-1").as_deref(), Some("local"));
        assert_eq!(store.run_profile("run-2"), None);
    }
}
