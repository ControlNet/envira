use std::{
    collections::{BTreeMap, BTreeSet},
    io::{self, Stdout},
};

use crossterm::{
    event::{self, Event, KeyCode, KeyEvent, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
    Frame, Terminal,
};

use crate::{
    catalog::{Catalog, CatalogBundle, CatalogItem},
    engine::{
        CommandErrorResponse, CommandName, CommandPayload, CommandRequest, CommandResponse, Engine,
        EngineError, InstallMode, InstallWorkflowResult, InterfaceMode, OutputFormat,
        VerificationWorkflowResult,
    },
    error::Result,
    planner::{
        ActionPlan, ActionPlanStep, InstallTargetPreference, PlanSelection, PlannedAction,
        PlannerRequest,
    },
    platform::PlatformContext,
    verifier::{
        EvidenceStatus, ObservedScope, ServiceKind, ServiceUsabilityState, VerificationHealth,
        VerifierResult,
    },
};

const HEADER_HEIGHT: u16 = 6;
const RESULT_HEIGHT: u16 = 7;
const BUNDLE_HEIGHT: u16 = 10;
const DRAFT_HEIGHT: u16 = 10;
const BROWSER_WIDTH: u16 = 48;

pub trait TuiEnginePort {
    fn execute(&self, request: CommandRequest)
        -> std::result::Result<CommandResponse, EngineError>;
}

impl TuiEnginePort for Engine {
    fn execute(
        &self,
        request: CommandRequest,
    ) -> std::result::Result<CommandResponse, EngineError> {
        Engine::execute(self, request)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FocusPane {
    Bundles,
    Items,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ViewSnapshot {
    pub header: String,
    pub bundles: String,
    pub items: String,
    pub details: String,
    pub draft: String,
    pub results: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UiState {
    catalog: Catalog,
    focus: FocusPane,
    bundle_index: usize,
    item_index: usize,
    selected_bundles: BTreeSet<String>,
    selected_items: BTreeSet<String>,
    install_target: InstallTargetPreference,
    cached_plan: Option<ActionPlan>,
    cached_verification: Option<VerificationWorkflowResult>,
    cached_install: Option<InstallWorkflowResult>,
    verification_by_item: BTreeMap<String, VerifierResult>,
    platform: Option<PlatformContext>,
    status_message: String,
    last_error: Option<String>,
    confirmation_dialog: Option<ConfirmationDialog>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ConfirmationDialog {
    command: CommandName,
    prompt: String,
}

impl UiState {
    fn new(catalog: Catalog) -> Self {
        Self {
            catalog,
            focus: FocusPane::Items,
            bundle_index: 0,
            item_index: 0,
            selected_bundles: BTreeSet::new(),
            selected_items: BTreeSet::new(),
            install_target: InstallTargetPreference::Auto,
            cached_plan: None,
            cached_verification: None,
            cached_install: None,
            verification_by_item: BTreeMap::new(),
            platform: None,
            status_message:
                "Loaded catalog. Refreshing installation state and preparing the new draft view."
                    .to_string(),
            last_error: None,
            confirmation_dialog: None,
        }
    }

    pub fn focus(&self) -> FocusPane {
        self.focus
    }

    pub fn planner_request(&self) -> Option<PlannerRequest> {
        let mut selections = Vec::new();

        for bundle in &self.catalog.bundles {
            if self.selected_bundles.contains(bundle.id.as_str()) {
                selections.push(PlanSelection::bundle(bundle.id.as_str().to_string()));
            }
        }

        for item in &self.catalog.items {
            if self.selected_items.contains(item.id.as_str()) {
                selections.push(PlanSelection::item(item.id.as_str().to_string()));
            }
        }

        (!selections.is_empty()).then(|| PlannerRequest::new(selections))
    }

    pub fn snapshot(&self) -> ViewSnapshot {
        ViewSnapshot {
            header: self.header_text(),
            bundles: self.bundle_browser_text(),
            items: self.item_browser_text(),
            details: self.detail_text(),
            draft: self.draft_text(),
            results: self.result_text(),
        }
    }

    fn handle_key(&mut self, key: KeyEvent) -> TuiAction {
        if key.kind != KeyEventKind::Press {
            return TuiAction::None;
        }

        if self.confirmation_dialog.is_some() {
            return match key.code {
                KeyCode::Enter => {
                    self.confirmation_dialog
                        .take()
                        .expect("confirmation dialog should exist while handling confirmation");
                    TuiAction::DispatchInstall(InstallMode::Apply)
                }
                KeyCode::Esc => {
                    self.confirmation_dialog = None;
                    self.status_message =
                        "Install cancelled. Space still toggles bundles/items; press Enter again when ready."
                            .to_string();
                    TuiAction::None
                }
                _ => TuiAction::None,
            };
        }

        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => TuiAction::Exit,
            KeyCode::Tab | KeyCode::Right => {
                self.focus = match self.focus {
                    FocusPane::Bundles => FocusPane::Items,
                    FocusPane::Items => FocusPane::Bundles,
                };
                TuiAction::None
            }
            KeyCode::BackTab | KeyCode::Left => {
                self.focus = match self.focus {
                    FocusPane::Bundles => FocusPane::Items,
                    FocusPane::Items => FocusPane::Bundles,
                };
                TuiAction::None
            }
            KeyCode::Up => {
                self.move_selection(-1);
                TuiAction::None
            }
            KeyCode::Down => {
                self.move_selection(1);
                TuiAction::None
            }
            KeyCode::Char(' ') => {
                self.toggle_focused_selection();
                TuiAction::None
            }
            KeyCode::Char('a') => {
                self.set_install_target(InstallTargetPreference::Auto);
                TuiAction::None
            }
            KeyCode::Char('u') => {
                self.set_install_target(InstallTargetPreference::User);
                TuiAction::None
            }
            KeyCode::Char('s') => {
                self.set_install_target(InstallTargetPreference::System);
                TuiAction::None
            }
            KeyCode::Enter => {
                self.open_install_confirmation();
                TuiAction::None
            }
            KeyCode::Char('c') => {
                self.clear_selection();
                TuiAction::None
            }
            KeyCode::Char('r') => TuiAction::RefreshCatalogState,
            KeyCode::Char('p') => TuiAction::Dispatch(CommandName::Plan),
            KeyCode::Char('v') => TuiAction::VerifyCatalogState,
            KeyCode::Char('i') => TuiAction::DispatchInstall(InstallMode::DryRun),
            _ => TuiAction::None,
        }
    }

    fn apply_response(&mut self, response: CommandResponse) {
        self.last_error = None;

        match response.payload {
            CommandPayload::Catalog { catalog } => {
                let item_count = catalog.items.len();
                let bundle_count = catalog.bundles.len();
                self.catalog = catalog;
                self.bundle_index = self.bundle_index.min(bundle_count.saturating_sub(1));
                self.item_index = self.item_index.min(item_count.saturating_sub(1));
                self.status_message = format!(
                    "Catalog refreshed with {item_count} items across {bundle_count} bundles. Refreshing installation state keeps inline status accurate."
                );
            }
            CommandPayload::Plan { action_plan } => {
                let item_count = action_plan.steps.len();
                let install_steps = action_plan
                    .steps
                    .iter()
                    .filter(|step| step.action == PlannedAction::Install)
                    .count();
                let repair_steps = action_plan
                    .steps
                    .iter()
                    .filter(|step| step.action == PlannedAction::Repair)
                    .count();
                let blocked_steps = action_plan
                    .steps
                    .iter()
                    .filter(|step| step.action == PlannedAction::Blocked)
                    .count();
                self.cached_plan = Some(action_plan);
                self.cached_verification = None;
                self.cached_install = None;
                self.status_message = format!(
                    "Planned {item_count} catalog item{}: {install_steps} install, {repair_steps} repair, {blocked_steps} blocked.",
                    plural_suffix(item_count)
                );
            }
            CommandPayload::Verify { verification } => {
                self.merge_verification(&verification);
                let item_count = verification.summary.total_steps;
                self.status_message = format!(
                    "State refreshed for {item_count} catalog item{}: {} met the requested threshold and {} did not.",
                    plural_suffix(item_count),
                    verification.summary.threshold_met_steps,
                    verification.summary.threshold_unmet_steps
                );
                self.cached_plan = None;
                self.cached_install = None;
                self.cached_verification = Some(verification);
            }
            CommandPayload::Install { install } => {
                self.merge_verification(&install.post_verification);
                let actionable_steps = install.outcome.actionable_steps;
                self.status_message = format!(
                    "Install {} to {} finished as {}; execution succeeded={} and {} of {actionable_steps} actionable catalog item{} met the requested threshold.",
                    install_mode_name(install.install_mode),
                    install_target_name(self.install_target),
                    install_status_name(install.outcome.status),
                    yes_no(install.outcome.execution_succeeded),
                    install.outcome.threshold_met_steps,
                    plural_suffix(actionable_steps)
                );
                self.cached_plan = Some(install.action_plan.clone());
                self.cached_verification = Some(install.post_verification.clone());
                self.cached_install = Some(install);
            }
        }
    }

    fn apply_error(&mut self, command: CommandName, error: EngineError) {
        let message = CommandErrorResponse::new(
            command,
            InterfaceMode::Tui,
            OutputFormat::Text,
            error.into_envelope(),
        )
        .render_text();
        self.last_error = Some(message.clone());
        self.status_message = message;
    }

    fn move_selection(&mut self, delta: isize) {
        match self.focus {
            FocusPane::Bundles => {
                self.bundle_index =
                    move_index(self.bundle_index, self.catalog.bundles.len(), delta);
            }
            FocusPane::Items => {
                self.item_index = move_index(self.item_index, self.catalog.items.len(), delta);
            }
        }
    }

    fn open_install_confirmation(&mut self) {
        let selection = self.selection_summary();
        self.confirmation_dialog = Some(ConfirmationDialog {
            command: CommandName::Install,
            prompt: format!(
                "Install the current selection ({selection}) to {}? Press Enter to confirm or Esc to cancel.",
                install_target_name(self.install_target)
            ),
        });
        self.status_message = format!(
            "Install confirmation is open for target {}. Press Enter to apply or Esc to cancel.",
            install_target_name(self.install_target)
        );
    }

    fn merge_verification(&mut self, verification: &VerificationWorkflowResult) {
        self.platform = Some(verification.platform.clone());

        for result in &verification.results {
            self.verification_by_item
                .insert(result.step.item_id.clone(), result.result.clone());
        }
    }

    fn set_install_target(&mut self, install_target: InstallTargetPreference) {
        if !self.supports_install_target(install_target) {
            self.status_message = format!(
                "Install target `{}` is unavailable in the current runtime. Use auto or system instead.",
                install_target_name(install_target)
            );
            return;
        }

        self.install_target = install_target;
        self.cached_plan = None;
        self.cached_install = None;
        self.last_error = None;
        self.status_message = format!(
            "Install target set to {}. Plan, dry-run, and apply actions will use this target.",
            install_target_name(self.install_target)
        );
    }

    fn toggle_focused_selection(&mut self) {
        match self.focus {
            FocusPane::Bundles => {
                if let Some(bundle_id) = self
                    .focused_bundle()
                    .map(|bundle| bundle.id.as_str().to_string())
                {
                    toggle_id(&mut self.selected_bundles, bundle_id.as_str());
                    self.status_message = format!(
                        "Selection draft updated for catalog bundle `{}`. Explicit selections replace implicit default_bundles.",
                        bundle_id
                    );
                }
            }
            FocusPane::Items => {
                if let Some(item_id) = self.focused_item().map(|item| item.id.as_str().to_string())
                {
                    toggle_id(&mut self.selected_items, item_id.as_str());
                    self.status_message = format!(
                        "Selection draft updated for catalog item `{}`. Explicit selections replace implicit default_bundles.",
                        item_id
                    );
                }
            }
        }

        self.invalidate_action_caches();
    }

    fn clear_selection(&mut self) {
        self.selected_bundles.clear();
        self.selected_items.clear();
        self.invalidate_action_caches();
        self.status_message =
            "Selection draft cleared. New actions will use the catalog default_bundles until you make an explicit selection."
                .to_string();
    }

    fn invalidate_action_caches(&mut self) {
        self.cached_plan = None;
        self.cached_install = None;
        self.last_error = None;
    }

    fn focused_bundle(&self) -> Option<&CatalogBundle> {
        self.catalog.bundles.get(self.bundle_index)
    }

    fn focused_item(&self) -> Option<&CatalogItem> {
        self.catalog.items.get(self.item_index)
    }

    fn header_text(&self) -> String {
        format!(
            "Envira TUI\nDraft: {}\nTarget: {} | Available: {}\nKeys: Tab switch pane | Space toggle | u user | s system | a auto | Enter confirm install | i dry-run | v refresh state | p plan | c clear | r reload | q quit",
            self.selection_summary(),
            install_target_name(self.install_target),
            self.available_install_targets_summary()
        )
    }

    fn bundle_browser_text(&self) -> String {
        if self.catalog.bundles.is_empty() {
            return "No bundles available.".to_string();
        }

        self.catalog
            .bundles
            .iter()
            .enumerate()
            .map(|(index, bundle)| {
                let status = self.bundle_status_badge(bundle);
                let scope = self.bundle_scope_badge(bundle);
                format!(
                    "{} {} {} [{} | {} item{} | {}]",
                    focus_marker(index == self.bundle_index),
                    bundle_selection_marker(self, bundle.id.as_str()),
                    bundle.name,
                    status,
                    bundle.items.len(),
                    plural_suffix(bundle.items.len()),
                    scope,
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn item_browser_text(&self) -> String {
        if self.catalog.items.is_empty() {
            return "No catalog items available.".to_string();
        }

        self.catalog
            .items
            .iter()
            .enumerate()
            .map(|(index, item)| {
                let state = self.item_state_badge(item.id.as_str());
                let scope = self.item_scope_badge(item.id.as_str());
                let capability = install_scope_name(item.install_scope());

                format!(
                    "{} {} {} [{} | {} | cap:{}]",
                    focus_marker(index == self.item_index),
                    item_selection_marker(self, item.id.as_str()),
                    item.name,
                    state,
                    scope,
                    capability,
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn draft_text(&self) -> String {
        let mut lines = vec![
            format!("Selection: {}", self.selection_summary()),
            format!(
                "Install target: {}",
                install_target_name(self.install_target)
            ),
            format!(
                "Available targets: {}",
                self.available_install_targets_summary()
            ),
        ];

        if let Some(item) = self.focused_item() {
            lines.push(format!(
                "Focused item capability: {}",
                install_scope_name(item.install_scope())
            ));
            lines.push(format!(
                "Focused item target support: {}",
                yes_no(item_supports_install_target(item, self.install_target))
            ));
        }

        if let Some(plan) = self.cached_plan.as_ref() {
            let install_steps = plan
                .steps
                .iter()
                .filter(|step| step.action == PlannedAction::Install)
                .count();
            let repair_steps = plan
                .steps
                .iter()
                .filter(|step| step.action == PlannedAction::Repair)
                .count();
            let blocked_steps = plan
                .steps
                .iter()
                .filter(|step| step.action == PlannedAction::Blocked)
                .count();

            lines.push(String::new());
            lines.push(format!(
                "Plan: {} install, {} repair, {} blocked",
                install_steps, repair_steps, blocked_steps
            ));
        }

        if let Some(install) = self.cached_install.as_ref() {
            lines.push(String::new());
            lines.push(format!(
                "Last install: {} to {} ({})",
                install_mode_name(install.install_mode),
                install_target_name(self.install_target),
                install_status_name(install.outcome.status)
            ));
            lines.push(format!(
                "Execution succeeded: {}",
                yes_no(install.outcome.execution_succeeded)
            ));
        }

        lines.join("\n")
    }

    fn detail_text(&self) -> String {
        match self.focus {
            FocusPane::Bundles => self.bundle_detail_text(),
            FocusPane::Items => self.item_detail_text(),
        }
    }

    fn bundle_detail_text(&self) -> String {
        let Some(bundle) = self.focused_bundle() else {
            return "Select a bundle to inspect its members and dispatch actions through the engine."
                .to_string();
        };

        let mut lines = vec![
            format!("Bundle: {}", bundle.name),
            format!("ID: {}", bundle.id),
            format!("Installed summary: {}", self.bundle_status_badge(bundle)),
            format!("Observed location: {}", self.bundle_scope_badge(bundle)),
            format!(
                "Selection: {}",
                bundle_selection_description(self, bundle.id.as_str())
            ),
            "Members:".to_string(),
        ];

        for item_id in &bundle.items {
            let marker = item_selection_marker(self, item_id.as_str());
            lines.push(format!(
                "- {marker} {item_id} [{} | {}]",
                self.item_state_badge(item_id.as_str()),
                self.item_scope_badge(item_id.as_str())
            ));
        }

        lines.push(String::new());
        lines.push(
            "Bundle selection stays declarative here; the engine still expands bundle membership, resolves dependencies, classifies actions, and builds install results."
                .to_string(),
        );

        lines.join("\n")
    }

    fn item_detail_text(&self) -> String {
        let Some(item) = self.focused_item() else {
            return "Select a catalog item to inspect state and verifier evidence.".to_string();
        };

        let mut lines = vec![
            format!("Item: {}", item.name),
            format!("ID: {}", item.id),
            format!("Capability: {}", install_scope_name(item.install_scope())),
            format!(
                "Observed state: {}",
                self.item_state_badge(item.id.as_str())
            ),
            format!(
                "Observed location: {}",
                self.item_scope_badge(item.id.as_str())
            ),
            format!("Draft target: {}", install_target_name(self.install_target)),
            format!(
                "Draft target supported: {}",
                yes_no(item_supports_install_target(item, self.install_target))
            ),
            format!(
                "Required stage: {}",
                stage_name(crate::verifier::required_stage_for_catalog_commands(
                    &item.verifiers,
                ))
            ),
            format!("Dependencies: {}", dependencies_text(&item.depends_on)),
            format!(
                "Selection: {}",
                item_selection_description(self, item.id.as_str())
            ),
        ];

        if let Some(action_step) = self.action_step(item.id.as_str()) {
            lines.push(format!(
                "Action: {}",
                planned_action_name(action_step.action)
            ));
            lines.push(format!("Rationale: {}", action_step.rationale.summary));
        }

        if let Some(install) = &self.cached_install {
            if let Some(step) = install
                .execution
                .steps
                .iter()
                .find(|step| step.step.action_step.step.item_id == item.id.as_str())
            {
                lines.push(format!(
                    "Execution: {} ({})",
                    execution_disposition_name(step.disposition),
                    step.message
                ));
            }
        }

        if let Some(verifier) = self.verifier_result(item.id.as_str()) {
            lines.push(format!(
                "Verifier: achieved={} threshold={} health={}",
                verifier.achieved_stage.map(stage_name).unwrap_or("none"),
                threshold_text(verifier.threshold_met),
                verification_health_name(verifier.health),
            ));
            lines.push(format!(
                "Observed scope model: {}",
                observed_scope_name(verifier.observed_scope)
            ));
            lines.push(format!(
                "Evidence: {} total, {} required failures",
                verifier.summary.total_checks, verifier.summary.required_failures
            ));

            if let Some(service) = verifier.service.as_ref() {
                lines.push(format!(
                    "Service: {} {}",
                    service_kind_name(service.kind),
                    service_state_name(service.state)
                ));
                lines.push(format!("Service summary: {}", service.summary));

                if let Some(detail) = service.detail.as_ref() {
                    if !detail.trim().is_empty() {
                        lines.push(format!("Service detail: {detail}"));
                    }
                }

                if !verifier.service_evidence.is_empty() {
                    lines.push("Service probes:".to_string());

                    for evidence in verifier.service_evidence.iter().take(4) {
                        lines.push(format!(
                            "- {} [{}] {}",
                            evidence.id,
                            evidence_status_name(evidence.record.status),
                            evidence.record.summary
                        ));
                    }
                }
            }

            lines.push(String::new());
            lines.push("Verifier Evidence:".to_string());

            for evidence in verifier.evidence.iter().take(8) {
                lines.push(format!(
                    "- [{}] {}",
                    evidence_status_name(evidence.record.status),
                    evidence.record.summary
                ));

                if let Some(detail) = evidence.record.detail.as_ref() {
                    if !detail.trim().is_empty() {
                        lines.push(format!("  detail: {detail}"));
                    }
                }
            }

            if verifier.evidence.len() > 8 {
                lines.push(format!(
                    "- ... {} more evidence record{}",
                    verifier.evidence.len() - 8,
                    plural_suffix(verifier.evidence.len() - 8)
                ));
            }
        } else {
            lines.push(String::new());
            lines.push(
                "No verifier snapshot cached for this item yet. Press v to refresh catalog state, p to inspect planner actions, i to inspect install preview results, or Enter to install with confirmation."
                    .to_string(),
            );
        }

        lines.join("\n")
    }

    fn result_text(&self) -> String {
        if let Some(error) = self.last_error.as_ref() {
            return format!("Status\n{error}");
        }

        if let Some(install) = self.cached_install.as_ref() {
            let mut lines = vec![
                "Status".to_string(),
                format!(
                    "Last action: install ({} request to {}, {})",
                    install_mode_name(install.install_mode),
                    install_target_name(self.install_target),
                    install_status_name(install.outcome.status)
                ),
                format!(
                    "Execution succeeded: {} | actionable: {} | blocked: {} | threshold met: {}",
                    yes_no(install.outcome.execution_succeeded),
                    install.outcome.actionable_steps,
                    install.outcome.blocked_steps,
                    install.outcome.threshold_met_steps,
                ),
            ];

            for failure in install.outcome.failures.iter().take(3) {
                lines.push(format!(
                    "- {} => {} / {}",
                    failure.item_id,
                    planned_action_name(failure.action),
                    threshold_text(failure.verifier.threshold_met)
                ));
            }

            return lines.join("\n");
        }

        if let Some(plan) = self.cached_plan.as_ref() {
            let install_steps = plan
                .steps
                .iter()
                .filter(|step| step.action == PlannedAction::Install)
                .count();
            let repair_steps = plan
                .steps
                .iter()
                .filter(|step| step.action == PlannedAction::Repair)
                .count();
            let blocked_steps = plan
                .steps
                .iter()
                .filter(|step| step.action == PlannedAction::Blocked)
                .count();

            return format!(
                "Status\nLast action: plan ({})\n{} step{} => {} install, {} repair, {} blocked",
                install_target_name(self.install_target),
                plan.steps.len(),
                plural_suffix(plan.steps.len()),
                install_steps,
                repair_steps,
                blocked_steps,
            );
        }

        if !self.status_message.starts_with("State refreshed")
            && !self.status_message.starts_with("Loaded catalog")
        {
            return format!("Status\n{}", self.status_message);
        }

        if let Some(verification) = self.cached_verification.as_ref() {
            return format!(
                "Status\nLast action: verify all item states\n{} total | {} threshold met | {} threshold unmet",
                verification.summary.total_steps,
                verification.summary.threshold_met_steps,
                verification.summary.threshold_unmet_steps,
            );
        }

        format!("Status\n{}", self.status_message)
    }

    fn selection_summary(&self) -> String {
        let bundle_count = self.selected_bundles.len();
        let item_count = self.selected_items.len();

        if bundle_count == 0 && item_count == 0 {
            format!(
                "implicit default_bundles ({})",
                default_bundle_summary(&self.catalog)
            )
        } else {
            format!(
                "{} bundle{} + {} item{}",
                bundle_count,
                plural_suffix(bundle_count),
                item_count,
                plural_suffix(item_count)
            )
        }
    }

    fn action_step(&self, item_id: &str) -> Option<ActionPlanStep> {
        self.cached_install
            .as_ref()
            .and_then(|install| {
                install
                    .action_plan
                    .steps
                    .iter()
                    .find(|step| step.step.item_id == item_id)
                    .cloned()
            })
            .or_else(|| {
                self.cached_plan.as_ref().and_then(|plan| {
                    plan.steps
                        .iter()
                        .find(|step| step.step.item_id == item_id)
                        .cloned()
                })
            })
    }

    fn verifier_result(&self, item_id: &str) -> Option<VerifierResult> {
        self.verification_by_item
            .get(item_id)
            .cloned()
            .or_else(|| {
                self.cached_install.as_ref().and_then(|install| {
                    install
                        .post_verification
                        .result_for(item_id)
                        .map(|result| result.result.clone())
                })
            })
            .or_else(|| {
                self.cached_verification.as_ref().and_then(|verification| {
                    verification
                        .result_for(item_id)
                        .map(|result| result.result.clone())
                })
            })
            .or_else(|| {
                self.action_step(item_id)
                    .map(|step| step.rationale.verifier)
            })
    }

    fn item_state_badge(&self, item_id: &str) -> &'static str {
        self.verifier_result(item_id)
            .as_ref()
            .map(item_state_from_verifier)
            .unwrap_or("unknown")
    }

    fn item_scope_badge(&self, item_id: &str) -> &'static str {
        self.verifier_result(item_id)
            .map(|result| observed_scope_name(result.observed_scope))
            .unwrap_or("unknown")
    }

    fn bundle_status_badge(&self, bundle: &CatalogBundle) -> String {
        let total = bundle.items.len();
        let installed = bundle
            .items
            .iter()
            .filter(|item_id| self.item_state_badge(item_id.as_str()) == "installed")
            .count();

        if total == 0 {
            return "empty".to_string();
        }

        let label = if installed == total {
            "installed"
        } else if installed > 0 {
            "partial"
        } else if bundle
            .items
            .iter()
            .any(|item_id| self.item_state_badge(item_id.as_str()) == "missing")
        {
            "missing"
        } else {
            "unknown"
        };

        format!("{label} {installed}/{total}")
    }

    fn bundle_scope_badge(&self, bundle: &CatalogBundle) -> &'static str {
        let mut saw_user = false;
        let mut saw_system = false;

        for item_id in &bundle.items {
            match self
                .verifier_result(item_id.as_str())
                .map(|result| result.observed_scope)
                .unwrap_or(ObservedScope::Unknown)
            {
                ObservedScope::User => saw_user = true,
                ObservedScope::System => saw_system = true,
                ObservedScope::Both => {
                    saw_user = true;
                    saw_system = true;
                }
                ObservedScope::Unknown => {}
            }
        }

        match (saw_user, saw_system) {
            (true, true) => "mixed",
            (true, false) => "user",
            (false, true) => "system",
            (false, false) => "unknown",
        }
    }

    fn supports_install_target(&self, install_target: InstallTargetPreference) -> bool {
        match install_target {
            InstallTargetPreference::Auto | InstallTargetPreference::System => true,
            InstallTargetPreference::User => self
                .platform
                .as_ref()
                .map(platform_supports_user_target)
                .unwrap_or(true),
        }
    }

    fn available_install_targets_summary(&self) -> String {
        let mut targets = vec!["auto", "system"];

        if self.supports_install_target(InstallTargetPreference::User) {
            targets.insert(1, "user");
        }

        targets.join(", ")
    }

    fn confirmation_dialog(&self) -> Option<&ConfirmationDialog> {
        self.confirmation_dialog.as_ref()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TuiAction {
    None,
    Exit,
    Dispatch(CommandName),
    DispatchInstall(InstallMode),
    RefreshCatalogState,
    VerifyCatalogState,
}

pub struct TuiApp<'a, E: TuiEnginePort> {
    engine: &'a E,
    state: UiState,
}

impl<'a, E: TuiEnginePort> TuiApp<'a, E> {
    pub fn bootstrap(engine: &'a E) -> std::result::Result<Self, EngineError> {
        let response = engine.execute(CommandRequest::new(
            CommandName::Catalog,
            InterfaceMode::Tui,
            OutputFormat::Text,
        ))?;

        let CommandPayload::Catalog { catalog } = response.payload else {
            unreachable!("catalog command must return a catalog payload")
        };

        let mut app = Self {
            engine,
            state: UiState::new(catalog),
        };

        app.verify_catalog_state();

        Ok(app)
    }

    pub fn state(&self) -> &UiState {
        &self.state
    }

    pub fn snapshot(&self) -> ViewSnapshot {
        self.state.snapshot()
    }

    pub fn on_key(&mut self, key: KeyEvent) -> bool {
        match self.state.handle_key(key) {
            TuiAction::None => false,
            TuiAction::Exit => true,
            TuiAction::Dispatch(command) => {
                self.dispatch(command);
                false
            }
            TuiAction::DispatchInstall(install_mode) => {
                self.dispatch_install(install_mode);
                false
            }
            TuiAction::RefreshCatalogState => {
                self.refresh_catalog_state();
                false
            }
            TuiAction::VerifyCatalogState => {
                self.verify_catalog_state();
                false
            }
        }
    }

    pub fn render(&self, frame: &mut Frame<'_>) {
        render_shell(frame, &self.state);
    }

    fn dispatch(&mut self, command: CommandName) {
        let request = match command {
            CommandName::Catalog => {
                CommandRequest::new(CommandName::Catalog, InterfaceMode::Tui, OutputFormat::Text)
            }
            CommandName::Plan | CommandName::Verify => planner_request_command(
                CommandRequest::new(command, InterfaceMode::Tui, OutputFormat::Text)
                    .with_install_target(self.state.install_target),
                self.state.planner_request(),
            ),
            CommandName::Install => {
                unreachable!("install dispatch is handled with an explicit mode")
            }
            CommandName::Tui => return,
        };

        match self.engine.execute(request) {
            Ok(response) => self.state.apply_response(response),
            Err(error) => self.state.apply_error(command, error),
        }
    }

    fn dispatch_install(&mut self, install_mode: InstallMode) {
        let request = planner_request_command(
            CommandRequest::new(CommandName::Install, InterfaceMode::Tui, OutputFormat::Text)
                .with_install_target(self.state.install_target)
                .with_install_mode(install_mode),
            self.state.planner_request(),
        );

        match self.engine.execute(request) {
            Ok(response) => self.state.apply_response(response),
            Err(error) => self.state.apply_error(CommandName::Install, error),
        }
    }

    fn verify_catalog_state(&mut self) {
        let request =
            CommandRequest::new(CommandName::Verify, InterfaceMode::Tui, OutputFormat::Text)
                .with_planner_request(PlannerRequest::all_items());

        match self.engine.execute(request) {
            Ok(response) => self.state.apply_response(response),
            Err(error) => self.state.apply_error(CommandName::Verify, error),
        }
    }

    fn refresh_catalog_state(&mut self) {
        let catalog_request =
            CommandRequest::new(CommandName::Catalog, InterfaceMode::Tui, OutputFormat::Text);

        match self.engine.execute(catalog_request) {
            Ok(response) => self.state.apply_response(response),
            Err(error) => {
                self.state.apply_error(CommandName::Catalog, error);
                return;
            }
        }

        self.verify_catalog_state();
    }
}

pub fn run(engine: &impl TuiEnginePort) -> Result<()> {
    let mut terminal = TerminalSession::enter()?;
    let mut app =
        TuiApp::bootstrap(engine).map_err(|error| io::Error::new(io::ErrorKind::Other, error))?;

    loop {
        terminal.draw(|frame| app.render(frame))?;

        let event = event::read()?;
        if let Event::Key(key) = event {
            if app.on_key(key) {
                break;
            }
        }
    }

    Ok(())
}

fn render_shell(frame: &mut Frame<'_>, state: &UiState) {
    let snapshot = state.snapshot();
    let area = frame.area();
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(HEADER_HEIGHT),
            Constraint::Min(0),
            Constraint::Length(RESULT_HEIGHT),
        ])
        .split(area);
    let main = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(BROWSER_WIDTH), Constraint::Min(0)])
        .split(vertical[1]);
    let sidebar = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(DRAFT_HEIGHT)])
        .split(main[1]);
    let browser = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(BUNDLE_HEIGHT), Constraint::Min(0)])
        .split(main[0]);

    frame.render_widget(
        paragraph(snapshot.header, "envira", Style::default().fg(Color::Cyan)),
        vertical[0],
    );
    frame.render_widget(
        browser_paragraph(
            snapshot.bundles,
            "bundles",
            pane_title_style(state.focus == FocusPane::Bundles),
            scroll_offset_for_selection(state.bundle_index, browser[0].height),
        ),
        browser[0],
    );
    frame.render_widget(
        browser_paragraph(
            snapshot.items,
            "items",
            pane_title_style(state.focus == FocusPane::Items),
            scroll_offset_for_selection(state.item_index, browser[1].height),
        ),
        browser[1],
    );
    frame.render_widget(
        paragraph(
            snapshot.details,
            "details",
            Style::default().fg(Color::Yellow),
        ),
        sidebar[0],
    );
    frame.render_widget(
        paragraph(
            snapshot.draft,
            "draft",
            Style::default().fg(Color::LightMagenta),
        ),
        sidebar[1],
    );
    frame.render_widget(
        paragraph(
            snapshot.results,
            "result",
            Style::default().fg(Color::Green),
        ),
        vertical[2],
    );

    if let Some(dialog) = state.confirmation_dialog() {
        render_confirmation_dialog(frame, dialog);
    }
}

fn paragraph<'a>(text: String, title: &'a str, title_style: Style) -> Paragraph<'a> {
    paragraph_with_scroll(text, title, title_style, 0)
}

fn browser_paragraph<'a>(
    text: String,
    title: &'a str,
    title_style: Style,
    scroll: u16,
) -> Paragraph<'a> {
    Paragraph::new(text)
        .block(
            Block::default()
                .title(title)
                .title_style(title_style.add_modifier(Modifier::BOLD))
                .borders(Borders::ALL),
        )
        .scroll((scroll, 0))
}

