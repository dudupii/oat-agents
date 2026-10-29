// SPDX-License-Identifier: MIT
// Copyright (c) 2026 oat-agents contributors

//! An ordered list of validated plugins in, a `RoleCatalog` out — or the conflicts between
//! them (ticket Architecture). This is the only part of `plugin` that knows more than one
//! plugin exists.

use super::load::{LoadedCoreRole, LoadedPlugin};
use super::PluginError;
use crate::role::{
    Backend, CoreRole, CoreRoleDefinition, RoleCatalog, RoleDefinition, SkillRef,
};
use anyhow::Result;
use std::collections::BTreeMap;

#[derive(Debug)]
pub struct PluginCatalog {
    roles: BTreeMap<String, RoleDefinition>,
    core_roles: BTreeMap<CoreRole, CoreRoleDefinition>,
}

impl RoleCatalog for PluginCatalog {
    fn role_names(&self) -> Vec<String> {
        self.roles.keys().cloned().collect()
    }

    fn role(&self, name: &str) -> Result<&RoleDefinition> {
        self.roles
            .get(name)
            .ok_or_else(|| crate::role::unknown_role_error(self, name))
    }

    fn core_role(&self, role: CoreRole) -> Result<&CoreRoleDefinition> {
        self.core_roles
            .get(&role)
            .ok_or_else(|| crate::role::missing_core_instructions_error(role))
    }
}

/// Combines validated plugins into one catalog, in the order they are listed (the order that
/// decides core-role instruction joining, ADR-0002). Two plugins defining the same role name,
/// two plugins supplying a same-named skill or a same-backend model for the same core role, and
/// no plugin at all supplying a core role's instructions, are each reported by name rather than
/// resolved by a silent precedence rule.
pub fn build_catalog(plugins: &[LoadedPlugin]) -> Result<PluginCatalog, Vec<PluginError>> {
    let mut errors = Vec::new();
    let mut roles = BTreeMap::new();
    let mut owner: BTreeMap<&str, &str> = BTreeMap::new();

    for plugin in plugins {
        for (role_name, role) in &plugin.roles {
            if let Some(existing_plugin) = owner.get(role_name.as_str()) {
                errors.push(PluginError::combination(format!(
                    "role '{role_name}' is defined by both plugin '{existing_plugin}' and plugin '{}'",
                    plugin.name
                )));
                continue;
            }
            owner.insert(role_name.as_str(), plugin.name.as_str());
            roles.insert(
                role_name.clone(),
                RoleDefinition {
                    name: role_name.clone(),
                    instructions: role.instructions.clone(),
                    models: role.models.clone(),
                    skills: role.skills.clone(),
                    start: role.start,
                    exec_environment: role.exec_environment,
                    prior_verification: role.prior_verification,
                    max_concurrent: role.max_concurrent,
                },
            );
        }
    }

    let meta = combine_core_role(
        plugins,
        |p| p.meta.as_ref(),
        "core/oat-meta-instruction.md",
        &mut errors,
    );
    let console = combine_core_role(
        plugins,
        |p| p.console.as_ref(),
        "core/oat-console-instruction.md",
        &mut errors,
    );

    if !errors.is_empty() {
        return Err(errors);
    }

    let mut core_roles = BTreeMap::new();
    core_roles.insert(CoreRole::Meta, meta.expect("no errors implies present"));
    core_roles.insert(CoreRole::Console, console.expect("no errors implies present"));

    Ok(PluginCatalog { roles, core_roles })
}

fn combine_core_role<'a>(
    plugins: &'a [LoadedPlugin],
    getter: impl Fn(&'a LoadedPlugin) -> Option<&'a LoadedCoreRole>,
    instruction_file: &str,
    errors: &mut Vec<PluginError>,
) -> Option<CoreRoleDefinition> {
    let contributions: Vec<(&'a LoadedPlugin, &'a LoadedCoreRole)> =
        plugins.iter().filter_map(|p| getter(p).map(|c| (p, c))).collect();

    if contributions.is_empty() {
        errors.push(PluginError::combination(format!(
            "no plugin supplies {instruction_file}"
        )));
        return None;
    }

    let instructions = contributions
        .iter()
        .map(|(_, c)| c.instructions.trim_end())
        .collect::<Vec<_>>()
        .join("\n\n");

    let mut skills: Vec<SkillRef> = Vec::new();
    let mut skill_owner: BTreeMap<&str, &str> = BTreeMap::new();
    for (plugin, core) in &contributions {
        for skill in &core.skills {
            if let Some(existing_plugin) = skill_owner.get(skill.name.as_str()) {
                errors.push(PluginError::combination(format!(
                    "skill '{}' for {instruction_file} is supplied by both plugin '{existing_plugin}' and plugin '{}'",
                    skill.name, plugin.name
                )));
                continue;
            }
            skill_owner.insert(skill.name.as_str(), plugin.name.as_str());
            skills.push(skill.clone());
        }
    }

    let mut models = BTreeMap::new();
    let mut model_owner: BTreeMap<Backend, &str> = BTreeMap::new();
    for (plugin, core) in &contributions {
        for (backend, setting) in &core.models {
            if let Some(existing_plugin) = model_owner.get(backend) {
                errors.push(PluginError::combination(format!(
                    "model for backend '{backend:?}' on {instruction_file} is supplied by both plugin '{existing_plugin}' and plugin '{}'",
                    plugin.name
                )));
                continue;
            }
            model_owner.insert(*backend, plugin.name.as_str());
            models.insert(*backend, setting.clone());
        }
    }

    let mut backend = None;
    let mut backend_owner: Option<&str> = None;
    for (plugin, core) in &contributions {
        let Some(chosen) = core.backend else {
            continue;
        };
        if let Some(existing_plugin) = backend_owner {
            errors.push(PluginError::combination(format!(
                "the backend for {instruction_file} is chosen by both plugin '{existing_plugin}' and plugin '{}'",
                plugin.name
            )));
            continue;
        }
        backend_owner = Some(plugin.name.as_str());
        backend = Some(chosen);
    }

    // The rework loop's bounds follow the same rule as the backend: one plugin sets them, and
    // two setting them is a conflict reported by name (ADR-0009).
    let mut rework = crate::findings::ReworkSettings::default();
    let mut rework_owner: Option<&str> = None;
    for (plugin, core) in &contributions {
        if core.rework.is_default() {
            continue;
        }
        if let Some(existing_plugin) = rework_owner {
            errors.push(PluginError::combination(format!(
                "the rework loop's bounds for {instruction_file} are set by both plugin '{existing_plugin}' and plugin '{}'",
                plugin.name
            )));
            continue;
        }
        rework_owner = Some(plugin.name.as_str());
        rework = core.rework.clone();
    }

    Some(CoreRoleDefinition {
        instructions,
        models,
        skills,
        backend,
        rework,
    })
}
