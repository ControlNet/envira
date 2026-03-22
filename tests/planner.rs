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
    let platform = platform_context(Some(TargetBackend::Apt), RuntimeScope::User);
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
fn planner_expands_all_default_bundle_in_catalog_order() {
    let catalog = envira::catalog::load_embedded_catalog().expect("embedded catalog should parse");
    let platform = platform_context(Some(TargetBackend::Apt), RuntimeScope::User);

    let plan = build_install_plan(&catalog, &platform, &PlannerRequest::all_default())
        .expect("default bundle plan should build");

    assert_eq!(
        step_ids(&plan),
        vec!["essentials", "bat", "ctop", "fastfetch", "btop"]
    );
    assert!(plan.steps.iter().all(|step| step.requested));
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
            assert_eq!(item_id, "vnc");
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
            assert_eq!(item_id, "docker");
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
schema_version = 1
default_bundles = ["bundle-a"]

[[items]]
id = "base"
display_name = "Base"
category = "foundation"
scope = "hybrid"
depends_on = []
targets = [{ backend = "apt", source = "distribution_package" }]
success_threshold = "present"
standalone = false

  [[items.verifier.checks]]
  threshold = "required"
  kind = "command"
  command = "base"

[[items]]
id = "tool-a"
display_name = "Tool A"
category = "terminal_tool"
scope = "user"
depends_on = ["base"]
targets = [{ backend = "archive", source = "github_release" }]
success_threshold = "present"
standalone = false

  [[items.verifier.checks]]
  threshold = "required"
  kind = "command"
  command = "tool-a"

[[items]]
id = "tool-b"
display_name = "Tool B"
category = "terminal_tool"
scope = "user"
depends_on = ["base"]
targets = [{ backend = "direct_binary", source = "github_release" }]
success_threshold = "present"
standalone = false

  [[items.verifier.checks]]
  threshold = "required"
  kind = "command"
  command = "tool-b"

[[bundles]]
id = "bundle-a"
display_name = "Bundle A"
items = ["base", "tool-a"]

[[bundles]]
id = "bundle-b"
display_name = "Bundle B"
items = ["tool-a", "tool-b"]
"#
}

fn cycle_manifest() -> &'static str {
    r#"
schema_version = 1
default_bundles = ["core"]

[[items]]
id = "alpha"
display_name = "Alpha"
category = "foundation"
scope = "user"
depends_on = ["beta"]
targets = [{ backend = "archive", source = "github_release" }]
success_threshold = "present"
standalone = false

  [[items.verifier.checks]]
  threshold = "required"
  kind = "command"
  command = "alpha"

[[items]]
id = "beta"
display_name = "Beta"
category = "terminal_tool"
scope = "user"
depends_on = ["gamma"]
targets = [{ backend = "archive", source = "github_release" }]
success_threshold = "present"
standalone = false

  [[items.verifier.checks]]
  threshold = "required"
  kind = "command"
  command = "beta"

[[items]]
id = "gamma"
display_name = "Gamma"
category = "terminal_tool"
scope = "user"
depends_on = ["alpha"]
targets = [{ backend = "archive", source = "github_release" }]
success_threshold = "present"
standalone = false

  [[items.verifier.checks]]
  threshold = "required"
  kind = "command"
  command = "gamma"

[[bundles]]
id = "core"
display_name = "Core"
items = ["alpha", "beta", "gamma"]
"#
}

fn target_error_manifest() -> &'static str {
    r#"
schema_version = 1
default_bundles = ["core"]

[[items]]
id = "native-only"
display_name = "Native Only"
category = "foundation"
scope = "system"
depends_on = []
targets = [{ backend = "apt", source = "distribution_package" }]
success_threshold = "present"
standalone = false

  [[items.verifier.checks]]
  threshold = "required"
  kind = "command"
  command = "native-only"

[[bundles]]
id = "core"
display_name = "Core"
items = ["native-only"]
"#
}
