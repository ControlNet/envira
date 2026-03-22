use std::{
    env, fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{self, Command, Output},
    time::{SystemTime, UNIX_EPOCH},
};

use serde_json::Value;

const TEST_COMMAND: &str = "envira-headless-test-tool";

#[test]
fn each_headless_command_supports_json_output() {
    let fixture = HeadlessFixture::new(false);

    for command in ["catalog", "plan", "verify", "install"] {
        let output = fixture.run([command, "--format", "json"]);
        let json = parse_stdout_json(&output);

        assert_eq!(json["format"], "json", "unexpected format for {command}");
        assert_eq!(json["command"], command, "unexpected command for {command}");
        assert!(json["ok"].as_bool().unwrap_or(false));
    }
}

#[test]
fn plan_bundle_all_default_json_preserves_requested_selection() {
    let fixture = HeadlessFixture::new(false);
    let output = fixture.run(["plan", "--bundle", "all-default", "--format", "json"]);
    let json = parse_stdout_json(&output);

    assert!(output.status.success());
    assert_eq!(json["payload"]["kind"], "plan");
    assert_eq!(
        json["payload"]["action_plan"]["request"]["selections"][0]["kind"],
        "bundle"
    );
    assert_eq!(
        json["payload"]["action_plan"]["request"]["selections"][0]["id"],
        "all-default"
    );
}

#[test]
fn plan_json_includes_rationale_and_verifier_summary() {
    let fixture = HeadlessFixture::new(false);
    let output = fixture.run(["plan", "--format", "json"]);
    let json = parse_stdout_json(&output);
    let step = &json["payload"]["action_plan"]["steps"][0];

    assert!(output.status.success());
    assert_eq!(json["payload"]["kind"], "plan");
    assert_eq!(step["action"], "install");
    assert!(step["rationale"]["summary"]
        .as_str()
        .is_some_and(|summary| !summary.is_empty()));
    assert_eq!(
        step["rationale"]["verifier"]["summary"]["missing_checks"],
        1
    );
    assert_eq!(step["rationale"]["verifier"]["threshold_met"], false);
}

#[test]
fn verify_json_includes_evidence_and_threshold_fields() {
    let fixture = HeadlessFixture::new(false);
    let output = fixture.run(["verify", "--format", "json"]);
    let json = parse_stdout_json(&output);
    let result = &json["payload"]["verification"]["results"][0]["result"];

    assert!(!output.status.success());
    assert_eq!(json["payload"]["kind"], "verify");
    assert_eq!(result["threshold_met"], false);
    assert_eq!(result["evidence"][0]["record"]["status"], "missing");
    assert!(result["evidence"]
        .as_array()
        .is_some_and(|entries| !entries.is_empty()));
}

#[test]
fn verify_item_quick_json_preserves_requested_selection() {
    let fixture = HeadlessFixture::new(false);
    let output = fixture.run([
        "verify",
        "--item",
        "headless-tool",
        "--profile",
        "quick",
        "--format",
        "json",
    ]);
    let json = parse_stdout_json(&output);

    assert!(!output.status.success());
    assert_eq!(json["payload"]["kind"], "verify");
    assert_eq!(json["payload"]["verification"]["profile"], "quick");
    assert_eq!(
        json["payload"]["verification"]["request"]["selections"][0]["kind"],
        "item"
    );
    assert_eq!(
        json["payload"]["verification"]["request"]["selections"][0]["id"],
        "headless-tool"
    );
}

#[test]
fn verify_all_strict_json_preserves_requested_profile() {
    let fixture = HeadlessFixture::new(true);
    let output = fixture.run(["verify", "--all", "--profile", "strict", "--format", "json"]);
    let json = parse_stdout_json(&output);
    let results = json["payload"]["verification"]["results"]
        .as_array()
        .expect("verify results should be an array");
    let result_ids = results
        .iter()
        .map(|result| {
            result["step"]["item_id"]
                .as_str()
                .expect("result item id should be a string")
        })
        .collect::<Vec<_>>();

    assert!(output.status.success());
    assert_eq!(json["payload"]["kind"], "verify");
    assert_eq!(json["payload"]["verification"]["profile"], "strict");
    assert_eq!(
        json["payload"]["verification"]["request"]["selections"][0]["kind"],
        "all_items"
    );
    assert_eq!(result_ids, vec!["headless-tool", "vnc"]);
}

#[test]
fn verify_help_lists_item_bundle_and_all_selection_modes() {
    let output = Command::new(env!("CARGO_BIN_EXE_envira"))
        .args(["verify", "--help"])
        .output()
        .expect("binary should run");

    assert!(output.status.success());

    let stdout = String::from_utf8(output.stdout).expect("stdout should be valid utf-8");
    for flag in ["--item <ID>", "--bundle <BUNDLE>", "--all"] {
        assert!(
            stdout.contains(flag),
            "expected verify help to include {flag}, got:\n{stdout}"
        );
    }
}

#[test]
fn install_json_reports_verifier_failure_after_successful_execution() {
    let fixture = HeadlessFixture::new(false);
    let output = fixture.run(["install", "--format", "json"]);
    let json = parse_stdout_json(&output);
    let install = &json["payload"]["install"];

    assert!(!output.status.success());
    assert_eq!(json["payload"]["kind"], "install");
    assert_eq!(install["execution"]["summary"]["failed_steps"], 0);
    assert_eq!(install["outcome"]["execution_succeeded"], true);
    assert_eq!(install["outcome"]["status"], "verification_failed");
    assert_eq!(
        install["outcome"]["failures"][0]["verifier"]["threshold_met"],
        false
    );
}

