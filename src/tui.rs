use std::{
    collections::BTreeSet,
    io::{self, Stdout},
};

use crossterm::{
    event::{self, Event, KeyCode, KeyEvent, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame, Terminal,
};

use crate::{
    catalog::{Catalog, CatalogBundle, CatalogItem},
    engine::{
        CommandName, CommandPayload, CommandRequest, CommandResponse, Engine, EngineError,
        InstallMode, InstallWorkflowResult, InterfaceMode, OutputFormat,
        VerificationWorkflowResult,
    },
    error::Result,
    planner::{ActionPlan, ActionPlanStep, PlanSelection, PlannedAction, PlannerRequest},
    verifier::{EvidenceStatus, VerificationHealth, VerifierResult},
};

const HEADER_HEIGHT: u16 = 3;
const RESULT_HEIGHT: u16 = 10;
const BUNDLE_HEIGHT: u16 = 10;
const BROWSER_WIDTH: u16 = 40;

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
    cached_plan: Option<ActionPlan>,
    cached_verification: Option<VerificationWorkflowResult>,
    cached_install: Option<InstallWorkflowResult>,
    status_message: String,
    last_error: Option<String>,
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
            cached_plan: None,
            cached_verification: None,
            cached_install: None,
            status_message:
                "Loaded catalog. Use Tab to switch panes, Space to toggle, v to verify, p to plan, i to preview install (dry-run), and q to quit."
                    .to_string(),
            last_error: None,
        }
    }

    pub fn focus(&self) -> FocusPane {
        self.focus
    }

    pub fn planner_request(&self) -> PlannerRequest {
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

        if selections.is_empty() {
            PlannerRequest::all_default()
        } else {
            PlannerRequest::new(selections)
        }
    }

    pub fn snapshot(&self) -> ViewSnapshot {
        ViewSnapshot {
            header: self.header_text(),
            bundles: self.bundle_browser_text(),
            items: self.item_browser_text(),
            details: self.detail_text(),
            results: self.result_text(),
        }
    }

    fn handle_key(&mut self, key: KeyEvent) -> TuiAction {
        if key.kind != KeyEventKind::Press {
            return TuiAction::None;
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
            KeyCode::Char('c') => {
                self.clear_selection();
                TuiAction::None
            }
            KeyCode::Char('r') => TuiAction::Dispatch(CommandName::Catalog),
            KeyCode::Char('p') => TuiAction::Dispatch(CommandName::Plan),
            KeyCode::Char('v') => TuiAction::Dispatch(CommandName::Verify),
            KeyCode::Char('i') => TuiAction::Dispatch(CommandName::Install),
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
                    "Catalog refreshed with {item_count} items across {bundle_count} bundles."
                );
            }
            CommandPayload::Plan { action_plan } => {
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
                    "Planned selection through the shared engine: {install_steps} install, {repair_steps} repair, {blocked_steps} blocked."
                );
            }
            CommandPayload::Verify { verification } => {
                self.status_message = format!(
                    "Verified selection through the shared engine: {} met threshold, {} did not.",
                    verification.summary.threshold_met_steps,
                    verification.summary.threshold_unmet_steps
                );
                self.cached_plan = None;
                self.cached_verification = Some(verification);
                self.cached_install = None;
            }
            CommandPayload::Install { install } => {
                self.status_message = format!(
                    "Install preview ran through the shared engine in {} mode and finished as {}.",
                    install_mode_name(install.install_mode),
                    install_status_name(install.outcome.status)
                );
                self.cached_plan = Some(install.action_plan.clone());
                self.cached_verification = Some(install.post_verification.clone());
                self.cached_install = Some(install);
            }
        }
    }

    fn apply_error(&mut self, command: CommandName, error: EngineError) {
        let message = format!(
            "{} failed through the shared engine: {error}",
            command.as_str()
        );
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

    fn toggle_focused_selection(&mut self) {
        match self.focus {
            FocusPane::Bundles => {
                if let Some(bundle_id) = self
                    .focused_bundle()
                    .map(|bundle| bundle.id.as_str().to_string())
                {
                    toggle_id(&mut self.selected_bundles, bundle_id.as_str());
                    self.status_message = format!(
                        "Selection draft updated for bundle `{}`. Dispatch still flows through the engine.",
                        bundle_id
                    );
                }
            }
            FocusPane::Items => {
                if let Some(item_id) = self.focused_item().map(|item| item.id.as_str().to_string())
                {
                    toggle_id(&mut self.selected_items, item_id.as_str());
                    self.status_message = format!(
                        "Selection draft updated for item `{}`. Dispatch still flows through the engine.",
                        item_id
                    );
                }
            }
        }

        self.invalidate_results();
    }

    fn clear_selection(&mut self) {
        self.selected_bundles.clear();
        self.selected_items.clear();
        self.invalidate_results();
        self.status_message =
            "Selection draft cleared. New actions will fall back to the catalog default bundle expansion."
                .to_string();
    }

    fn invalidate_results(&mut self) {
        self.cached_plan = None;
        self.cached_verification = None;
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
            "Envira Ratatui\nDraft: {}\nKeys: Tab switch pane | Space toggle | v verify | p plan | i install preview (dry-run) | c clear | r reload | q quit",
            self.selection_summary()
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
                format!(
                    "{} {} {} ({} item{})",
                    focus_marker(self.focus == FocusPane::Bundles && index == self.bundle_index),
                    selection_marker(self.selected_bundles.contains(bundle.id.as_str())),
                    bundle.display_name,
                    bundle.items.len(),
                    plural_suffix(bundle.items.len())
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
                let action = self
                    .action_step(item.id.as_str())
                    .map(|step| planned_action_name(step.action).to_string())
                    .unwrap_or_else(|| "idle".to_string());
                let verification = self
                    .verifier_result(item.id.as_str())
                    .map(|result| verifier_badge(&result))
                    .unwrap_or_else(|| "unverified".to_string());

                format!(
                    "{} {} {} [{action} | {verification}]",
                    focus_marker(self.focus == FocusPane::Items && index == self.item_index),
                    item_selection_marker(self, item.id.as_str()),
                    item.display_name,
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
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
            format!("Bundle: {}", bundle.display_name),
            format!("ID: {}", bundle.id),
            format!(
                "Selected: {}",
                yes_no(self.selected_bundles.contains(bundle.id.as_str()))
            ),
            "Members:".to_string(),
        ];

        for item_id in &bundle.items {
            let marker = if self.selected_items.contains(item_id.as_str()) {
                "[x]"
            } else {
                "[ ]"
            };
            lines.push(format!("- {marker} {item_id}"));
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
            format!("Item: {}", item.display_name),
            format!("ID: {}", item.id),
            format!("Category: {}", item_category_name(item.category)),
            format!("Scope: {}", install_scope_name(item.scope)),
            format!("Threshold: {}", stage_name(item.success_threshold)),
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
                "Evidence: {} total, {} required failures",
                verifier.summary.total_checks, verifier.summary.required_failures
            ));
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
                "No verifier snapshot cached for this item yet. Press v to inspect evidence, p to inspect planner actions, or i to inspect install preview results."
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
                    "Last action: install preview ({} request, {})",
                    install_mode_name(install.install_mode),
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
                "Status\nLast action: plan\n{} step{} => {} install, {} repair, {} blocked",
                plan.steps.len(),
                plural_suffix(plan.steps.len()),
                install_steps,
                repair_steps,
                blocked_steps,
            );
        }

        if let Some(verification) = self.cached_verification.as_ref() {
            return format!(
                "Status\nLast action: verify\n{} total | {} threshold met | {} threshold unmet",
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
            "all-default bundle expansion".to_string()
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
        self.cached_install
            .as_ref()
            .and_then(|install| {
                install
                    .post_verification
                    .result_for(item_id)
                    .map(|result| result.result.clone())
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
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TuiAction {
    None,
    Exit,
    Dispatch(CommandName),
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

        Ok(Self {
            engine,
            state: UiState::new(catalog),
        })
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
            CommandName::Plan | CommandName::Verify => {
                CommandRequest::new(command, InterfaceMode::Tui, OutputFormat::Text)
                    .with_planner_request(self.state.planner_request())
            }
            CommandName::Install => {
                CommandRequest::new(CommandName::Install, InterfaceMode::Tui, OutputFormat::Text)
                    .with_planner_request(self.state.planner_request())
                    .with_install_mode(InstallMode::DryRun)
            }
            CommandName::Tui => return,
        };

        match self.engine.execute(request) {
            Ok(response) => self.state.apply_response(response),
            Err(error) => self.state.apply_error(command, error),
        }
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
    let browser = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(BUNDLE_HEIGHT), Constraint::Min(0)])
        .split(main[0]);

    frame.render_widget(
        paragraph(snapshot.header, "envira", Style::default().fg(Color::Cyan)),
        vertical[0],
    );
    frame.render_widget(
        paragraph(
            snapshot.bundles,
            "bundles",
            pane_title_style(state.focus == FocusPane::Bundles),
        ),
        browser[0],
    );
    frame.render_widget(
        paragraph(
            snapshot.items,
            "items",
            pane_title_style(state.focus == FocusPane::Items),
        ),
        browser[1],
    );
    frame.render_widget(
        paragraph(
            snapshot.details,
            "details",
            Style::default().fg(Color::Yellow),
        ),
        main[1],
    );
    frame.render_widget(
        paragraph(
            snapshot.results,
            "result",
            Style::default().fg(Color::Green),
        ),
        vertical[2],
    );
}

fn paragraph<'a>(text: String, title: &'a str, title_style: Style) -> Paragraph<'a> {
    Paragraph::new(text)
        .block(
            Block::default()
                .title(title)
                .title_style(title_style.add_modifier(Modifier::BOLD))
                .borders(Borders::ALL),
        )
        .wrap(Wrap { trim: false })
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

