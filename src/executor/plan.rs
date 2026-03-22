use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::catalog::{
    Catalog, CatalogItem, InstallTarget, RecipeArchiveFormat, RecipeBuildSystem, RecipeSpec,
};
use crate::planner::{ActionPlan, ActionPlanStep, PlannedAction, PlannedScope};
use crate::platform::{InvocationKind, PlatformContext};

use super::backends::{build_native_backend_operations, BackendMappingError, NativePackageRecipe};
use super::builtin::plan_builtin_operations;
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
    NativePackage(NativePackageRecipe),
    Builtin(BuiltinRecipePlan),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "family", rename_all = "snake_case")]
pub enum BuiltinRecipePlan {
    DirectBinaryInstall {
        url: String,
        destination: PathBuf,
        binary_name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        checksum_sha256: Option<String>,
    },
    ArchiveInstall {
        url: String,
        destination_dir: PathBuf,
        format: RecipeArchiveFormat,
        binary_name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        member_path: Option<PathBuf>,
        strip_components: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        checksum_sha256: Option<String>,
    },
    SourceBuildInstall {
        source_url: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        revision: Option<String>,
        build_system: RecipeBuildSystem,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        working_subdir: Option<PathBuf>,
        install_prefix: PathBuf,
    },
}

#[derive(Debug, Error)]
pub enum ExecutionPlanError {
    #[error("planned item `{item_id}` is missing from the catalog during execution planning")]
    MissingCatalogItem { item_id: String },
    #[error(
        "item `{item_id}` is missing a recipe overlay for selected target `{:?}/{:?}`",
        target.backend,
        target.source
    )]
    MissingRecipe {
        item_id: String,
        target: InstallTarget,
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
    #[error(
        "item `{item_id}` selected target `{:?}/{:?}` is not supported by executor translation",
        target.backend,
        target.source
    )]
    UnsupportedTarget {
        item_id: String,
        target: InstallTarget,
    },
    #[error(transparent)]
    BackendMapping(#[from] BackendMappingError),
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
            build_operations_for_item(item, platform, action_step, execution_target)?
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
    item: &CatalogItem,
    platform: &PlatformContext,
    action_step: &ActionPlanStep,
    execution_target: ExecutionTarget,
) -> Result<(Option<ExecutionRecipe>, Vec<OperationSpec>), ExecutionPlanError> {
    let selected_target = &action_step.step.selected_target;
    let Some(recipe_overlay) = item.recipe_for_target(selected_target) else {
        return Err(ExecutionPlanError::MissingRecipe {
            item_id: item.id.as_str().to_string(),
            target: selected_target.clone(),
        });
    };

    match &recipe_overlay.recipe {
        RecipeSpec::NativePackage { packages } => {
            let recipe = NativePackageRecipe {
                packages: packages.clone(),
            };
            let operations = build_native_backend_operations(
                selected_target.backend,
                &recipe,
                execution_target,
            )?;
            Ok((Some(ExecutionRecipe::NativePackage(recipe)), operations))
        }
        RecipeSpec::DirectBinary {
            url,
            binary_name,
            checksum_sha256,
        } => {
            let recipe = BuiltinRecipePlan::DirectBinaryInstall {
                url: url.clone(),
                destination: install_bin_path(platform, execution_target, binary_name),
                binary_name: binary_name.clone(),
                checksum_sha256: checksum_sha256.clone(),
            };
            let operations = plan_builtin_operations(item.id.as_str(), &recipe, execution_target);
            Ok((Some(ExecutionRecipe::Builtin(recipe)), operations))
        }
        RecipeSpec::Archive {
            url,
            format,
            binary_name,
            member_path,
            strip_components,
            checksum_sha256,
        } => {
            let recipe = BuiltinRecipePlan::ArchiveInstall {
                url: url.clone(),
                destination_dir: install_bin_dir(platform, execution_target),
                format: *format,
                binary_name: binary_name.clone(),
                member_path: member_path.clone(),
                strip_components: *strip_components,
                checksum_sha256: checksum_sha256.clone(),
            };
            let operations = plan_builtin_operations(item.id.as_str(), &recipe, execution_target);
            Ok((Some(ExecutionRecipe::Builtin(recipe)), operations))
        }
        RecipeSpec::SourceBuild {
            source_url,
            revision,
            build_system,
            working_subdir,
        } => {
            let recipe = BuiltinRecipePlan::SourceBuildInstall {
                source_url: source_url.clone(),
                revision: revision.clone(),
                build_system: *build_system,
                working_subdir: working_subdir.clone(),
                install_prefix: install_prefix(platform, execution_target),
            };
            let operations = plan_builtin_operations(item.id.as_str(), &recipe, execution_target);
            Ok((Some(ExecutionRecipe::Builtin(recipe)), operations))
        }
    }
}

fn install_bin_path(
    platform: &PlatformContext,
    target: ExecutionTarget,
    binary_name: &str,
) -> PathBuf {
    install_bin_dir(platform, target).join(binary_name)
}

fn install_bin_dir(platform: &PlatformContext, target: ExecutionTarget) -> PathBuf {
    install_prefix(platform, target).join("bin")
}

fn install_prefix(platform: &PlatformContext, target: ExecutionTarget) -> PathBuf {
    match target {
        ExecutionTarget::System => PathBuf::from("/usr/local"),
        ExecutionTarget::CurrentProcess => platform.effective_user.home_dir.join(".local"),
        ExecutionTarget::TargetUser => platform
            .target_user
            .as_ref()
            .map(|user| user.home_dir.join(".local"))
            .unwrap_or_else(|| platform.effective_user.home_dir.join(".local")),
    }
}

#[allow(dead_code)]
fn _assert_absolute(path: &Path) -> bool {
    path.is_absolute()
}
