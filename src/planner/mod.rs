use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;

mod action;

use crate::catalog::{
    Catalog, CatalogCommand, CatalogError, CatalogItem, CommandMode, InstallScope, InstallTarget,
    TargetBackend,
};
use crate::platform::{PlatformContext, RuntimeScope};
use crate::verifier::{required_stage_for_catalog_commands, VerificationStage};

pub use self::action::{
    classify_install_plan, ActionPlan, ActionPlanError, ActionPlanStep, ActionRationale,
    ActionReasonCode, BlockedDependency, PlannedAction,
};

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct PlannerRequest {
    pub selections: Vec<PlanSelection>,
}

impl PlannerRequest {
    pub fn new(selections: Vec<PlanSelection>) -> Self {
        Self { selections }
    }

    pub fn item(id: impl Into<String>) -> Self {
        Self::new(vec![PlanSelection::item(id)])
    }

    pub fn bundle(id: impl Into<String>) -> Self {
        Self::new(vec![PlanSelection::bundle(id)])
    }

    pub fn all_items() -> Self {
        Self::new(vec![PlanSelection::AllItems])
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PlanSelection {
    Item { id: String },
    Bundle { id: String },
    AllItems,
}

impl PlanSelection {
    pub fn item(id: impl Into<String>) -> Self {
        Self::Item { id: id.into() }
    }

    pub fn bundle(id: impl Into<String>) -> Self {
        Self::Bundle { id: id.into() }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct InstallPlan {
    pub request: PlannerRequest,
    pub platform: PlanPlatformSnapshot,
    pub steps: Vec<PlanStep>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PlanPlatformSnapshot {
    pub native_backend: Option<TargetBackend>,
    pub runtime_scope: RuntimeScope,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PlanStep {
    pub item_id: String,
    pub display_name: String,
    pub requested: bool,
    pub depends_on: Vec<String>,
    pub catalog_scope: InstallScope,
    pub planned_scope: PlannedScope,
    pub selected_target: InstallTarget,
    pub required_stage: VerificationStage,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlannedScope {
    System,
    User,
}

#[derive(Debug, Error)]
pub enum PlannerError {
    #[error(transparent)]
    Catalog(#[from] CatalogError),
    #[error("requested item `{item_id}` is not defined in the catalog")]
    UnknownItem { item_id: String },
    #[error("requested bundle `{bundle_id}` is not defined in the catalog")]
    UnknownBundle { bundle_id: String },
    #[error(
        "item `{item_id}` has no supported {contract_kind} for distro `{distro_id}` in runtime scope `{runtime_scope:?}`; available coverage: {available_coverage:?}"
    )]
    UnsupportedCoverage {
        item_id: String,
        contract_kind: String,
        distro_id: String,
        runtime_scope: RuntimeScope,
        available_coverage: Vec<String>,
    },
    #[error(
        "item `{item_id}` has no supported target for native backend `{native_backend:?}`; available targets: {available_targets:?}"
    )]
    UnsupportedTarget {
        item_id: String,
        native_backend: Option<TargetBackend>,
        available_targets: Vec<InstallTarget>,
    },
    #[error(
        "item `{item_id}` with catalog scope `{item_scope:?}` is not supported in runtime scope `{runtime_scope:?}`"
    )]
    UnsupportedScope {
        item_id: String,
        item_scope: InstallScope,
        runtime_scope: RuntimeScope,
    },
    #[error("dependency cycle detected: {cycle:?}")]
    DependencyCycle { cycle: Vec<String> },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum VisitState {
    Visiting,
    Visited,
}

pub fn build_install_plan(
    catalog: &Catalog,
    platform: &PlatformContext,
    request: &PlannerRequest,
) -> Result<InstallPlan, PlannerError> {
    let requested_items = expand_requested_items(catalog, request)?;
    let requested_ids = requested_items
        .iter()
        .map(|item| item.id.as_str().to_string())
        .collect::<BTreeSet<_>>();
    let mut states = BTreeMap::new();
    let mut stack = Vec::new();
    let mut steps = Vec::new();

    for item in requested_items {
        visit_item(
            catalog,
            platform,
            item,
            &requested_ids,
            &mut states,
            &mut stack,
            &mut steps,
        )?;
    }

    Ok(InstallPlan {
        request: request.clone(),
        platform: PlanPlatformSnapshot {
            native_backend: platform.native_backend,
            runtime_scope: platform.runtime_scope,
        },
        steps,
    })
}

fn expand_requested_items<'a>(
    catalog: &'a Catalog,
    request: &PlannerRequest,
) -> Result<Vec<&'a CatalogItem>, PlannerError> {
    let mut items = Vec::new();
    let mut requested_item_ids = BTreeSet::new();

    if request.selections.is_empty() {
        for item in catalog.expand_default_bundles()? {
            if requested_item_ids.insert(item.id.as_str().to_string()) {
                items.push(item);
            }
        }
    } else {
        for selection in &request.selections {
            match selection {
                PlanSelection::Item { id } => {
                    let item = catalog.item(id).ok_or_else(|| PlannerError::UnknownItem {
                        item_id: id.clone(),
                    })?;

                    if requested_item_ids.insert(id.clone()) {
                        items.push(item);
                    }
                }
                PlanSelection::Bundle { id } => {
                    let bundle = catalog
                        .bundle(id)
                        .ok_or_else(|| PlannerError::UnknownBundle {
                            bundle_id: id.clone(),
                        })?;

                    for item_id in &bundle.items {
                        let item_id = item_id.as_str().to_string();
                        if !requested_item_ids.insert(item_id.clone()) {
                            continue;
                        }

                        let item = catalog.item(item_id.as_str()).ok_or_else(|| {
                            PlannerError::UnknownItem {
                                item_id: item_id.clone(),
                            }
                        })?;
                        items.push(item);
                    }
                }
                PlanSelection::AllItems => {
                    for item in &catalog.items {
                        if requested_item_ids.insert(item.id.as_str().to_string()) {
                            items.push(item);
                        }
                    }
                }
            }
        }
    }

    Ok(items)
}

fn visit_item(
    catalog: &Catalog,
    platform: &PlatformContext,
    item: &CatalogItem,
    requested_ids: &BTreeSet<String>,
    states: &mut BTreeMap<String, VisitState>,
    stack: &mut Vec<String>,
    steps: &mut Vec<PlanStep>,
) -> Result<(), PlannerError> {
    let item_id = item.id.as_str().to_string();

    match states.get(item_id.as_str()) {
        Some(VisitState::Visited) => return Ok(()),
        Some(VisitState::Visiting) => {
            return Err(PlannerError::DependencyCycle {
                cycle: cycle_from_stack(stack, item_id.as_str()),
            });
        }
        None => {}
    }

    states.insert(item_id.clone(), VisitState::Visiting);
    stack.push(item_id.clone());

    for dependency_id in &item.depends_on {
        let dependency =
            catalog
                .item(dependency_id.as_str())
                .ok_or_else(|| PlannerError::UnknownItem {
                    item_id: dependency_id.as_str().to_string(),
                })?;
        visit_item(
            catalog,
            platform,
            dependency,
            requested_ids,
            states,
            stack,
            steps,
        )?;
    }

    let supported_recipes =
        supported_commands(item, &item.recipes, "recipes", platform, None, true)?;
    let available_targets =
        compatible_targets_for_commands(&supported_recipes, platform.distro.id.as_str());
    let selected_target = select_target(
        item.id.as_str(),
        &available_targets,
        platform.native_backend,
    )?;
    let item_scope = scope_for_commands(&supported_recipes);
    let planned_scope = resolve_scope(
        item.id.as_str(),
        item_scope,
        platform.runtime_scope,
        selected_target.backend,
    )?;
    supported_commands(
        item,
        &item.verifiers,
        "verifiers",
        platform,
        Some(planned_scope),
        false,
    )?;

    steps.push(PlanStep {
        item_id: item_id.clone(),
        display_name: item.name.clone(),
        requested: requested_ids.contains(item_id.as_str()),
        depends_on: item
            .depends_on
            .iter()
            .map(|dependency| dependency.as_str().to_string())
            .collect(),
        catalog_scope: item.install_scope(),
        planned_scope,
        selected_target,
        required_stage: required_stage_for_catalog_commands(&item.verifiers),
    });

    stack.pop();
    states.insert(item_id, VisitState::Visited);

    Ok(())
}

fn cycle_from_stack(stack: &[String], repeated_id: &str) -> Vec<String> {
    let start_index = stack
        .iter()
        .position(|item_id| item_id == repeated_id)
        .unwrap_or(0);
    let mut cycle = stack[start_index..].to_vec();
    cycle.push(repeated_id.to_string());
    cycle
}

fn supported_commands<'a>(
    item: &CatalogItem,
    commands: &'a [CatalogCommand],
    contract_kind: &str,
    platform: &PlatformContext,
    required_scope: Option<PlannedScope>,
    allow_unknown_distro_fallback: bool,
) -> Result<Vec<&'a CatalogCommand>, PlannerError> {
    let supported = commands
        .iter()
        .filter(|command| command_supports_distro(command, platform.distro.id.as_str()))
        .filter(|command| {
            required_scope
                .map(|scope| command_supports_scope(command, scope))
                .unwrap_or(true)
        })
        .collect::<Vec<_>>();

    if supported.is_empty() && allow_unknown_distro_fallback && platform.distro.id == "unknown" {
        return Ok(commands.iter().collect());
    }

    if supported.is_empty() {
        return Err(PlannerError::UnsupportedCoverage {
            item_id: item.id.as_str().to_string(),
            contract_kind: contract_kind.to_string(),
            distro_id: platform.distro.id.clone(),
            runtime_scope: platform.runtime_scope,
            available_coverage: commands.iter().map(command_coverage_summary).collect(),
        });
    }

    Ok(supported)
}

