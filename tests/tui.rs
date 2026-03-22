use std::{
    cell::RefCell,
    collections::{BTreeMap, VecDeque},
    path::PathBuf,
};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use envira::{
    catalog::{Catalog, TargetBackend},
    engine::{
        CommandName, CommandPayload, CommandRequest, CommandResponse, InstallMode,
        InstallWorkflowFailure, InstallWorkflowOutcome, InstallWorkflowResult,
        InstallWorkflowStatus, InterfaceMode, OutputFormat, VerificationItemResult,
        VerificationWorkflowResult, VerificationWorkflowSummary,
    },
    executor::{
        ExecutionDisposition, ExecutionPlan, ExecutionPlanReport, ExecutionPlanSummary,
        ExecutionStep, ExecutionStepReport, ExecutionTarget,
    },
    planner::{build_install_plan, classify_install_plan, PlannedAction, PlannerRequest},
    platform::{
        ArchitectureIdentity, ArchitectureKind, DistroIdentity, DistroKind, InvocationKind,
        PlatformContext, RuntimeScope, UserAccount,
    },
    tui::{TuiApp, TuiEnginePort},
    verifier::{
        EvidenceRecord, EvidenceStatus, ObservedScope, ProbeKind, ProbeRequirement,
        VerificationHealth, VerificationProfile, VerificationStage, VerificationSummary,
        VerifierCheck, VerifierEvidence, VerifierResult,
    },
};

#[test]
fn bundle_selection_dispatches_shared_plan_request_without_expanding_items() {
    let catalog = fixture_catalog();
    let request = PlannerRequest::bundle("bundle-a");
    let plan = fixture_action_plan(
        &catalog,
        &request,
        &BTreeMap::from([(
            "tool-a".to_string(),
            missing_result(VerificationStage::Present),
        )]),
    );
    let engine = MockEngine::new(vec![
        Ok(catalog_response(catalog.clone())),
        Ok(CommandResponse::success(
            CommandName::Plan,
            InterfaceMode::Tui,
            OutputFormat::Text,
            CommandPayload::Plan { action_plan: plan },
        )),
    ]);

    let mut app = TuiApp::bootstrap(&engine).expect("catalog should load");
    assert!(!app.on_key(key(KeyCode::Tab)));
    assert!(!app.on_key(key(KeyCode::Char(' '))));
    assert!(!app.on_key(key(KeyCode::Char('p'))));

    let requests = engine.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].command, CommandName::Plan);
    assert_eq!(requests[1].mode, InterfaceMode::Tui);
    assert_eq!(requests[1].planner_request, Some(request));
    assert!(app.snapshot().results.contains("Last action: plan"));
}

#[test]
fn item_selection_dispatches_shared_verify_request_and_surfaces_evidence() {
    let catalog = fixture_catalog();
    let request = PlannerRequest::item("tool-a");
    let verification = fixture_verification(
        &catalog,
        &request,
        missing_result(VerificationStage::Present),
    );
    let engine = MockEngine::new(vec![
        Ok(catalog_response(catalog.clone())),
        Ok(CommandResponse::success(
            CommandName::Verify,
            InterfaceMode::Tui,
            OutputFormat::Text,
            CommandPayload::Verify { verification },
        )),
    ]);

    let mut app = TuiApp::bootstrap(&engine).expect("catalog should load");
    assert!(!app.on_key(key(KeyCode::Char(' '))));
    assert!(!app.on_key(key(KeyCode::Char('v'))));

    let requests = engine.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].command, CommandName::Verify);
    assert_eq!(requests[1].planner_request, Some(request));

    let snapshot = app.snapshot();
    assert!(snapshot.details.contains("Verifier Evidence:"));
    assert!(snapshot.details.contains("missing"));
    assert!(snapshot.results.contains("Last action: verify"));
}

#[test]
fn install_preview_dispatches_dry_run_request_and_uses_shared_engine_results() {
    let catalog = fixture_catalog();
    let request = PlannerRequest::item("tool-a");
    let verification_result = missing_result(VerificationStage::Present);
    let verification = fixture_verification(&catalog, &request, verification_result.clone());
    let action_plan = fixture_action_plan(
        &catalog,
        &request,
        &BTreeMap::from([("tool-a".to_string(), verification_result.clone())]),
    );
    let execution_plan = ExecutionPlan {
        request: action_plan.request.clone(),
        platform: action_plan.platform.clone(),
        steps: action_plan
            .steps
            .iter()
            .cloned()
            .map(|action_step| ExecutionStep {
                action_step,
                execution_target: ExecutionTarget::CurrentProcess,
                recipe: None,
                operations: Vec::new(),
            })
            .collect(),
    };
    let execution = ExecutionPlanReport {
        summary: ExecutionPlanSummary {
            total_steps: 1,
            actionable_steps: 1,
            successful_steps: 1,
            failed_steps: 0,
            skipped_steps: 0,
        },
        steps: execution_plan
            .steps
            .iter()
            .cloned()
            .map(|step| ExecutionStepReport {
                step,
                disposition: ExecutionDisposition::Success,
                message: "Executed shared engine step.".to_string(),
                operations: Vec::new(),
            })
            .collect(),
    };
    let install = InstallWorkflowResult {
        install_mode: InstallMode::DryRun,
        action_plan: action_plan.clone(),
        execution_plan,
        execution,
        post_verification: verification,
        outcome: InstallWorkflowOutcome {
            status: InstallWorkflowStatus::DryRun,
            execution_succeeded: true,
            actionable_steps: 1,
            blocked_steps: 0,
            threshold_met_steps: 0,
            failures: vec![InstallWorkflowFailure {
                item_id: "tool-a".to_string(),
                action: PlannedAction::Install,
                execution_disposition: ExecutionDisposition::Success,
                verifier: verification_result,
            }],
        },
    };
    let engine = MockEngine::new(vec![
        Ok(catalog_response(catalog)),
        Ok(CommandResponse::success(
            CommandName::Install,
            InterfaceMode::Tui,
            OutputFormat::Text,
            CommandPayload::Install { install },
        )),
    ]);

    let mut app = TuiApp::bootstrap(&engine).expect("catalog should load");
    assert!(!app.on_key(key(KeyCode::Char(' '))));
    assert!(!app.on_key(key(KeyCode::Char('i'))));

    let requests = engine.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].command, CommandName::Install);
    assert_eq!(requests[1].planner_request, Some(request));
    assert_eq!(requests[1].install_mode, InstallMode::DryRun);

    let snapshot = app.snapshot();
    assert!(snapshot.header.contains("i install preview (dry-run)"));
    assert!(snapshot
        .results
        .contains("install preview (dry-run request, dry_run)"));
    assert!(snapshot.details.contains("Execution: success"));
    assert!(snapshot.details.contains("Rationale:"));
}

