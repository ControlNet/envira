pub mod types;

use std::{
    collections::BTreeMap,
    env, fs,
    path::{Path, PathBuf},
};

use thiserror::Error;

use crate::{
    catalog::{embedded_manifest, Catalog, CatalogError},
    executor::{
        build_execution_plan, execute_execution_plan, ExecutionDisposition, ExecutionPlanError,
        ExecutionPlanReport, ExecutionPlanSummary, ExecutionStepReport, OperationExecutionReport,
        OperationState,
    },
    planner::{
        build_install_plan, build_install_plan_with_target, classify_install_plan, ActionPlan,
        ActionPlanError, InstallPlan, PlannedAction, PlannerError,
    },
    platform::{PlatformContext, PlatformError, RuntimeScope},
    verifier::{
        verify_with_context, VerificationContext, VerificationError, VerificationProfile,
        VerifierSpec,
    },
};

pub use self::types::{
    CommandErrorEnvelope, CommandErrorResponse, CommandName, CommandPayload, CommandRequest,
    CommandResponse, InstallMode, InstallWorkflowFailure, InstallWorkflowOutcome,
    InstallWorkflowResult, InstallWorkflowStatus, InterfaceMode, OutputFormat,
    VerificationItemResult, VerificationWorkflowResult, VerificationWorkflowSummary,
};

const CATALOG_PATH_ENV: &str = "ENVIRA_CATALOG_PATH";
pub const CURRENT_VERSION_ENV: &str = "ENVIRA_CURRENT_VERSION";

