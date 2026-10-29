// SPDX-License-Identifier: MIT
// Copyright (c) 2026 oat-agents contributors

use serde::Serialize;
use std::fmt;

/// A stable, machine-readable failure. Every failing command returns one of these
/// instead of an opaque `anyhow::Error`, so a caller can act on `code` without parsing text.
#[derive(Debug, Clone, Serialize)]
pub struct CliFailure {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}

impl CliFailure {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_string(),
            message: message.into(),
            details: None,
        }
    }

    pub fn with_details(mut self, details: serde_json::Value) -> Self {
        self.details = Some(details);
        self
    }

    pub fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or_else(|_| serde_json::json!({"code": self.code}))
    }
}

impl fmt::Display for CliFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for CliFailure {}

/// Every error code this crate returns. Later tickets and role instructions name these by
/// value, so a rename here is a breaking change.
pub mod codes {
    pub const INVALID_CLI_ARGUMENTS: &str = "invalid_cli_arguments";
    pub const CLAP_DISPLAY: &str = "clap_display";
    pub const INVALID_INPUT: &str = "invalid_input";
    pub const RUN_NOT_BOUND: &str = "run_not_bound";
    pub const DISPATCH_NOT_BOUND: &str = "dispatch_not_bound";
    pub const ALREADY_SETTLED: &str = "already_settled";
    pub const RUN_STILL_OPEN: &str = "run_still_open";
    pub const INVALID_WORKTREE_ID: &str = "invalid_worktree_id";
    pub const INVALID_ROLE_OPTION: &str = "invalid_role_option";
    pub const ROLE_SOURCE_REQUIRED: &str = "role_source_required";
    pub const UNKNOWN_ROLE: &str = "unknown_role";
    pub const ROLE_LIMITS_INVALID: &str = "role_limits_invalid";
    pub const ROLE_SETTINGS_INVALID: &str = "role_settings_invalid";
    pub const CORE_ROLE_INSTRUCTIONS_MISSING: &str = "core_role_instructions_missing";
    pub const PLUGIN_LOAD_FAILED: &str = "plugin_load_failed";
    pub const SKILL_CONFLICT: &str = "skill_conflict";
    pub const SKILL_WRITE_FAILED: &str = "skill_write_failed";
    pub const TMUX_MISSING: &str = "tmux_missing";
    pub const INVALID_LOCAL_SETTINGS: &str = "invalid_local_settings";
    pub const TRANSCRIPT_NOT_FOUND: &str = "transcript_not_found";
    pub const FINDINGS_INVALID: &str = "findings_invalid";
    pub const REWORK_SETTINGS_INVALID: &str = "rework_settings_invalid";
    pub const META_NOT_FOUND: &str = "meta_not_found";
    pub const CONSOLE_NOT_FOUND: &str = "console_not_found";
    pub const INTERNAL_ERROR: &str = "internal_error";
    pub const ENV_PROFILE_UNRESOLVED: &str = "env_profile_unresolved";
    pub const ENV_STATE_UNAVAILABLE: &str = "env_state_unavailable";
    pub const ENV_NOT_READY: &str = "env_not_ready";
    pub const ENV_ROLE_MISMATCH: &str = "env_role_mismatch";
    pub const ENV_OUTSIDE_WORKSPACE: &str = "env_outside_workspace";
    pub const ENV_BACKEND_ERROR: &str = "env_backend_error";
    pub const ENV_EXEC_TIMEOUT: &str = "env_exec_timeout";
    pub const ENV_IMAGE_BUILD_FAILED: &str = "env_image_build_failed";
    pub const PLUGIN_UNINITIALISED: &str = "plugin_uninitialised";
    pub const PLUGIN_UNTRUSTED: &str = "plugin_untrusted";
    pub const PLUGIN_PIN_INVALID: &str = "plugin_pin_invalid";
    pub const PLUGIN_PIN_MISMATCH: &str = "plugin_pin_mismatch";
    pub const PLUGIN_NAME_MISMATCH: &str = "plugin_name_mismatch";
    pub const PLUGIN_FETCH_FAILED: &str = "plugin_fetch_failed";
    pub const PLUGIN_ALREADY_INITIALISED: &str = "plugin_already_initialised";
}

pub fn err(code: &str, message: impl Into<String>) -> anyhow::Error {
    anyhow::Error::new(CliFailure::new(code, message))
}

/// Renders any error as the stable JSON failure shape on stderr. An error that is not a
/// `CliFailure` (a bug, an I/O surprise) becomes `internal_error` rather than leaking a
/// Rust `Debug` string as the contract.
pub fn to_failure(error: &anyhow::Error) -> CliFailure {
    if let Some(failure) = error.downcast_ref::<CliFailure>() {
        failure.clone()
    } else {
        CliFailure::new(codes::INTERNAL_ERROR, error.to_string())
    }
}
