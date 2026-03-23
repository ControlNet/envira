use std::{
    cell::RefCell,
    collections::{BTreeMap, VecDeque},
    path::PathBuf,
};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use envira::{
    catalog::{Catalog, CatalogError, TargetBackend},
    engine::{
        CommandName, CommandPayload, CommandRequest, CommandResponse, EngineError, InstallMode,
        InstallWorkflowFailure, InstallWorkflowOutcome, InstallWorkflowResult,
        InstallWorkflowStatus, InterfaceMode, OutputFormat, VerificationItemResult,
        VerificationWorkflowResult, VerificationWorkflowSummary,
    },
    executor::{
        ExecutionDisposition, ExecutionPlan, ExecutionPlanReport, ExecutionPlanSummary,
        ExecutionStep, ExecutionStepReport, ExecutionTarget,
    },
    planner::{
        build_install_plan, classify_install_plan, PlanSelection, PlannedAction, PlannerRequest,
    },
    platform::{
        ArchitectureIdentity, ArchitectureKind, DistroIdentity, DistroKind, InvocationKind,
        PlatformContext, RuntimeScope, UserAccount,
    },
    tui::{TuiApp, TuiEnginePort},
    verifier::{
        EvidenceRecord, EvidenceStatus, ObservedScope, ProbeKind, ProbeRequirement,
        ServiceAssessment, ServiceKind, ServiceProbeEvidence, ServiceUsabilityState,
        VerificationHealth, VerificationProfile, VerificationStage, VerificationSummary,
        VerifierCheck, VerifierEvidence, VerifierResult,
    },
};
use ratatui::{backend::TestBackend, Terminal};

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
        .contains("Last action: install (dry-run request, dry_run)"));
    assert!(snapshot.details.contains("Execution: success"));
    assert!(snapshot.details.contains("Rationale:"));
}

#[test]
fn enter_opens_confirmation_before_dispatching_install() {
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
                message: "Executed install after confirmation.".to_string(),
                operations: Vec::new(),
            })
            .collect(),
    };
    let install = InstallWorkflowResult {
        install_mode: InstallMode::Apply,
        action_plan: action_plan.clone(),
        execution_plan,
        execution,
        post_verification: verification,
        outcome: InstallWorkflowOutcome {
            status: InstallWorkflowStatus::Success,
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
    assert!(!app.on_key(key(KeyCode::Enter)));
    assert_eq!(
        engine.requests().len(),
        1,
        "first Enter should only open confirmation"
    );

    let confirmation = render_app_text(&app, 100, 30);
    assert!(
        confirmation.contains("confirm install"),
        "rendered confirmation dialog:\n{confirmation}"
    );
    assert!(
        confirmation.contains("Install the current selection"),
        "rendered confirmation dialog:\n{confirmation}"
    );
    assert!(
        confirmation.contains("Press Enter"),
        "rendered confirmation dialog:\n{confirmation}"
    );
    assert!(
        confirmation.contains("Esc to cancel."),
        "rendered confirmation dialog:\n{confirmation}"
    );

    assert!(!app.on_key(key(KeyCode::Enter)));

    let requests = engine.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].command, CommandName::Install);
    assert_eq!(requests[1].install_mode, InstallMode::Apply);
    assert_eq!(requests[1].planner_request, Some(request));
    assert!(app
        .snapshot()
        .results
        .contains("Last action: install (apply request, success)"));
}

#[test]
fn enter_confirmation_can_be_cancelled_with_escape() {
    let engine = MockEngine::new(vec![Ok(catalog_response(fixture_catalog()))]);
    let mut app = TuiApp::bootstrap(&engine).expect("catalog should load");

    assert!(!app.on_key(key(KeyCode::Enter)));
    assert!(!app.on_key(key(KeyCode::Esc)));

    assert_eq!(engine.requests().len(), 1);
    assert!(app.snapshot().results.contains("Install cancelled"));
}