fn paragraph_with_scroll<'a>(
    text: String,
    title: &'a str,
    title_style: Style,
    scroll: u16,
) -> Paragraph<'a> {
    Paragraph::new(text)
        .block(
            Block::default()
                .title(title)
                .title_style(title_style.add_modifier(Modifier::BOLD))
                .borders(Borders::ALL),
        )
        .scroll((scroll, 0))
        .wrap(Wrap { trim: false })
}

fn scroll_offset_for_selection(selected_index: usize, pane_height: u16) -> u16 {
    let inner_height = pane_height.saturating_sub(2).max(1) as usize;
    selected_index
        .saturating_sub(inner_height.saturating_sub(1))
        .min(u16::MAX as usize) as u16
}

fn render_confirmation_dialog(frame: &mut Frame<'_>, dialog: &ConfirmationDialog) {
    let area = centered_rect(60, 20, frame.area());
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(dialog.prompt.clone())
            .block(
                Block::default()
                    .title("confirm install")
                    .title_style(
                        Style::default()
                            .fg(Color::LightRed)
                            .add_modifier(Modifier::BOLD),
                    )
                    .borders(Borders::ALL),
            )
            .wrap(Wrap { trim: false }),
        area,
    );
}

fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vertical[1])[1]
}

fn pane_title_style(active: bool) -> Style {
    if active {
        Style::default().fg(Color::LightCyan)
    } else {
        Style::default().fg(Color::DarkGray)
    }
}

