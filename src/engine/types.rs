use std::collections::BTreeMap;

use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    catalog::Catalog,
    executor::{
        AssertOperation, BuiltinOperation, CommandOperation, DownloadOperation,
        ExecutionDisposition, ExecutionPlan, ExecutionPlanReport, ExecutionRecipe,
        ExecutionStepReport, OperationExecutionReport, OperationSpec,
    },
    planner::{ActionPlan, PlanStep, PlannedAction, PlannerRequest},
    platform::PlatformContext,
    verifier::{
        ServiceAssessment, ServiceProbeEvidence, VerificationHealth, VerificationProfile,
        VerificationStage, VerificationSummary, VerifierEvidence, VerifierResult,
    },
};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandName {
    Catalog,
    Plan,
    Install,
    Verify,
    Tui,
}

impl CommandName {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Catalog => "catalog",
            Self::Plan => "plan",
            Self::Install => "install",
            Self::Verify => "verify",
            Self::Tui => "tui",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InterfaceMode {
    Headless,
    Tui,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum OutputFormat {
    #[default]
    Text,
    Json,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallMode {
    #[default]
    Apply,
    DryRun,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CommandRequest {
    pub command: CommandName,
    pub mode: InterfaceMode,
    pub format: OutputFormat,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub planner_request: Option<PlannerRequest>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification_profile: Option<VerificationProfile>,
    #[serde(default)]
    pub install_mode: InstallMode,
}

impl CommandRequest {
    pub fn new(command: CommandName, mode: InterfaceMode, format: OutputFormat) -> Self {
        Self {
            command,
            mode,
            format,
            planner_request: None,
            verification_profile: None,
            install_mode: InstallMode::Apply,
        }
    }

    pub fn with_planner_request(mut self, planner_request: PlannerRequest) -> Self {
        self.planner_request = Some(planner_request);
        self
    }

    pub fn with_verification_profile(mut self, verification_profile: VerificationProfile) -> Self {
        self.verification_profile = Some(verification_profile);
        self
    }

    pub fn with_install_mode(mut self, install_mode: InstallMode) -> Self {
        self.install_mode = install_mode;
        self
    }

    pub fn resolved_planner_request(&self) -> PlannerRequest {
        self.planner_request.clone().unwrap_or_default()
    }

    pub fn resolved_verification_profile(&self) -> VerificationProfile {
        self.verification_profile
            .unwrap_or(VerificationProfile::Quick)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CommandResponse {
    pub ok: bool,
    pub command: CommandName,
    pub mode: InterfaceMode,
    pub format: OutputFormat,
    pub payload: CommandPayload,
}

impl CommandResponse {
    pub fn success(
        command: CommandName,
        mode: InterfaceMode,
        format: OutputFormat,
        payload: CommandPayload,
    ) -> Self {
        Self {
            ok: true,
            command,
            mode,
            format,
            payload,
        }
    }

    pub fn as_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(&CommandResponseJson::from(self))
    }

    pub fn exit_code(&self) -> i32 {
        if self.payload.is_success() {
            0
        } else {
            1
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CommandErrorResponse {
    pub ok: bool,
    pub command: CommandName,
    pub mode: InterfaceMode,
    pub format: OutputFormat,
    pub error: CommandErrorEnvelope,
}

impl CommandErrorResponse {
    pub fn new(
        command: CommandName,
        mode: InterfaceMode,
        format: OutputFormat,
        error: CommandErrorEnvelope,
    ) -> Self {
        Self {
            ok: false,
            command,
            mode,
            format,
            error,
        }
    }

    pub fn as_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(&CommandErrorResponseJson::from(self))
    }

    pub fn render_text(&self) -> String {
        let message = normalize_error_message(self.error.message.as_str());
        let context = normalize_error_context(&self.error.context);
        let mut output = format!("{}: {message}", self.error.code);

        if !context.is_empty() {
            output.push_str("\nContext:");
            for (key, value) in context {
                output.push_str(&format!("\n- {key}: {value}"));
            }
        }

        output
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CommandErrorEnvelope {
    pub code: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub context: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CommandPayload {
    Catalog {
        catalog: Catalog,
    },
    Plan {
        action_plan: ActionPlan,
    },
    Verify {
        verification: VerificationWorkflowResult,
    },
    Install {
        install: InstallWorkflowResult,
    },
}

impl CommandPayload {
    pub fn is_success(&self) -> bool {
        match self {
            Self::Catalog { .. } | Self::Plan { .. } => true,
            Self::Verify { verification } => verification.summary.threshold_unmet_steps == 0,
            Self::Install { install } => matches!(
                install.outcome.status,
                InstallWorkflowStatus::Success | InstallWorkflowStatus::DryRun
            ),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct VerificationWorkflowResult {
    pub request: PlannerRequest,
    pub profile: VerificationProfile,
    pub platform: PlatformContext,
    pub summary: VerificationWorkflowSummary,
    pub results: Vec<VerificationItemResult>,
}

impl VerificationWorkflowResult {
    pub fn result_for(&self, item_id: &str) -> Option<&VerificationItemResult> {
        self.results
            .iter()
            .find(|result| result.step.item_id == item_id)
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct VerificationWorkflowSummary {
    pub total_steps: usize,
    pub threshold_met_steps: usize,
    pub threshold_unmet_steps: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct VerificationItemResult {
    pub step: PlanStep,
    pub result: VerifierResult,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct InstallWorkflowResult {
    pub install_mode: InstallMode,
    pub action_plan: ActionPlan,
    pub execution_plan: ExecutionPlan,
    pub execution: ExecutionPlanReport,
    pub post_verification: VerificationWorkflowResult,
    pub outcome: InstallWorkflowOutcome,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct InstallWorkflowOutcome {
    pub status: InstallWorkflowStatus,
    pub execution_succeeded: bool,
    pub actionable_steps: usize,
    pub blocked_steps: usize,
    pub threshold_met_steps: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failures: Vec<InstallWorkflowFailure>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallWorkflowStatus {
    Success,
    DryRun,
    VerificationFailed,
    Blocked,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct InstallWorkflowFailure {
    pub item_id: String,
    pub action: PlannedAction,
    pub execution_disposition: ExecutionDisposition,
    pub verifier: VerifierResult,
}

#[derive(Clone, Debug, Serialize)]
struct CommandResponseJson {
    ok: bool,
    command: CommandName,
    mode: InterfaceMode,
    format: OutputFormat,
    payload: CommandPayloadJson,
}

impl From<&CommandResponse> for CommandResponseJson {
    fn from(value: &CommandResponse) -> Self {
        Self {
            ok: value.ok,
            command: value.command,
            mode: value.mode,
            format: value.format,
            payload: CommandPayloadJson::from(&value.payload),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum CommandPayloadJson {
    Catalog {
        catalog: Catalog,
        summary: CatalogSummaryJson,
    },
    Plan {
        request: PlannerRequest,
        summary: PlanSummaryJson,
        items: Vec<PlannedItemJson>,
    },
    Verify {
        request: PlannerRequest,
        profile: VerificationProfile,
        summary: VerificationWorkflowSummary,
        items: Vec<VerifiedItemJson>,
    },
    Install {
        request: PlannerRequest,
        install_mode: InstallMode,
        summary: InstallSummaryJson,
        items: Vec<InstalledItemJson>,
        execution: HeadlessExecutionReportJson,
        outcome: InstallOutcomeJson,
    },
}

impl From<&CommandPayload> for CommandPayloadJson {
    fn from(value: &CommandPayload) -> Self {
        match value {
            CommandPayload::Catalog { catalog } => Self::Catalog {
                catalog: catalog.clone(),
                summary: CatalogSummaryJson {
                    bundle_count: catalog.bundles.len(),
                    item_count: catalog.items.len(),
                    default_bundle_count: catalog.default_bundles.len(),
                },
            },
            CommandPayload::Plan { action_plan } => Self::Plan {
                request: action_plan.request.clone(),
                summary: PlanSummaryJson::from(action_plan),
                items: action_plan
                    .steps
                    .iter()
                    .map(PlannedItemJson::from)
                    .collect(),
            },
            CommandPayload::Verify { verification } => Self::Verify {
                request: verification.request.clone(),
                profile: verification.profile,
                summary: verification.summary.clone(),
                items: verification
                    .results
                    .iter()
                    .map(VerifiedItemJson::from)
                    .collect(),
            },
            CommandPayload::Install { install } => Self::Install {
                request: install.action_plan.request.clone(),
                install_mode: install.install_mode,
                summary: InstallSummaryJson::from(install),
                items: install_items_json(install),
                execution: HeadlessExecutionReportJson::from(&install.execution),
                outcome: InstallOutcomeJson::from(&install.outcome),
            },
        }
    }
}

#[derive(Clone, Debug, Serialize)]
struct CommandErrorResponseJson {
    ok: bool,
    command: CommandName,
    mode: InterfaceMode,
    format: OutputFormat,
    error: CommandErrorEnvelopeJson,
}

impl From<&CommandErrorResponse> for CommandErrorResponseJson {
    fn from(value: &CommandErrorResponse) -> Self {
        Self {
            ok: value.ok,
            command: value.command,
            mode: value.mode,
            format: value.format,
            error: CommandErrorEnvelopeJson::from(&value.error),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
struct CommandErrorEnvelopeJson {
    code: String,
    message: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    context: BTreeMap<String, String>,
}

impl From<&CommandErrorEnvelope> for CommandErrorEnvelopeJson {
    fn from(value: &CommandErrorEnvelope) -> Self {
        Self {
            code: value.code.clone(),
            message: normalize_error_message(value.message.as_str()),
            context: normalize_error_context(&value.context),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
struct CatalogSummaryJson {
    bundle_count: usize,
    item_count: usize,
    default_bundle_count: usize,
}

#[derive(Clone, Debug, Serialize)]
struct PlanSummaryJson {
    total_items: usize,
    requested_items: usize,
    install_items: usize,
    repair_items: usize,
    skip_items: usize,
    blocked_items: usize,
}

impl From<&ActionPlan> for PlanSummaryJson {
    fn from(value: &ActionPlan) -> Self {
        Self {
            total_items: value.steps.len(),
            requested_items: value
                .steps
                .iter()
                .filter(|step| step.step.requested)
                .count(),
            install_items: value
                .steps
                .iter()
                .filter(|step| step.action == PlannedAction::Install)
                .count(),
            repair_items: value
                .steps
                .iter()
                .filter(|step| step.action == PlannedAction::Repair)
                .count(),
            skip_items: value
                .steps
                .iter()
                .filter(|step| step.action == PlannedAction::Skip)
                .count(),
            blocked_items: value
                .steps
                .iter()
                .filter(|step| step.action == PlannedAction::Blocked)
                .count(),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
struct PlannedItemJson {
    #[serde(flatten)]
    step: HeadlessPlanStepJson,
    action: PlannedAction,
    reason_code: crate::planner::ActionReasonCode,
    summary: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    blocked_by: Vec<crate::planner::BlockedDependency>,
}

impl From<&crate::planner::ActionPlanStep> for PlannedItemJson {
    fn from(value: &crate::planner::ActionPlanStep) -> Self {
        Self {
            step: HeadlessPlanStepJson::from(&value.step),
            action: value.action,
            reason_code: value.rationale.code,
            summary: value.rationale.summary.clone(),
            blocked_by: value.rationale.blocked_by.clone(),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
struct HeadlessPlanStepJson {
    item_id: String,
    display_name: String,
    requested: bool,
    depends_on: Vec<String>,
    catalog_scope: crate::catalog::InstallScope,
    planned_scope: crate::planner::PlannedScope,
    required_stage: VerificationStage,
}

impl From<&PlanStep> for HeadlessPlanStepJson {
    fn from(value: &PlanStep) -> Self {
        Self {
            item_id: value.item_id.clone(),
            display_name: value.display_name.clone(),
            requested: value.requested,
            depends_on: value.depends_on.clone(),
            catalog_scope: value.catalog_scope,
            planned_scope: value.planned_scope,
            required_stage: value.required_stage,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
struct VerifiedItemJson {
    #[serde(flatten)]
    step: HeadlessPlanStepJson,
    threshold_met: bool,
    health: VerificationHealth,
    observed_scope: crate::verifier::ObservedScope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    achieved_stage: Option<VerificationStage>,
    summary: VerificationSummary,
    evidence: Vec<VerifierEvidence>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    service_evidence: Vec<ServiceProbeEvidence>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    service: Option<ServiceAssessment>,
}

impl From<&VerificationItemResult> for VerifiedItemJson {
    fn from(value: &VerificationItemResult) -> Self {
        Self {
            step: HeadlessPlanStepJson::from(&value.step),
            threshold_met: value.result.threshold_met,
            health: value.result.health,
            observed_scope: value.result.observed_scope,
            achieved_stage: value.result.achieved_stage,
            summary: value.result.summary.clone(),
            evidence: value.result.evidence.clone(),
            service_evidence: value.result.service_evidence.clone(),
            service: value.result.service.clone(),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
struct InstallSummaryJson {
    total_items: usize,
    requested_items: usize,
    actionable_items: usize,
    blocked_items: usize,
    threshold_met_items: usize,
    failed_items: usize,
    status: InstallWorkflowStatus,
    execution_succeeded: bool,
}

impl From<&InstallWorkflowResult> for InstallSummaryJson {
    fn from(value: &InstallWorkflowResult) -> Self {
        Self {
            total_items: value.action_plan.steps.len(),
            requested_items: value
                .action_plan
                .steps
                .iter()
                .filter(|step| step.step.requested)
                .count(),
            actionable_items: value.outcome.actionable_steps,
            blocked_items: value.outcome.blocked_steps,
            threshold_met_items: value.outcome.threshold_met_steps,
            failed_items: value.outcome.failures.len(),
            status: value.outcome.status,
            execution_succeeded: value.outcome.execution_succeeded,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
struct InstalledItemJson {
    #[serde(flatten)]
    step: HeadlessPlanStepJson,
    action: PlannedAction,
    reason_code: crate::planner::ActionReasonCode,
    summary: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    blocked_by: Vec<crate::planner::BlockedDependency>,
    execution_disposition: ExecutionDisposition,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    execution_message: Option<String>,
    threshold_met: bool,
    health: VerificationHealth,
}

#[derive(Clone, Debug, Serialize)]
struct HeadlessExecutionReportJson {
    summary: crate::executor::ExecutionPlanSummary,
    steps: Vec<HeadlessExecutionStepJson>,
}

impl From<&ExecutionPlanReport> for HeadlessExecutionReportJson {
    fn from(value: &ExecutionPlanReport) -> Self {
        Self {
            summary: value.summary.clone(),
            steps: value
                .steps
                .iter()
                .map(HeadlessExecutionStepJson::from)
                .collect(),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
struct HeadlessExecutionStepJson {
    item_id: String,
    display_name: String,
    action: PlannedAction,
    disposition: ExecutionDisposition,
    message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    recipe: Option<ExecutionRecipe>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    operations: Vec<HeadlessOperationReportJson>,
}

impl From<&ExecutionStepReport> for HeadlessExecutionStepJson {
    fn from(value: &ExecutionStepReport) -> Self {
        Self {
            item_id: value.step.action_step.step.item_id.clone(),
            display_name: value.step.action_step.step.display_name.clone(),
            action: value.step.action_step.action,
            disposition: value.disposition,
            message: value.message.clone(),
            recipe: value.step.recipe.clone(),
            operations: value
                .operations
                .iter()
                .map(HeadlessOperationReportJson::from)
                .collect(),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
struct HeadlessOperationReportJson {
    operation: Value,
    state: crate::executor::OperationState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    message: Option<String>,
}

impl From<&OperationExecutionReport> for HeadlessOperationReportJson {
    fn from(value: &OperationExecutionReport) -> Self {
        Self {
            operation: headless_operation_value(&value.operation),
            state: value.state,
            message: value.message.clone(),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
struct InstallOutcomeJson {
    status: InstallWorkflowStatus,
    execution_succeeded: bool,
    actionable_items: usize,
    blocked_items: usize,
    threshold_met_items: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    failures: Vec<InstallFailureJson>,
}

impl From<&InstallWorkflowOutcome> for InstallOutcomeJson {
    fn from(value: &InstallWorkflowOutcome) -> Self {
        Self {
            status: value.status,
            execution_succeeded: value.execution_succeeded,
            actionable_items: value.actionable_steps,
            blocked_items: value.blocked_steps,
            threshold_met_items: value.threshold_met_steps,
            failures: value
                .failures
                .iter()
                .map(InstallFailureJson::from)
                .collect(),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
struct InstallFailureJson {
    item_id: String,
    action: PlannedAction,
    execution_disposition: ExecutionDisposition,
    threshold_met: bool,
    health: VerificationHealth,
}

impl From<&InstallWorkflowFailure> for InstallFailureJson {
    fn from(value: &InstallWorkflowFailure) -> Self {
        Self {
            item_id: value.item_id.clone(),
            action: value.action,
            execution_disposition: value.execution_disposition,
            threshold_met: value.verifier.threshold_met,
            health: value.verifier.health,
        }
    }
}

fn install_items_json(install: &InstallWorkflowResult) -> Vec<InstalledItemJson> {
    let execution_by_item = install
        .execution
        .steps
        .iter()
        .map(|step| (step.step.action_step.step.item_id.as_str(), step))
        .collect::<BTreeMap<_, _>>();
    let verification_by_item = install
        .post_verification
        .results
        .iter()
        .map(|result| (result.step.item_id.as_str(), result))
        .collect::<BTreeMap<_, _>>();

    install
        .action_plan
        .steps
        .iter()
        .map(|step| {
            let execution = execution_by_item.get(step.step.item_id.as_str()).copied();
            let verification = verification_by_item
                .get(step.step.item_id.as_str())
                .copied()
                .expect("post-verification result should exist for every planned install item");

            InstalledItemJson {
                step: HeadlessPlanStepJson::from(&step.step),
                action: step.action,
                reason_code: step.rationale.code,
                summary: step.rationale.summary.clone(),
                blocked_by: step.rationale.blocked_by.clone(),
                execution_disposition: execution
                    .map(|report| report.disposition)
                    .unwrap_or(ExecutionDisposition::Skipped),
                execution_message: execution.map(|report| report.message.clone()),
                threshold_met: verification.result.threshold_met,
                health: verification.result.health,
            }
        })
        .collect()
}

fn headless_operation_value(operation: &OperationSpec) -> Value {
    match operation {
        OperationSpec::Command(command) => headless_command_operation_value(command),
        OperationSpec::Download(download) => headless_download_operation_value(download),
        OperationSpec::Assert(assertion) => headless_assert_operation_value(assertion),
        OperationSpec::Builtin(builtin) => headless_builtin_operation_value(builtin),
    }
}

fn headless_command_operation_value(operation: &CommandOperation) -> Value {
    serde_json::json!({
        "kind": "command",
        "program": operation.program,
        "args": operation.args,
    })
}

fn headless_download_operation_value(operation: &DownloadOperation) -> Value {
    let mut value = serde_json::json!({
        "kind": "download",
        "url": operation.url,
        "destination": operation.destination,
        "executable": operation.executable,
    });

    if let Some(checksum_sha256) = &operation.checksum_sha256 {
        value
            .as_object_mut()
            .expect("download operation should serialize to an object")
            .insert(
                "checksum_sha256".to_string(),
                Value::String(checksum_sha256.clone()),
            );
    }

    value
}

fn headless_assert_operation_value(operation: &AssertOperation) -> Value {
    let mut value = serde_json::json!({
        "kind": "assert",
        "condition": operation.condition,
    });

    if let Some(message) = &operation.message {
        value
            .as_object_mut()
            .expect("assert operation should serialize to an object")
            .insert("message".to_string(), Value::String(message.clone()));
    }

    value
}

fn headless_builtin_operation_value(operation: &BuiltinOperation) -> Value {
    serde_json::to_value(operation).expect("builtin operation should serialize")
}

fn normalize_error_message(message: &str) -> String {
    message.replace("catalog manifest", "catalog")
}

fn normalize_error_context(context: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    context
        .iter()
        .map(|(key, value)| {
            let normalized_key = if key == "manifest_path" {
                "catalog_path".to_string()
            } else {
                key.clone()
            };

            (normalized_key, value.clone())
        })
        .collect()
}
