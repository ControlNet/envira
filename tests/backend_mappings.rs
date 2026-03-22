use std::collections::BTreeMap;
use std::path::PathBuf;

use envira::catalog::{Catalog, TargetBackend};
use envira::executor::{
    build_execution_plan, BuiltinRecipePlan, ExecutionPlanError, ExecutionRecipe, ExecutionTarget,
    OperationSpec,
};
use envira::planner::{
    build_install_plan, classify_install_plan, PlannedAction, PlannerError, PlannerRequest,
};
use envira::platform::{
    ArchitectureIdentity, ArchitectureKind, DistroIdentity, DistroKind, InvocationKind,
    PlatformContext, RuntimeScope, UserAccount,
};
use envira::verifier::{
    EvidenceRecord, EvidenceStatus, ObservedScope, ProbeKind, ProbeRequirement, VerificationHealth,
    VerificationProfile, VerificationStage, VerificationSummary, VerifierCheck, VerifierEvidence,
    VerifierResult,
};
use serde_json::json;

#[test]
fn same_item_maps_to_different_native_backend_commands_per_distribution() {
    let catalog = envira::catalog::load_embedded_catalog().expect("embedded catalog should parse");

    let ubuntu = execution_plan_for(
        &catalog,
        platform_context(TargetBackend::Apt, RuntimeScope::System),
        PlannerRequest::item("vnc"),
        &["essentials", "vnc"],
    );
    let fedora = execution_plan_for(
        &catalog,
        platform_context(TargetBackend::Dnf, RuntimeScope::System),
        PlannerRequest::item("vnc"),
        &["essentials", "vnc"],
    );
    let arch = execution_plan_for(
        &catalog,
        platform_context(TargetBackend::Pacman, RuntimeScope::System),
        PlannerRequest::item("vnc"),
        &["essentials", "vnc"],
    );
    let opensuse = execution_plan_for(
        &catalog,
        platform_context(TargetBackend::Zypper, RuntimeScope::System),
        PlannerRequest::item("vnc"),
        &["essentials", "vnc"],
    );

    let ubuntu_step = step_for(&ubuntu, "vnc");
    let fedora_step = step_for(&fedora, "vnc");
    let arch_step = step_for(&arch, "vnc");
    let opensuse_step = step_for(&opensuse, "vnc");

    assert_eq!(ubuntu_step.execution_target, ExecutionTarget::System);
    assert_eq!(
        command_programs(&ubuntu_step.operations),
        vec!["apt", "apt"]
    );
    assert_eq!(
        command_args(&ubuntu_step.operations[1]),
        vec![
            "install",
            "-y",
            "tigervnc-standalone-server",
            "tigervnc-common",
            "tigervnc-xorg-extension",
        ]
    );

    assert_eq!(command_programs(&fedora_step.operations), vec!["dnf"]);
    assert_eq!(
        command_args(&fedora_step.operations[0]),
        vec!["install", "-y", "tigervnc-server"]
    );

    assert_eq!(command_programs(&arch_step.operations), vec!["pacman"]);
    assert_eq!(
        command_args(&arch_step.operations[0]),
        vec!["-Sy", "--noconfirm", "tigervnc"]
    );

    assert_eq!(command_programs(&opensuse_step.operations), vec!["zypper"]);
    assert_eq!(
        command_args(&opensuse_step.operations[0]),
        vec!["install", "-y", "tigervnc"]
    );
}

