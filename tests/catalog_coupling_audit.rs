use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::PathBuf,
};

use serde::Deserialize;

const CANONICAL_FIXTURE_PATH: &str = "tests/fixtures/catalog_contract_valid.toml";

const LEGACY_FIELDS: [&str; 10] = [
    "schema_version",
    "display_name",
    "category",
    "scope",
    "targets",
    "success_threshold",
    "standalone",
    "verifier.service",
    "::recipe_",
    "::verifier_",
];

fn coupling_hotspots() -> Vec<CouplingHotspot> {
    vec![CouplingHotspot {
        path: "src/catalog/mod.rs",
        subsystem: "catalog parser",
        action: MigrationAction::Retest,
        reason: "the runtime parser now centers the keyed TOML contract, but embedded loading and downstream consumers still need follow-up coverage around the new required_version/distros/shell/default_bundles model",
        anchors: &[
            "const EMBEDDED_MANIFEST: &str = include_str!(\"manifest.toml\");",
            "pub required_version: String,",
            "pub distros: Vec<String>,",
            "pub shell: String,",
            "pub default_bundles: Vec<CanonicalId>,",
            "pub items: Vec<CatalogItem>,",
            "pub bundles: Vec<CatalogBundle>,",
            "legacy catalog shape is no longer supported",
        ],
    },
    CouplingHotspot {
        path: "src/planner/mod.rs",
        subsystem: "planner selection semantics",
        action: MigrationAction::Retest,
        reason: "planner now treats an empty selection list as the only implicit default-bundle path and derives required stage from verifier shell contracts, so downstream CLI/TUI/action consumers need parity retesting against the final planner contract",
        anchors: &[
            "if request.selections.is_empty() {",
            "let supported_recipes =",
        ],
    },
    CouplingHotspot {
        path: "src/executor/plan.rs",
        subsystem: "executor recipe planning",
        action: MigrationAction::Retest,
        reason: "executor now selects direct recipe shell contracts and wraps them in a single shell command operation, so downstream dry-run/install payloads still need regression coverage around the approved shell model",
        anchors: &[
            "use crate::catalog::{Catalog, CatalogCommand, CatalogItem, CommandMode};",
            "Shell { shell: String, command: String }",
            "plan_shell_operations",
            "item `{item_id}` is missing a recipe shell contract",
        ],
    },
    CouplingHotspot {
        path: "src/verifier/spec.rs",
        subsystem: "verifier metadata",
        action: MigrationAction::Replace,
        reason: "verifier runtime data is now projected explicitly from catalog shell contracts, so the audit should pin the constructor that keeps the schema single-sourced",
        anchors: &[
            "pub fn from_catalog_commands(commands: &[CatalogCommand]) -> Self {",
            "pub struct VerifierSpec {",
            "pub checks: Vec<VerifierCheck>,",
            "pub service: Option<ServiceVerificationSpec>,",
        ],
    },
    CouplingHotspot {
        path: "src/cli.rs",
        subsystem: "headless CLI selection",
        action: MigrationAction::Replace,
        reason: "CLI flags now expose explicit item/bundle/all selection semantics, including `--all` mapping to PlannerRequest::all_items() for headless workflows",
        anchors: &[
            "bundle: Option<String>,",
            "all: bool,",
            "Some(PlannerRequest::all_items())",
        ],
    },
    CouplingHotspot {
        path: "src/tui.rs",
        subsystem: "TUI selection messaging",
        action: MigrationAction::Replace,
        reason: "TUI now models implicit defaults via an absent planner request, derived `default_bundles` markers, and shared error text rendering, so the audit should track those current semantics instead of deleted pre-Task-10 wording",
        anchors: &[
            "pub fn planner_request(&self) -> Option<PlannerRequest> {",
            "implicit default_bundles ({})",
            ".render_text();",
            "planner_request_command(",
        ],
    },
    CouplingHotspot {
        path: "src/engine/mod.rs",
        subsystem: "catalog load boundary",
        action: MigrationAction::Retest,
        reason: "ENVIRA_CATALOG_PATH and embedded manifest loading will need retesting after the schema swap",
        anchors: &[
            "const CATALOG_PATH_ENV: &str = \"ENVIRA_CATALOG_PATH\";",
            "context.insert(\"manifest_path\".to_string(), \"embedded\".to_string());",
            "load_catalog_from_manifest(embedded_manifest(), None)",
            "load_catalog_from_manifest(&raw_manifest, Some(path))",
        ],
    },
    CouplingHotspot {
        path: "src/engine/types.rs",
        subsystem: "JSON payload serialization",
        action: MigrationAction::Retest,
        reason: "CommandPayload serializes catalog-driven data directly, so schema changes will ripple into JSON payloads",
        anchors: &[
            "pub enum CommandPayload {",
            "Catalog {",
            "Verify {",
            "Install {",
        ],
    },
    CouplingHotspot {
        path: "tests/catalog_validation.rs",
        subsystem: "catalog contract tests",
        action: MigrationAction::Remove,
        reason: "catalog validation now locks the converged runtime parser behavior directly, so stale Task-3 migration wording must stay gone",
        anchors: &[
            "parser_contract_accepts_canonical_new_schema_fixture",
            "runtime parser should accept the canonical keyed TOML schema",
            "runtime parser should reject the legacy catalog shape",
        ],
    },
    CouplingHotspot {
        path: "tests/headless_cli.rs",
        subsystem: "headless CLI tests",
        action: MigrationAction::Retest,
        reason: "headless CLI tests now use keyed TOML fixtures with plain shell commands and the final JSON contract, so they remain the main regression guard for user-facing catalog semantics",
        anchors: &[
            "test_manifest_with_required_version(\"0.1.0\")",
            "cmd = \"curl -fsSL https://example.com/headless-tool -o ~/.local/bin/{TEST_COMMAND} && chmod +x ~/.local/bin/{TEST_COMMAND}\"",
            "plan_bundle_json_preserves_requested_selection",
        ],
    },
    CouplingHotspot {
        path: "tests/backend_mappings.rs",
        subsystem: "backend mapping tests",
        action: MigrationAction::Retest,
        reason: "backend tests now lock the direct shell-contract executor path while planner target selection still needs parity coverage across distro-native and generic user flows",
        anchors: &[
            "required_version = \"0.1.0\"",
            "native_execution_plan_uses_catalog_shell_contract_instead_of_backend_adapter",
            "CommandOperation::shell(\"bash\", \"sudo apt install -y native-only\")",
            "TargetBackend::DirectBinary",
        ],
    },
    CouplingHotspot {
        path: "tests/planner_verifier_integration.rs",
        subsystem: "planner/verifier integration tests",
        action: MigrationAction::Retest,
        reason: "planner-verifier integration now covers the final required-stage and service-derivation semantics that come from verifier shell contracts",
        anchors: &[
            "required_version = \"0.1.0\"",
            "cmd = \"curl -fsSL https://example.com/ready-tool.tar.gz | tar -xz -C ~/.local/bin\"",
            "cmd = \"command -v ready-tool\"",
        ],
    },
    CouplingHotspot {
        path: "tests/verifier_coverage.rs",
        subsystem: "verifier coverage tests",
        action: MigrationAction::Retest,
        reason: "verifier coverage now derives required stage and service readiness straight from verifier shell contracts, so it guards the final convergence behavior rather than a future verifier rewrite",
        anchors: &[
            "load_embedded_catalog",
            "required_version = \"0.1.0\"",
            "cmd = \"sudo apt install -y tigervnc-standalone-server\"",
            "cmd = \"command -v vncserver\"",
        ],
    },
    CouplingHotspot {
        path: "tests/tui.rs",
        subsystem: "TUI tests",
        action: MigrationAction::Retest,
        reason: "TUI tests now exercise the final command-contract planner and verifier wording, including implicit defaults without the removed all_default compatibility path",
        anchors: &[
            "required_version = \"0.1.0\"",
            "cmd = \"curl -fsSL https://example.com/tool-a -o ~/.local/bin/tool-a && chmod +x ~/.local/bin/tool-a\"",
            "cmd = \"command -v tool-a\"",
        ],
    },
    CouplingHotspot {
        path: "tests/launch_parity_matrix.rs",
        subsystem: "launch parity evidence",
        action: MigrationAction::Retest,
        reason: "launch parity now drives catalog/plan/verify/install against the keyed TOML override fixture and writes Task 11 evidence for the active contract rather than the retired embedded-manifest blocker story",
        anchors: &[
            "const FIXTURE_PATH: &str = \"tests/fixtures/launch_parity_container_catalog.toml\";",
            "task-11-launch-parity-catalog.json",
            "task-11-launch-parity-commands.json",
            "launch_parity_commands_capture_default_bundle_plan_verify_and_dry_run_install",
        ],
    },
    CouplingHotspot {
        path: "tests/fixtures/launch_parity_container_catalog.toml",
        subsystem: "parity fixture seed",
        action: MigrationAction::Retest,
        reason: "the parity fixture now needs to stay aligned with the keyed TOML catalog contract because launch parity and CI consume it through ENVIRA_CATALOG_PATH",
        anchors: &[
            "required_version = \"0.1.0\"",
            "[bundles.launch-parity]",
            "[[items.shell-path.recipes]]",
        ],
    },
    CouplingHotspot {
        path: "scripts/run-container-parity-check.sh",
        subsystem: "parity workflow script",
        action: MigrationAction::Retest,
        reason: "the container parity workflow now mounts the keyed TOML fixture, seeds a verifier command in PATH, and records Task 11 contract evidence instead of embedded terminal-tools assumptions",
        anchors: &[
            "fixture_path=\"/workspace/tests/fixtures/launch_parity_container_catalog.toml\"",
            "task-11-launch-parity-container-matrix.json",
            "\"catalog_source\": \"envira_catalog_path\"",
        ],
    },
    ]
}

