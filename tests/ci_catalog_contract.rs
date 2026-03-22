use std::fs;
use std::path::PathBuf;

#[test]
fn launch_parity_ci_uses_current_contract_names_and_commands() {
    let workflow = repo_file(".github/workflows/test.yaml");
    let matrix_script = repo_file("scripts/run-launch-parity-matrix.sh");
    let container_script = repo_file("scripts/run-container-parity-check.sh");

    for anchor in [
        "name: Catalog Contract Test",
        "launch-parity-contract:",
        "Run keyed TOML launch parity matrix",
        "run: ./scripts/run-launch-parity-matrix.sh",
        "name: launch-parity-contract-evidence",
    ] {
        assert!(
            workflow.contains(anchor),
            "expected .github/workflows/test.yaml to contain `{anchor}`"
        );
    }

    for anchor in [
        "task-11-launch-parity-catalog.json",
        "task-11-launch-parity-commands.json",
        "task-11-launch-parity-container-matrix.json",
        "task-11-launch-parity-summary.json",
    ] {
        assert!(
            matrix_script.contains(anchor),
            "expected scripts/run-launch-parity-matrix.sh to contain `{anchor}`"
        );
    }

    for anchor in [
        "fixture_path=\"/workspace/tests/fixtures/launch_parity_container_catalog.toml\"",
        "task-11-launch-parity-containers",
        "task-11-launch-parity-container-matrix.json",
        "\"catalog_source\": \"envira_catalog_path\"",
    ] {
        assert!(
            container_script.contains(anchor),
            "expected scripts/run-container-parity-check.sh to contain `{anchor}`"
        );
    }

    for stale in ["task-15", "manifest_source", "terminal-tools"] {
        assert!(
            !matrix_script.contains(stale),
            "scripts/run-launch-parity-matrix.sh should not contain stale anchor `{stale}`"
        );
        assert!(
            !container_script.contains(stale),
            "scripts/run-container-parity-check.sh should not contain stale anchor `{stale}`"
        );
    }
}

#[test]
fn legacy_installer_workflow_name_is_explicit() {
    let workflow = repo_file(".github/workflows/lagacy_test.yaml");

    assert!(workflow.contains("name: Legacy Installer Test"));
    assert!(workflow.contains("name: Legacy ${{ matrix.os }} ${{ matrix.mode }} installer test"));
}

fn repo_file(relative_path: &str) -> String {
    fs::read_to_string(repo_path(relative_path))
        .unwrap_or_else(|error| panic!("failed to read {relative_path}: {error}"))
}

fn repo_path(relative_path: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(relative_path)
}