struct MockEngine {
    responses: RefCell<VecDeque<std::result::Result<CommandResponse, envira::engine::EngineError>>>,
    requests: RefCell<Vec<CommandRequest>>,
}

impl MockEngine {
    fn new(
        responses: Vec<std::result::Result<CommandResponse, envira::engine::EngineError>>,
    ) -> Self {
        Self {
            responses: RefCell::new(responses.into()),
            requests: RefCell::new(Vec::new()),
        }
    }

    fn requests(&self) -> Vec<CommandRequest> {
        self.requests.borrow().clone()
    }
}

impl TuiEnginePort for MockEngine {
    fn execute(
        &self,
        request: CommandRequest,
    ) -> std::result::Result<CommandResponse, envira::engine::EngineError> {
        self.requests.borrow_mut().push(request);
        self.responses
            .borrow_mut()
            .pop_front()
            .expect("test should queue enough responses")
    }
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn catalog_response(catalog: Catalog) -> CommandResponse {
    CommandResponse::success(
        CommandName::Catalog,
        InterfaceMode::Tui,
        OutputFormat::Text,
        CommandPayload::Catalog { catalog },
    )
}

fn fixture_catalog() -> Catalog {
    Catalog::from_toml_str(
        r#"
schema_version = 1
default_bundles = ["bundle-a"]

[[items]]
id = "tool-a"
display_name = "Tool A"
category = "terminal_tool"
scope = "user"
depends_on = []
targets = [{ backend = "direct_binary", source = "github_release" }]
success_threshold = "present"
standalone = false

  [[items.verifier.checks]]
  requirement = "required"
  kind = "command"
  command = "tool-a"

[[items]]
id = "tool-b"
display_name = "Tool B"
category = "terminal_tool"
scope = "user"
depends_on = []
targets = [{ backend = "direct_binary", source = "github_release" }]
success_threshold = "present"
standalone = true

  [[items.verifier.checks]]
  requirement = "required"
  kind = "command"
  command = "tool-b"

[[bundles]]
id = "bundle-a"
display_name = "Bundle A"
items = ["tool-a"]
"#,
    )
    .expect("fixture catalog should parse")
}

fn fixture_action_plan(
    catalog: &Catalog,
    request: &PlannerRequest,
    verifier_results: &BTreeMap<String, VerifierResult>,
) -> envira::planner::ActionPlan {
    let plan =
        build_install_plan(catalog, &platform_context(), request).expect("plan should build");
    classify_install_plan(&plan, verifier_results).expect("action plan should classify")
}

fn fixture_verification(
    catalog: &Catalog,
    request: &PlannerRequest,
    verifier_result: VerifierResult,
) -> VerificationWorkflowResult {
    let plan =
        build_install_plan(catalog, &platform_context(), request).expect("plan should build");
    let step = plan.steps[0].clone();

    VerificationWorkflowResult {
        request: request.clone(),
        profile: VerificationProfile::Quick,
        platform: platform_context(),
        summary: VerificationWorkflowSummary {
            total_steps: 1,
            threshold_met_steps: usize::from(verifier_result.threshold_met),
            threshold_unmet_steps: usize::from(!verifier_result.threshold_met),
        },
        results: vec![VerificationItemResult {
            step,
            result: verifier_result,
        }],
    }
}

fn missing_result(required_stage: VerificationStage) -> VerifierResult {
    let evidence = vec![VerifierEvidence {
        check: VerifierCheck {
            requirement: ProbeRequirement::Required,
            stage: VerificationStage::Present,
            min_profile: VerificationProfile::Quick,
            kind: ProbeKind::Command,
            command: Some("tool-a".to_string()),
            commands: None,
            path: None,
            pattern: None,
        },
        record: EvidenceRecord {
            status: EvidenceStatus::Missing,
            observed_scope: ObservedScope::Unknown,
            summary: "tool-a is missing from PATH".to_string(),
            detail: Some(
                "The binary was not discoverable during the shared verifier run.".to_string(),
            ),
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

fn platform_context() -> PlatformContext {
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
        effective_user: user("alice", "/home/alice", 1000, 1000),
        target_user: Some(user("alice", "/home/alice", 1000, 1000)),
        runtime_scope: RuntimeScope::User,
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