#[test]
fn catalog_coupling_inventory_tracks_current_legacy_hotspots() {
    let mut actions = BTreeSet::new();
    let hotspots = coupling_hotspots();

    for hotspot in &hotspots {
        actions.insert(hotspot.action);

        let contents = repo_file(hotspot.path);
        for anchor in hotspot.anchors {
            assert!(
                contents.contains(anchor),
                "expected {} [{} / {}] to contain `{anchor}` so the Task 1 audit stays tied to the current repo: {}",
                hotspot.path,
                hotspot.subsystem,
                hotspot.action.as_str(),
                hotspot.reason,
            );
        }
    }

    assert_eq!(
        actions,
        BTreeSet::from([
            MigrationAction::Replace,
            MigrationAction::Remove,
            MigrationAction::Retest,
        ]),
        "the coupling audit should distinguish replace/remove/retest work across the refactor backlog",
    );

    assert!(
        repo_path(CANONICAL_FIXTURE_PATH).exists(),
        "expected canonical fixture at {CANONICAL_FIXTURE_PATH}",
    );
}

#[test]
fn canonical_catalog_fixture_is_minimal_note_derived_and_approved_only() {
    let fixture = fixture_contents();
    let catalog: ApprovedCatalog = toml::from_str(&fixture)
        .expect("canonical fixture should parse against the approved Task 1 schema freeze");

    assert_eq!(catalog.required_version, "0.1.0");
    assert_eq!(catalog.distros, vec!["ubuntu"]);
    assert_eq!(catalog.shell, "bash");
    assert_eq!(catalog.default_bundles, vec!["essentials"]);
    assert_eq!(catalog.bundles.len(), 1, "fixture should stay minimal");
    assert_eq!(catalog.items.len(), 1, "fixture should stay minimal");

    let bundle = catalog
        .bundles
        .get("essentials")
        .expect("essentials bundle should exist");
    assert_eq!(bundle.name, "Essentials");
    assert_eq!(
        bundle.desc,
        "Essentials is a bundle of essential tools for the system."
    );
    assert_eq!(bundle.items, vec!["git"]);

    let item = catalog.items.get("git").expect("git item should exist");
    assert_eq!(item.name, "Git");
    assert_eq!(item.desc, "Git is a distributed version control system.");
    assert!(
        item.depends_on.is_empty(),
        "fixture should stay self-contained"
    );
    assert_eq!(item.recipes.len(), 1);
    assert_eq!(item.verifiers.len(), 1);
    assert_eq!(
        item.recipes[0],
        CommandContract {
            mode: "user".to_string(),
            distros: vec!["ubuntu".to_string()],
            cmd: "mkdir -p ~/.local/bin && ln -sf \"$(command -v git)\" ~/.local/bin/git"
                .to_string(),
        }
    );
    assert_eq!(
        item.verifiers[0],
        CommandContract {
            mode: "user".to_string(),
            distros: vec!["ubuntu".to_string()],
            cmd: "command -v git".to_string(),
        }
    );

    for legacy_field in &LEGACY_FIELDS {
        assert!(
            !fixture.contains(legacy_field),
            "canonical fixture should not carry legacy field or generated-id fragment `{legacy_field}`",
        );
    }

    assert_catalog_is_self_contained(&catalog);
}

