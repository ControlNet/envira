use envira::catalog::{load_embedded_catalog, Catalog, CatalogItem, ItemCategory};
use envira::verifier::{ProbeRequirement, ServiceKind, VerificationProfile, VerificationStage};

#[test]
fn embedded_launch_catalog_has_semantically_complete_verifier_coverage() {
    let catalog = load_embedded_catalog().expect("embedded catalog should load");
    let violations = collect_semantic_violations(&catalog);

    assert!(
        violations.is_empty(),
        "launch catalog semantic verifier coverage violations:\n{}",
        violations.join("\n")
    );
}

#[test]
fn semantic_coverage_guard_reports_all_structurally_valid_incomplete_items() {
    let catalog = Catalog::from_toml_str(SEMANTIC_NEGATIVE_FIXTURE)
        .expect("negative fixture should remain structurally valid");
    let violations = collect_semantic_violations(&catalog);

    assert_eq!(
        violations,
        vec![
            "item `quick-gap`: missing required quick-profile verifier check".to_string(),
            "item `service-gap`: service-like launch contract is missing verifier.service metadata"
                .to_string(),
            "item `threshold-gap`: success_threshold `configured` is unreachable in quick profile (max reachable: `present`)"
                .to_string(),
        ]
    );
}

#[test]
fn embedded_launch_catalog_service_items_keep_operational_service_metadata() {
    let catalog = load_embedded_catalog().expect("embedded catalog should load");

    for (item_id, expected_kind) in [
        ("docker", ServiceKind::Docker),
        ("jupyter", ServiceKind::Jupyter),
        ("pm2", ServiceKind::Pm2),
        ("vnc", ServiceKind::Vnc),
    ] {
        let item = catalog.item(item_id).expect("service item should exist");
        assert_eq!(item.success_threshold, VerificationStage::Operational);
        assert_eq!(
            item.verifier.service.as_ref().map(|service| service.kind),
            Some(expected_kind),
            "{item_id} should keep machine-readable service metadata"
        );
    }
}

fn collect_semantic_violations(catalog: &Catalog) -> Vec<String> {
    let mut violations = catalog
        .items
        .iter()
        .flat_map(item_semantic_violations)
        .collect::<Vec<_>>();
    violations.sort();
    violations
}

fn item_semantic_violations(item: &CatalogItem) -> Vec<String> {
    let mut violations = Vec::new();

    if !has_required_quick_check(item) {
        violations.push(format!(
            "item `{}`: missing required quick-profile verifier check",
            item.id
        ));
    }

    if requires_service_metadata(item) && item.verifier.service.is_none() {
        violations.push(format!(
            "item `{}`: service-like launch contract is missing verifier.service metadata",
            item.id
        ));
    }

    let max_reachable = max_quick_reachable_stage(item);
    if !max_reachable.is_some_and(|stage| stage.meets(item.success_threshold)) {
        violations.push(format!(
            "item `{}`: success_threshold `{}` is unreachable in quick profile (max reachable: `{}`)",
            item.id,
            stage_name(item.success_threshold),
            max_reachable.map(stage_name).unwrap_or("none")
        ));
    }

    violations
}

fn has_required_quick_check(item: &CatalogItem) -> bool {
    item.verifier.checks.iter().any(|check| {
        check.requirement == ProbeRequirement::Required
            && check.min_profile == VerificationProfile::Quick
    })
}

fn max_quick_reachable_stage(item: &CatalogItem) -> Option<VerificationStage> {
    let check_stage = item
        .verifier
        .checks
        .iter()
        .filter(|check| {
            check.requirement == ProbeRequirement::Required
                && check.min_profile == VerificationProfile::Quick
        })
        .map(|check| check.stage)
        .max();
    let service_stage = item
        .verifier
        .service
        .as_ref()
        .map(|_| VerificationStage::Operational);

    match (check_stage, service_stage) {
        (Some(left), Some(right)) => Some(left.max(right)),
        (Some(stage), None) => Some(stage),
        (None, Some(stage)) => Some(stage),
        (None, None) => None,
    }
}

fn requires_service_metadata(item: &CatalogItem) -> bool {
    item.category == ItemCategory::RemoteAccess
        || item.success_threshold == VerificationStage::Operational
}

fn stage_name(stage: VerificationStage) -> &'static str {
    match stage {
        VerificationStage::Present => "present",
        VerificationStage::Configured => "configured",
        VerificationStage::Operational => "operational",
    }
}

const SEMANTIC_NEGATIVE_FIXTURE: &str = r#"
schema_version = 1
default_bundles = ["coverage-fixture"]

[[items]]
id = "threshold-gap"
display_name = "Threshold gap"
category = "foundation"
scope = "hybrid"
depends_on = []
targets = [
  { backend = "apt", source = "distribution_package" },
]
success_threshold = "configured"
standalone = false

  [[items.verifier.checks]]
  stage = "present"
  threshold = "required"
  min_profile = "quick"
  kind = "command"
  command = "git"

[[items]]
id = "quick-gap"
display_name = "Quick gap"
category = "remote_access"
scope = "system"
depends_on = []
targets = [
  { backend = "apt", source = "distribution_package" },
]
success_threshold = "operational"
standalone = false

  [[items.verifier.checks]]
  stage = "present"
  threshold = "required"
  min_profile = "standard"
  kind = "any_command"
  commands = ["vncserver"]

  [items.verifier.service]
  kind = "vnc"
  commands = ["vncserver"]

[[items]]
id = "service-gap"
display_name = "Service gap"
category = "remote_access"
scope = "system"
depends_on = []
targets = [
  { backend = "apt", source = "distribution_package" },
]
success_threshold = "present"
standalone = false

  [[items.verifier.checks]]
  stage = "present"
  threshold = "required"
  min_profile = "quick"
  kind = "any_command"
  commands = ["vncserver"]

[[bundles]]
id = "coverage-fixture"
display_name = "Coverage fixture"
items = ["threshold-gap", "quick-gap", "service-gap"]
"#;