fn move_index(index: usize, len: usize, delta: isize) -> usize {
    if len == 0 {
        return 0;
    }

    let last = len.saturating_sub(1) as isize;
    (index as isize + delta).clamp(0, last) as usize
}

fn toggle_id(selected: &mut BTreeSet<String>, id: &str) {
    if !selected.remove(id) {
        selected.insert(id.to_string());
    }
}

fn focus_marker(active: bool) -> &'static str {
    if active {
        ">"
    } else {
        " "
    }
}

fn selection_marker(selected: bool) -> &'static str {
    if selected {
        "[x]"
    } else {
        "[ ]"
    }
}

fn derived_selection_marker(selected: bool) -> &'static str {
    if selected {
        "[-]"
    } else {
        "[ ]"
    }
}

fn bundle_selection_marker(state: &UiState, bundle_id: &str) -> &'static str {
    if state.selected_bundles.contains(bundle_id) {
        selection_marker(true)
    } else {
        derived_selection_marker(bundle_selected_via_default(state, bundle_id))
    }
}

fn item_selection_marker(state: &UiState, item_id: &str) -> &'static str {
    if state.selected_items.contains(item_id) {
        "[x]"
    } else if item_selected_via_bundle(state, item_id)
        || item_selected_via_default_bundle(state, item_id)
    {
        "[-]"
    } else {
        "[ ]"
    }
}

