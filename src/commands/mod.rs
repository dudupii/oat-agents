// SPDX-License-Identifier: MIT
// Copyright (c) 2026 oat-agents contributors

pub mod goal;
pub mod checklist;
pub mod console;
pub mod dispatch;
pub mod init;
pub mod log;
pub mod meta;
pub mod plugin;
pub mod rework;
pub mod role;
pub mod run;
pub mod tui;

use sha2::{Digest, Sha256};
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A short, unique identifier for a new Run or Dispatch: not cryptographically meaningful,
/// just distinct across a process and stable enough to eyeball in a log line.
pub fn generate_id(prefix: &str) -> String {
    let counter = COUNTER.fetch_add(1, Ordering::SeqCst);
    let seed = format!(
        "{prefix}-{:?}-{counter}",
        std::time::SystemTime::now()
    );
    let mut hasher = Sha256::new();
    hasher.update(seed.as_bytes());
    let digest = hasher.finalize();
    let mut hex = String::new();
    for b in digest.iter().take(4) {
        use std::fmt::Write;
        let _ = write!(hex, "{b:02x}");
    }
    format!("{prefix}-{hex}")
}
