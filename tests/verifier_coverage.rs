use envira::catalog::{load_embedded_catalog, Catalog, CatalogItem};
use envira::verifier::{
    required_stage_for_catalog_commands, ProbeRequirement, VerificationProfile, VerificationStage,
    VerifierSpec,
};

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

    assert!(
        violations.is_empty(),
        "plain-command fallback verifier coverage should stay structurally complete under Task 3: {violations:?}"
    );
}

#[test]
fn embedded_launch_catalog_service_items_derive_service_readiness_from_plain_command_contracts() {
    let catalog = load_embedded_catalog().expect("embedded catalog should load");

    for (item_id, expected_kind) in [
        ("docker", "docker"),
        ("jupyter", "jupyter"),
        ("pm2", "pm2"),
        ("vnc", "vnc"),
    ] {
        let item = catalog.item(item_id).expect("service item should exist");
        assert_eq!(
            VerifierSpec::from_catalog_commands(&item.verifiers).service,
            None,
            "{item_id} should keep the approved plain-command catalog shape"
        );
        assert_eq!(
            VerifierSpec::from_catalog_commands(&item.verifiers)
                .effective_service()
                .expect("service readiness should be derived")
                .kind
                .as_str(),
            expected_kind
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
    let verifier = VerifierSpec::from_catalog_commands(&item.verifiers);
    let required_stage = required_stage_for_catalog_commands(&item.verifiers);

    if !has_required_quick_check(item) {
        violations.push(format!(
            "item `{}`: missing required quick-profile verifier check",
            item.id
        ));
    }

    if requires_service_metadata(item) && verifier.effective_service().is_none() {
        violations.push(format!(
            "item `{}`: service-like launch contract does not derive service readiness from verifiers[].cmd",
            item.id
        ));
    }

    let max_reachable = max_quick_reachable_stage(item);
    if !max_reachable.is_some_and(|stage| stage.meets(required_stage)) {
        violations.push(format!(
            "item `{}`: required_stage `{}` is unreachable in quick profile (max reachable: `{}`)",
            item.id,
            stage_name(required_stage),
            max_reachable.map(stage_name).unwrap_or("none")
        ));
    }

    violations
}

fn has_required_quick_check(item: &CatalogItem) -> bool {
    let verifier = VerifierSpec::from_catalog_commands(&item.verifiers);

    verifier.checks.iter().any(|check| {
        check.requirement == ProbeRequirement::Required
            && check.min_profile == VerificationProfile::Quick
    })
}

fn max_quick_reachable_stage(item: &CatalogItem) -> Option<VerificationStage> {
    let verifier = VerifierSpec::from_catalog_commands(&item.verifiers);
    let check_stage = verifier
        .checks
        .iter()
        .filter(|check| {
            check.requirement == ProbeRequirement::Required
                && check.min_profile == VerificationProfile::Quick
        })
        .map(|check| check.stage)
        .max();
    let service_stage = verifier
        .effective_service()
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
    VerifierSpec::from_catalog_commands(&item.verifiers)
        .effective_service()
        .is_some()
}

fn stage_name(stage: VerificationStage) -> &'static str {
    match stage {
        VerificationStage::Present => "present",
        VerificationStage::Configured => "configured",
        VerificationStage::Operational => "operational",
    }
}

const SEMANTIC_NEGATIVE_FIXTURE: &str = r#"
required_version = "0.1.0"
distros = ["ubuntu"]
shell = "bash"
default_bundles = ["coverage-fixture"]

[items.threshold-gap]
name = "Threshold gap"
desc = "Threshold gap"
depends_on = []

[[items.threshold-gap.recipes]]
mode = "user"
distros = ["ubuntu"]
cmd = "sudo apt install -y git"

[[items.threshold-gap.verifiers]]
mode = "user"
distros = ["ubuntu"]
cmd = "command -v git"

[items.quick-gap]
name = "Quick gap"
desc = "Quick gap"
depends_on = []

[[items.quick-gap.recipes]]
mode = "sudo"
distros = ["ubuntu"]
cmd = "sudo apt install -y tigervnc-standalone-server"

[[items.quick-gap.verifiers]]
mode = "sudo"
distros = ["ubuntu"]
cmd = "command -v vncserver"

[items.service-gap]
name = "Service gap"
desc = "Service gap"
depends_on = []

[[items.service-gap.recipes]]
mode = "sudo"
distros = ["ubuntu"]
cmd = "sudo apt install -y tigervnc-standalone-server"

[[items.service-gap.verifiers]]
mode = "sudo"
distros = ["ubuntu"]
cmd = "command -v vncserver"

[bundles.coverage-fixture]
name = "Coverage fixture"
desc = "Coverage fixture"
items = ["threshold-gap", "quick-gap", "service-gap"]
"#;
