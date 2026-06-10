use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::verifier::{
    ProbeRequirement, ServiceUsabilityState, VerificationHealth, VerifierResult,
};

use super::{InstallPlan, PlanPlatformSnapshot, PlanStep, PlannerRequest};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ActionPlan {
    pub request: PlannerRequest,
    pub platform: PlanPlatformSnapshot,
    pub steps: Vec<ActionPlanStep>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ActionPlanStep {
    #[serde(flatten)]
    pub step: PlanStep,
    pub action: PlannedAction,
    pub rationale: ActionRationale,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlannedAction {
    Skip,
    Install,
    Repair,
    Blocked,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionReasonCode {
    ThresholdMet,
    Missing,
    BelowThreshold,
    VerificationUnknown,
    ServiceNonUsable,
    ServiceOnDemand,
    ServiceBlocked,
    ServiceUnknown,
    DependencyBlocked,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ActionRationale {
    pub code: ActionReasonCode,
    pub summary: String,
    pub verifier: VerifierResult,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocked_by: Vec<BlockedDependency>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct BlockedDependency {
    pub item_id: String,
    pub action: PlannedAction,
    pub reason_code: ActionReasonCode,
    pub summary: String,
}

#[derive(Debug, Error)]
pub enum ActionPlanError {
    #[error("missing verifier result for planned item `{item_id}`")]
    MissingVerifierResult { item_id: String },
}

pub fn classify_install_plan(
    install_plan: &InstallPlan,
    verifier_results: &BTreeMap<String, VerifierResult>,
) -> Result<ActionPlan, ActionPlanError> {
    let mut steps = Vec::with_capacity(install_plan.steps.len());
    let mut resolved = BTreeMap::<String, ResolvedAction>::new();

    for step in &install_plan.steps {
        let verifier = verifier_results
            .get(step.item_id.as_str())
            .cloned()
            .ok_or_else(|| ActionPlanError::MissingVerifierResult {
                item_id: step.item_id.clone(),
            })?;

        let blocked_by = blocked_dependencies(step, &resolved);
        let (action, rationale) = if blocked_by.is_empty() {
            classify_step(step, verifier)
        } else {
            classify_dependency_blocked(step, verifier, blocked_by)
        };

        resolved.insert(
            step.item_id.clone(),
            ResolvedAction {
                action,
                code: rationale.code,
                summary: rationale.summary.clone(),
            },
        );

        steps.push(ActionPlanStep {
            step: step.clone(),
            action,
            rationale,
        });
    }

    Ok(ActionPlan {
        request: install_plan.request.clone(),
        platform: install_plan.platform.clone(),
        steps,
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ResolvedAction {
    action: PlannedAction,
    code: ActionReasonCode,
    summary: String,
}

fn blocked_dependencies(
    step: &PlanStep,
    resolved: &BTreeMap<String, ResolvedAction>,
) -> Vec<BlockedDependency> {
    step.depends_on
        .iter()
        .filter_map(|dependency_id| {
            let dependency = resolved.get(dependency_id)?;
            (dependency.action == PlannedAction::Blocked).then(|| BlockedDependency {
                item_id: dependency_id.clone(),
                action: dependency.action,
                reason_code: dependency.code,
                summary: dependency.summary.clone(),
            })
        })
        .collect()
}

fn classify_dependency_blocked(
    step: &PlanStep,
    verifier: VerifierResult,
    blocked_by: Vec<BlockedDependency>,
) -> (PlannedAction, ActionRationale) {
    let dependency_ids = blocked_by
        .iter()
        .map(|dependency| dependency.item_id.as_str())
        .collect::<Vec<_>>()
        .join(", ");

    (
        PlannedAction::Blocked,
        ActionRationale {
            code: ActionReasonCode::DependencyBlocked,
            summary: format!(
                "Dependencies are blocked for `{}`: {dependency_ids}.",
                step.item_id
            ),
            verifier,
            blocked_by,
        },
    )
}

fn classify_step(step: &PlanStep, verifier: VerifierResult) -> (PlannedAction, ActionRationale) {
    if let Some(service) = verifier.service.as_ref() {
        let decision = match service.state {
            ServiceUsabilityState::Operational => (
                PlannedAction::Skip,
                ActionReasonCode::ThresholdMet,
                format!(
                    "Service verification is already operational for `{}`.",
                    step.item_id
                ),
            ),
            ServiceUsabilityState::Missing => (
                PlannedAction::Install,
                ActionReasonCode::Missing,
                format!(
                    "Service verification shows `{}` is missing and should be installed.",
                    step.item_id
                ),
            ),
            ServiceUsabilityState::NonUsable => (
                PlannedAction::Repair,
                ActionReasonCode::ServiceNonUsable,
                format!(
                    "Service verification shows `{}` is present but not usable yet, so it should be repaired.",
                    step.item_id
                ),
            ),
            ServiceUsabilityState::OnDemand => (
                PlannedAction::Blocked,
                ActionReasonCode::ServiceOnDemand,
                format!(
                    "Service verification shows `{}` is configured on demand and is blocked from automatic repair semantics.",
                    step.item_id
                ),
            ),
            ServiceUsabilityState::Blocked => (
                PlannedAction::Blocked,
                ActionReasonCode::ServiceBlocked,
                format!(
                    "Service verification shows `{}` is blocked by an access or runtime condition.",
                    step.item_id
                ),
            ),
            ServiceUsabilityState::Unknown => (
                PlannedAction::Blocked,
                ActionReasonCode::ServiceUnknown,
                format!(
                    "Service verification could not determine whether `{}` is usable.",
                    step.item_id
                ),
            ),
        };

        return (decision.0, rationale(decision.1, decision.2, verifier));
    }

    if verifier.threshold_met {
        return (
            PlannedAction::Skip,
            rationale(
                ActionReasonCode::ThresholdMet,
                format!(
                    "Verifier already meets the required `{}` threshold for `{}`.",
                    stage_name(step.required_stage),
                    step.item_id
                ),
                verifier,
            ),
        );
    }

    if verifier.health == VerificationHealth::Unknown {
        return (
            PlannedAction::Blocked,
            rationale(
                ActionReasonCode::VerificationUnknown,
                format!(
                    "Verifier returned an unknown state for `{}`, so the planner marks it blocked.",
                    step.item_id
                ),
                verifier,
            ),
        );
    }

    if required_checks_are_all_missing(&verifier) {
        return (
            PlannedAction::Install,
            rationale(
                ActionReasonCode::Missing,
                format!(
                    "Required verifier checks are missing for `{}`, so the planner selects install.",
                    step.item_id
                ),
                verifier,
            ),
        );
    }

    (
        PlannedAction::Repair,
        rationale(
            ActionReasonCode::BelowThreshold,
            format!(
                "Verifier shows `{}` is below the required `{}` threshold and should be repaired.",
                step.item_id,
                stage_name(step.required_stage)
            ),
            verifier,
        ),
    )
}

fn rationale(code: ActionReasonCode, summary: String, verifier: VerifierResult) -> ActionRationale {
    ActionRationale {
        code,
        summary,
        verifier,
        blocked_by: Vec::new(),
    }
}

fn required_checks_are_all_missing(verifier: &VerifierResult) -> bool {
    let mut saw_required = false;

    for evidence in &verifier.evidence {
        if !evidence.participates || evidence.check.requirement != ProbeRequirement::Required {
            continue;
        }

        saw_required = true;

        if evidence.record.status != crate::verifier::EvidenceStatus::Missing {
            return false;
        }
    }

    saw_required
}

fn stage_name(stage: crate::verifier::VerificationStage) -> &'static str {
    match stage {
        crate::verifier::VerificationStage::Present => "present",
        crate::verifier::VerificationStage::Configured => "configured",
        crate::verifier::VerificationStage::Operational => "operational",
    }
}
