use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::PathBuf,
};

use envira::catalog::{embedded_manifest, load_embedded_catalog, Catalog, CatalogError};
use serde::Deserialize;

const CANONICAL_FIXTURE_PATH: &str = "tests/fixtures/catalog_contract_valid.toml";
const LEGACY_FIXTURE_PATH: &str = "tests/fixtures/catalog_contract_legacy_shape.toml";
const DUPLICATE_ITEM_FIXTURE_PATH: &str =
    "tests/fixtures/catalog_contract_duplicate_item_table.toml";
const DUPLICATE_BUNDLE_FIXTURE_PATH: &str =
    "tests/fixtures/catalog_contract_duplicate_bundle_table.toml";
const MISSING_DEPENDENCY_FIXTURE_PATH: &str =
    "tests/fixtures/catalog_contract_missing_dependency.toml";
const MISSING_DEFAULT_BUNDLE_FIXTURE_PATH: &str =
    "tests/fixtures/catalog_contract_missing_default_bundle.toml";
const WILDCARD_RECIPE_OVERLAP_FIXTURE_PATH: &str =
    "tests/fixtures/catalog_contract_recipe_wildcard_overlap.toml";
const WILDCARD_VERIFIER_OVERLAP_FIXTURE_PATH: &str =
    "tests/fixtures/catalog_contract_verifier_wildcard_overlap.toml";
const MISSING_RECIPE_FIXTURE_PATH: &str = "tests/fixtures/catalog_contract_missing_recipe.toml";
const MISSING_VERIFIER_FIXTURE_PATH: &str = "tests/fixtures/catalog_contract_missing_verifier.toml";

#[test]
fn canonical_new_schema_fixture_is_valid_against_contract_model() {
    let raw = fixture_text(CANONICAL_FIXTURE_PATH);
    let catalog = load_approved_catalog(CANONICAL_FIXTURE_PATH).expect(
        "Task 2 canonical fixture should stay valid independently of runtime parser support",
    );

    assert_eq!(catalog.required_version, "0.1.0");
    assert_eq!(catalog.distros, vec!["ubuntu"]);
    assert_eq!(catalog.shell, "bash");
    assert_eq!(catalog.default_bundles, vec!["essentials"]);

    let bundle = catalog
        .bundles
        .get("essentials")
        .expect("canonical bundle should exist");
    assert_eq!(bundle.name, "Essentials");
    assert_eq!(bundle.items, vec!["git"]);

    let item = catalog
        .items
        .get("git")
        .expect("canonical item should exist");
    assert_eq!(item.name, "Git");
    assert_eq!(item.depends_on, Vec::<String>::new());
    assert_eq!(item.recipes.len(), 1);
    assert_eq!(item.verifiers.len(), 1);

    for legacy_fragment in [
        "schema_version",
        "display_name",
        "targets",
        "success_threshold",
        "verifier.service",
        "standalone",
        "all-default",
    ] {
        assert!(
            !raw.contains(legacy_fragment),
            "canonical fixture should not preserve legacy manifest fragment `{legacy_fragment}`",
        );
    }
}

#[test]
fn legacy_shape_fixture_is_rejected_by_schema_freeze_model() {
    let error = load_approved_catalog(LEGACY_FIXTURE_PATH).expect_err(
        "legacy array-of-tables manifest should be rejected by the Task 2 schema model",
    );

    assert!(
        error.contains("unknown field `schema_version`"),
        "unexpected schema-freeze error: {error}",
    );
}

#[test]
fn duplicate_item_tables_are_rejected_by_toml_before_schema_validation() {
    assert_duplicate_table_error(DUPLICATE_ITEM_FIXTURE_PATH, "items.git");
}

#[test]
fn duplicate_bundle_tables_are_rejected_by_toml_before_schema_validation() {
    assert_duplicate_table_error(DUPLICATE_BUNDLE_FIXTURE_PATH, "bundles.essentials");
}

#[test]
fn missing_dependency_reference_is_rejected_by_contract_validator() {
    let error = load_approved_catalog(MISSING_DEPENDENCY_FIXTURE_PATH)
        .expect_err("fixtures with undefined dependencies should fail contract validation");

    assert_eq!(error, "item `bat` depends_on undefined item `wget`");
}

