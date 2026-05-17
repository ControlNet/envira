use serde::{Deserialize, Serialize};

use crate::planner::PlannedAction;

use super::{
    operation::OperationSpec,
    plan::{ExecutionPlan, ExecutionStep},
    result::{CommandExecution, ExecutionDisposition, OperationState},
    CommandRunner,
};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ExecutionPlanReport {
    pub summary: ExecutionPlanSummary,
    pub steps: Vec<ExecutionStepReport>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ExecutionPlanSummary {
    pub total_steps: usize,
    pub actionable_steps: usize,
    pub successful_steps: usize,
    pub failed_steps: usize,
    pub skipped_steps: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ExecutionStepReport {
    pub step: ExecutionStep,
    pub disposition: ExecutionDisposition,
    pub message: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub operations: Vec<OperationExecutionReport>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct OperationExecutionReport {
    pub operation: OperationSpec,
    pub state: OperationState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<CommandExecution>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

pub fn execute_execution_plan(plan: &ExecutionPlan) -> ExecutionPlanReport {
    let runner = CommandRunner::default();
    let steps = plan
        .steps
        .iter()
        .map(|step| execute_step(&runner, step))
        .collect::<Vec<_>>();
    let actionable_steps = plan
        .steps
        .iter()
        .filter(|step| {
            matches!(
                step.action_step.action,
                PlannedAction::Install | PlannedAction::Repair
            )
        })
        .count();
    let successful_steps = steps
        .iter()
        .filter(|step| step.disposition == ExecutionDisposition::Success)
        .count();
    let failed_steps = steps
        .iter()
        .filter(|step| step.disposition == ExecutionDisposition::Failure)
        .count();
    let skipped_steps = steps
        .iter()
        .filter(|step| step.disposition == ExecutionDisposition::Skipped)
        .count();

    ExecutionPlanReport {
        summary: ExecutionPlanSummary {
            total_steps: steps.len(),
            actionable_steps,
            successful_steps,
            failed_steps,
            skipped_steps,
        },
        steps,
    }
}

fn execute_step(runner: &CommandRunner, step: &ExecutionStep) -> ExecutionStepReport {
    match step.action_step.action {
        PlannedAction::Skip => ExecutionStepReport {
            step: step.clone(),
            disposition: ExecutionDisposition::Skipped,
            message: step.action_step.rationale.summary.clone(),
            operations: Vec::new(),
        },
        PlannedAction::Blocked => ExecutionStepReport {
            step: step.clone(),
            disposition: ExecutionDisposition::Skipped,
            message: step.action_step.rationale.summary.clone(),
            operations: Vec::new(),
        },
        PlannedAction::Install | PlannedAction::Repair => execute_action_step(runner, step),
    }
}

fn execute_action_step(runner: &CommandRunner, step: &ExecutionStep) -> ExecutionStepReport {
    let mut operations = Vec::with_capacity(step.operations.len());
    let mut failed = false;
    let mut failure_message = None;

    for operation in &step.operations {
        if failed {
            operations.push(OperationExecutionReport {
                operation: operation.clone(),
                state: OperationState::Skipped,
                command: None,
                message: Some("Skipped after a previous operation failure.".to_string()),
            });
            continue;
        }

        match operation {
            OperationSpec::Command(command) => match runner.execute(command) {
                Ok(execution) => {
                    let state = execution.state();
                    let message = execution.summary.message.clone();

                    if execution.failed() {
                        failed = true;
                        failure_message = Some(message.clone());
                    }

                    operations.push(OperationExecutionReport {
                        operation: operation.clone(),
                        state,
                        command: Some(execution),
                        message: Some(message),
                    });
                }
                Err(error) => {
                    failed = true;
                    failure_message = Some(error.to_string());
                    operations.push(OperationExecutionReport {
                        operation: operation.clone(),
                        state: OperationState::Failure,
                        command: None,
                        message: Some(error.to_string()),
                    });
                }
            },
            _ => {
                let message = format!(
                    "Operation kind `{}` is not executable by the headless command runner.",
                    operation_kind(operation)
                );
                failed = true;
                failure_message = Some(message.clone());
                operations.push(OperationExecutionReport {
                    operation: operation.clone(),
                    state: OperationState::Failure,
                    command: None,
                    message: Some(message),
                });
            }
        }
    }

    let disposition = if failed {
        ExecutionDisposition::Failure
    } else {
        ExecutionDisposition::Success
    };
    let message = match disposition {
        ExecutionDisposition::Success => format!(
            "Executed {} operation(s) for `{}`.",
            step.operations.len(),
            step.action_step.step.item_id
        ),
        ExecutionDisposition::Failure => failure_message.unwrap_or_else(|| {
            format!("Execution failed for `{}`.", step.action_step.step.item_id)
        }),
        ExecutionDisposition::Skipped => step.action_step.rationale.summary.clone(),
    };

    ExecutionStepReport {
        step: step.clone(),
        disposition,
        message,
        operations,
    }
}

fn operation_kind(operation: &OperationSpec) -> &'static str {
    match operation {
        OperationSpec::Command(_) => "command",
        OperationSpec::Download(_) => "download",
        OperationSpec::Assert(_) => "assert",
        OperationSpec::Builtin(_) => "builtin",
    }
}
