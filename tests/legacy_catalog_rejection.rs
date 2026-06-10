use std::{path::PathBuf, process::Command};

use envira::catalog::{Catalog, CatalogError};
use serde_json::Value;

const LEGACY_FIXTURE_PATH: &str = "tests/fixtures/catalog_contract_legacy_shape.toml";

#[test]
fn parser_rejects_legacy_catalog_fixture_with_explicit_message() {
    let legacy_manifest = std::fs::read_to_string(repo_path(LEGACY_FIXTURE_PATH))
        .expect("legacy fixture should be readable");
    let error = Catalog::from_toml_str(&legacy_manifest)
        .expect_err("legacy array-of-tables fixture should be rejected");

    assert!(matches!(error, CatalogError::Validation(_)));
    assert!(
        error
            .to_string()
            .contains("legacy catalog shape is no longer supported"),
        "unexpected parser rejection: {error}",
    );
}

#[test]
fn headless_catalog_command_rejects_legacy_override_fixture() {
    let legacy_manifest = repo_path(LEGACY_FIXTURE_PATH);
    let output = Command::new(env!("CARGO_BIN_EXE_envira"))
        .args(["catalog", "--format", "json"])
        .env("ENVIRA_CATALOG_PATH", &legacy_manifest)
        .output()
        .expect("envira catalog command should run");
    let json = parse_stdout_json(&output);

    assert!(!output.status.success());
    assert_eq!(json["ok"], false);
    assert_eq!(json["error"]["code"], "catalog_invalid");
    assert_eq!(
        json["error"]["context"]["catalog_path"],
        legacy_manifest.display().to_string()
    );
    assert!(json["error"]["message"]
        .as_str()
        .is_some_and(|message| message.contains("legacy catalog shape is no longer supported")));
}

fn parse_stdout_json(output: &std::process::Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "stdout should contain parseable JSON: {error}\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        )
    })
}

fn repo_path(relative_path: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(relative_path)
}