#[test]
fn missing_default_bundle_reference_is_rejected_by_contract_validator() {
    let error = load_approved_catalog(MISSING_DEFAULT_BUNDLE_FIXTURE_PATH)
        .expect_err("fixtures with undefined default bundles should fail contract validation");

    assert_eq!(
        error,
        "default_bundles references undefined bundle `missing-bundle`"
    );
}

#[test]
fn wildcard_recipe_overlap_is_rejected_by_contract_validator() {
    let error = load_approved_catalog(WILDCARD_RECIPE_OVERLAP_FIXTURE_PATH)
        .expect_err("recipe wildcard overlap should be rejected before Task 3 runtime work");

    assert_eq!(
        error,
        "item `git` recipes overlap for mode `sudo` on distro `ubuntu`"
    );
}

#[test]
fn wildcard_verifier_overlap_is_rejected_by_contract_validator() {
    let error = load_approved_catalog(WILDCARD_VERIFIER_OVERLAP_FIXTURE_PATH)
        .expect_err("verifier wildcard overlap should be rejected before Task 3 runtime work");

    assert_eq!(
        error,
        "item `git` verifiers overlap for mode `sudo` on distro `ubuntu`"
    );
}

#[test]
fn item_without_recipe_is_rejected_by_contract_validator() {
    let error = load_approved_catalog(MISSING_RECIPE_FIXTURE_PATH)
        .expect_err("items without recipes should fail the Task 2 contract model");

    assert_eq!(error, "item `git` must define at least one recipe");
}

#[test]
fn item_without_verifier_is_rejected_by_contract_validator() {
    let error = load_approved_catalog(MISSING_VERIFIER_FIXTURE_PATH)
        .expect_err("items without verifiers should fail the Task 2 contract model");

    assert_eq!(error, "item `git` must define at least one verifier");
}

#[test]
fn parser_contract_accepts_canonical_new_schema_fixture() {
    let manifest = fixture_text(CANONICAL_FIXTURE_PATH);

    Catalog::from_toml_str(&manifest)
        .expect("runtime parser should accept the canonical keyed TOML schema");
}

#[test]
fn embedded_manifest_is_new_schema_source_of_truth() {
    let raw = embedded_manifest();
    let catalog = load_embedded_catalog()
        .expect("embedded catalog should load through the keyed TOML runtime parser");

    for legacy_fragment in [
        "schema_version",
        "display_name",
        "targets",
        "success_threshold",
    ] {
        assert!(
            !raw.contains(legacy_fragment),
            "embedded manifest should not contain legacy fragment `{legacy_fragment}`"
        );
    }

    assert_eq!(catalog.required_version, "0.1.0");
    assert_eq!(catalog.shell, "bash");
    assert_eq!(catalog.default_bundles.len(), 3);
    assert_eq!(catalog.default_bundles[0].as_str(), "core");
    assert!(catalog.bundle("core").is_some());
    assert!(catalog.item("essentials").is_some());
}