#[test]
fn space_toggles_focused_item_selection_marker() {
    let engine = MockEngine::new(vec![Ok(catalog_response(fixture_catalog()))]);
    let mut app = TuiApp::bootstrap(&engine).expect("catalog should load");

    let initial = app.snapshot();
    assert!(initial.items.contains("> [-] Tool A [idle | unverified]"));

    assert!(!app.on_key(key(KeyCode::Char(' '))));
    let selected = app.snapshot();
    assert!(selected.items.contains("> [x] Tool A [idle | unverified]"));

    assert!(!app.on_key(key(KeyCode::Char(' '))));
    let cleared = app.snapshot();
    assert!(cleared.items.contains("> [-] Tool A [idle | unverified]"));
}

#[test]
fn space_toggles_focused_bundle_selection_marker() {
    let engine = MockEngine::new(vec![Ok(catalog_response(fixture_catalog()))]);
    let mut app = TuiApp::bootstrap(&engine).expect("catalog should load");

    assert!(!app.on_key(key(KeyCode::Tab)));

    let initial = app.snapshot();
    assert!(initial.bundles.contains("> [-] Bundle A (1 item)"));

    assert!(!app.on_key(key(KeyCode::Char(' '))));
    let selected = app.snapshot();
    assert!(selected.bundles.contains("> [x] Bundle A (1 item)"));

    assert!(!app.on_key(key(KeyCode::Char(' '))));
    let cleared = app.snapshot();
    assert!(cleared.bundles.contains("> [-] Bundle A (1 item)"));
}

#[test]
fn default_bundle_confirmation_dispatches_apply_install_without_explicit_selection() {
    let catalog = fixture_catalog();
    let request = PlannerRequest::default();
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
                message: "Executed default-bundle install after confirmation.".to_string(),
                operations: Vec::new(),
            })
            .collect(),
    };
    let install = InstallWorkflowResult {
        install_mode: InstallMode::Apply,
        action_plan,
        execution_plan,
        execution,
        post_verification: verification,
        outcome: InstallWorkflowOutcome {
            status: InstallWorkflowStatus::Success,
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
    assert!(!app.on_key(key(KeyCode::Enter)));
    assert!(!app.on_key(key(KeyCode::Enter)));

    let requests = engine.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].command, CommandName::Install);
    assert_eq!(requests[1].install_mode, InstallMode::Apply);
    assert_eq!(requests[1].planner_request, None);
}

#[test]
fn bundle_selection_confirmation_dispatches_apply_install() {
    let catalog = fixture_catalog();
    let request = PlannerRequest::bundle("bundle-a");
    let verification_result = missing_result(VerificationStage::Present);
    let verification = fixture_verification(
        &catalog,
        &PlannerRequest::item("tool-a"),
        verification_result.clone(),
    );
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
                message: "Executed bundle install after confirmation.".to_string(),
                operations: Vec::new(),
            })
            .collect(),
    };
    let install = InstallWorkflowResult {
        install_mode: InstallMode::Apply,
        action_plan,
        execution_plan,
        execution,
        post_verification: verification,
        outcome: InstallWorkflowOutcome {
            status: InstallWorkflowStatus::Success,
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
    assert!(!app.on_key(key(KeyCode::Tab)));
    assert!(!app.on_key(key(KeyCode::Char(' '))));
    assert!(!app.on_key(key(KeyCode::Enter)));
    assert!(!app.on_key(key(KeyCode::Enter)));

    let requests = engine.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].command, CommandName::Install);
    assert_eq!(requests[1].install_mode, InstallMode::Apply);
    assert_eq!(requests[1].planner_request, Some(request));
}