fn item_selection_description(state: &UiState, item_id: &str) -> String {
    if state.selected_items.contains(item_id) {
        return "selected directly".to_string();
    }

    let selected_bundles = explicit_bundle_memberships(state, item_id);

    if !selected_bundles.is_empty() {
        return format!(
            "selected through bundle{} {}",
            plural_suffix(selected_bundles.len()),
            selected_bundles.join(", ")
        );
    }

    let default_bundles = implicit_default_bundle_memberships(state, item_id);

    if default_bundles.is_empty() {
        "not selected".to_string()
    } else {
        format!(
            "selected through implicit default_bundles {}",
            default_bundles.join(", ")
        )
    }
}

fn bundle_selection_description(state: &UiState, bundle_id: &str) -> String {
    if state.selected_bundles.contains(bundle_id) {
        "selected directly".to_string()
    } else if bundle_selected_via_default(state, bundle_id) {
        "selected through implicit default_bundles".to_string()
    } else {
        "not selected".to_string()
    }
}

fn explicit_bundle_memberships(state: &UiState, item_id: &str) -> Vec<String> {
    state
        .catalog
        .bundles
        .iter()
        .filter(|bundle| {
            state.selected_bundles.contains(bundle.id.as_str())
                && bundle
                    .items
                    .iter()
                    .any(|candidate| candidate.as_str() == item_id)
        })
        .map(|bundle| bundle.id.as_str().to_string())
        .collect()
}