fn item_selection_marker(state: &UiState, item_id: &str) -> &'static str {
    if state.selected_items.contains(item_id) {
        "[x]"
    } else if item_selected_via_bundle(state, item_id) {
        "[-]"
    } else {
        "[ ]"
    }
}

fn item_selection_description(state: &UiState, item_id: &str) -> String {
    if state.selected_items.contains(item_id) {
        return "selected directly".to_string();
    }

    let selected_bundles = state
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
        .collect::<Vec<_>>();

    if selected_bundles.is_empty() {
        "not selected".to_string()
    } else {
        format!(
            "selected through bundle{} {}",
            plural_suffix(selected_bundles.len()),
            selected_bundles.join(", ")
        )
    }
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

fn verifier_badge(result: &VerifierResult) -> String {
    let achieved = result.achieved_stage.map(stage_name).unwrap_or("none");
    format!(
        "{achieved}/{}",
        if result.threshold_met {
            "met"
        } else {
            verification_health_name(result.health)
        }
    )
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

fn item_category_name(category: crate::catalog::ItemCategory) -> &'static str {
    match category {
        crate::catalog::ItemCategory::Foundation => "foundation",
        crate::catalog::ItemCategory::TerminalTool => "terminal_tool",
        crate::catalog::ItemCategory::ContainerTool => "container_tool",
        crate::catalog::ItemCategory::SystemMonitor => "system_monitor",
        crate::catalog::ItemCategory::RemoteAccess => "remote_access",
    }
}

fn install_scope_name(scope: crate::catalog::InstallScope) -> &'static str {
    match scope {
        crate::catalog::InstallScope::System => "system",
        crate::catalog::InstallScope::User => "user",
        crate::catalog::InstallScope::Hybrid => "hybrid",
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