fn command_supports_distro(command: &CatalogCommand, distro_id: &str) -> bool {
    command
        .distros
        .iter()
        .any(|distro| distro == "*" || distro == distro_id)
}

fn command_supports_scope(command: &CatalogCommand, planned_scope: PlannedScope) -> bool {
    matches!(
        (command.mode, planned_scope),
        (CommandMode::Sudo, PlannedScope::System) | (CommandMode::User, PlannedScope::User)
    )
}

fn command_coverage_summary(command: &CatalogCommand) -> String {
    format!(
        "mode={} distros={:?}",
        command.mode.as_str(),
        command.distros
    )
}

fn scope_for_commands(commands: &[&CatalogCommand]) -> InstallScope {
    let has_user = commands
        .iter()
        .any(|command| command.mode == CommandMode::User);
    let has_sudo = commands
        .iter()
        .any(|command| command.mode == CommandMode::Sudo);

    match (has_user, has_sudo) {
        (true, true) => InstallScope::Hybrid,
        (true, false) => InstallScope::User,
        _ => InstallScope::System,
    }
}

fn compatible_targets_for_commands(
    commands: &[&CatalogCommand],
    distro_id: &str,
) -> Vec<InstallTarget> {
    let mut targets = Vec::new();
    let mut seen = BTreeSet::new();

    for command in commands {
        for target in compatible_targets_for_command(command, distro_id) {
            let key = (target.backend, target.source);
            if seen.insert(key) {
                targets.push(target);
            }
        }
    }

    targets
}

