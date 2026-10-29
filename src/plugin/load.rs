// SPDX-License-Identifier: MIT
// Copyright (c) 2026 oat-agents contributors

//! One directory in, one validated plugin out — or every error found in it (ticket
//! Architecture). Nothing here knows about any other plugin; combining several is `catalog`'s
//! job.

use super::format::{
    is_executable, list_files_recursive, list_subdirectories, parse_toml, CoreRoleToml, ModelToml,
    PluginManifest, RoleToml, StartLocationToml, SUPPORTED_FORMAT_VERSION,
};
use super::PluginError;
use crate::role::{Backend, CoreRole, ModelSetting, SkillFile, SkillRef, StartLocation};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;

#[derive(Debug, Clone)]
pub struct LoadedRole {
    pub name: String,
    pub instructions: String,
    pub start: StartLocation,
    pub exec_environment: bool,
    pub prior_verification: bool,
    pub max_concurrent: Option<u32>,
    pub skills: Vec<SkillRef>,
    pub models: BTreeMap<Backend, ModelSetting>,
}

#[derive(Debug, Clone)]
pub struct LoadedCoreRole {
    pub instructions: String,
    pub skills: Vec<SkillRef>,
    pub models: BTreeMap<Backend, ModelSetting>,
    pub backend: Option<Backend>,
    pub rework: crate::findings::ReworkSettings,
}

#[derive(Debug, Clone)]
pub struct LoadedPlugin {
    pub name: String,
    pub dir: PathBuf,
    pub roles: BTreeMap<String, LoadedRole>,
    pub meta: Option<LoadedCoreRole>,
    pub console: Option<LoadedCoreRole>,
}

const CORE_ROLES: &[(&str, &str)] = &[("oat-meta", "meta"), ("oat-console", "console")];

/// The only file names `core/` may contain (ADR-0002): one instruction file and one optional
/// TOML file per core role. Anything else — a misspelled name, a file for a core role that does
/// not exist — names a role no plugin format defines, and is refused.
const CORE_FILE_NAMES: &[&str] = &[
    "oat-meta-instruction.md",
    "oat-meta.toml",
    "oat-console-instruction.md",
    "oat-console.toml",
];

pub fn load_plugin(dir: &Path) -> Result<LoadedPlugin, Vec<PluginError>> {
    let mut errors = Vec::new();

    let manifest = load_manifest(dir, &mut errors);
    let skills = load_all_skills(dir, &mut errors);
    let roles = load_all_roles(dir, &skills, &mut errors);
    check_core_dir(dir, &mut errors);
    let mut core = BTreeMap::new();
    for (slug, _) in CORE_ROLES {
        if let Some(loaded) = load_core_role(dir, slug, &skills, &mut errors) {
            core.insert(*slug, loaded);
        }
    }

    if !errors.is_empty() {
        return Err(errors);
    }

    let manifest = manifest.expect("no errors implies the manifest loaded");
    Ok(LoadedPlugin {
        name: manifest.name,
        dir: dir.to_path_buf(),
        roles,
        meta: core.remove("oat-meta"),
        console: core.remove("oat-console"),
    })
}

/// Enumerates `core/` (when it exists) the same way `roles/` and `skills/` are enumerated, and
/// refuses every entry that is not one of the format's defined `core/` file names — the plugin
/// with a `core/` file for a role that does not exist that ADR-0002 says is refused.
fn check_core_dir(dir: &Path, errors: &mut Vec<PluginError>) {
    let core_dir = dir.join("core");
    let entries = match fs::read_dir(&core_dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
        Err(e) => {
            errors.push(PluginError::new(dir, "core/", format!("could not read core/: {e}")));
            return;
        }
    };
    for entry in entries {
        let Ok(entry) = entry else { continue };
        let name = entry.file_name().to_string_lossy().to_string();
        if !CORE_FILE_NAMES.contains(&name.as_str()) {
            errors.push(PluginError::new(
                dir,
                format!("core/{name}"),
                "is not a file this plugin format defines under core/",
            ));
        }
    }
}

fn load_manifest(dir: &Path, errors: &mut Vec<PluginError>) -> Option<PluginManifest> {
    let path = dir.join("oat-plugin.toml");
    let contents = match fs::read_to_string(&path) {
        Ok(c) => c,
        Err(e) => {
            errors.push(PluginError::new(
                dir,
                "oat-plugin.toml",
                format!("could not read oat-plugin.toml: {e}"),
            ));
            return None;
        }
    };
    match parse_toml::<PluginManifest>("oat-plugin.toml", &contents) {
        Ok(manifest) => {
            if manifest.format_version != SUPPORTED_FORMAT_VERSION {
                errors.push(PluginError::new(
                    dir,
                    "oat-plugin.toml",
                    format!(
                        "unknown plugin format version {}; this build understands version {}",
                        manifest.format_version, SUPPORTED_FORMAT_VERSION
                    ),
                ));
                return None;
            }
            if manifest.name.trim().is_empty() {
                errors.push(PluginError::new(dir, "oat-plugin.toml", "name must not be empty"));
                return None;
            }
            Some(manifest)
        }
        Err(message) => {
            errors.push(PluginError::new(dir, "oat-plugin.toml", message));
            None
        }
    }
}

