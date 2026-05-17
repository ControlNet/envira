use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::catalog::{Catalog, CatalogCommand, CatalogItem, CommandMode};
use crate::planner::{ActionPlan, ActionPlanStep, PlannedAction, PlannedScope};
use crate::platform::{InvocationKind, PlatformContext};

use super::builtin::plan_shell_operations;
use super::operation::{ExecutionTarget, OperationSpec};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ExecutionPlan {
    pub request: crate::planner::PlannerRequest,
    pub platform: crate::planner::PlanPlatformSnapshot,
    pub steps: Vec<ExecutionStep>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ExecutionStep {
    pub action_step: ActionPlanStep,
    pub execution_target: ExecutionTarget,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recipe: Option<ExecutionRecipe>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub operations: Vec<OperationSpec>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ExecutionRecipe {
    Shell { shell: String, command: String },
}

#[derive(Debug, Error)]
pub enum ExecutionPlanError {
    #[error("planned item `{item_id}` is missing from the catalog during execution planning")]
    MissingCatalogItem { item_id: String },
    #[error(
        "item `{item_id}` is missing a recipe shell contract for planned scope `{planned_scope:?}` on distro `{distro_id}`"
    )]
    MissingRecipe {
        item_id: String,
        planned_scope: PlannedScope,
        distro_id: String,
    },
    #[error(
        "item `{item_id}` cannot resolve a `{planned_scope:?}` execution target from invocation `{:?}`",
        invocation
    )]
    UnsupportedExecutionTarget {
        item_id: String,
        planned_scope: PlannedScope,
        invocation: InvocationKind,
    },
}

pub fn build_execution_plan(
    catalog: &Catalog,
    platform: &PlatformContext,
    action_plan: &ActionPlan,
) -> Result<ExecutionPlan, ExecutionPlanError> {
    let mut steps = Vec::with_capacity(action_plan.steps.len());

    for action_step in &action_plan.steps {
        steps.push(build_execution_step(catalog, platform, action_step)?);
    }

    Ok(ExecutionPlan {
        request: action_plan.request.clone(),
        platform: action_plan.platform.clone(),
        steps,
    })
}

pub fn resolve_execution_target(
    platform: &PlatformContext,
    item_id: &str,
    planned_scope: PlannedScope,
) -> Result<ExecutionTarget, ExecutionPlanError> {
    match planned_scope {
        PlannedScope::System => Ok(ExecutionTarget::System),
        PlannedScope::User => match platform.invocation {
            InvocationKind::User => Ok(ExecutionTarget::CurrentProcess),
            InvocationKind::Sudo if platform.target_user.is_some() => {
                Ok(ExecutionTarget::TargetUser)
            }
            invocation => Err(ExecutionPlanError::UnsupportedExecutionTarget {
                item_id: item_id.to_string(),
                planned_scope,
                invocation,
            }),
        },
    }
}

fn build_execution_step(
    catalog: &Catalog,
    platform: &PlatformContext,
    action_step: &ActionPlanStep,
) -> Result<ExecutionStep, ExecutionPlanError> {
    let item_id = action_step.step.item_id.as_str();
    let execution_target =
        resolve_execution_target(platform, item_id, action_step.step.planned_scope)?;

    let (recipe, operations) = match action_step.action {
        PlannedAction::Install | PlannedAction::Repair => {
            let item =
                catalog
                    .item(item_id)
                    .ok_or_else(|| ExecutionPlanError::MissingCatalogItem {
                        item_id: item_id.to_string(),
                    })?;
            build_operations_for_item(
                catalog.shell.as_str(),
                item,
                platform,
                action_step,
                execution_target,
            )?
        }
        PlannedAction::Skip | PlannedAction::Blocked => (None, Vec::new()),
    };

    Ok(ExecutionStep {
        action_step: action_step.clone(),
        execution_target,
        recipe,
        operations,
    })
}

fn build_operations_for_item(
    catalog_shell: &str,
    item: &CatalogItem,
    platform: &PlatformContext,
    action_step: &ActionPlanStep,
    execution_target: ExecutionTarget,
) -> Result<(Option<ExecutionRecipe>, Vec<OperationSpec>), ExecutionPlanError> {
    let planned_scope = action_step.step.planned_scope;
    let distro_id = platform.distro.id.as_str();
    let Some(recipe_contract) = select_recipe_contract(item, planned_scope, distro_id) else {
        return Err(ExecutionPlanError::MissingRecipe {
            item_id: item.id.as_str().to_string(),
            planned_scope,
            distro_id: distro_id.to_string(),
        });
    };

    let recipe = ExecutionRecipe::Shell {
        shell: catalog_shell.to_string(),
        command: recipe_contract.cmd.clone(),
    };
    let operations = plan_shell_operations(
        catalog_shell,
        recipe_contract.cmd.as_str(),
        execution_target,
    );

    Ok((Some(recipe), operations))
}

fn select_recipe_contract<'a>(
    item: &'a CatalogItem,
    planned_scope: PlannedScope,
    distro_id: &str,
) -> Option<&'a CatalogCommand> {
    let selected = item
        .recipes
        .iter()
        .find(|recipe| recipe_supports_plan(recipe, planned_scope, distro_id));

    if selected.is_some() || distro_id != "unknown" {
        return selected;
    }

    item.recipes
        .iter()
        .find(|recipe| recipe_supports_scope(recipe, planned_scope))
}

fn recipe_supports_plan(
    recipe: &CatalogCommand,
    planned_scope: PlannedScope,
    distro_id: &str,
) -> bool {
    recipe_supports_scope(recipe, planned_scope)
        && recipe
            .distros
            .iter()
            .any(|distro| distro == "*" || distro == distro_id)
}

fn recipe_supports_scope(recipe: &CatalogCommand, planned_scope: PlannedScope) -> bool {
    matches!(
        (recipe.mode, planned_scope),
        (CommandMode::Sudo, PlannedScope::System) | (CommandMode::User, PlannedScope::User)
    )
}
