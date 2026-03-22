use std::{collections::BTreeMap, path::PathBuf};

use envira::catalog::{
    embedded_manifest, load_embedded_catalog, Catalog, CatalogError, InstallScope,
    SuccessThreshold, TargetBackend, ALL_DEFAULT_BUNDLE_ID, SUPPORTED_SCHEMA_VERSION,
};
use envira::executor::build_execution_plan;
use envira::planner::{build_install_plan, classify_install_plan, PlannerError, PlannerRequest};
use envira::platform::{
    ArchitectureIdentity, ArchitectureKind, DistroIdentity, DistroKind, InvocationKind,
    PlatformContext, RuntimeScope, UserAccount,
};
use envira::verifier::{
    EvidenceRecord, EvidenceStatus, ObservedScope, ProbeKind, ProbeRequirement, VerificationHealth,
    VerificationProfile, VerificationStage, VerificationSummary, VerifierCheck, VerifierEvidence,
    VerifierResult,
};

#[test]
fn catalog_schema_validation_valid_manifest_parses() {
    let catalog = load_embedded_catalog().expect("embedded catalog should parse");

    assert_eq!(catalog.schema_version, SUPPORTED_SCHEMA_VERSION);
    assert_eq!(catalog.items.len(), 60);
    assert_eq!(catalog.bundles.len(), 12);
    assert_eq!(
        catalog
            .item("essentials")
            .expect("essentials item exists")
            .success_threshold,
        SuccessThreshold::Present
    );
    assert_eq!(
        catalog
            .default_bundles
            .iter()
            .map(|bundle| bundle.as_str())
            .collect::<Vec<_>>(),
        vec!["core", "terminal-tools", "observability"]
    );
}

#[test]
fn catalog_schema_validation_rejects_unknown_fields() {
    let manifest = r#"
schema_version = 1
default_bundles = ["core"]
unexpected = true

[[items]]
id = "essentials"
display_name = "Essentials"
category = "foundation"
scope = "system"
depends_on = []
targets = [{ backend = "apt", source = "distribution_package" }]
success_threshold = "present"
standalone = false

  [[items.verifier.checks]]
  threshold = "required"
  kind = "command"
  command = "git"

[[bundles]]
id = "core"
display_name = "Core"
items = ["essentials"]
"#;

    let error = Catalog::from_toml_str(manifest).expect_err("unknown fields should fail");

    assert!(matches!(error, CatalogError::Parse(_)));
    assert!(error.to_string().contains("unknown field `unexpected`"));
}

#[test]
fn catalog_schema_validation_rejects_incomplete_item_metadata() {
    let manifest = r#"
schema_version = 1
default_bundles = ["core"]

[[items]]
id = "essentials"
category = "foundation"
scope = "system"
depends_on = []
targets = [{ backend = "apt", source = "distribution_package" }]
success_threshold = "present"
standalone = false

  [[items.verifier.checks]]
  threshold = "required"
  kind = "command"
  command = "git"

[[bundles]]
id = "core"
display_name = "Core"
items = ["essentials"]
"#;

    let error = Catalog::from_toml_str(manifest).expect_err("missing item fields should fail");

    assert!(matches!(error, CatalogError::Parse(_)));
    assert!(error.to_string().contains("missing field `display_name`"));
}

#[test]
fn catalog_schema_validation_default_bundle_semantics_are_data_driven() {
    let catalog = load_embedded_catalog().expect("embedded catalog should parse");

    let default_ids = catalog
        .default_install_ids()
        .expect("default bundle expansion should work");
    let all_default_ids = catalog
        .expand_bundle(ALL_DEFAULT_BUNDLE_ID)
        .expect("all-default alias should expand")
        .into_iter()
        .map(|item| item.id.as_str())
        .collect::<Vec<_>>();

    assert_eq!(
        default_ids,
        vec!["essentials", "bat", "ctop", "fastfetch", "btop"]
    );
    assert_eq!(all_default_ids, default_ids);
    assert!(!default_ids.contains(&"vnc"));
}

#[test]
fn catalog_schema_validation_item_threshold_is_stage_oriented() {
    let manifest = r#"
schema_version = 1
default_bundles = ["core"]

[[items]]
id = "essentials"
display_name = "Essentials"
category = "foundation"
scope = "system"
depends_on = []
targets = [{ backend = "apt", source = "distribution_package" }]
success_threshold = "configured"
standalone = false

  [[items.verifier.checks]]
  threshold = "optional"
  kind = "command"
  command = "git"

[[bundles]]
id = "core"
display_name = "Core"
items = ["essentials"]
"#;

    let catalog = Catalog::from_toml_str(manifest)
        .expect("stage-oriented item thresholds should not depend on verifier reduction");

    assert_eq!(
        catalog
            .item("essentials")
            .expect("essentials item exists")
            .success_threshold,
        SuccessThreshold::Configured
    );
}

