use std::collections::BTreeMap;

use clap::ValueEnum;
use serde::{Deserialize, Serialize};

use crate::{
    catalog::Catalog,
    executor::{ExecutionDisposition, ExecutionPlan, ExecutionPlanReport},
    planner::{ActionPlan, PlanStep, PlannedAction, PlannerRequest},
    platform::PlatformContext,
    verifier::{VerificationProfile, VerifierResult},
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
        self.planner_request
            .clone()
            .unwrap_or_else(PlannerRequest::all_default)
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
        serde_json::to_string_pretty(self)
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
        serde_json::to_string_pretty(self)
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
