use std::collections::BTreeMap;
use std::path::PathBuf;

use envira::catalog::{Catalog, TargetBackend, TargetSource};
use envira::executor::{
    build_execution_plan, CommandOperation, ExecutionRecipe, ExecutionTarget, OperationSpec,
};
use envira::planner::{
    build_install_plan, classify_install_plan, InstallPlan, PlanStep, PlannedScope, PlannerError,
    PlannerRequest,
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
fn same_item_selects_different_native_targets_per_distribution() {
    let catalog = envira::catalog::load_embedded_catalog().expect("embedded catalog should parse");

    let ubuntu = install_plan_for(
        &catalog,
        platform_context(TargetBackend::Apt, RuntimeScope::System),
        PlannerRequest::item("vnc"),
    );
    let fedora = install_plan_for(
        &catalog,
        platform_context(TargetBackend::Dnf, RuntimeScope::System),
        PlannerRequest::item("vnc"),
    );
    let arch = install_plan_for(
        &catalog,
        platform_context(TargetBackend::Pacman, RuntimeScope::System),
        PlannerRequest::item("vnc"),
    );
    let opensuse = install_plan_for(
        &catalog,
        platform_context(TargetBackend::Zypper, RuntimeScope::System),
        PlannerRequest::item("vnc"),
    );

    let ubuntu_step = planned_step_for(&ubuntu, "vnc");
    let fedora_step = planned_step_for(&fedora, "vnc");
    let arch_step = planned_step_for(&arch, "vnc");
    let opensuse_step = planned_step_for(&opensuse, "vnc");

    assert_eq!(ubuntu_step.selected_target.backend, TargetBackend::Apt);
    assert_eq!(fedora_step.selected_target.backend, TargetBackend::Dnf);
    assert_eq!(arch_step.selected_target.backend, TargetBackend::Pacman);
    assert_eq!(opensuse_step.selected_target.backend, TargetBackend::Zypper);
    assert_eq!(
        ubuntu_step.selected_target.source,
        TargetSource::DistributionPackage
    );
    assert_eq!(
        fedora_step.selected_target.source,
        TargetSource::DistributionPackage
    );
    assert_eq!(
        arch_step.selected_target.source,
        TargetSource::DistributionPackage
    );
    assert_eq!(
        opensuse_step.selected_target.source,
        TargetSource::DistributionPackage
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
fn native_execution_plan_uses_catalog_shell_contract_instead_of_backend_adapter() {
    let catalog =
        Catalog::from_toml_str(native_only_manifest()).expect("fixture catalog should parse");
    let platform = platform_context(TargetBackend::Apt, RuntimeScope::System);
    let install_plan =
        build_install_plan(&catalog, &platform, &PlannerRequest::item("native-only"))
            .expect("native target should plan");
    let action_plan = classify_install_plan(
        &install_plan,
        &BTreeMap::from([(
            "native-only".to_string(),
            missing_result(VerificationStage::Present),
        )]),
    )
    .expect("action plan should classify");

    let execution_plan = build_execution_plan(&catalog, &platform, &action_plan)
        .expect("execution planning should use the catalog shell contract");
    let step = execution_plan
        .steps
        .iter()
        .find(|step| step.action_step.step.item_id == "native-only")
        .expect("native-only execution step should exist");

    assert_eq!(step.execution_target, ExecutionTarget::System);
    assert_eq!(
        step.recipe,
        Some(ExecutionRecipe::Shell {
            shell: "bash".to_string(),
            command: "sudo apt install -y native-only".to_string(),
        })
    );
    assert_eq!(
        step.operations,
        vec![OperationSpec::Command(
            CommandOperation::shell("bash", "sudo apt install -y native-only")
                .with_target(ExecutionTarget::System)
        )]
    );
}

#[test]
fn user_recipe_planning_uses_generic_user_shell_target() {
    let catalog =
        Catalog::from_toml_str(direct_binary_manifest()).expect("fixture catalog should parse");
    let platform = platform_context(TargetBackend::Apt, RuntimeScope::User);
    let install_plan =
        build_install_plan(&catalog, &platform, &PlannerRequest::item("portable-tool"))
            .expect("install plan should build");

    let step = planned_step_for(&install_plan, "portable-tool");
    assert_eq!(step.catalog_scope, envira::catalog::InstallScope::User);
    assert_eq!(step.planned_scope, PlannedScope::User);
    assert_eq!(step.selected_target.backend, TargetBackend::DirectBinary);
    assert_eq!(step.selected_target.source, TargetSource::GithubRelease);
}

#[test]
fn install_plan_serialization_exposes_selected_target_details_for_active_consumers() {
    let catalog =
        Catalog::from_toml_str(direct_binary_manifest()).expect("fixture catalog should parse");
    let platform = platform_context(TargetBackend::Apt, RuntimeScope::User);
    let install_plan =
        build_install_plan(&catalog, &platform, &PlannerRequest::item("portable-tool"))
            .expect("install plan should build");

    let json_value = serde_json::to_value(&install_plan).expect("install plan should serialize");

    assert_eq!(json_value["steps"][0]["recipe"], serde_json::Value::Null);
    assert_eq!(
        json_value["steps"][0]["selected_target"],
        json!({
            "backend": "direct_binary",
            "source": "github_release"
        })
    );
    assert_eq!(json_value["steps"][0]["planned_scope"], "user");
}

fn install_plan_for(
    catalog: &Catalog,
    platform: PlatformContext,
    request: PlannerRequest,
) -> InstallPlan {
    build_install_plan(catalog, &platform, &request).expect("install plan should build")
}

fn planned_step_for<'a>(plan: &'a InstallPlan, item_id: &str) -> &'a PlanStep {
    plan.steps
        .iter()
        .find(|step| step.item_id == item_id)
        .expect("expected item in install plan")
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

fn direct_binary_manifest() -> &'static str {
    r#"
required_version = "0.1.0"
distros = ["ubuntu"]
shell = "bash"
default_bundles = ["portable"]

[items.portable-tool]
name = "Portable Tool"
desc = "Portable tool"
depends_on = []

[[items.portable-tool.recipes]]
mode = "user"
distros = ["ubuntu"]
cmd = "curl -fsSL https://example.com/portable-tool -o ~/.local/bin/portable-tool && chmod +x ~/.local/bin/portable-tool"

[[items.portable-tool.verifiers]]
mode = "user"
distros = ["ubuntu"]
cmd = "command -v portable-tool"

[bundles.portable]
name = "Portable"
desc = "Portable"
items = ["portable-tool"]
"#
}
