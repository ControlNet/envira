use std::collections::BTreeMap;
use std::path::PathBuf;

use envira::catalog::{Catalog, TargetBackend};
use envira::planner::{
    build_install_plan, classify_install_plan, ActionReasonCode, PlannedAction, PlannerRequest,
};
use envira::platform::{
    ArchitectureIdentity, ArchitectureKind, DistroIdentity, DistroKind, InvocationKind,
    PlatformContext, RuntimeScope, UserAccount,
};
use envira::verifier::{
    EvidenceRecord, EvidenceStatus, ObservedScope, ProbeKind, ProbeRequirement, ServiceAssessment,
    ServiceKind, ServiceUsabilityState, VerificationHealth, VerificationProfile, VerificationStage,
    VerificationSummary, VerifierCheck, VerifierEvidence, VerifierResult,
};

#[test]
fn already_operational_item_becomes_skip_and_serializes_verifier_summary() {
    let catalog = fixture_catalog();
    let plan = plan_for(&catalog, "ready-tool");
    let action_plan = classify_install_plan(
        &plan,
        &BTreeMap::from([(
            "ready-tool".to_string(),
            successful_result(VerificationStage::Operational),
        )]),
    )
    .expect("action plan should classify");

    let step = &action_plan.steps[0];
    assert_eq!(step.action, PlannedAction::Skip);
    assert_eq!(step.rationale.code, ActionReasonCode::ThresholdMet);
    assert!(step.rationale.verifier.threshold_met);

    let json = serde_json::to_value(&action_plan).expect("action plan should serialize");
    assert_eq!(json["steps"][0]["action"], "skip");
    assert_eq!(
        json["steps"][0]["rationale"]["verifier"]["summary"]["satisfied_checks"],
        1
    );
}

#[test]
fn missing_item_becomes_install() {
    let catalog = fixture_catalog();
    let plan = plan_for(&catalog, "missing-tool");
    let action_plan = classify_install_plan(
        &plan,
        &BTreeMap::from([(
            "missing-tool".to_string(),
            missing_result(VerificationStage::Present),
        )]),
    )
    .expect("action plan should classify");

    let step = &action_plan.steps[0];
    assert_eq!(step.action, PlannedAction::Install);
    assert_eq!(step.rationale.code, ActionReasonCode::Missing);
    assert_eq!(step.rationale.verifier.achieved_stage, None);
}

#[test]
fn partially_present_broken_item_becomes_repair() {
    let catalog = fixture_catalog();
    let plan = plan_for(&catalog, "broken-tool");
    let action_plan = classify_install_plan(
        &plan,
        &BTreeMap::from([(
            "broken-tool".to_string(),
            repair_result(VerificationStage::Configured),
        )]),
    )
    .expect("action plan should classify");

    let step = &action_plan.steps[0];
    assert_eq!(step.action, PlannedAction::Repair);
    assert_eq!(step.rationale.code, ActionReasonCode::BelowThreshold);
    assert_eq!(
        step.rationale.verifier.achieved_stage,
        Some(VerificationStage::Present)
    );
}

#[test]
fn unknown_item_becomes_blocked() {
    let catalog = fixture_catalog();
    let plan = plan_for(&catalog, "unknown-tool");
    let action_plan = classify_install_plan(
        &plan,
        &BTreeMap::from([(
            "unknown-tool".to_string(),
            unknown_result(VerificationStage::Present),
        )]),
    )
    .expect("action plan should classify");

    let step = &action_plan.steps[0];
    assert_eq!(step.action, PlannedAction::Blocked);
    assert_eq!(step.rationale.code, ActionReasonCode::VerificationUnknown);
}

#[test]
fn service_on_demand_item_becomes_blocked() {
    let catalog = fixture_catalog();
    let plan = plan_for(&catalog, "on-demand-service");
    let action_plan = classify_install_plan(
        &plan,
        &BTreeMap::from([(
            "on-demand-service".to_string(),
            service_result(
                VerificationStage::Operational,
                ServiceUsabilityState::OnDemand,
                Some(VerificationStage::Configured),
            ),
        )]),
    )
    .expect("action plan should classify");

    let step = &action_plan.steps[0];
    assert_eq!(step.action, PlannedAction::Blocked);
    assert_eq!(step.rationale.code, ActionReasonCode::ServiceOnDemand);
    assert_eq!(
        step.rationale
            .verifier
            .service
            .as_ref()
            .map(|service| service.state),
        Some(ServiceUsabilityState::OnDemand)
    );
}