fn compatible_targets_for_command(command: &CatalogCommand, distro_id: &str) -> Vec<InstallTarget> {
    match command.mode {
        CommandMode::User => vec![InstallTarget::generic_user()],
        CommandMode::Sudo => {
            if command_supports_distro(command, distro_id) {
                native_targets_for_distro(distro_id)
            } else {
                command
                    .distros
                    .iter()
                    .flat_map(|declared_distro| native_targets_for_distro(declared_distro))
                    .collect()
            }
        }
    }
}

fn native_targets_for_distro(distro_id: &str) -> Vec<InstallTarget> {
    native_backend_for_distro(distro_id)
        .map(native_install_target)
        .into_iter()
        .collect()
}

fn native_backend_for_distro(distro_id: &str) -> Option<TargetBackend> {
    match distro_id {
        "ubuntu" | "debian" | "mint" | "linuxmint" => Some(TargetBackend::Apt),
        "arch" | "archlinux" | "manjaro" => Some(TargetBackend::Pacman),
        "fedora" | "rhel" | "redhat" | "centos" => Some(TargetBackend::Dnf),
        "opensuse" | "opensuse-leap" | "opensuse-tumbleweed" | "sles" => {
            Some(TargetBackend::Zypper)
        }
        _ => None,
    }
}

fn native_install_target(backend: TargetBackend) -> InstallTarget {
    InstallTarget::native_package(backend)
}

fn resolve_scope(
    item_id: &str,
    item_scope: InstallScope,
    runtime_scope: RuntimeScope,
    selected_backend: TargetBackend,
) -> Result<PlannedScope, PlannerError> {
    match (item_scope, runtime_scope) {
        (InstallScope::System, RuntimeScope::System | RuntimeScope::Both) => {
            Ok(PlannedScope::System)
        }
        (InstallScope::User, RuntimeScope::User | RuntimeScope::Both) => Ok(PlannedScope::User),
        (InstallScope::Hybrid, RuntimeScope::System | RuntimeScope::Both) => {
            Ok(PlannedScope::System)
        }
        (InstallScope::Hybrid, RuntimeScope::User) => {
            if matches!(
                selected_backend,
                TargetBackend::Apt
                    | TargetBackend::Pacman
                    | TargetBackend::Dnf
                    | TargetBackend::Zypper
            ) {
                Ok(PlannedScope::System)
            } else {
                Ok(PlannedScope::User)
            }
        }
        _ => Err(PlannerError::UnsupportedScope {
            item_id: item_id.to_string(),
            item_scope,
            runtime_scope,
        }),
    }
}

fn select_target(
    item_id: &str,
    targets: &[InstallTarget],
    native_backend: Option<TargetBackend>,
) -> Result<InstallTarget, PlannerError> {
    let mut selected = None;

    for (index, target) in targets.iter().enumerate() {
        if !target_is_supported(target, native_backend) {
            continue;
        }

        let priority = if Some(target.backend) == native_backend {
            (0usize, index)
        } else {
            (1usize, index)
        };

        match &selected {
            Some((current_priority, _)) if *current_priority <= priority => {}
            _ => selected = Some((priority, target.clone())),
        }
    }

    selected
        .map(|(_, target)| target)
        .ok_or_else(|| PlannerError::UnsupportedTarget {
            item_id: item_id.to_string(),
            native_backend,
            available_targets: targets.to_vec(),
        })
}

fn target_is_supported(target: &InstallTarget, native_backend: Option<TargetBackend>) -> bool {
    match target.backend {
        TargetBackend::Apt | TargetBackend::Pacman | TargetBackend::Dnf | TargetBackend::Zypper => {
            native_backend == Some(target.backend)
        }
        TargetBackend::DirectBinary | TargetBackend::Archive | TargetBackend::SourceBuild => true,
    }
}