fn load_all_skills(dir: &Path, errors: &mut Vec<PluginError>) -> BTreeMap<String, SkillRef> {
    let mut out = BTreeMap::new();
    let skills_dir = dir.join("skills");
    let entries = match list_subdirectories(&skills_dir) {
        Ok(Some(entries)) => entries,
        Ok(None) => return out,
        Err(e) => {
            errors.push(PluginError::new(dir, "skills/", format!("could not read skills/: {e}")));
            return out;
        }
    };
    for (name, path) in entries {
        if let Some(skill) = load_skill(dir, &name, &path, errors) {
            out.insert(name, skill);
        }
    }
    out
}

fn load_skill(dir: &Path, name: &str, path: &Path, errors: &mut Vec<PluginError>) -> Option<SkillRef> {
    let skill_md = path.join("SKILL.md");
    if !skill_md.is_file() {
        errors.push(PluginError::new(
            dir,
            format!("skills/{name}/SKILL.md"),
            "skill is missing its SKILL.md",
        ));
        return None;
    }

    let relative_paths = match list_files_recursive(path) {
        Ok(paths) => paths,
        Err(e) => {
            errors.push(PluginError::new(
                dir,
                format!("skills/{name}/"),
                format!("could not read skill directory: {e}"),
            ));
            return None;
        }
    };

    let mut files = Vec::new();
    for relative_path in relative_paths {
        let full_path = path.join(&relative_path);
        let contents = match fs::read(&full_path) {
            Ok(c) => c,
            Err(e) => {
                errors.push(PluginError::new(
                    dir,
                    format!("skills/{name}/{}", relative_path.display()),
                    format!("could not read file: {e}"),
                ));
                continue;
            }
        };
        let executable = is_executable(&full_path).unwrap_or(false);
        files.push(SkillFile {
            relative_path,
            contents,
            executable,
        });
    }

    Some(SkillRef {
        name: name.to_string(),
        files,
    })
}

fn load_all_roles(
    dir: &Path,
    skills: &BTreeMap<String, SkillRef>,
    errors: &mut Vec<PluginError>,
) -> BTreeMap<String, LoadedRole> {
    let mut out = BTreeMap::new();
    let roles_dir = dir.join("roles");
    let entries = match list_subdirectories(&roles_dir) {
        Ok(Some(entries)) => entries,
        Ok(None) => return out,
        Err(e) => {
            errors.push(PluginError::new(dir, "roles/", format!("could not read roles/: {e}")));
            return out;
        }
    };
    for (name, path) in entries {
        if let Some(role) = load_role(dir, &name, &path, skills, errors) {
            out.insert(name, role);
        }
    }
    out
}

fn load_role(
    dir: &Path,
    name: &str,
    path: &Path,
    skills: &BTreeMap<String, SkillRef>,
    errors: &mut Vec<PluginError>,
) -> Option<LoadedRole> {
    let toml_file = format!("roles/{name}/role.toml");
    let toml_path = path.join("role.toml");
    let toml_contents = match fs::read_to_string(&toml_path) {
        Ok(c) => c,
        Err(e) => {
            errors.push(PluginError::new(dir, &toml_file, format!("could not read role.toml: {e}")));
            return None;
        }
    };
    let role_toml: RoleToml = match parse_toml(&toml_file, &toml_contents) {
        Ok(t) => t,
        Err(message) => {
            errors.push(PluginError::new(dir, &toml_file, message));
            return None;
        }
    };

    let instructions_file = format!("roles/{name}/instructions.md");
    let instructions_path = path.join("instructions.md");
    let instructions = match fs::read_to_string(&instructions_path) {
        Ok(c) => c,
        Err(e) => {
            errors.push(PluginError::new(
                dir,
                &instructions_file,
                format!("could not read instructions.md: {e}"),
            ));
            return None;
        }
    };

    if role_toml.max_concurrent == Some(0) {
        errors.push(PluginError::new(dir, &toml_file, "max_concurrent must be at least 1".to_string()));
        return None;
    }

    let resolved_skills = resolve_skills(dir, &toml_file, &role_toml.skills, skills, errors);
    let models = convert_models(dir, &toml_file, role_toml.model, errors);

    Some(LoadedRole {
        name: name.to_string(),
        instructions,
        start: match role_toml.start {
            StartLocationToml::Fresh => StartLocation::Fresh,
            StartLocationToml::Existing => StartLocation::Existing,
        },
        exec_environment: role_toml.exec_environment,
        prior_verification: role_toml.prior_verification,
        max_concurrent: role_toml.max_concurrent,
        skills: resolved_skills,
        models,
    })
}