fn item_selected_via_bundle(state: &UiState, item_id: &str) -> bool {
    state.catalog.bundles.iter().any(|bundle| {
        state.selected_bundles.contains(bundle.id.as_str())
            && bundle
                .items
                .iter()
                .any(|candidate| candidate.as_str() == item_id)
    })
}

fn bundle_selected_via_default(state: &UiState, bundle_id: &str) -> bool {
    !has_explicit_selection(state)
        && state
            .catalog
            .default_bundles
            .iter()
            .any(|candidate| candidate.as_str() == bundle_id)
}

fn item_selected_via_default_bundle(state: &UiState, item_id: &str) -> bool {
    !has_explicit_selection(state)
        && state.catalog.bundles.iter().any(|bundle| {
            state
                .catalog
                .default_bundles
                .iter()
                .any(|candidate| candidate.as_str() == bundle.id.as_str())
                && bundle
                    .items
                    .iter()
                    .any(|candidate| candidate.as_str() == item_id)
        })
}

fn implicit_default_bundle_memberships(state: &UiState, item_id: &str) -> Vec<String> {
    if has_explicit_selection(state) {
        return Vec::new();
    }

    state
        .catalog
        .bundles
        .iter()
        .filter(|bundle| {
            state
                .catalog
                .default_bundles
                .iter()
                .any(|candidate| candidate.as_str() == bundle.id.as_str())
                && bundle
                    .items
                    .iter()
                    .any(|candidate| candidate.as_str() == item_id)
        })
        .map(|bundle| bundle.id.as_str().to_string())
        .collect()
}