#[test]
fn unsupported_native_target_is_reported_before_execution_planning() {
    let catalog =
        Catalog::from_toml_str(native_only_manifest()).expect("fixture catalog should parse");
    let platform = unsupported_platform_context();

    let error = build_install_plan(&catalog, &platform, &PlannerRequest::item("native-only"))
        .expect_err("unsupported native target should be rejected");

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
fn missing_recipe_overlay_reports_explicit_execution_error() {
    let catalog =
        Catalog::from_toml_str(missing_recipe_manifest()).expect("fixture catalog should parse");
    let platform = platform_context(TargetBackend::Apt, RuntimeScope::User);
    let install_plan =
        build_install_plan(&catalog, &platform, &PlannerRequest::item("portable-tool"))
            .expect("portable target should plan");
    let action_plan = classify_install_plan(
        &install_plan,
        &BTreeMap::from([(
            "portable-tool".to_string(),
            missing_result(VerificationStage::Present),
        )]),
    )
    .expect("action plan should classify");

    let error = build_execution_plan(&catalog, &platform, &action_plan)
        .expect_err("execution planning should require a matching recipe overlay");

    match error {
        ExecutionPlanError::MissingRecipe { item_id, target } => {
            assert_eq!(item_id, "portable-tool");
            assert_eq!(target.backend, TargetBackend::Archive);
        }
        other => panic!("expected missing recipe error, got {other}"),
    }
}

#[test]
fn builtin_recipe_planning_stays_typed_and_bounded() {
    let catalog =
        Catalog::from_toml_str(direct_binary_manifest()).expect("fixture catalog should parse");
    let platform = platform_context(TargetBackend::Apt, RuntimeScope::User);
    let execution_plan = execution_plan_for(
        &catalog,
        platform,
        PlannerRequest::item("portable-tool"),
        &["portable-tool"],
    );

    let step = step_for(&execution_plan, "portable-tool");
    assert_eq!(step.action_step.action, PlannedAction::Install);
    assert_eq!(step.execution_target, ExecutionTarget::CurrentProcess);

    match step
        .recipe
        .as_ref()
        .expect("builtin recipe should be attached")
    {
        ExecutionRecipe::Builtin(BuiltinRecipePlan::DirectBinaryInstall {
            url,
            destination,
            binary_name,
            ..
        }) => {
            assert_eq!(url, "https://example.com/portable-tool");
            assert_eq!(binary_name, "portable-tool");
            assert_eq!(
                destination,
                &PathBuf::from("/home/alice/.local/bin/portable-tool")
            );
        }
        other => panic!("expected typed direct binary recipe, got {other:?}"),
    }

    assert!(step
        .operations
        .iter()
        .all(|operation| matches!(operation, OperationSpec::Command(_))));
    assert!(command_programs(&step.operations)
        .into_iter()
        .all(|program| program != "sh" && program != "bash"));
}

#[test]
fn execution_plan_serialization_exposes_operation_details_for_future_consumers() {
    let catalog =
        Catalog::from_toml_str(direct_binary_manifest()).expect("fixture catalog should parse");
    let platform = platform_context(TargetBackend::Apt, RuntimeScope::User);
    let execution_plan = execution_plan_for(
        &catalog,
        platform,
        PlannerRequest::item("portable-tool"),
        &["portable-tool"],
    );

    let json_value =
        serde_json::to_value(&execution_plan).expect("execution plan should serialize");

    assert_eq!(json_value["steps"][0]["action_step"]["action"], "install");
    assert_eq!(
        json_value["steps"][0]["execution_target"],
        "current_process"
    );
    assert_eq!(
        json_value["steps"][0]["recipe"],
        json!({
            "kind": "builtin",
            "family": "direct_binary_install",
            "url": "https://example.com/portable-tool",
            "destination": "/home/alice/.local/bin/portable-tool",
            "binary_name": "portable-tool"
        })
    );
    assert_eq!(
        json_value["steps"][0]["operations"][2],
        json!({
            "kind": "command",
            "program": "curl",
            "args": ["-fsSL", "https://example.com/portable-tool", "-o", "/tmp/envira/direct-binary/portable-tool/portable-tool"],
            "env": {},
            "cwd": null,
            "timeout_ms": null,
            "target": "current_process"
        })
    );
}

fn execution_plan_for(
    catalog: &Catalog,
    platform: PlatformContext,
    request: PlannerRequest,
    missing_items: &[&str],
) -> envira::executor::ExecutionPlan {
    let install_plan =
        build_install_plan(catalog, &platform, &request).expect("install plan should build");
    let verifier_results = missing_items
        .iter()
        .map(|item_id| {
            (
                (*item_id).to_string(),
                missing_result(VerificationStage::Present),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let action_plan = classify_install_plan(&install_plan, &verifier_results)
        .expect("action plan should classify");
    build_execution_plan(catalog, &platform, &action_plan).expect("execution plan should build")
}

fn step_for<'a>(
    plan: &'a envira::executor::ExecutionPlan,
    item_id: &str,
) -> &'a envira::executor::ExecutionStep {
    plan.steps
        .iter()
        .find(|step| step.action_step.step.item_id == item_id)
        .expect("expected item in execution plan")
}

fn command_programs(operations: &[OperationSpec]) -> Vec<&str> {
    operations
        .iter()
        .map(|operation| match operation {
            OperationSpec::Command(command) => command.program.as_str(),
            other => panic!("expected command operation, got {other:?}"),
        })
        .collect()
}

fn command_args(operation: &OperationSpec) -> Vec<&str> {
    match operation {
        OperationSpec::Command(command) => command.args.iter().map(String::as_str).collect(),
        other => panic!("expected command operation, got {other:?}"),
    }
}

fn platform_context(native_backend: TargetBackend, runtime_scope: RuntimeScope) -> PlatformContext {
    let (distro_kind, distro_id, distro_name) = match native_backend {
        TargetBackend::Apt => (DistroKind::Ubuntu, "ubuntu", "Ubuntu"),
        TargetBackend::Pacman => (DistroKind::Arch, "arch", "Arch Linux"),
        TargetBackend::Dnf => (DistroKind::Fedora, "fedora", "Fedora"),
        TargetBackend::Zypper => (
            DistroKind::OpenSuse,
            "opensuse-tumbleweed",
            "openSUSE Tumbleweed",
        ),
        other => panic!("unexpected native backend fixture: {other:?}"),
    };
    let effective_user = match runtime_scope {
        RuntimeScope::System => user("root", "/root", 0, 0),
        RuntimeScope::User | RuntimeScope::Both | RuntimeScope::Unknown => {
            user("alice", "/home/alice", 1000, 1000)
        }
    };
    let target_user = match runtime_scope {
        RuntimeScope::Both => Some(user("alice", "/home/alice", 1000, 1000)),
        RuntimeScope::User => Some(effective_user.clone()),
        RuntimeScope::System | RuntimeScope::Unknown => None,
    };
    let invocation = match runtime_scope {
        RuntimeScope::System => InvocationKind::Root,
        RuntimeScope::User | RuntimeScope::Unknown => InvocationKind::User,
        RuntimeScope::Both => InvocationKind::Sudo,
    };

    PlatformContext {
        distro: DistroIdentity {
            kind: distro_kind,
            id: distro_id.to_string(),
            name: distro_name.to_string(),
            pretty_name: Some(distro_name.to_string()),
            version_id: Some("latest".to_string()),
        },
        arch: ArchitectureIdentity {
            kind: ArchitectureKind::X86_64,
            raw: "x86_64".to_string(),
        },
        native_backend: Some(native_backend),
        invocation,
        effective_user,
        target_user,
        runtime_scope,
    }
}

fn unsupported_platform_context() -> PlatformContext {
    PlatformContext {
        distro: DistroIdentity {
            kind: DistroKind::Unknown,
            id: "unknown".to_string(),
            name: "Unknown".to_string(),
            pretty_name: None,
            version_id: None,
        },
        arch: ArchitectureIdentity {
            kind: ArchitectureKind::X86_64,
            raw: "x86_64".to_string(),
        },
        native_backend: None,
        invocation: InvocationKind::Root,
        effective_user: user("root", "/root", 0, 0),
        target_user: None,
        runtime_scope: RuntimeScope::System,
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

fn missing_result(required_stage: VerificationStage) -> VerifierResult {
    let evidence = vec![VerifierEvidence {
        check: VerifierCheck {
            stage: VerificationStage::Present,
            requirement: ProbeRequirement::Required,
            min_profile: VerificationProfile::Quick,
            kind: ProbeKind::Command,
            command: Some("fixture-command".to_string()),
            commands: None,
            path: None,
            pattern: None,
        },
        record: EvidenceRecord {
            status: EvidenceStatus::Missing,
            observed_scope: ObservedScope::Unknown,
            summary: "required command is missing".to_string(),
            detail: None,
        },
        participates: true,
    }];

    VerifierResult {
        requested_profile: VerificationProfile::Quick,
        required_stage,
        achieved_stage: None,
        threshold_met: false,
        health: VerificationHealth::Missing,
        observed_scope: ObservedScope::Unknown,
        summary: VerificationSummary {
            total_checks: 1,
            participating_checks: 1,
            skipped_checks: 0,
            satisfied_checks: 0,
            missing_checks: 1,
            broken_checks: 0,
            unknown_checks: 0,
            not_applicable_checks: 0,
            required_failures: 1,
        },
        evidence,
        service_evidence: Vec::new(),
        service: None,
    }
}

fn native_only_manifest() -> &'static str {
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

  [[items.recipes]]
  backend = "apt"
  source = "distribution_package"
  recipe = "native_package"
  packages = ["native-only"]

[[bundles]]
id = "core"
display_name = "Core"
items = ["native-only"]
"#
}

fn missing_recipe_manifest() -> &'static str {
    r#"
schema_version = 1
default_bundles = ["portable"]

[[items]]
id = "portable-tool"
display_name = "Portable Tool"
category = "terminal_tool"
scope = "user"
depends_on = []
targets = [{ backend = "archive", source = "github_release" }]
success_threshold = "present"
standalone = false

  [[items.verifier.checks]]
  threshold = "required"
  kind = "command"
  command = "portable-tool"

[[bundles]]
id = "portable"
display_name = "Portable"
items = ["portable-tool"]
"#
}

fn direct_binary_manifest() -> &'static str {
    r#"
schema_version = 1
default_bundles = ["portable"]

[[items]]
id = "portable-tool"
display_name = "Portable Tool"
category = "terminal_tool"
scope = "user"
depends_on = []
targets = [{ backend = "direct_binary", source = "github_release" }]
success_threshold = "present"
standalone = false

  [[items.verifier.checks]]
  threshold = "required"
  kind = "command"
  command = "portable-tool"

  [[items.recipes]]
  backend = "direct_binary"
  source = "github_release"
  recipe = "direct_binary"
  url = "https://example.com/portable-tool"
  binary_name = "portable-tool"

[[bundles]]
id = "portable"
display_name = "Portable"
items = ["portable-tool"]
"#
}
