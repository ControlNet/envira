use std::path::PathBuf;

use envira::catalog::{Catalog, InstallScope, TargetBackend};
use envira::planner::{
    build_install_plan, PlanSelection, PlannedScope, PlannerError, PlannerRequest,
};
use envira::platform::{
    ArchitectureIdentity, ArchitectureKind, DistroIdentity, DistroKind, InvocationKind,
    PlatformContext, RuntimeScope, UserAccount,
};

#[test]
fn planner_repeated_identical_input_yields_identical_serializable_output() {
    let catalog = envira::catalog::load_embedded_catalog().expect("embedded catalog should parse");
    let platform = platform_context(Some(TargetBackend::Apt), RuntimeScope::Both);
    let request = PlannerRequest::new(vec![
        PlanSelection::bundle("terminal-tools"),
        PlanSelection::item("fastfetch"),
    ]);

    let first = build_install_plan(&catalog, &platform, &request).expect("plan should build");
    let second = build_install_plan(&catalog, &platform, &request).expect("plan should build");

    assert_eq!(first, second);
    assert_eq!(
        serde_json::to_string(&first).expect("plan should serialize"),
        serde_json::to_string(&second).expect("plan should serialize")
    );
    assert_eq!(
        step_ids(&first),
        vec!["essentials", "bat", "ctop", "fastfetch"]
    );
    assert_eq!(first.steps[0].planned_scope, PlannedScope::System);
    assert!(!first.steps[0].requested);
    assert!(first.steps[1].requested);
    assert_eq!(
        first.steps[2].selected_target.backend,
        TargetBackend::DirectBinary
    );
}

#[test]
fn planner_uses_default_bundles_when_request_has_no_explicit_selection() {
    let catalog = envira::catalog::load_embedded_catalog().expect("embedded catalog should parse");
    let platform = platform_context(Some(TargetBackend::Apt), RuntimeScope::Both);

    let plan = build_install_plan(&catalog, &platform, &PlannerRequest::default())
        .expect("default bundle plan should build");

    assert_eq!(
        step_ids(&plan),
        vec!["essentials", "bat", "ctop", "fastfetch", "btop"]
    );
    assert!(plan.steps.iter().all(|step| step.requested));
    assert!(plan.request.selections.is_empty());
}

#[test]
fn planner_deduplicates_items_across_bundles_and_dependencies() {
    let catalog = Catalog::from_toml_str(dedup_manifest()).expect("fixture catalog should parse");
    let platform = platform_context(Some(TargetBackend::Apt), RuntimeScope::User);
    let request = PlannerRequest::new(vec![
        PlanSelection::bundle("bundle-a"),
        PlanSelection::bundle("bundle-b"),
        PlanSelection::item("tool-b"),
    ]);

    let plan = build_install_plan(&catalog, &platform, &request).expect("plan should build");

    assert_eq!(step_ids(&plan), vec!["base", "tool-a", "tool-b"]);
    assert_eq!(
        plan.steps
            .iter()
            .map(|step| (step.item_id.as_str(), step.requested))
            .collect::<Vec<_>>(),
        vec![("base", true), ("tool-a", true), ("tool-b", true)]
    );
}

#[test]
fn planner_explicit_selections_replace_implicit_defaults() {
    let catalog = Catalog::from_toml_str(default_replacement_manifest())
        .expect("fixture catalog should parse");
    let platform = platform_context(Some(TargetBackend::Apt), RuntimeScope::User);
    let request = PlannerRequest::bundle("explicit");

    let plan = build_install_plan(&catalog, &platform, &request).expect("plan should build");

    assert_eq!(step_ids(&plan), vec!["explicit-tool"]);
    assert_eq!(
        plan.steps
            .iter()
            .map(|step| (step.item_id.as_str(), step.requested))
            .collect::<Vec<_>>(),
        vec![("explicit-tool", true)]
    );
}

#[test]
fn planner_preserves_explicit_selection_declaration_order_after_dependency_expansion() {
    let catalog = Catalog::from_toml_str(dedup_manifest()).expect("fixture catalog should parse");
    let platform = platform_context(Some(TargetBackend::Apt), RuntimeScope::User);
    let request = PlannerRequest::new(vec![
        PlanSelection::item("tool-b"),
        PlanSelection::item("tool-a"),
    ]);

    let plan = build_install_plan(&catalog, &platform, &request).expect("plan should build");

    assert_eq!(step_ids(&plan), vec!["base", "tool-b", "tool-a"]);
}