#[test]
fn service_blocked_item_becomes_blocked() {
    let catalog = fixture_catalog();
    let plan = plan_for(&catalog, "blocked-service");
    let action_plan = classify_install_plan(
        &plan,
        &BTreeMap::from([(
            "blocked-service".to_string(),
            service_result(
                VerificationStage::Operational,
                ServiceUsabilityState::Blocked,
                Some(VerificationStage::Present),
            ),
        )]),
    )
    .expect("action plan should classify");

    let step = &action_plan.steps[0];
    assert_eq!(step.action, PlannedAction::Blocked);
    assert_eq!(step.rationale.code, ActionReasonCode::ServiceBlocked);
}

#[test]
fn present_but_non_usable_service_becomes_repair_even_when_present_stage_exists() {
    let catalog = fixture_catalog();
    let plan = plan_for(&catalog, "blocked-service");
    let action_plan = classify_install_plan(
        &plan,
        &BTreeMap::from([(
            "blocked-service".to_string(),
            service_result(
                VerificationStage::Present,
                ServiceUsabilityState::NonUsable,
                Some(VerificationStage::Present),
            ),
        )]),
    )
    .expect("action plan should classify");

    let step = &action_plan.steps[0];
    assert_eq!(step.action, PlannedAction::Repair);
    assert_eq!(step.rationale.code, ActionReasonCode::ServiceNonUsable);
    assert!(!step.rationale.verifier.threshold_met);
}

#[test]
fn dependency_blocked_reason_propagates_to_dependents() {
    let catalog = fixture_catalog();
    let plan = plan_for(&catalog, "app-with-dependency");
    let action_plan = classify_install_plan(
        &plan,
        &BTreeMap::from([
            (
                "blocked-service".to_string(),
                service_result(
                    VerificationStage::Operational,
                    ServiceUsabilityState::Blocked,
                    Some(VerificationStage::Present),
                ),
            ),
            (
                "app-with-dependency".to_string(),
                missing_result(VerificationStage::Present),
            ),
        ]),
    )
    .expect("action plan should classify");

    assert_eq!(action_plan.steps.len(), 2);
    assert_eq!(action_plan.steps[0].step.item_id, "blocked-service");
    assert_eq!(action_plan.steps[0].action, PlannedAction::Blocked);

    let dependent = &action_plan.steps[1];
    assert_eq!(dependent.step.item_id, "app-with-dependency");
    assert_eq!(dependent.action, PlannedAction::Blocked);
    assert_eq!(
        dependent.rationale.code,
        ActionReasonCode::DependencyBlocked
    );
    assert_eq!(dependent.rationale.blocked_by.len(), 1);
    assert_eq!(dependent.rationale.blocked_by[0].item_id, "blocked-service");
    assert_eq!(
        dependent.rationale.blocked_by[0].reason_code,
        ActionReasonCode::ServiceBlocked
    );
}

fn fixture_catalog() -> Catalog {
    Catalog::from_toml_str(fixture_manifest()).expect("fixture catalog should parse")
}

fn plan_for(catalog: &Catalog, item_id: &str) -> envira::planner::InstallPlan {
    build_install_plan(
        catalog,
        &platform_context(),
        &PlannerRequest::item(item_id.to_string()),
    )
    .expect("plan should build")
}

fn successful_result(required_stage: VerificationStage) -> VerifierResult {
    let evidence = vec![required_evidence(
        VerificationStage::Operational,
        EvidenceStatus::Satisfied,
    )];

    VerifierResult {
        requested_profile: VerificationProfile::Quick,
        required_stage,
        achieved_stage: Some(VerificationStage::Operational),
        threshold_met: true,
        health: VerificationHealth::Healthy,
        observed_scope: ObservedScope::User,
        summary: summarize(&evidence),
        evidence,
        service_evidence: Vec::new(),
        service: None,
    }
}

fn missing_result(required_stage: VerificationStage) -> VerifierResult {
    let evidence = vec![required_evidence(
        VerificationStage::Present,
        EvidenceStatus::Missing,
    )];

    VerifierResult {
        requested_profile: VerificationProfile::Quick,
        required_stage,
        achieved_stage: None,
        threshold_met: false,
        health: VerificationHealth::Missing,
        observed_scope: ObservedScope::Unknown,
        summary: summarize(&evidence),
        evidence,
        service_evidence: Vec::new(),
        service: None,
    }
}