#[test]
fn mixed_bundle_and_item_confirmation_dispatches_combined_apply_install() {
    let catalog = fixture_catalog();
    let request = PlannerRequest::new(vec![
        PlanSelection::bundle("bundle-a"),
        PlanSelection::item("tool-b"),
    ]);
    let verification_result = missing_result(VerificationStage::Present);
    let verification = fixture_verification(&catalog, &request, verification_result.clone());
    let action_plan = fixture_action_plan(
        &catalog,
        &request,
        &BTreeMap::from([
            ("tool-a".to_string(), verification_result.clone()),
            ("tool-b".to_string(), verification_result.clone()),
        ]),
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
            total_steps: 2,
            actionable_steps: 2,
            successful_steps: 2,
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
                message: "Executed mixed bundle+item install after confirmation.".to_string(),
                operations: Vec::new(),
            })
            .collect(),
    };
    let install = InstallWorkflowResult {
        install_mode: InstallMode::Apply,
        action_plan,
        execution_plan,
        execution,
        post_verification: verification,
        outcome: InstallWorkflowOutcome {
            status: InstallWorkflowStatus::Success,
            execution_succeeded: true,
            actionable_steps: 2,
            blocked_steps: 0,
            threshold_met_steps: 0,
            failures: vec![
                InstallWorkflowFailure {
                    item_id: "tool-a".to_string(),
                    action: PlannedAction::Install,
                    execution_disposition: ExecutionDisposition::Success,
                    verifier: verification_result.clone(),
                },
                InstallWorkflowFailure {
                    item_id: "tool-b".to_string(),
                    action: PlannedAction::Install,
                    execution_disposition: ExecutionDisposition::Success,
                    verifier: verification_result,
                },
            ],
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
    assert!(!app.on_key(key(KeyCode::Tab)));
    assert!(!app.on_key(key(KeyCode::Char(' '))));
    assert!(!app.on_key(key(KeyCode::Tab)));
    assert!(!app.on_key(key(KeyCode::Down)));
    assert!(!app.on_key(key(KeyCode::Char(' '))));

    let snapshot = app.snapshot();
    assert!(snapshot.header.contains("Draft: 1 bundle + 1 item"));

    assert!(!app.on_key(key(KeyCode::Enter)));
    let dialog = render_app_text(&app, 100, 30);
    assert!(dialog.contains("Install the current selection (1 bundle + 1 item)?"));

    assert!(!app.on_key(key(KeyCode::Enter)));

    let requests = engine.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].command, CommandName::Install);
    assert_eq!(requests[1].install_mode, InstallMode::Apply);
    assert_eq!(requests[1].planner_request, Some(request));
}

#[test]
fn mixed_bundle_and_item_confirmation_can_be_cancelled_without_dispatch() {
    let engine = MockEngine::new(vec![Ok(catalog_response(fixture_catalog()))]);
    let mut app = TuiApp::bootstrap(&engine).expect("catalog should load");

    assert!(!app.on_key(key(KeyCode::Tab)));
    assert!(!app.on_key(key(KeyCode::Char(' '))));
    assert!(!app.on_key(key(KeyCode::Tab)));
    assert!(!app.on_key(key(KeyCode::Down)));
    assert!(!app.on_key(key(KeyCode::Char(' '))));

    let snapshot = app.snapshot();
    assert!(snapshot.header.contains("Draft: 1 bundle + 1 item"));

    assert!(!app.on_key(key(KeyCode::Enter)));
    let dialog = render_app_text(&app, 100, 30);
    assert!(dialog.contains("Install the current selection (1 bundle + 1 item)?"));

    assert!(!app.on_key(key(KeyCode::Esc)));

    let requests = engine.requests();
    assert_eq!(requests.len(), 1);
    assert!(app.snapshot().results.contains("Install cancelled"));
}

#[test]
fn long_item_lists_keep_selected_entry_visible_after_focus_moves() {
    let engine = MockEngine::new(vec![Ok(catalog_response(long_list_catalog()))]);
    let mut app = TuiApp::bootstrap(&engine).expect("catalog should load");

    for _ in 0..11 {
        assert!(!app.on_key(key(KeyCode::Down)));
    }
    assert!(!app.on_key(key(KeyCode::Tab)));

    let rendered = render_app_text(&app, 100, 30);
    assert!(
        rendered.contains("> [-] Item 12 very long scroll target"),
        "rendered list:\n{rendered}"
    );
    assert!(
        !rendered.contains("[-] Item 1 very long scroll target"),
        "rendered list:\n{rendered}"
    );
}