fn has_explicit_selection(state: &UiState) -> bool {
    !(state.selected_bundles.is_empty() && state.selected_items.is_empty())
}

fn default_bundle_summary(catalog: &Catalog) -> String {
    let bundle_count = catalog.default_bundles.len();
    let item_count = catalog
        .expand_default_bundles()
        .map(|items| items.len())
        .unwrap_or(0);
    format!(
        "{} bundle{} / {} item{}",
        bundle_count,
        plural_suffix(bundle_count),
        item_count,
        plural_suffix(item_count)
    )
}

fn plural_suffix(count: usize) -> &'static str {
    if count == 1 {
        ""
    } else {
        "s"
    }
}

fn yes_no(value: bool) -> &'static str {
    if value {
        "yes"
    } else {
        "no"
    }
}

fn threshold_text(value: bool) -> &'static str {
    if value {
        "met"
    } else {
        "unmet"
    }
}

fn stage_name(stage: crate::verifier::VerificationStage) -> &'static str {
    match stage {
        crate::verifier::VerificationStage::Present => "present",
        crate::verifier::VerificationStage::Configured => "configured",
        crate::verifier::VerificationStage::Operational => "operational",
    }
}

fn planned_action_name(action: PlannedAction) -> &'static str {
    match action {
        PlannedAction::Skip => "skip",
        PlannedAction::Install => "install",
        PlannedAction::Repair => "repair",
        PlannedAction::Blocked => "blocked",
    }
}