fn repair_result(required_stage: VerificationStage) -> VerifierResult {
    let evidence = vec![
        required_evidence(VerificationStage::Present, EvidenceStatus::Satisfied),
        required_evidence(VerificationStage::Configured, EvidenceStatus::Broken),
    ];

    VerifierResult {
        requested_profile: VerificationProfile::Quick,
        required_stage,
        achieved_stage: Some(VerificationStage::Present),
        threshold_met: false,
        health: VerificationHealth::Broken,
        observed_scope: ObservedScope::User,
        summary: summarize(&evidence),
        evidence,
        service_evidence: Vec::new(),
        service: None,
    }
}

fn unknown_result(required_stage: VerificationStage) -> VerifierResult {
    let evidence = vec![required_evidence(
        VerificationStage::Present,
        EvidenceStatus::Unknown,
    )];

    VerifierResult {
        requested_profile: VerificationProfile::Quick,
        required_stage,
        achieved_stage: None,
        threshold_met: false,
        health: VerificationHealth::Unknown,
        observed_scope: ObservedScope::Unknown,
        summary: summarize(&evidence),
        evidence,
        service_evidence: Vec::new(),
        service: None,
    }
}

fn service_result(
    required_stage: VerificationStage,
    state: ServiceUsabilityState,
    achieved_stage: Option<VerificationStage>,
) -> VerifierResult {
    let evidence = vec![required_evidence(
        VerificationStage::Present,
        EvidenceStatus::Satisfied,
    )];
    let health = VerificationHealth::Healthy.max(state.health());
    let threshold_met = state == ServiceUsabilityState::Operational
        && achieved_stage.is_some_and(|stage| stage.meets(required_stage));

    VerifierResult {
        requested_profile: VerificationProfile::Quick,
        required_stage,
        achieved_stage,
        threshold_met,
        health,
        observed_scope: ObservedScope::User,
        summary: summarize(&evidence),
        evidence,
        service_evidence: Vec::new(),
        service: Some(ServiceAssessment {
            kind: ServiceKind::Jupyter,
            state,
            achieved_stage,
            observed_scope: ObservedScope::User,
            summary: format!("service state: {}", service_state_name(state)),
            detail: None,
        }),
    }
}

fn required_evidence(stage: VerificationStage, status: EvidenceStatus) -> VerifierEvidence {
    VerifierEvidence {
        check: VerifierCheck {
            stage,
            requirement: ProbeRequirement::Required,
            min_profile: VerificationProfile::Quick,
            kind: ProbeKind::Command,
            command: Some("fixture-command".to_string()),
            commands: None,
            path: None,
            pattern: None,
        },
        record: EvidenceRecord {
            status,
            observed_scope: ObservedScope::User,
            summary: format!("required evidence is {}", evidence_status_name(status)),
            detail: None,
        },
        participates: true,
    }
}

fn summarize(evidence: &[VerifierEvidence]) -> VerificationSummary {
    let mut summary = VerificationSummary {
        total_checks: evidence.len(),
        participating_checks: evidence.iter().filter(|entry| entry.participates).count(),
        skipped_checks: evidence.iter().filter(|entry| !entry.participates).count(),
        ..VerificationSummary::default()
    };

    for entry in evidence.iter().filter(|entry| entry.participates) {
        match entry.record.status {
            EvidenceStatus::Satisfied => summary.satisfied_checks += 1,
            EvidenceStatus::Missing => {
                summary.missing_checks += 1;
                if entry.check.requirement == ProbeRequirement::Required {
                    summary.required_failures += 1;
                }
            }
            EvidenceStatus::Broken => {
                summary.broken_checks += 1;
                if entry.check.requirement == ProbeRequirement::Required {
                    summary.required_failures += 1;
                }
            }
            EvidenceStatus::Unknown => {
                summary.unknown_checks += 1;
                if entry.check.requirement == ProbeRequirement::Required {
                    summary.required_failures += 1;
                }
            }
            EvidenceStatus::NotApplicable => summary.not_applicable_checks += 1,
        }
    }

    summary
}

fn evidence_status_name(status: EvidenceStatus) -> &'static str {
    match status {
        EvidenceStatus::Satisfied => "satisfied",
        EvidenceStatus::Missing => "missing",
        EvidenceStatus::Broken => "broken",
        EvidenceStatus::Unknown => "unknown",
        EvidenceStatus::NotApplicable => "not_applicable",
    }
}