#[test]
fn planner_reports_dependency_cycles_with_explicit_cycle_path() {
    let catalog = Catalog::from_toml_str(cycle_manifest()).expect("fixture catalog should parse");
    let platform = platform_context(Some(TargetBackend::Apt), RuntimeScope::User);
    let request = PlannerRequest::item("alpha");

    let error = build_install_plan(&catalog, &platform, &request)
        .expect_err("cyclic dependencies should be rejected");

    match error {
        PlannerError::DependencyCycle { cycle } => {
            assert_eq!(cycle, vec!["alpha", "beta", "gamma", "alpha"]);
        }
        other => panic!("expected dependency cycle error, got {other}"),
    }
}

#[test]
fn planner_rejects_items_without_supported_target_for_platform() {
    let catalog =
        Catalog::from_toml_str(target_error_manifest()).expect("fixture catalog should parse");
    let platform = platform_context(None, RuntimeScope::System);
    let request = PlannerRequest::item("native-only");

    let error = build_install_plan(&catalog, &platform, &request)
        .expect_err("unsupported targets should be rejected");

    match error {
        PlannerError::UnsupportedTarget {
            item_id,
            native_backend,
            available_targets,
        } => {
            assert_eq!(item_id, "native-only");
            assert_eq!(native_backend, None);
            assert_eq!(available_targets.len(), 1);
            assert_eq!(available_targets[0].backend, TargetBackend::Apt);
        }
        other => panic!("expected unsupported target error, got {other}"),
    }
}

#[test]
fn invalid_selection_rejected_for_unknown_item_and_bundle() {
    let catalog = Catalog::from_toml_str(dedup_manifest()).expect("fixture catalog should parse");
    let platform = platform_context(Some(TargetBackend::Apt), RuntimeScope::User);

    let missing_item = build_install_plan(&catalog, &platform, &PlannerRequest::item("missing"))
        .expect_err("unknown items should be rejected");
    let missing_bundle = build_install_plan(
        &catalog,
        &platform,
        &PlannerRequest::bundle("missing-bundle"),
    )
    .expect_err("unknown bundles should be rejected");

    match missing_item {
        PlannerError::UnknownItem { item_id } => assert_eq!(item_id, "missing"),
        other => panic!("expected unknown item error, got {other}"),
    }

    match missing_bundle {
        PlannerError::UnknownBundle { bundle_id } => assert_eq!(bundle_id, "missing-bundle"),
        other => panic!("expected unknown bundle error, got {other}"),
    }
}

#[test]
fn unsupported_distro_or_mode_rejected_for_platform_specific_contracts() {
    let catalog = Catalog::from_toml_str(unsupported_coverage_manifest())
        .expect("fixture catalog should parse");
    let ubuntu_user = platform_context(Some(TargetBackend::Apt), RuntimeScope::User);
    let ubuntu_both = platform_context(Some(TargetBackend::Apt), RuntimeScope::Both);

    let missing_distro =
        build_install_plan(&catalog, &ubuntu_user, &PlannerRequest::item("fedora-user"))
            .expect_err("unsupported distro coverage should be rejected");
    let missing_mode = build_install_plan(
        &catalog,
        &ubuntu_both,
        &PlannerRequest::item("ubuntu-system-user-verified"),
    )
    .expect_err("unsupported verifier mode coverage should be rejected");

    match missing_distro {
        PlannerError::UnsupportedCoverage {
            item_id,
            contract_kind,
            distro_id,
            runtime_scope,
            available_coverage,
        } => {
            assert_eq!(item_id, "fedora-user");
            assert_eq!(contract_kind, "recipes");
            assert_eq!(distro_id, "ubuntu");
            assert_eq!(runtime_scope, RuntimeScope::User);
            assert_eq!(available_coverage, vec!["mode=user distros=[\"fedora\"]"]);
        }
        other => panic!("expected unsupported coverage error, got {other}"),
    }

    match missing_mode {
        PlannerError::UnsupportedCoverage {
            item_id,
            contract_kind,
            distro_id,
            runtime_scope,
            available_coverage,
        } => {
            assert_eq!(item_id, "ubuntu-system-user-verified");
            assert_eq!(contract_kind, "verifiers");
            assert_eq!(distro_id, "ubuntu");
            assert_eq!(runtime_scope, RuntimeScope::Both);
            assert_eq!(available_coverage, vec!["mode=user distros=[\"ubuntu\"]"]);
        }
        other => panic!("expected unsupported coverage error, got {other}"),
    }
}

#[test]
fn planner_rejects_items_outside_runtime_scope() {
    let catalog = envira::catalog::load_embedded_catalog().expect("embedded catalog should parse");
    let platform = platform_context(Some(TargetBackend::Apt), RuntimeScope::User);
    let request = PlannerRequest::item("vnc");

    let error = build_install_plan(&catalog, &platform, &request)
        .expect_err("unsupported scope should be rejected");

    match error {
        PlannerError::UnsupportedScope {
            item_id,
            item_scope,
            runtime_scope,
        } => {
            assert_eq!(item_id, "essentials");
            assert_eq!(item_scope, InstallScope::System);
            assert_eq!(runtime_scope, RuntimeScope::User);
        }
        other => panic!("expected unsupported scope error, got {other}"),
    }
}

