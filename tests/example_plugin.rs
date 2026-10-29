// SPDX-License-Identifier: MIT
// Copyright (c) 2026 oat-agents contributors

use oat_agents::plugin::load_catalog;
use oat_agents::role::{Backend, CoreRole, RoleCatalog, StartLocation};
use std::path::PathBuf;

fn example_plugin_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("plugins/example")
}

// ---- The example plugin loads through the same path as any external plugin (ticket AC). ----

#[test]
fn the_example_plugin_loads_with_no_errors_through_the_external_plugin_path() {
    let dirs = vec![example_plugin_dir()];
    let (catalog, refs) = load_catalog(&dirs).unwrap_or_else(|errors| {
        panic!(
            "the example plugin failed to load: {}",
            errors.iter().map(|e| e.to_string()).collect::<Vec<_>>().join("; ")
        )
    });
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].name, "example");

    // It supplies both core-role instructions.
    let meta = catalog.core_role(CoreRole::Meta).unwrap();
    assert!(!meta.instructions.trim().is_empty());
    let console = catalog.core_role(CoreRole::Console).unwrap();
    assert!(!console.instructions.trim().is_empty());

    // Its three roles have the start locations and properties the ticket lists.
    let mut role_names = catalog.role_names();
    role_names.sort();
    assert_eq!(
        role_names,
        vec!["planner".to_string(), "reviewer".to_string(), "worker".to_string()]
    );

    let planner = catalog.role("planner").unwrap();
    assert_eq!(planner.start, StartLocation::Fresh);
    assert!(!planner.exec_environment, "planner gets no execution environment");
    assert!(planner.skills.is_empty(), "planner loads no skill");

    let worker = catalog.role("worker").unwrap();
    assert_eq!(worker.start, StartLocation::Fresh);
    assert!(worker.exec_environment, "worker gets an execution environment");
    assert_eq!(worker.skills.len(), 1);
    assert_eq!(worker.skills[0].name, "committing");

    let reviewer = catalog.role("reviewer").unwrap();
    assert_eq!(reviewer.start, StartLocation::Existing);
    assert!(reviewer.exec_environment, "reviewer gets an execution environment");
    assert!(reviewer.prior_verification, "reviewer gets prior verification");
    assert!(reviewer.skills.is_empty(), "the committing skill lands with no other role");

    // No role pins a model: each backend falls back to its own default.
    for (name, role) in [("planner", &planner), ("worker", &worker), ("reviewer", &reviewer)] {
        for backend in [Backend::Claude, Backend::Codex] {
            assert!(
                !role.models.contains_key(&backend),
                "{name} pins a model for {}",
                backend.label()
            );
        }
    }
}