fn service_state_name(state: ServiceUsabilityState) -> &'static str {
    match state {
        ServiceUsabilityState::Operational => "operational",
        ServiceUsabilityState::OnDemand => "on_demand",
        ServiceUsabilityState::Blocked => "blocked",
        ServiceUsabilityState::NonUsable => "non_usable",
        ServiceUsabilityState::Missing => "missing",
        ServiceUsabilityState::Unknown => "unknown",
    }
}

fn platform_context() -> PlatformContext {
    let user = UserAccount {
        username: "alice".to_string(),
        home_dir: PathBuf::from("/home/alice"),
        uid: Some(1000),
        gid: Some(1000),
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
        native_backend: Some(TargetBackend::Apt),
        invocation: InvocationKind::User,
        effective_user: user.clone(),
        target_user: Some(user),
        runtime_scope: RuntimeScope::User,
    }
}

fn fixture_manifest() -> &'static str {
    r#"
required_version = "0.1.0"
distros = ["ubuntu"]
shell = "bash"
default_bundles = ["core"]

[items.ready-tool]
name = "Ready Tool"
desc = "Ready tool"
depends_on = []

[[items.ready-tool.recipes]]
mode = "user"
distros = ["ubuntu"]
cmd = "curl -fsSL https://example.com/ready-tool.tar.gz | tar -xz -C ~/.local/bin"

[[items.ready-tool.verifiers]]
mode = "user"
distros = ["ubuntu"]
cmd = "command -v ready-tool"

[items.missing-tool]
name = "Missing Tool"
desc = "Missing tool"
depends_on = []

[[items.missing-tool.recipes]]
mode = "user"
distros = ["ubuntu"]
cmd = "curl -fsSL https://example.com/missing-tool.tar.gz | tar -xz -C ~/.local/bin"

[[items.missing-tool.verifiers]]
mode = "user"
distros = ["ubuntu"]
cmd = "command -v missing-tool"

[items.broken-tool]
name = "Broken Tool"
desc = "Broken tool"
depends_on = []

[[items.broken-tool.recipes]]
mode = "user"
distros = ["ubuntu"]
cmd = "curl -fsSL https://example.com/broken-tool.tar.gz | tar -xz -C ~/.local/bin"

[[items.broken-tool.verifiers]]
mode = "user"
distros = ["ubuntu"]
cmd = "command -v broken-tool"

[items.unknown-tool]
name = "Unknown Tool"
desc = "Unknown tool"
depends_on = []

[[items.unknown-tool.recipes]]
mode = "user"
distros = ["ubuntu"]
cmd = "curl -fsSL https://example.com/unknown-tool.tar.gz | tar -xz -C ~/.local/bin"

[[items.unknown-tool.verifiers]]
mode = "user"
distros = ["ubuntu"]
cmd = "command -v unknown-tool"

[items.blocked-service]
name = "Blocked Service"
desc = "Blocked service"
depends_on = []

[[items.blocked-service.recipes]]
mode = "user"
distros = ["ubuntu"]
cmd = "curl -fsSL https://example.com/blocked-service.tar.gz | tar -xz -C ~/.local/bin"

[[items.blocked-service.verifiers]]
mode = "user"
distros = ["ubuntu"]
cmd = "command -v blocked-service"

[items.on-demand-service]
name = "On Demand Service"
desc = "On demand service"
depends_on = []

[[items.on-demand-service.recipes]]
mode = "user"
distros = ["ubuntu"]
cmd = "curl -fsSL https://example.com/on-demand-service.tar.gz | tar -xz -C ~/.local/bin"

[[items.on-demand-service.verifiers]]
mode = "user"
distros = ["ubuntu"]
cmd = "command -v on-demand-service"

[items.app-with-dependency]
name = "App With Dependency"
desc = "App With Dependency"
depends_on = ["blocked-service"]

[[items.app-with-dependency.recipes]]
mode = "user"
distros = ["ubuntu"]
cmd = "curl -fsSL https://example.com/app-with-dependency.tar.gz | tar -xz -C ~/.local/bin"

[[items.app-with-dependency.verifiers]]
mode = "user"
distros = ["ubuntu"]
cmd = "command -v app-with-dependency"

[bundles.core]
name = "Core"
desc = "Core"
items = [
  "ready-tool",
  "missing-tool",
  "broken-tool",
  "unknown-tool",
  "blocked-service",
  "on-demand-service",
  "app-with-dependency",
]
"#
}