#[test]
fn planner_rejects_containers_bundle_in_user_runtime_because_docker_is_system_scoped() {
    let catalog = envira::catalog::load_embedded_catalog().expect("embedded catalog should parse");
    let platform = platform_context(Some(TargetBackend::Apt), RuntimeScope::User);
    let request = PlannerRequest::bundle("containers");

    let error = build_install_plan(&catalog, &platform, &request)
        .expect_err("containers bundle should keep an explicit user-runtime scope contract");

    match error {
        PlannerError::UnsupportedScope {
            item_id,
            item_scope,
            runtime_scope,
        } => {
            assert_eq!(item_id, "essentials");
            assert_eq!(item_scope, InstallScope::System);
            assert_eq!(runtime_scope, RuntimeScope::User);
        }
        other => panic!("expected unsupported scope error, got {other}"),
    }
}

fn step_ids(plan: &envira::planner::InstallPlan) -> Vec<&str> {
    plan.steps
        .iter()
        .map(|step| step.item_id.as_str())
        .collect()
}

fn platform_context(
    native_backend: Option<TargetBackend>,
    runtime_scope: RuntimeScope,
) -> PlatformContext {
    let effective_user = match runtime_scope {
        RuntimeScope::System => user("root", "/root", 0, 0),
        RuntimeScope::User | RuntimeScope::Both | RuntimeScope::Unknown => {
            user("alice", "/home/alice", 1000, 1000)
        }
    };
    let target_user = match runtime_scope {
        RuntimeScope::System => None,
        RuntimeScope::User => Some(effective_user.clone()),
        RuntimeScope::Both => Some(user("alice", "/home/alice", 1000, 1000)),
        RuntimeScope::Unknown => None,
    };
    let invocation = match runtime_scope {
        RuntimeScope::System => InvocationKind::Root,
        RuntimeScope::User | RuntimeScope::Unknown => InvocationKind::User,
        RuntimeScope::Both => InvocationKind::Sudo,
    };

    PlatformContext {
        distro: DistroIdentity {
            kind: DistroKind::Ubuntu,
            id: "ubuntu".to_string(),
            name: "Ubuntu".to_string(),
            pretty_name: Some("Ubuntu 24.04 LTS".to_string()),
            version_id: Some("24.04".to_string()),
        },
        arch: ArchitectureIdentity {
            kind: ArchitectureKind::X86_64,
            raw: "x86_64".to_string(),
        },
        native_backend,
        invocation,
        effective_user,
        target_user,
        runtime_scope,
    }
}

fn user(username: &str, home_dir: &str, uid: u32, gid: u32) -> UserAccount {
    UserAccount {
        username: username.to_string(),
        home_dir: PathBuf::from(home_dir),
        uid: Some(uid),
        gid: Some(gid),
    }
}

fn dedup_manifest() -> &'static str {
    r#"
required_version = "0.1.0"
distros = ["ubuntu"]
shell = "bash"
default_bundles = ["bundle-a"]

[items.base]
name = "Base"
desc = "Base"
depends_on = []

[[items.base.recipes]]
mode = "user"
distros = ["ubuntu"]
cmd = "sudo apt install -y base"

[[items.base.verifiers]]
mode = "user"
distros = ["ubuntu"]
cmd = "command -v base"

[items.tool-a]
name = "Tool A"
desc = "Tool A"
depends_on = ["base"]

[[items.tool-a.recipes]]
mode = "user"
distros = ["ubuntu"]
cmd = "curl -fsSL https://example.com/tool-a.tar.gz | tar -xz -C ~/.local/bin"

[items.tool-b]
name = "Tool B"
desc = "Tool B"
depends_on = ["base"]

[[items.tool-a.verifiers]]
mode = "user"
distros = ["ubuntu"]
cmd = "command -v tool-a"

[[items.tool-b.recipes]]
mode = "user"
distros = ["ubuntu"]
cmd = "curl -fsSL https://example.com/tool-b -o ~/.local/bin/tool-b && chmod +x ~/.local/bin/tool-b"

[[items.tool-b.verifiers]]
mode = "user"
distros = ["ubuntu"]
cmd = "command -v tool-b"

[bundles.bundle-a]
name = "Bundle A"
desc = "Bundle A"
items = ["base", "tool-a"]

[bundles.bundle-b]
name = "Bundle B"
desc = "Bundle B"
items = ["tool-a", "tool-b"]
"#
}

fn cycle_manifest() -> &'static str {
    r#"