#[test]
fn embedded_manifest_covers_legacy_run_script_tool_surface() {
    let catalog = load_embedded_catalog().expect("embedded catalog should load");

    for bundle_id in [
        "core",
        "terminal-tools",
        "observability",
        "containers",
        "remote-access",
        "services",
        "desktop-tools",
        "shell-customization",
        "languages",
        "editors",
        "git-tooling",
        "tui-tooling",
        "ai-agents",
        "monitoring",
        "cloud-data",
        "devops",
    ] {
        assert!(
            catalog.bundle(bundle_id).is_some(),
            "expected embedded manifest bundle `{bundle_id}` to exist"
        );
    }

    for item_id in [
        "essentials",
        "git-lfs",
        "bat",
        "ctop",
        "fastfetch",
        "btop",
        "neofetch",
        "ncdu",
        "gitkraken",
        "meslo-font",
        "oh-my-zsh",
        "zsh-theme",
        "zsh-plugins",
        "fzf",
        "pipx",
        "miniconda",
        "rust-toolchain",
        "go-toolchain",
        "fnm",
        "nodejs",
        "neovim",
        "lunarvim",
        "jupyter",
        "vnc",
        "docker",
        "pm2",
        "lazygit",
        "lazydocker",
        "lemonade",
        "cargo-binstall",
        "zellij",
        "lsd",
        "cargo-cache",
        "git-delta",
        "duf",
        "dust",
        "fd",
        "ripgrep",
        "gping",
        "procs",
        "xh",
        "uv",
        "pixi",
        "speedtest-cli",
        "gdown",
        "archey4",
        "genact",
        "zoxide",
        "micro",
        "scc",
        "viu",
        "dive",
        "tldr",
        "huggingface-cli",
        "superfile",
        "yazi",
        "codex-cli",
        "gemini-cli",
        "cursor-cli",
        "claude-cli",
        "opencode-cli",
        "bun",
        "oh-my-opencode",
        "openchamber-web",
        "oh-my-pi",
        "beads",
        "perles",
        "dolt",
        "rustscan",
        "gotify",
        "bottom",
        "nvitop",
        "nviwatch",
        "bandwhich",
        "rich-cli",
        "gh",
    ] {
        assert!(
            catalog.item(item_id).is_some(),
            "expected embedded manifest item `{item_id}` to exist"
        );
    }
}