fn verification_health_name(health: VerificationHealth) -> &'static str {
    match health {
        VerificationHealth::Healthy => "healthy",
        VerificationHealth::Unknown => "unknown",
        VerificationHealth::Missing => "missing",
        VerificationHealth::Broken => "broken",
    }
}

fn evidence_status_name(status: EvidenceStatus) -> &'static str {
    match status {
        EvidenceStatus::Satisfied => "satisfied",
        EvidenceStatus::Missing => "missing",
        EvidenceStatus::Broken => "broken",
        EvidenceStatus::Unknown => "unknown",
        EvidenceStatus::NotApplicable => "n/a",
    }
}

fn execution_disposition_name(disposition: crate::executor::ExecutionDisposition) -> &'static str {
    match disposition {
        crate::executor::ExecutionDisposition::Success => "success",
        crate::executor::ExecutionDisposition::Failure => "failure",
        crate::executor::ExecutionDisposition::Skipped => "skipped",
    }
}

fn install_status_name(status: crate::engine::InstallWorkflowStatus) -> &'static str {
    match status {
        crate::engine::InstallWorkflowStatus::Success => "success",
        crate::engine::InstallWorkflowStatus::DryRun => "dry_run",
        crate::engine::InstallWorkflowStatus::VerificationFailed => "verification_failed",
        crate::engine::InstallWorkflowStatus::Blocked => "blocked",
    }
}