#[test]
fn catalog_schema_validation_embedded_manifest_is_the_source_of_truth() {
    let catalog =
        Catalog::from_toml_str(embedded_manifest()).expect("embedded manifest should parse");

    assert_eq!(
        catalog.item("vnc").expect("vnc item exists").display_name,
        "TigerVNC"
    );
    assert!(catalog.item("docker").is_some(), "docker item should exist");
    for synthetic_id in [
        "git-tools",
        "editor-suite",
        "rust-cli-tools",
        "python-cli-tools",
        "go-cli-tools",
        "agent-clis",
        "advanced-monitoring",
    ] {
        assert!(
            catalog.item(synthetic_id).is_none(),
            "{synthetic_id} should not survive the item-level launch freeze"
        );
    }
}

#[test]
fn catalog_schema_validation_embedded_manifest_freezes_expected_launch_inventory() {
    let catalog = load_embedded_catalog().expect("embedded catalog should parse");
    let mut item_ids = catalog
        .items
        .iter()
        .map(|item| item.id.as_str())
        .collect::<Vec<_>>();
    item_ids.sort_unstable();

    assert_eq!(
        item_ids,
        vec![
            "archey4",
            "bandwhich",
            "bat",
            "bottom",
            "btop",
            "bun",
            "cargo-binstall",
            "cargo-cache",
            "claude",
            "codex",
            "ctop",
            "dive",
            "docker",
            "duf",
            "dust",
            "essentials",
            "fastfetch",
            "fd",
            "fzf",
            "gdown",
            "gemini",
            "genact",
            "git-delta",
            "github-cli",
            "gitkraken",
            "go-toolchain",
            "gotify",
            "gping",
            "huggingface-cli",
            "jupyter",
            "lazydocker",
            "lazygit",
            "lemonade",
            "lsd",
            "lunarvim",
            "micro",
            "miniconda",
            "neovim",
            "nodejs",
            "nvitop",
            "nviwatch",
            "opencode",
            "pixi",
            "pm2",
            "procs",
            "rich-cli",
            "ripgrep",
            "rust-toolchain",
            "rustscan",
            "scc",
            "speedtest-cli",
            "superfile",
            "tldr",
            "uv",
            "viu",
            "vnc",
            "xh",
            "yazi",
            "zellij",
            "zoxide",
        ]
    );
}

#[test]
fn catalog_schema_validation_toolchains_bundle_is_execution_plannable_for_user_runtime() {
    let catalog = load_embedded_catalog().expect("embedded catalog should parse");
    let platform = platform_context(TargetBackend::Apt, RuntimeScope::User);
    let install_plan =
        build_install_plan(&catalog, &platform, &PlannerRequest::bundle("toolchains"))
            .expect("toolchains bundle should build for user runtime");

    let action_plan = classify_install_plan(
        &install_plan,
        &install_plan
            .steps
            .iter()
            .map(|step| (step.item_id.clone(), missing_result(step.success_threshold)))
            .collect::<BTreeMap<_, _>>(),
    )
    .expect("toolchains actions should classify");

    let execution_plan = build_execution_plan(&catalog, &platform, &action_plan)
        .expect("toolchains bundle should have recipe overlays for selected targets");

    assert_eq!(
        execution_plan
            .steps
            .iter()
            .map(|step| step.action_step.step.item_id.as_str())
            .collect::<Vec<_>>(),
        vec![
            "miniconda",
            "essentials",
            "rust-toolchain",
            "cargo-binstall",
            "cargo-cache",
            "go-toolchain",
            "nodejs",
            "pixi",
            "jupyter",
        ]
    );
    assert!(execution_plan
        .steps
        .iter()
        .all(|step| step.recipe.is_some() || step.operations.is_empty()));
}

#[test]
fn catalog_schema_validation_embedded_items_are_execution_plannable_when_scope_allows_it() {
    let catalog = load_embedded_catalog().expect("embedded catalog should parse");

    for runtime_scope in [RuntimeScope::User, RuntimeScope::System] {
        let platform = platform_context(TargetBackend::Apt, runtime_scope);

        for item in &catalog.items {
            let request = PlannerRequest::item(item.id.as_str());

            match build_install_plan(&catalog, &platform, &request) {
                Ok(install_plan) => {
                    let action_plan = classify_install_plan(
                        &install_plan,
                        &install_plan
                            .steps
                            .iter()
                            .map(|step| {
                                (
                                    step.item_id.clone(),
                                    missing_result(step.success_threshold),
                                )
                            })
                            .collect::<BTreeMap<_, _>>(),
                    )
                    .unwrap_or_else(|error| {
                        panic!(
                            "classification should succeed for item `{}` in runtime `{runtime_scope:?}`: {error}",
                            item.id.as_str()
                        )
                    });

                    build_execution_plan(&catalog, &platform, &action_plan).unwrap_or_else(|error| {
                        panic!(
                            "execution planning should succeed for item `{}` in runtime `{runtime_scope:?}`: {error}",
                            item.id.as_str()
                        )
                    });
                }
                Err(PlannerError::UnsupportedScope {
                    item_id,
                    item_scope,
                    runtime_scope: error_runtime_scope,
                }) => {
                    assert_eq!(error_runtime_scope, runtime_scope);
                    assert_eq!(
                        catalog
                            .item(item_id.as_str())
                            .expect("rejected item should exist in the embedded catalog")
                            .scope,
                        item_scope
                    );
                    assert!(
                        matches!(
                            (item_scope, runtime_scope),
                            (InstallScope::System, RuntimeScope::User)
                                | (InstallScope::User, RuntimeScope::System)
                        ),
                        "unexpected scope rejection for selected item `{}` via rejected item `{item_id}` in runtime `{runtime_scope:?}`",
                        item.id.as_str(),
                    );
                }
                Err(error) => panic!(
                    "planning embedded item `{}` in runtime `{runtime_scope:?}` should not fail unexpectedly: {error}",
                    item.id.as_str()
                ),
            }
        }
    }
}