fn load_core_role(
    dir: &Path,
    slug: &str,
    skills: &BTreeMap<String, SkillRef>,
    errors: &mut Vec<PluginError>,
) -> Option<LoadedCoreRole> {
    let instruction_file = format!("core/{slug}-instruction.md");
    let instruction_path = dir.join("core").join(format!("{slug}-instruction.md"));
    let toml_file = format!("core/{slug}.toml");
    let toml_path = dir.join("core").join(format!("{slug}.toml"));

    let instructions_exist = instruction_path.is_file();
    let toml_exists = toml_path.is_file();

    if !instructions_exist && !toml_exists {
        return None;
    }
    if !instructions_exist {
        errors.push(PluginError::new(
            dir,
            &instruction_file,
            format!("{toml_file} is present but {instruction_file} is missing"),
        ));
        return None;
    }

    let instructions = match fs::read_to_string(&instruction_path) {
        Ok(c) => c,
        Err(e) => {
            errors.push(PluginError::new(
                dir,
                &instruction_file,
                format!("could not read {instruction_file}: {e}"),
            ));
            return None;
        }
    };

    let core_toml: CoreRoleToml = if toml_exists {
        let contents = match fs::read_to_string(&toml_path) {
            Ok(c) => c,
            Err(e) => {
                errors.push(PluginError::new(dir, &toml_file, format!("could not read {toml_file}: {e}")));
                return None;
            }
        };
        match parse_toml(&toml_file, &contents) {
            Ok(t) => t,
            Err(message) => {
                errors.push(PluginError::new(dir, &toml_file, message));
                return None;
            }
        }
    } else {
        CoreRoleToml::default()
    };

    let resolved_skills = resolve_skills(dir, &toml_file, &core_toml.skills, skills, errors);
    let models = convert_models(dir, &toml_file, core_toml.model, errors);
    let rework = match core_toml.rework {
        None => crate::findings::ReworkSettings::default(),
        Some(_) if slug != CoreRole::Meta.name() => {
            errors.push(PluginError::new(
                dir,
                &toml_file,
                format!(
                    "{toml_file} sets `[rework]`, which only core/oat-meta.toml may set; \
                     the rework loop is the meta-agent's to run"
                ),
            ));
            crate::findings::ReworkSettings::default()
        }
        Some(table) => {
            let settings = crate::findings::ReworkSettings {
                max_rounds: table
                    .max_rounds
                    .unwrap_or_else(crate::findings::ReworkSettings::default_max_rounds),
                p2_required: table
                    .p2_required
                    .unwrap_or_else(crate::findings::ReworkSettings::default_p2_required),
            };
            if settings.max_rounds == 0 {
                errors.push(PluginError::new(
                    dir,
                    &toml_file,
                    format!("{toml_file} sets max_rounds = 0; a work gets at least one review round"),
                ));
            }
            settings
        }
    };
    let backend = match core_toml.backend.as_deref() {
        None => None,
        Some(_) if slug != CoreRole::Console.name() => {
            errors.push(PluginError::new(
                dir,
                &toml_file,
                format!(
                    "{toml_file} sets `backend`, which only core/oat-console.toml may set; \
                     oat-meta runs on the Run's backend, chosen by `meta fire --agent`"
                ),
            ));
            None
        }
        Some(name) => match Backend::from_str(name) {
            Ok(backend) => Some(backend),
            Err(_) => {
                errors.push(PluginError::new(
                    dir,
                    &toml_file,
                    format!("unknown backend '{name}' in {toml_file}; expected claude or codex"),
                ));
                None
            }
        },
    };

    Some(LoadedCoreRole {
        instructions,
        skills: resolved_skills,
        models,
        backend,
        rework,
    })
}

fn resolve_skills(
    dir: &Path,
    file: &str,
    names: &[String],
    skills: &BTreeMap<String, SkillRef>,
    errors: &mut Vec<PluginError>,
) -> Vec<SkillRef> {
    let mut out = Vec::new();
    for name in names {
        match skills.get(name) {
            Some(skill) => out.push(skill.clone()),
            None => errors.push(PluginError::new(
                dir,
                file,
                format!("declares skill '{name}', which has no skills/{name}/ directory"),
            )),
        }
    }
    out
}

fn convert_models(
    dir: &Path,
    file: &str,
    models: BTreeMap<String, ModelToml>,
    errors: &mut Vec<PluginError>,
) -> BTreeMap<Backend, ModelSetting> {
    let mut out = BTreeMap::new();
    for (key, value) in models {
        match Backend::from_str(&key) {
            Ok(backend) => {
                out.insert(
                    backend,
                    ModelSetting {
                        model: value.model,
                        reasoning_effort: value.reasoning_effort,
                    },
                );
            }
            Err(_) => errors.push(PluginError::new(
                dir,
                file,
                format!("unknown backend '{key}' in [model]"),
            )),
        }
    }
    out
}