required_version = "0.1.0"
distros = ["ubuntu"]
shell = "bash"
default_bundles = ["core"]

[items.alpha]
name = "Alpha"
desc = "Alpha"
depends_on = ["beta"]

[[items.alpha.recipes]]
mode = "user"
distros = ["ubuntu"]
cmd = "curl -fsSL https://example.com/alpha.tar.gz | tar -xz -C ~/.local/bin"

[[items.alpha.verifiers]]
mode = "user"
distros = ["ubuntu"]
cmd = "command -v alpha"

[items.beta]
name = "Beta"
desc = "Beta"
depends_on = ["gamma"]

[[items.beta.recipes]]
mode = "user"
distros = ["ubuntu"]
cmd = "curl -fsSL https://example.com/beta.tar.gz | tar -xz -C ~/.local/bin"

[items.gamma]
name = "Gamma"
desc = "Gamma"
depends_on = ["alpha"]

[[items.beta.verifiers]]
mode = "user"
distros = ["ubuntu"]
cmd = "command -v beta"

[[items.gamma.recipes]]
mode = "user"
distros = ["ubuntu"]
cmd = "curl -fsSL https://example.com/gamma.tar.gz | tar -xz -C ~/.local/bin"

[[items.gamma.verifiers]]
mode = "user"
distros = ["ubuntu"]
cmd = "command -v gamma"

[bundles.core]
name = "Core"
desc = "Core"
items = ["alpha", "beta", "gamma"]
"#
}

fn target_error_manifest() -> &'static str {
    r#"
required_version = "0.1.0"
distros = ["ubuntu"]
shell = "bash"
default_bundles = ["core"]

[items.native-only]
name = "Native Only"
desc = "Native only"
depends_on = []

[[items.native-only.recipes]]
mode = "sudo"
distros = ["ubuntu"]
cmd = "sudo apt install -y native-only"

[[items.native-only.verifiers]]
mode = "sudo"
distros = ["ubuntu"]
cmd = "command -v native-only"

[bundles.core]
name = "Core"
desc = "Core"
items = ["native-only"]
"#
}

fn default_replacement_manifest() -> &'static str {
    r#"
required_version = "0.1.0"
distros = ["ubuntu"]
shell = "bash"
default_bundles = ["defaults"]

[items.default-tool]
name = "Default Tool"
desc = "Default Tool"
depends_on = []

[[items.default-tool.recipes]]
mode = "user"
distros = ["ubuntu"]
cmd = "curl -fsSL https://example.com/default-tool -o ~/.local/bin/default-tool && chmod +x ~/.local/bin/default-tool"

[[items.default-tool.verifiers]]
mode = "user"
distros = ["ubuntu"]
cmd = "command -v default-tool"

[items.explicit-tool]
name = "Explicit Tool"
desc = "Explicit Tool"
depends_on = []

[[items.explicit-tool.recipes]]
mode = "user"
distros = ["ubuntu"]
cmd = "curl -fsSL https://example.com/explicit-tool -o ~/.local/bin/explicit-tool && chmod +x ~/.local/bin/explicit-tool"

[[items.explicit-tool.verifiers]]
mode = "user"
distros = ["ubuntu"]
cmd = "command -v explicit-tool"

[bundles.defaults]
name = "Defaults"
desc = "Defaults"
items = ["default-tool"]

[bundles.explicit]
name = "Explicit"
desc = "Explicit"
items = ["explicit-tool"]
"#
}

fn unsupported_coverage_manifest() -> &'static str {
    r#"
required_version = "0.1.0"
distros = ["ubuntu", "fedora"]
shell = "bash"
default_bundles = ["core"]

[items.fedora-user]
name = "Fedora User"
desc = "Fedora User"
depends_on = []

[[items.fedora-user.recipes]]
mode = "user"
distros = ["fedora"]
cmd = "curl -fsSL https://example.com/fedora-user -o ~/.local/bin/fedora-user && chmod +x ~/.local/bin/fedora-user"

[[items.fedora-user.verifiers]]
mode = "user"
distros = ["fedora"]
cmd = "command -v fedora-user"

[items.ubuntu-system-user-verified]
name = "Ubuntu System User Verified"
desc = "Ubuntu System User Verified"
depends_on = []

[[items.ubuntu-system-user-verified.recipes]]
mode = "sudo"
distros = ["ubuntu"]
cmd = "sudo apt install -y ubuntu-system-user-verified"

[[items.ubuntu-system-user-verified.verifiers]]
mode = "user"
distros = ["ubuntu"]
cmd = "command -v ubuntu-system-user-verified"

[bundles.core]
name = "Core"
desc = "Core"
items = ["fedora-user", "ubuntu-system-user-verified"]
"#
}