fn install_mode_name(mode: InstallMode) -> &'static str {
    match mode {
        InstallMode::Apply => "apply",
        InstallMode::DryRun => "dry-run",
    }
}

fn install_target_name(target: InstallTargetPreference) -> &'static str {
    match target {
        InstallTargetPreference::Auto => "auto",
        InstallTargetPreference::User => "user",
        InstallTargetPreference::System => "system",
    }
}

fn observed_scope_name(scope: ObservedScope) -> &'static str {
    match scope {
        ObservedScope::Unknown => "unknown",
        ObservedScope::System => "system",
        ObservedScope::User => "user",
        ObservedScope::Both => "both",
    }
}

fn item_state_from_verifier(result: &VerifierResult) -> &'static str {
    match result.health {
        VerificationHealth::Broken => "broken",
        VerificationHealth::Missing if result.achieved_stage.is_none() => "missing",
        _ if result.achieved_stage.is_some() => "installed",
        VerificationHealth::Unknown => "unknown",
        VerificationHealth::Missing => "missing",
        VerificationHealth::Healthy => "unknown",
    }
}

fn item_supports_install_target(
    item: &CatalogItem,
    install_target: InstallTargetPreference,
) -> bool {
    match install_target {
        InstallTargetPreference::Auto => true,
        InstallTargetPreference::User => matches!(
            item.install_scope(),
            crate::catalog::InstallScope::User | crate::catalog::InstallScope::Hybrid
        ),
        InstallTargetPreference::System => matches!(
            item.install_scope(),
            crate::catalog::InstallScope::System | crate::catalog::InstallScope::Hybrid
        ),
    }
}

fn platform_supports_user_target(platform: &PlatformContext) -> bool {
    platform.target_user.is_some() || !platform.effective_user.is_root()
}

fn install_scope_name(scope: crate::catalog::InstallScope) -> &'static str {
    match scope {
        crate::catalog::InstallScope::System => "system",
        crate::catalog::InstallScope::User => "user",
        crate::catalog::InstallScope::Hybrid => "hybrid",
    }
}

fn service_kind_name(kind: ServiceKind) -> &'static str {
    match kind {
        ServiceKind::Docker => "docker",
        ServiceKind::Jupyter => "jupyter",
        ServiceKind::Pm2 => "pm2",
        ServiceKind::Syncthing => "syncthing",
        ServiceKind::Vnc => "vnc",
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

fn planner_request_command(
    request: CommandRequest,
    planner_request: Option<PlannerRequest>,
) -> CommandRequest {
    if let Some(planner_request) = planner_request {
        request.with_planner_request(planner_request)
    } else {
        request
    }
}

fn dependencies_text(dependencies: &[crate::catalog::CanonicalId]) -> String {
    if dependencies.is_empty() {
        "none".to_string()
    } else {
        dependencies
            .iter()
            .map(|dependency| dependency.as_str().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

struct TerminalSession {
    terminal: Terminal<CrosstermBackend<Stdout>>,
}

impl TerminalSession {
    fn enter() -> Result<Self> {
        enable_raw_mode()?;

        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen)?;

        let backend = CrosstermBackend::new(stdout);
        let terminal = Terminal::new(backend)?;

        Ok(Self { terminal })
    }

    fn draw<F>(&mut self, render: F) -> Result<()>
    where
        F: FnOnce(&mut Frame<'_>),
    {
        self.terminal.draw(render)?;
        Ok(())
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(self.terminal.backend_mut(), LeaveAlternateScreen);
        let _ = self.terminal.show_cursor();
    }
}