#[derive(Debug, Error)]
pub enum EngineError {
    #[error("{command:?} is not available in {mode:?} mode")]
    UnsupportedInterface {
        command: CommandName,
        mode: InterfaceMode,
    },
    #[error("failed to read catalog manifest from `{path}`: {source}")]
    ReadCatalogManifest {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{source}")]
    LoadCatalog {
        manifest_path: Option<PathBuf>,
        source: CatalogError,
    },
    #[error("catalog required_version `{required_version}` is invalid: {reason}")]
    InvalidRequiredVersion {
        required_version: String,
        reason: String,
    },
    #[error(
        "catalog required_version `{required_version}` uses unsupported prerelease semantics; use a stable major.minor.patch version"
    )]
    UnsupportedRequiredVersionPrerelease { required_version: String },
    #[error("envira binary version `{current_version}` is invalid: {reason}")]
    InvalidBinaryVersion {
        current_version: String,
        reason: String,
    },
    #[error(
        "envira binary version `{current_version}` uses unsupported prerelease semantics; prerelease binaries cannot satisfy catalog minimum versions"
    )]
    UnsupportedBinaryPrerelease { current_version: String },
    #[error(
        "envira {current_version} is older than catalog minimum {required_version}; run the approved update flow before continuing"
    )]
    UpdateRequired {
        current_version: String,
        required_version: String,
    },
    #[error(
        "envira {current_version} is older than catalog minimum {required_version}, and the approved update flow failed: {detail}"
    )]
    AutoUpdateFailed {
        current_version: String,
        required_version: String,
        updater: String,
        detail: String,
        exit_code: Option<i32>,
    },
    #[error(transparent)]
    DetectPlatform(#[from] PlatformError),
    #[error(transparent)]
    BuildPlan(#[from] PlannerError),
    #[error(transparent)]
    ClassifyPlan(#[from] ActionPlanError),
    #[error(transparent)]
    Verify(#[from] VerificationError),
    #[error(transparent)]
    BuildExecutionPlan(#[from] ExecutionPlanError),
    #[error("planned item `{item_id}` is missing from the loaded catalog")]
    MissingCatalogItem { item_id: String },
}

impl EngineError {
    pub fn into_envelope(self) -> CommandErrorEnvelope {
        let code = self.code().to_string();
        let message = self.to_string();
        let context = self.context();

        CommandErrorEnvelope {
            code,
            message,
            context,
        }
    }

    fn code(&self) -> &'static str {
        match self {
            Self::UnsupportedInterface { .. } => "unsupported_interface",
            Self::ReadCatalogManifest { .. } => "catalog_read_failed",
            Self::LoadCatalog { .. } => "catalog_invalid",
            Self::InvalidRequiredVersion { .. } => "catalog_required_version_invalid",
            Self::UnsupportedRequiredVersionPrerelease { .. } => {
                "catalog_required_version_prerelease_unsupported"
            }
            Self::InvalidBinaryVersion { .. } => "envira_binary_version_invalid",
            Self::UnsupportedBinaryPrerelease { .. } => "envira_binary_prerelease_unsupported",
            Self::UpdateRequired { .. } => "envira_update_required",
            Self::AutoUpdateFailed { .. } => "envira_auto_update_failed",
            Self::DetectPlatform(_) => "platform_detect_failed",
            Self::BuildPlan(_) => "planning_failed",
            Self::ClassifyPlan(_) => "action_classification_failed",
            Self::Verify(_) => "verification_failed",
            Self::BuildExecutionPlan(_) => "execution_planning_failed",
            Self::MissingCatalogItem { .. } => "catalog_item_missing",
        }
    }

    fn context(&self) -> BTreeMap<String, String> {
        let mut context = BTreeMap::new();

        match self {
            Self::UnsupportedInterface { command, mode } => {
                context.insert("command".to_string(), command.as_str().to_string());
                context.insert("mode".to_string(), format!("{mode:?}").to_ascii_lowercase());
            }
            Self::ReadCatalogManifest { path, .. } => {
                context.insert("manifest_path".to_string(), path.display().to_string());
            }
            Self::LoadCatalog { manifest_path, .. } => {
                if let Some(path) = manifest_path {
                    context.insert("manifest_path".to_string(), path.display().to_string());
                } else {
                    context.insert("manifest_path".to_string(), "embedded".to_string());
                }
            }
            Self::InvalidRequiredVersion {
                required_version,
                reason,
            } => {
                context.insert("required_version".to_string(), required_version.clone());
                context.insert("reason".to_string(), reason.clone());
            }
            Self::UnsupportedRequiredVersionPrerelease { required_version } => {
                context.insert("required_version".to_string(), required_version.clone());
            }
            Self::InvalidBinaryVersion {
                current_version,
                reason,
            } => {
                context.insert("current_version".to_string(), current_version.clone());
                context.insert("reason".to_string(), reason.clone());
            }
            Self::UnsupportedBinaryPrerelease { current_version } => {
                context.insert("current_version".to_string(), current_version.clone());
            }
            Self::UpdateRequired {
                current_version,
                required_version,
            } => {
                context.insert("current_version".to_string(), current_version.clone());
                context.insert("required_version".to_string(), required_version.clone());
            }
            Self::AutoUpdateFailed {
                current_version,
                required_version,
                updater,
                detail,
                exit_code,
            } => {
                context.insert("current_version".to_string(), current_version.clone());
                context.insert("required_version".to_string(), required_version.clone());
                context.insert("updater".to_string(), updater.clone());
                context.insert("detail".to_string(), detail.clone());
                if let Some(exit_code) = exit_code {
                    context.insert("exit_code".to_string(), exit_code.to_string());
                }
            }
            Self::MissingCatalogItem { item_id } => {
                context.insert("item_id".to_string(), item_id.clone());
            }
            Self::DetectPlatform(_)
            | Self::BuildPlan(_)
            | Self::ClassifyPlan(_)
            | Self::Verify(_)
            | Self::BuildExecutionPlan(_) => {}
        }

        context
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VersionGate {
    NotApplicable,
    Satisfied,
    UpdateRequired {
        current_version: String,
        required_version: String,
    },
}

#[derive(Debug, Default)]
pub struct Engine;

impl Engine {
    pub fn execute(&self, request: CommandRequest) -> Result<CommandResponse, EngineError> {
        match request.command {
            CommandName::Catalog => self.execute_catalog(request),
            CommandName::Plan => self.execute_plan(request),
            CommandName::Install => self.execute_install(request),
            CommandName::Verify => self.execute_verify(request),
            CommandName::Tui => Err(EngineError::UnsupportedInterface {
                command: request.command,
                mode: request.mode,
            }),
        }
    }

    fn execute_catalog(&self, request: CommandRequest) -> Result<CommandResponse, EngineError> {
        let catalog = self.load_catalog()?;

        Ok(CommandResponse::success(
            request.command,
            request.mode,
            request.format,
            CommandPayload::Catalog { catalog },
        ))
    }

    fn execute_plan(&self, request: CommandRequest) -> Result<CommandResponse, EngineError> {
        let workflow = self.prepare_workflow(&request)?;

        Ok(CommandResponse::success(
            request.command,
            request.mode,
            request.format,
            CommandPayload::Plan {
                action_plan: workflow.action_plan,
            },
        ))
    }

    fn execute_verify(&self, request: CommandRequest) -> Result<CommandResponse, EngineError> {
        let workflow = self.prepare_workflow(&request)?;

        Ok(CommandResponse::success(
            request.command,
            request.mode,
            request.format,
            CommandPayload::Verify {
                verification: workflow.verification,
            },
        ))
    }

    fn execute_install(&self, request: CommandRequest) -> Result<CommandResponse, EngineError> {
        let workflow = self.prepare_workflow(&request)?;
        let execution_plan =
            build_execution_plan(&workflow.catalog, &workflow.platform, &workflow.action_plan)?;
        let execution = match request.install_mode {
            InstallMode::Apply => execute_execution_plan(&execution_plan),
            InstallMode::DryRun => dry_run_execution_report(&execution_plan),
        };
        let post_verification = self.verify_install_plan(
            &workflow.catalog,
            &workflow.platform,
            &workflow.install_plan,
            workflow.verification_profile,
        )?;
        let outcome = summarize_install_outcome(
            &workflow.action_plan,
            &execution,
            &post_verification,
            request.install_mode,
        );

        Ok(CommandResponse::success(
            request.command,
            request.mode,
            request.format,
            CommandPayload::Install {
                install: InstallWorkflowResult {
                    install_mode: request.install_mode,
                    action_plan: workflow.action_plan,
                    execution_plan,
                    execution,
                    post_verification,
                    outcome,
                },
            },
        ))
    }

    pub fn assess_version_gate(
        &self,
        request: &CommandRequest,
    ) -> Result<VersionGate, EngineError> {
        if !matches!(
            request.command,
            CommandName::Plan | CommandName::Install | CommandName::Verify | CommandName::Tui
        ) {
            return Ok(VersionGate::NotApplicable);
        }

        let catalog = self.load_catalog()?;
        let required_version_raw = catalog.required_version.trim().to_string();
        let current_version_raw = env::var(CURRENT_VERSION_ENV)
            .unwrap_or_else(|_| env!("CARGO_PKG_VERSION").to_string())
            .trim()
            .to_string();
        let required_version =
            parse_stable_version(required_version_raw.as_str(), VersionField::CatalogRequired)?;
        let current_version =
            parse_stable_version(current_version_raw.as_str(), VersionField::CurrentBinary)?;

        if current_version >= required_version {
            Ok(VersionGate::Satisfied)
        } else {
            Ok(VersionGate::UpdateRequired {
                current_version: current_version_raw,
                required_version: required_version_raw,
            })
        }
    }

    fn prepare_workflow(&self, request: &CommandRequest) -> Result<PreparedWorkflow, EngineError> {
        if let VersionGate::UpdateRequired {
            current_version,
            required_version,
        } = self.assess_version_gate(request)?
        {
            return Err(EngineError::UpdateRequired {
                current_version,
                required_version,
            });
        }

        let catalog = self.load_catalog()?;
        let platform = PlatformContext::detect()?;
        let planner_request = request.resolved_planner_request();
        let verification_profile = request.resolved_verification_profile();
        let install_plan = self.build_workflow_install_plan(
            request.command,
            &catalog,
            &platform,
            &planner_request,
            request.install_target,
        )?;
        let verification =
            self.verify_install_plan(&catalog, &platform, &install_plan, verification_profile)?;
        let verifier_results = verification_results_by_item(&verification);
        let action_plan = classify_install_plan(&install_plan, &verifier_results)?;

        Ok(PreparedWorkflow {
            catalog,
            platform,
            install_plan,
            verification_profile,
            verification,
            action_plan,
        })
    }

    fn build_workflow_install_plan(
        &self,
        command: CommandName,
        catalog: &Catalog,
        platform: &PlatformContext,
        planner_request: &crate::planner::PlannerRequest,
        install_target: crate::planner::InstallTargetPreference,
    ) -> Result<InstallPlan, EngineError> {
        match build_install_plan_with_target(catalog, platform, planner_request, install_target) {
            Ok(install_plan) => Ok(install_plan),
            Err(PlannerError::UnsupportedScope { .. }) if command == CommandName::Verify => {
                let mut verification_platform = platform.clone();
                verification_platform.runtime_scope = RuntimeScope::Both;
                build_install_plan(catalog, &verification_platform, planner_request)
                    .map_err(EngineError::from)
            }
            Err(error) => Err(error.into()),
        }
    }

    fn load_catalog(&self) -> Result<Catalog, EngineError> {
        if let Some(path) = env::var_os(CATALOG_PATH_ENV).map(PathBuf::from) {
            return load_catalog_from_path(&path);
        }

        load_catalog_from_manifest(embedded_manifest(), None)
    }

    fn verify_install_plan(
        &self,
        catalog: &Catalog,
        platform: &PlatformContext,
        install_plan: &InstallPlan,
        verification_profile: VerificationProfile,
    ) -> Result<VerificationWorkflowResult, EngineError> {
        let context = VerificationContext::new(platform.clone(), verification_profile);
        let mut results = Vec::with_capacity(install_plan.steps.len());

        for step in &install_plan.steps {
            let item = catalog.item(step.item_id.as_str()).ok_or_else(|| {
                EngineError::MissingCatalogItem {
                    item_id: step.item_id.clone(),
                }
            })?;
            let verifier = VerifierSpec::from_catalog_commands(&item.verifiers);
            let verification_run = verify_with_context(step.required_stage, &verifier, &context)?;

            results.push(VerificationItemResult {
                step: step.clone(),
                result: verification_run.result,
            });
        }

        Ok(VerificationWorkflowResult {
            request: install_plan.request.clone(),
            profile: verification_profile,
            platform: platform.clone(),
            summary: summarize_verification(&results),
            results,
        })
    }
}

#[derive(Debug)]
struct PreparedWorkflow {
    catalog: Catalog,
    platform: PlatformContext,
    install_plan: InstallPlan,
    verification_profile: VerificationProfile,
    verification: VerificationWorkflowResult,
    action_plan: ActionPlan,
}

fn load_catalog_from_path(path: &Path) -> Result<Catalog, EngineError> {
    let raw_manifest =
        fs::read_to_string(path).map_err(|source| EngineError::ReadCatalogManifest {
            path: path.to_path_buf(),
            source,
        })?;

    load_catalog_from_manifest(&raw_manifest, Some(path))
}

fn load_catalog_from_manifest(
    raw_manifest: &str,
    manifest_path: Option<&Path>,
) -> Result<Catalog, EngineError> {
    Catalog::from_toml_str(raw_manifest).map_err(|source| EngineError::LoadCatalog {
        manifest_path: manifest_path.map(Path::to_path_buf),
        source,
    })
}

fn summarize_verification(results: &[VerificationItemResult]) -> VerificationWorkflowSummary {
    let threshold_met_steps = results
        .iter()
        .filter(|result| result.result.threshold_met)
        .count();

    VerificationWorkflowSummary {
        total_steps: results.len(),
        threshold_met_steps,
        threshold_unmet_steps: results.len().saturating_sub(threshold_met_steps),
    }
}

fn verification_results_by_item(
    verification: &VerificationWorkflowResult,
) -> BTreeMap<String, crate::verifier::VerifierResult> {
    verification
        .results
        .iter()
        .map(|result| (result.step.item_id.clone(), result.result.clone()))
        .collect()
}

fn summarize_install_outcome(
    action_plan: &ActionPlan,
    execution: &crate::executor::ExecutionPlanReport,
    post_verification: &VerificationWorkflowResult,
    install_mode: InstallMode,
) -> InstallWorkflowOutcome {
    let execution_by_item = execution
        .steps
        .iter()
        .map(|step| (step.step.action_step.step.item_id.clone(), step.disposition))
        .collect::<BTreeMap<_, _>>();

    let mut failures = Vec::new();
    let mut actionable_steps = 0usize;
    let mut blocked_steps = 0usize;
    let mut threshold_met_steps = 0usize;

    for step in &action_plan.steps {
        if step.action == PlannedAction::Blocked {
            blocked_steps += 1;
            continue;
        }

        actionable_steps += 1;

        if let Some(result) = post_verification.result_for(step.step.item_id.as_str()) {
            if result.result.threshold_met {
                threshold_met_steps += 1;
            } else {
                failures.push(InstallWorkflowFailure {
                    item_id: step.step.item_id.clone(),
                    action: step.action,
                    execution_disposition: execution_by_item
                        .get(step.step.item_id.as_str())
                        .copied()
                        .unwrap_or(ExecutionDisposition::Skipped),
                    verifier: result.result.clone(),
                });
            }
        }
    }

    let status = if install_mode == InstallMode::DryRun {
        InstallWorkflowStatus::DryRun
    } else if !failures.is_empty() {
        InstallWorkflowStatus::VerificationFailed
    } else if blocked_steps > 0 {
        InstallWorkflowStatus::Blocked
    } else {
        InstallWorkflowStatus::Success
    };

    InstallWorkflowOutcome {
        status,
        execution_succeeded: execution.summary.failed_steps == 0,
        actionable_steps,
        blocked_steps,
        threshold_met_steps,
        failures,
    }
}

fn dry_run_execution_report(plan: &crate::executor::ExecutionPlan) -> ExecutionPlanReport {
    let steps = plan
        .steps
        .iter()
        .map(|step| {
            let disposition = match step.action_step.action {
                PlannedAction::Install | PlannedAction::Repair => ExecutionDisposition::Skipped,
                PlannedAction::Skip | PlannedAction::Blocked => ExecutionDisposition::Skipped,
            };
            let operations = step
                .operations
                .iter()
                .cloned()
                .map(|operation| OperationExecutionReport {
                    operation,
                    state: OperationState::Skipped,
                    command: None,
                    message: Some("Skipped because install mode is dry_run.".to_string()),
                })
                .collect::<Vec<_>>();
            let message = match step.action_step.action {
                PlannedAction::Install | PlannedAction::Repair => format!(
                    "Dry run skipped {} operation(s) for `{}`.",
                    step.operations.len(),
                    step.action_step.step.item_id
                ),
                PlannedAction::Skip | PlannedAction::Blocked => {
                    step.action_step.rationale.summary.clone()
                }
            };

            ExecutionStepReport {
                step: step.clone(),
                disposition,
                message,
                operations,
            }
        })
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
    let skipped_steps = steps.len();

    ExecutionPlanReport {
        summary: ExecutionPlanSummary {
            total_steps: steps.len(),
            actionable_steps,
            successful_steps: 0,
            failed_steps: 0,
            skipped_steps,
        },
        steps,
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct StableVersion {
    major: u64,
    minor: u64,
    patch: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum VersionField {
    CatalogRequired,
    CurrentBinary,
}

fn parse_stable_version(raw: &str, field: VersionField) -> Result<StableVersion, EngineError> {
    let value = raw.trim();

    if value.is_empty() {
        return Err(field.invalid_version("expected `major.minor.patch`".to_string(), value));
    }

    if value.contains('-') {
        return Err(field.unsupported_prerelease(value));
    }

    if value.contains('+') {
        return Err(field.invalid_version(
            "build metadata is not supported; expected `major.minor.patch`".to_string(),
            value,
        ));
    }

    let parts = value.split('.').collect::<Vec<_>>();
    if parts.len() != 3 {
        return Err(field.invalid_version("expected `major.minor.patch`".to_string(), value));
    }

    Ok(StableVersion {
        major: parse_version_component(parts[0], "major", field, value)?,
        minor: parse_version_component(parts[1], "minor", field, value)?,
        patch: parse_version_component(parts[2], "patch", field, value)?,
    })
}

fn parse_version_component(
    component: &str,
    name: &str,
    field: VersionField,
    original: &str,
) -> Result<u64, EngineError> {
    if component.is_empty() {
        return Err(field.invalid_version(
            format!("missing {name} version component; expected `major.minor.patch`"),
            original,
        ));
    }

    if !component
        .chars()
        .all(|character| character.is_ascii_digit())
    {
        return Err(field.invalid_version(
            format!("{name} version component must contain only ASCII digits"),
            original,
        ));
    }

    if component.len() > 1 && component.starts_with('0') {
        return Err(field.invalid_version(
            format!("{name} version component must not contain leading zeroes"),
            original,
        ));
    }

    component.parse::<u64>().map_err(|_| {
        field.invalid_version(
            format!("{name} version component is too large to compare"),
            original,
        )
    })
}

impl VersionField {
    fn invalid_version(self, reason: String, version: &str) -> EngineError {
        match self {
            Self::CatalogRequired => EngineError::InvalidRequiredVersion {
                required_version: version.to_string(),
                reason,
            },
            Self::CurrentBinary => EngineError::InvalidBinaryVersion {
                current_version: version.to_string(),
                reason,
            },
        }
    }

    fn unsupported_prerelease(self, version: &str) -> EngineError {
        match self {
            Self::CatalogRequired => EngineError::UnsupportedRequiredVersionPrerelease {
                required_version: version.to_string(),
            },
            Self::CurrentBinary => EngineError::UnsupportedBinaryPrerelease {
                current_version: version.to_string(),
            },
        }
    }
}