#[test]
fn legacy_shape_rejected() {
    let manifest = fixture_text(LEGACY_FIXTURE_PATH);
    let error = Catalog::from_toml_str(&manifest)
        .expect_err("runtime parser should reject the legacy catalog shape");

    assert!(
        matches!(error, CatalogError::Validation(_)),
        "legacy shape rejection should be a validation error, got: {error}",
    );
    assert!(
        error.to_string().contains("legacy catalog shape"),
        "legacy rejection should mention the retired schema explicitly, got: {error}",
    );
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
    #[serde(default)]
    depends_on: Vec<String>,
    #[serde(default)]
    recipes: Vec<CommandContract>,
    #[serde(default)]
    verifiers: Vec<CommandContract>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CommandContract {
    mode: String,
    distros: Vec<String>,
    cmd: String,
}

fn load_approved_catalog(relative_path: &str) -> Result<ApprovedCatalog, String> {
    let raw = fixture_text(relative_path);
    let catalog: ApprovedCatalog =
        toml::from_str(&raw).map_err(|error| format_toml_error(relative_path, error))?;
    validate_catalog_contract(&catalog)?;
    Ok(catalog)
}

fn validate_catalog_contract(catalog: &ApprovedCatalog) -> Result<(), String> {
    if catalog.required_version.trim().is_empty() {
        return Err("required_version must not be empty".to_string());
    }

    if catalog.distros.is_empty() {
        return Err("distros must define at least one supported distro".to_string());
    }

    if catalog.shell.trim().is_empty() {
        return Err("shell must not be empty".to_string());
    }

    if catalog.default_bundles.is_empty() {
        return Err("default_bundles must reference at least one bundle".to_string());
    }

    for (bundle_id, bundle) in &catalog.bundles {
        if bundle.name.trim().is_empty() {
            return Err(format!("bundle `{bundle_id}` must define a non-empty name"));
        }

        if bundle.desc.trim().is_empty() {
            return Err(format!("bundle `{bundle_id}` must define a non-empty desc"));
        }

        if bundle.items.is_empty() {
            return Err(format!(
                "bundle `{bundle_id}` must reference at least one item"
            ));
        }

        for item_id in &bundle.items {
            if !catalog.items.contains_key(item_id) {
                return Err(format!(
                    "bundle `{bundle_id}` references undefined item `{item_id}`"
                ));
            }
        }
    }

    let supported_distro_ids = catalog.distros.iter().cloned().collect::<BTreeSet<_>>();

    for bundle_id in &catalog.default_bundles {
        if !catalog.bundles.contains_key(bundle_id) {
            return Err(format!(
                "default_bundles references undefined bundle `{bundle_id}`"
            ));
        }
    }

    for (item_id, item) in &catalog.items {
        if item.name.trim().is_empty() {
            return Err(format!("item `{item_id}` must define a non-empty name"));
        }

        if item.desc.trim().is_empty() {
            return Err(format!("item `{item_id}` must define a non-empty desc"));
        }

        if item.recipes.is_empty() {
            return Err(format!("item `{item_id}` must define at least one recipe"));
        }

        if item.verifiers.is_empty() {
            return Err(format!(
                "item `{item_id}` must define at least one verifier"
            ));
        }

        for dependency in &item.depends_on {
            if !catalog.items.contains_key(dependency) {
                return Err(format!(
                    "item `{item_id}` depends_on undefined item `{dependency}`"
                ));
            }
        }

        validate_command_contracts(item_id, "recipes", &item.recipes, &supported_distro_ids)?;
        validate_command_contracts(item_id, "verifiers", &item.verifiers, &supported_distro_ids)?;
    }

    Ok(())
}

fn validate_command_contracts(
    item_id: &str,
    contract_kind: &str,
    commands: &[CommandContract],
    supported_distro_ids: &BTreeSet<String>,
) -> Result<(), String> {
    let mut seen = BTreeSet::new();

    for command in commands {
        let mode = command.mode.as_str();
        if !matches!(mode, "sudo" | "user") {
            return Err(format!(
                "item `{item_id}` {contract_kind} entry uses unsupported mode `{mode}`"
            ));
        }

        if command.cmd.trim().is_empty() {
            return Err(format!(
                "item `{item_id}` {contract_kind} entry for mode `{mode}` must define a non-empty cmd"
            ));
        }

        if command.distros.is_empty() {
            return Err(format!(
                "item `{item_id}` {contract_kind} entry for mode `{mode}` must define at least one distro"
            ));
        }

        let expanded_distros =
            expand_distros(item_id, contract_kind, command, supported_distro_ids)?;
        for distro in expanded_distros {
            if !seen.insert((mode.to_string(), distro.clone())) {
                return Err(format!(
                    "item `{item_id}` {contract_kind} overlap for mode `{mode}` on distro `{distro}`"
                ));
            }
        }
    }

    Ok(())
}

fn expand_distros(
    item_id: &str,
    contract_kind: &str,
    command: &CommandContract,
    supported_distro_ids: &BTreeSet<String>,
) -> Result<BTreeSet<String>, String> {
    if command.distros.iter().any(|distro| distro == "*") {
        if command.distros.len() != 1 {
            return Err(format!(
                "item `{item_id}` {contract_kind} entry for mode `{}` must use `distros = [\"*\"]` only",
                command.mode,
            ));
        }

        return Ok(supported_distro_ids.clone());
    }

    let mut expanded = BTreeSet::new();
    for distro in &command.distros {
        if !supported_distro_ids.contains(distro) {
            return Err(format!(
                "item `{item_id}` {contract_kind} entry for mode `{}` references unsupported distro `{distro}`",
                command.mode,
            ));
        }

        expanded.insert(distro.clone());
    }

    Ok(expanded)
}

fn assert_duplicate_table_error(relative_path: &str, table_hint: &str) {
    let raw = fixture_text(relative_path);
    let error = toml::from_str::<ApprovedCatalog>(&raw).expect_err(
        "duplicate keyed tables should fail at the TOML parser layer before schema validation",
    );
    let message = format_toml_error(relative_path, error);

    let mentions_duplicate = ["duplicate", "redefinition", "defined twice"]
        .iter()
        .any(|needle| message.contains(needle));
    assert!(
        mentions_duplicate,
        "expected duplicate-table parser wording for {relative_path}, got: {message}",
    );
    assert!(
        message.contains("items") || message.contains("bundles") || message.contains(table_hint),
        "expected duplicate-table error to stay tied to `{table_hint}`, got: {message}",
    );
}

fn format_toml_error(relative_path: &str, error: toml::de::Error) -> String {
    format!("{relative_path}: {error}")
}

fn fixture_text(relative_path: &str) -> String {
    fs::read_to_string(repo_path(relative_path))
        .unwrap_or_else(|error| panic!("failed to read {relative_path}: {error}"))
}

fn repo_path(relative_path: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(relative_path)
}