#[test]
fn schema_freeze_rejects_unapproved_field() {
    let injected = format!("schema_version = 1\n{}", fixture_contents());
    let error = toml::from_str::<ApprovedCatalog>(&injected)
        .expect_err("legacy root fields should be rejected by the schema freeze");

    assert!(
        error.to_string().contains("unknown field `schema_version`"),
        "unexpected parse error: {error}",
    );
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum MigrationAction {
    Replace,
    Remove,
    Retest,
}

impl MigrationAction {
    fn as_str(self) -> &'static str {
        match self {
            Self::Replace => "replace",
            Self::Remove => "remove",
            Self::Retest => "retest",
        }
    }
}

#[derive(Debug)]
struct CouplingHotspot {
    path: &'static str,
    subsystem: &'static str,
    action: MigrationAction,
    reason: &'static str,
    anchors: &'static [&'static str],
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ApprovedCatalog {
    required_version: String,
    distros: Vec<String>,
    shell: String,
    default_bundles: Vec<String>,
    bundles: BTreeMap<String, ApprovedBundle>,
    items: BTreeMap<String, ApprovedItem>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ApprovedBundle {
    name: String,
    desc: String,
    items: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ApprovedItem {
    name: String,
    desc: String,
    depends_on: Vec<String>,
    recipes: Vec<CommandContract>,
    verifiers: Vec<CommandContract>,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
struct CommandContract {
    mode: String,
    distros: Vec<String>,
    cmd: String,
}

fn assert_catalog_is_self_contained(catalog: &ApprovedCatalog) {
    assert!(
        !catalog.default_bundles.is_empty(),
        "approved catalog fixture must keep at least one default bundle",
    );

    for bundle_id in &catalog.default_bundles {
        assert!(
            catalog.bundles.contains_key(bundle_id),
            "default bundle `{bundle_id}` should resolve to a declared bundle",
        );
    }

    for (bundle_id, bundle) in &catalog.bundles {
        assert!(
            !bundle.items.is_empty(),
            "bundle `{bundle_id}` should list at least one item",
        );

        for item_id in &bundle.items {
            assert!(
                catalog.items.contains_key(item_id),
                "bundle `{bundle_id}` should only reference declared items",
            );
        }
    }

    for (item_id, item) in &catalog.items {
        assert!(
            !item.recipes.is_empty(),
            "item `{item_id}` should define at least one recipe",
        );
        assert!(
            !item.verifiers.is_empty(),
            "item `{item_id}` should define at least one verifier",
        );

        for dependency in &item.depends_on {
            assert!(
                catalog.items.contains_key(dependency),
                "item `{item_id}` should only depend on declared items",
            );
        }
    }
}

fn fixture_contents() -> String {
    repo_file(CANONICAL_FIXTURE_PATH)
}

fn repo_file(relative_path: &str) -> String {
    fs::read_to_string(repo_path(relative_path))
        .unwrap_or_else(|error| panic!("failed to read {relative_path}: {error}"))
}

fn repo_path(relative_path: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(relative_path)
}