#[test]
fn long_bundle_lists_keep_selected_entry_visible_after_focus_moves() {
    let engine = MockEngine::new(vec![Ok(catalog_response(long_bundle_catalog()))]);
    let mut app = TuiApp::bootstrap(&engine).expect("catalog should load");

    assert!(!app.on_key(key(KeyCode::Tab)));
    for _ in 0..11 {
        assert!(!app.on_key(key(KeyCode::Down)));
    }
    assert!(!app.on_key(key(KeyCode::Tab)));

    let rendered = render_app_text(&app, 100, 30);
    assert!(
        rendered.contains("> [ ] Bundle 12 very long scroll targe"),
        "rendered list:\n{rendered}"
    );
    assert!(
        !rendered.contains("Bundle 1 very long scroll target"),
        "rendered list:\n{rendered}"
    );
}

#[test]
fn implicit_default_bundles_drive_tui_selection_state_until_user_selects_explicitly() {
    let catalog = fixture_catalog();
    let request = PlannerRequest::default();
    let plan = fixture_action_plan(
        &catalog,
        &request,
        &BTreeMap::from([(
            "tool-a".to_string(),
            missing_result(VerificationStage::Present),
        )]),
    );
    let engine = MockEngine::new(vec![
        Ok(catalog_response(catalog)),
        Ok(CommandResponse::success(
            CommandName::Plan,
            InterfaceMode::Tui,
            OutputFormat::Text,
            CommandPayload::Plan { action_plan: plan },
        )),
    ]);

    let mut app = TuiApp::bootstrap(&engine).expect("catalog should load");
    let initial = app.snapshot();
    assert!(initial.header.contains("implicit default_bundles"));
    assert!(initial.bundles.contains("[-] Bundle A"));
    assert!(initial.items.contains("[-] Tool A"));
    assert!(initial.items.contains("[ ] Tool B"));
    assert!(initial
        .details
        .contains("selected through implicit default_bundles bundle-a"));

    assert!(!app.on_key(key(KeyCode::Char('p'))));

    let requests = engine.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].command, CommandName::Plan);
    assert_eq!(requests[1].planner_request, None);
    assert!(app.snapshot().results.contains("Last action: plan"));
}

#[test]
fn explicit_item_selection_replaces_implicit_default_bundles() {
    let catalog = fixture_catalog();
    let request = PlannerRequest::item("tool-b");
    let plan = fixture_action_plan(
        &catalog,
        &request,
        &BTreeMap::from([(
            "tool-b".to_string(),
            missing_result(VerificationStage::Present),
        )]),
    );
    let engine = MockEngine::new(vec![
        Ok(catalog_response(catalog)),
        Ok(CommandResponse::success(
            CommandName::Plan,
            InterfaceMode::Tui,
            OutputFormat::Text,
            CommandPayload::Plan { action_plan: plan },
        )),
    ]);

    let mut app = TuiApp::bootstrap(&engine).expect("catalog should load");
    assert!(!app.on_key(key(KeyCode::Down)));
    assert!(!app.on_key(key(KeyCode::Char(' '))));

    let selected = app.snapshot();
    assert!(selected.header.contains("0 bundles + 1 item"));
    assert!(selected.items.contains("[ ] Tool A"));
    assert!(selected.items.contains("[x] Tool B"));
    assert!(selected.details.contains("Selection: selected directly"));

    assert!(!app.on_key(key(KeyCode::Char('p'))));

    let requests = engine.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].planner_request, Some(request));
}