#[test]
fn install_bundle_all_default_dry_run_json_skips_execution_and_succeeds() {
    let fixture = HeadlessFixture::new(false);
    let output = fixture.run([
        "install",
        "--bundle",
        "all-default",
        "--dry-run",
        "--format",
        "json",
    ]);
    let json = parse_stdout_json(&output);
    let install = &json["payload"]["install"];

    assert!(output.status.success());
    assert_eq!(json["payload"]["kind"], "install");
    assert_eq!(install["install_mode"], "dry_run");
    assert_eq!(install["outcome"]["status"], "dry_run");
    assert_eq!(install["execution"]["summary"]["failed_steps"], 0);
    assert!(
        install["execution"]["summary"]["skipped_steps"]
            .as_u64()
            .unwrap_or(0)
            > 0
    );
}

#[test]
fn json_errors_use_structured_engine_envelopes() {
    let fixture = HeadlessFixture::new(false);
    let missing_manifest = fixture.root().join("missing-manifest.toml");
    let output = Command::new(env!("CARGO_BIN_EXE_envira"))
        .args(["catalog", "--format", "json"])
        .env("ENVIRA_CATALOG_PATH", &missing_manifest)
        .env("PATH", fixture.path_env())
        .output()
        .expect("binary should run");
    let json = parse_stdout_json(&output);

    assert!(!output.status.success());
    assert_eq!(json["ok"], false);
    assert_eq!(json["error"]["code"], "catalog_read_failed");
    assert_eq!(
        json["error"]["context"]["manifest_path"],
        missing_manifest.display().to_string()
    );
}

struct HeadlessFixture {
    root: PathBuf,
    manifest_path: PathBuf,
    path_env: String,
}

impl HeadlessFixture {
    fn new(include_verified_command: bool) -> Self {
        let root = unique_temp_dir("headless-cli");
        let bin_dir = root.join("bin");
        fs::create_dir_all(&bin_dir).expect("bin directory should be created");

        for command in ["mkdir", "curl", "chmod", "install"] {
            write_executable(&bin_dir.join(command), "#!/bin/sh\nexit 0\n");
        }

        if include_verified_command {
            write_executable(&bin_dir.join(TEST_COMMAND), "#!/bin/sh\nexit 0\n");
        }

        let manifest_path = root.join("catalog.toml");
        fs::write(&manifest_path, test_manifest()).expect("manifest should be written");

        let path_env = env::join_paths(
            std::iter::once(bin_dir.clone()).chain(
                env::var_os("PATH")
                    .as_deref()
                    .map(env::split_paths)
                    .into_iter()
                    .flatten(),
            ),
        )
        .expect("path should join")
        .to_string_lossy()
        .into_owned();

        Self {
            root,
            manifest_path,
            path_env,
        }
    }

    fn run<const N: usize>(&self, args: [&str; N]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_envira"))
            .args(args)
            .env("ENVIRA_CATALOG_PATH", &self.manifest_path)
            .env("PATH", &self.path_env)
            .output()
            .expect("binary should run")
    }

    fn root(&self) -> &Path {
        &self.root
    }

    fn path_env(&self) -> &str {
        &self.path_env
    }
}

impl Drop for HeadlessFixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn parse_stdout_json(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "stdout should contain parseable JSON: {error}\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        )
    })
}

fn test_manifest() -> String {
    format!(
        r#"
schema_version = 1
default_bundles = ["headless-tests"]

[[items]]
id = "headless-tool"
display_name = "Headless Tool"
category = "terminal_tool"
scope = "user"
depends_on = []
targets = [{{ backend = "direct_binary", source = "github_release" }}]
success_threshold = "present"
standalone = false

[[items.recipes]]
backend = "direct_binary"
source = "github_release"
recipe = "direct_binary"
url = "https://example.com/headless-tool"
binary_name = "{TEST_COMMAND}"

[[items.verifier.checks]]
requirement = "required"
kind = "command"
command = "{TEST_COMMAND}"

[[items.verifier.checks]]
stage = "present"
requirement = "required"
min_profile = "strict"
kind = "command"
command = "{TEST_COMMAND}"

[[bundles]]
id = "headless-tests"
display_name = "Headless Tests"
items = ["headless-tool"]

[[items]]
id = "vnc"
display_name = "TigerVNC"
category = "remote_access"
scope = "system"
depends_on = []
targets = [{{ backend = "direct_binary", source = "github_release" }}]
success_threshold = "present"
standalone = false

[[items.recipes]]
backend = "direct_binary"
source = "github_release"
recipe = "direct_binary"
url = "https://example.com/vnc"
binary_name = "{TEST_COMMAND}"

[[items.verifier.checks]]
requirement = "required"
kind = "command"
command = "{TEST_COMMAND}"

[[bundles]]
id = "remote-access"
display_name = "Remote Access"
items = ["vnc"]
"#
    )
}

fn unique_temp_dir(label: &str) -> PathBuf {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock should be after unix epoch")
        .as_nanos();
    let path = env::temp_dir().join(format!("envira-{label}-{}-{unique}", process::id()));
    fs::create_dir_all(&path).expect("temporary directory should be created");
    path
}

fn write_executable(path: &Path, contents: &str) {
    fs::write(path, contents).expect("script should be written");
    let mut permissions = fs::metadata(path)
        .expect("script metadata should exist")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("script permissions should be updated");
}