fn platform_context(native_backend: TargetBackend, runtime_scope: RuntimeScope) -> PlatformContext {
    let effective_user = match runtime_scope {
        RuntimeScope::System => user("root", "/root", 0, 0),
        RuntimeScope::User | RuntimeScope::Both | RuntimeScope::Unknown => {
            user("alice", "/home/alice", 1000, 1000)
        }
    };
    let target_user = match runtime_scope {
        RuntimeScope::System | RuntimeScope::Unknown => None,
        RuntimeScope::User => Some(user("alice", "/home/alice", 1000, 1000)),
        RuntimeScope::Both => Some(user("alice", "/home/alice", 1000, 1000)),
    };
    let invocation = match runtime_scope {
        RuntimeScope::System => InvocationKind::Root,
        RuntimeScope::User | RuntimeScope::Unknown => InvocationKind::User,
        RuntimeScope::Both => InvocationKind::Sudo,
    };

    PlatformContext {
        distro: DistroIdentity {
            kind: DistroKind::Ubuntu,
            id: "ubuntu".to_string(),
            name: "Ubuntu".to_string(),
            pretty_name: Some("Ubuntu 24.04 LTS".to_string()),
            version_id: Some("24.04".to_string()),
        },
        arch: ArchitectureIdentity {
            kind: ArchitectureKind::X86_64,
            raw: "x86_64".to_string(),
        },
        native_backend: Some(native_backend),
        invocation,
        effective_user,
        target_user,
        runtime_scope,
    }
}

fn user(username: &str, home_dir: &str, uid: u32, gid: u32) -> UserAccount {
    UserAccount {
        username: username.to_string(),
        home_dir: PathBuf::from(home_dir),
        uid: Some(uid),
        gid: Some(gid),
    }
}

fn missing_result(required_stage: VerificationStage) -> VerifierResult {
    let evidence = vec![VerifierEvidence {
        check: VerifierCheck {
            stage: VerificationStage::Present,
            requirement: ProbeRequirement::Required,
            min_profile: VerificationProfile::Quick,
            kind: ProbeKind::Command,
            command: Some("missing".to_string()),
            commands: None,
            path: None,
            pattern: None,
        },
        record: EvidenceRecord {
            status: EvidenceStatus::Missing,
            observed_scope: ObservedScope::Unknown,
            summary: "missing".to_string(),
            detail: None,
        },
        participates: true,
    }];

    VerifierResult {
        requested_profile: VerificationProfile::Quick,
        required_stage,
        achieved_stage: None,
        threshold_met: false,
        health: VerificationHealth::Missing,
        observed_scope: ObservedScope::Unknown,
        summary: summarize(&evidence),
        evidence,
        service_evidence: Vec::new(),
        service: None,
    }
}

fn summarize(evidence: &[VerifierEvidence]) -> VerificationSummary {
    let mut summary = VerificationSummary {
        total_checks: evidence.len(),
        participating_checks: evidence.iter().filter(|entry| entry.participates).count(),
        skipped_checks: evidence.iter().filter(|entry| !entry.participates).count(),
        ..VerificationSummary::default()
    };

    for entry in evidence.iter().filter(|entry| entry.participates) {
        match entry.record.status {
            EvidenceStatus::Satisfied => summary.satisfied_checks += 1,
            EvidenceStatus::Missing => {
                summary.missing_checks += 1;
                if entry.check.requirement == ProbeRequirement::Required {
                    summary.required_failures += 1;
                }
            }
            EvidenceStatus::Broken => {
                summary.broken_checks += 1;
                if entry.check.requirement == ProbeRequirement::Required {
                    summary.required_failures += 1;
                }
            }
            EvidenceStatus::Unknown => {
                summary.unknown_checks += 1;
                if entry.check.requirement == ProbeRequirement::Required {
                    summary.required_failures += 1;
                }
            }
            EvidenceStatus::NotApplicable => summary.not_applicable_checks += 1,
        }
    }

    summary
}