#[test]
fn service_verification_outcomes_are_visible_in_tui_details() {
    let catalog = fixture_catalog();
    let request = PlannerRequest::item("tool-a");
    let verification = fixture_verification(
        &catalog,
        &request,
        blocked_service_result(VerificationStage::Operational),
    );
    let engine = MockEngine::new(vec![
        Ok(catalog_response(catalog)),
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

    let snapshot = app.snapshot();
    assert!(snapshot.details.contains("Service: docker blocked"));
    assert!(snapshot
        .details
        .contains("Service summary: Docker is installed but blocked."));
    assert!(snapshot
        .details
        .contains("Service detail: docker.service is missing."));
    assert!(snapshot.details.contains("Service probes:"));
    assert!(snapshot
        .details
        .contains("unit [missing] docker.service is missing"));
    assert!(snapshot.results.contains("Last action: verify"));
}

#[test]
fn gated_and_error_states_use_new_wording() {
    let catalog = fixture_catalog();
    let auto_update_engine = MockEngine::new(vec![
        Ok(catalog_response(catalog.clone())),
        Err(EngineError::AutoUpdateFailed {
            current_version: "0.1.0".to_string(),
            required_version: "0.2.0".to_string(),
            updater: "envira.sh".to_string(),
            detail: "[ERROR] bootstrap failed".to_string(),
            exit_code: Some(80),
        }),
    ]);

    let mut auto_update_app = TuiApp::bootstrap(&auto_update_engine).expect("catalog should load");
    assert!(!auto_update_app.on_key(key(KeyCode::Char('p'))));

    let update_results = auto_update_app.snapshot().results;
    assert!(update_results.contains("envira_auto_update_failed:"));
    assert!(update_results.contains("approved update flow failed"));
    assert!(update_results.contains("required_version: 0.2.0"));
    assert!(update_results.contains("exit_code: 80"));
    assert!(!update_results.contains("shared engine"));
    assert!(!update_results.contains("catalog manifest"));

    let legacy_catalog_engine = MockEngine::new(vec![
        Ok(catalog_response(catalog)),
        Err(EngineError::LoadCatalog {
            manifest_path: Some(PathBuf::from("/tmp/legacy-catalog.toml")),
            source: CatalogError::Validation(
                "legacy catalog shape is no longer supported; use `required_version`, `distros`, `shell`, `default_bundles`, and keyed `[bundles.<id>]` / `[items.<id>]` tables".to_string(),
            ),
        }),
    ]);

    let mut legacy_catalog_app =
        TuiApp::bootstrap(&legacy_catalog_engine).expect("catalog should load");
    assert!(!legacy_catalog_app.on_key(key(KeyCode::Char('r'))));

    let legacy_results = legacy_catalog_app.snapshot().results;
    assert!(legacy_results.contains("catalog_invalid:"));
    assert!(legacy_results.contains("legacy catalog shape is no longer supported"));
    assert!(legacy_results.contains("catalog_path: /tmp/legacy-catalog.toml"));
    assert!(!legacy_results.contains("shared engine"));
    assert!(!legacy_results.contains("catalog manifest"));
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

fn render_app_text(app: &TuiApp<'_, MockEngine>, width: u16, height: u16) -> String {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).expect("test terminal should build");
    terminal
        .draw(|frame| app.render(frame))
        .expect("render should succeed");

    let backend = terminal.backend();
    let buffer = backend.buffer();
    let mut lines = Vec::new();

    for y in 0..buffer.area.height {
        let mut line = String::new();
        for x in 0..buffer.area.width {
            line.push_str(buffer[(x, y)].symbol());
        }
        lines.push(line);
    }

    lines.join("\n")
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
required_version = "0.1.0"
distros = ["ubuntu"]
shell = "bash"
default_bundles = ["bundle-a"]

[items.tool-a]
name = "Tool A"
desc = "Tool A"
depends_on = []

[[items.tool-a.recipes]]
mode = "user"
distros = ["ubuntu"]
cmd = "curl -fsSL https://example.com/tool-a -o ~/.local/bin/tool-a && chmod +x ~/.local/bin/tool-a"

[[items.tool-a.verifiers]]
mode = "user"
distros = ["ubuntu"]
cmd = "command -v tool-a"

[items.tool-b]
name = "Tool B"
desc = "Tool B"
depends_on = []

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
items = ["tool-a"]

[bundles.bundle-b]
name = "Bundle B"
desc = "Bundle B"
items = ["tool-b"]
"#,
    )
    .expect("fixture catalog should parse")
}

fn long_list_catalog() -> Catalog {
    let mut manifest = vec![
        "required_version = \"0.1.0\"".to_string(),
        "distros = [\"ubuntu\"]".to_string(),
        "shell = \"bash\"".to_string(),
        "default_bundles = [\"bundle-a\"]".to_string(),
        String::new(),
        "[bundles.bundle-a]".to_string(),
        "name = \"Bundle A\"".to_string(),
        "desc = \"Bundle A\"".to_string(),
        "items = [".to_string(),
    ];

    for index in 1..=16 {
        let suffix = if index == 16 { "" } else { "," };
        manifest.push(format!("  \"item-{index:02}\"{suffix}"));
    }

    manifest.push("]".to_string());

    for index in 1..=16 {
        manifest.push(String::new());
        manifest.push(format!("[items.item-{index:02}]"));
        manifest.push(format!(
            "name = \"Item {index} very long scroll target label for visibility checks\""
        ));
        manifest.push(format!("desc = \"Item {index}\""));
        manifest.push("depends_on = []".to_string());
        manifest.push(String::new());
        manifest.push(format!("[[items.item-{index:02}.recipes]]"));
        manifest.push("mode = \"user\"".to_string());
        manifest.push("distros = [\"ubuntu\"]".to_string());
        manifest.push(format!(
            "cmd = \"printf item-{index:02} > ~/.local/bin/item-{index:02}\""
        ));
        manifest.push(String::new());
        manifest.push(format!("[[items.item-{index:02}.verifiers]]"));
        manifest.push("mode = \"user\"".to_string());
        manifest.push("distros = [\"ubuntu\"]".to_string());
        manifest.push(format!("cmd = \"command -v item-{index:02}\""));
    }

    Catalog::from_toml_str(&manifest.join("\n")).expect("long list catalog should parse")
}

fn long_bundle_catalog() -> Catalog {
    let mut manifest = vec![
        "required_version = \"0.1.0\"".to_string(),
        "distros = [\"ubuntu\"]".to_string(),
        "shell = \"bash\"".to_string(),
        "default_bundles = [\"bundle-01\"]".to_string(),
    ];

    for index in 1..=16 {
        manifest.push(String::new());
        manifest.push(format!("[items.item-{index:02}]"));
        manifest.push(format!("name = \"Item {index}\""));
        manifest.push(format!("desc = \"Item {index}\""));
        manifest.push("depends_on = []".to_string());
        manifest.push(String::new());
        manifest.push(format!("[[items.item-{index:02}.recipes]]"));
        manifest.push("mode = \"user\"".to_string());
        manifest.push("distros = [\"ubuntu\"]".to_string());
        manifest.push(format!(
            "cmd = \"printf item-{index:02} > ~/.local/bin/item-{index:02}\""
        ));
        manifest.push(String::new());
        manifest.push(format!("[[items.item-{index:02}.verifiers]]"));
        manifest.push("mode = \"user\"".to_string());
        manifest.push("distros = [\"ubuntu\"]".to_string());
        manifest.push(format!("cmd = \"command -v item-{index:02}\""));

        manifest.push(String::new());
        manifest.push(format!("[bundles.bundle-{index:02}]"));
        manifest.push(format!(
            "name = \"Bundle {index} very long scroll target label for visibility checks\""
        ));
        manifest.push(format!("desc = \"Bundle {index}\""));
        manifest.push(format!("items = [\"item-{index:02}\"]"));
    }

    Catalog::from_toml_str(&manifest.join("\n")).expect("long bundle catalog should parse")
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

fn blocked_service_result(required_stage: VerificationStage) -> VerifierResult {
    let mut result = missing_result(required_stage);
    result.required_stage = required_stage;
    result.health = VerificationHealth::Broken;
    result.service_evidence = vec![ServiceProbeEvidence {
        id: "unit".to_string(),
        stage: VerificationStage::Configured,
        probe: envira::verifier::ProbeSpec::ServiceUnit(envira::verifier::ServiceUnitProbe {
            unit: "docker.service".to_string(),
            scope: envira::verifier::ServiceManagerScope::System,
            condition: envira::verifier::ServiceUnitCondition::Exists,
            timeout_ms: None,
        }),
        record: EvidenceRecord {
            status: EvidenceStatus::Missing,
            observed_scope: ObservedScope::System,
            summary: "docker.service is missing".to_string(),
            detail: Some("The service unit was not found during verification.".to_string()),
        },
    }];
    result.service = Some(ServiceAssessment {
        kind: ServiceKind::Docker,
        state: ServiceUsabilityState::Blocked,
        achieved_stage: Some(VerificationStage::Present),
        observed_scope: ObservedScope::System,
        summary: "Docker is installed but blocked.".to_string(),
        detail: Some("docker.service is missing.".to_string()),
    });
    result
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
