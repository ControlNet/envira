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
        assert!(
            json["ok"].is_boolean(),
            "expected boolean ok field for {command}"
        );
    }
}

#[test]
fn plan_bundle_json_preserves_requested_selection() {
    let fixture = HeadlessFixture::new(false);
    let output = fixture.run(["plan", "--bundle", "headless-tests", "--format", "json"]);
    let json = parse_stdout_json(&output);

    assert!(output.status.success());
    assert_eq!(json["payload"]["kind"], "plan");
    assert_eq!(
        json["payload"]["request"]["selections"][0]["kind"],
        "bundle"
    );
    assert_eq!(
        json["payload"]["request"]["selections"][0]["id"],
        "headless-tests"
    );
}

#[test]
fn plan_all_json_selects_all_catalog_items_explicitly() {
    let fixture = HeadlessFixture::new(false);
    fixture.write_manifest(test_manifest_with_all_user_items());
    let output = fixture.run(["plan", "--all", "--format", "json"]);
    let json = parse_stdout_json(&output);
    let items = json["payload"]["items"]
        .as_array()
        .expect("plan items should be an array");
    let item_ids = items
        .iter()
        .map(|item| {
            item["item_id"]
                .as_str()
                .expect("planned item id should be a string")
        })
        .collect::<Vec<_>>();

    assert!(output.status.success());
    assert_eq!(json["payload"]["kind"], "plan");
    assert_eq!(
        json["payload"]["request"]["selections"][0]["kind"],
        "all_items"
    );
    assert_eq!(item_ids, vec!["headless-tool", "vnc"]);
}

#[test]
fn plan_json_uses_item_focused_payload_without_legacy_metadata() {
    let fixture = HeadlessFixture::new(false);
    let output = fixture.run(["plan", "--format", "json"]);
    let stdout = stdout_string(&output);
    let json = parse_stdout_json(&output);
    let item = &json["payload"]["items"][0];

    assert!(output.status.success());
    assert_eq!(json["payload"]["kind"], "plan");
    assert_eq!(json["payload"]["summary"]["requested_items"], 1);
    assert_eq!(json["payload"]["summary"]["install_items"], 1);
    assert_eq!(item["item_id"], "headless-tool");
    assert_eq!(item["action"], "install");
    assert_eq!(item["reason_code"], "missing");
    assert!(item["summary"]
        .as_str()
        .is_some_and(|summary| !summary.is_empty()));
    assert!(!stdout.contains("\"selected_target\""));
    assert!(!stdout.contains("\"verifier\""));
}

#[test]
fn verify_json_includes_evidence_and_threshold_fields() {
    let fixture = HeadlessFixture::new(false);
    let output = fixture.run(["verify", "--format", "json"]);
    let stdout = stdout_string(&output);
    let json = parse_stdout_json(&output);
    let result = &json["payload"]["items"][0];

    assert!(!output.status.success());
    assert_eq!(json["payload"]["kind"], "verify");
    assert_eq!(result["threshold_met"], false);
    assert_eq!(result["required_stage"], "present");
    assert_eq!(result["evidence"][0]["record"]["status"], "missing");
    assert!(result["evidence"]
        .as_array()
        .is_some_and(|entries| !entries.is_empty()));
    assert_eq!(stdout.matches("\"required_stage\"").count(), 1);
    assert!(!stdout.contains("\"selected_target\""));
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
    assert_eq!(json["payload"]["profile"], "quick");
    assert_eq!(json["payload"]["request"]["selections"][0]["kind"], "item");
    assert_eq!(
        json["payload"]["request"]["selections"][0]["id"],
        "headless-tool"
    );
}

#[test]
fn verify_all_strict_json_preserves_requested_profile() {
    let fixture = HeadlessFixture::new(true);
    let output = fixture.run(["verify", "--all", "--profile", "strict", "--format", "json"]);
    let json = parse_stdout_json(&output);
    let results = json["payload"]["items"]
        .as_array()
        .expect("verify results should be an array");
    let result_ids = results
        .iter()
        .map(|result| {
            result["item_id"]
                .as_str()
                .expect("result item id should be a string")
        })
        .collect::<Vec<_>>();

    assert!(output.status.success());
    assert_eq!(json["payload"]["kind"], "verify");
    assert_eq!(json["payload"]["profile"], "strict");
    assert_eq!(
        json["payload"]["request"]["selections"][0]["kind"],
        "all_items"
    );
    assert_eq!(result_ids, vec!["headless-tool", "vnc"]);
}

#[test]
fn install_json_reports_verification_failure_after_successful_execution() {
    let fixture = HeadlessFixture::new(false);
    let output = fixture.run(["install", "--format", "json"]);
    let stdout = stdout_string(&output);
    let json = parse_stdout_json(&output);
    let install = &json["payload"];
    let step = &install["execution"]["steps"][0];

    assert!(!output.status.success());
    assert_eq!(json["ok"], true);
    assert_eq!(json["payload"]["kind"], "install");
    assert_eq!(install["summary"]["status"], "verification_failed");
    assert_eq!(install["outcome"]["status"], "verification_failed");
    assert_eq!(install["execution"]["summary"]["failed_steps"], 0);
    assert_eq!(step["recipe"]["kind"], "shell");
    assert_eq!(step["recipe"]["shell"], "bash");
    assert_eq!(
        step["recipe"]["command"],
        Value::String(format!(
            "curl -fsSL https://example.com/headless-tool -o ~/.local/bin/{TEST_COMMAND} && chmod +x ~/.local/bin/{TEST_COMMAND}"
        ))
    );
    assert_eq!(step["operations"][0]["operation"]["kind"], "command");
    assert_eq!(step["operations"][0]["operation"]["program"], "bash");
    assert_eq!(
        step["operations"][0]["operation"]["args"],
        serde_json::json!([
            "-c",
            format!(
                "curl -fsSL https://example.com/headless-tool -o ~/.local/bin/{TEST_COMMAND} && chmod +x ~/.local/bin/{TEST_COMMAND}"
            )
        ])
    );
    assert!(!stdout.contains("\"action_plan\""));
    assert!(!stdout.contains("\"post_verification\""));
    assert!(!stdout.contains("\"selected_target\""));
    assert!(!stdout.contains("\"verifier\""));
}

#[test]
fn install_bundle_dry_run_reports_shell_contract_preview() {
    let fixture = HeadlessFixture::new(false);
    let output = fixture.run([
        "install",
        "--bundle",
        "headless-tests",
        "--dry-run",
        "--format",
        "json",
    ]);
    let json = parse_stdout_json(&output);
    let install = &json["payload"];
    let step = &install["execution"]["steps"][0];

    assert!(output.status.success());
    assert_eq!(json["ok"], true);
    assert_eq!(json["payload"]["kind"], "install");
    assert_eq!(install["install_mode"], "dry_run");
    assert_eq!(install["outcome"]["status"], "dry_run");
    assert_eq!(step["recipe"]["kind"], "shell");
    assert_eq!(step["recipe"]["shell"], "bash");
    assert_eq!(step["operations"][0]["operation"]["kind"], "command");
    assert_eq!(step["operations"][0]["operation"]["program"], "bash");
    assert_eq!(
        install["execution"]["steps"][0]["message"],
        "Dry run skipped 1 operation(s) for `headless-tool`."
    );
    assert_eq!(
        install["execution"]["steps"][0]["operations"][0]["state"],
        "skipped"
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
        json["error"]["context"]["catalog_path"],
        missing_manifest.display().to_string()
    );
    let stdout = stdout_string(&output);
    assert!(!stdout.contains("catalog manifest"));
    assert!(!stdout.contains("manifest_path"));
}

#[test]
fn invalid_selection_and_legacy_terms_rejected() {
    let fixture = HeadlessFixture::new(false);

    let missing_item_output = fixture.run(["plan", "--item", "missing-tool", "--format", "json"]);
    let missing_item_json = parse_stdout_json(&missing_item_output);
    let missing_item_stdout = stdout_string(&missing_item_output);

    assert!(!missing_item_output.status.success());
    assert_eq!(missing_item_json["ok"], false);
    assert_eq!(missing_item_json["error"]["code"], "planning_failed");
    assert!(missing_item_json["error"]["message"]
        .as_str()
        .is_some_and(|message| message
            .contains("requested item `missing-tool` is not defined in the catalog")));
    assert!(!missing_item_stdout.contains("manifest"));

    let missing_bundle_output =
        fixture.run(["plan", "--bundle", "missing-bundle", "--format", "json"]);
    let missing_bundle_json = parse_stdout_json(&missing_bundle_output);
    let missing_bundle_stdout = stdout_string(&missing_bundle_output);

    assert!(!missing_bundle_output.status.success());
    assert_eq!(missing_bundle_json["ok"], false);
    assert_eq!(missing_bundle_json["error"]["code"], "planning_failed");
    assert!(missing_bundle_json["error"]["message"]
        .as_str()
        .is_some_and(|message| message
            .contains("requested bundle `missing-bundle` is not defined in the catalog")));
    assert!(!missing_bundle_stdout.contains("manifest"));
}

#[test]
fn version_gate_errors_are_explicit() {
    let fixture = HeadlessFixture::new(false);

    fixture.write_manifest(test_manifest_with_required_version("banana"));
    let invalid_output = fixture.run(["plan", "--format", "json"]);
    let invalid_json = parse_stdout_json(&invalid_output);

    assert!(!invalid_output.status.success());
    assert_eq!(invalid_json["ok"], false);
    assert_eq!(
        invalid_json["error"]["code"],
        "catalog_required_version_invalid"
    );
    assert!(invalid_json["error"]["message"]
        .as_str()
        .is_some_and(|message| message.contains("major.minor.patch")));

    fixture.write_manifest(test_manifest_with_required_version("0.2.0-beta.1"));
    let prerelease_output = fixture.run(["plan", "--format", "json"]);
    let prerelease_json = parse_stdout_json(&prerelease_output);

    assert!(!prerelease_output.status.success());
    assert_eq!(prerelease_json["ok"], false);
    assert_eq!(
        prerelease_json["error"]["code"],
        "catalog_required_version_prerelease_unsupported"
    );
    assert_eq!(
        prerelease_json["error"]["context"]["required_version"],
        "0.2.0-beta.1"
    );

    fixture.write_manifest(test_manifest_with_required_version("0.2.0"));
    let wrapper_path = repo_path("envira.sh").display().to_string();
    let update_failure_output = fixture.run_with_env(
        ["plan", "--format", "json"],
        &[
            ("ENVIRA_CURRENT_VERSION", "0.1.0"),
            ("ENVIRA_UPDATE_WRAPPER_PATH", wrapper_path.as_str()),
            ("ENVIRA_BOOTSTRAP_BASE_URL", "http://127.0.0.1:9"),
        ][..],
    );
    let update_failure_json = parse_stdout_json(&update_failure_output);

    assert!(!update_failure_output.status.success());
    assert_eq!(update_failure_json["ok"], false);
    assert_eq!(
        update_failure_json["error"]["code"],
        "envira_auto_update_failed"
    );
    assert_eq!(
        update_failure_json["error"]["context"]["current_version"],
        "0.1.0"
    );
    assert_eq!(
        update_failure_json["error"]["context"]["required_version"],
        "0.2.0"
    );
    assert_eq!(update_failure_json["error"]["context"]["exit_code"], "80");
    assert!(update_failure_json["error"]["context"]["detail"]
        .as_str()
        .is_some_and(|detail| detail.starts_with("[ERROR]")));
    assert!(update_failure_json["error"]["message"]
        .as_str()
        .is_some_and(|message| message.contains("approved update flow failed")));
}

#[test]
fn env_catalog_path_override_loads_new_schema() {
    let fixture = HeadlessFixture::new(false);
    let output = fixture.run(["catalog", "--format", "json"]);
    let json = parse_stdout_json(&output);
    let bundle_ids = json_string_array(&json["payload"]["catalog"]["bundles"], "id");
    let item_ids = json_string_array(&json["payload"]["catalog"]["items"], "id");

    assert!(output.status.success());
    assert_eq!(json["payload"]["kind"], "catalog");
    assert_eq!(json["payload"]["catalog"]["required_version"], "0.1.0");
    assert_eq!(json["payload"]["catalog"]["shell"], "bash");
    assert_eq!(
        json["payload"]["catalog"]["default_bundles"],
        Value::Array(vec![Value::String("headless-tests".to_string())])
    );
    assert!(bundle_ids.contains(&"headless-tests".to_string()));
    assert!(item_ids.contains(&"headless-tool".to_string()));
}

#[test]
fn catalog_command_outputs_new_schema() {
    let output = Command::new(env!("CARGO_BIN_EXE_envira"))
        .args(["catalog", "--format", "json"])
        .env_remove("ENVIRA_CATALOG_PATH")
        .output()
        .expect("binary should run");
    let json = parse_stdout_json(&output);
    let bundle_ids = json_string_array(&json["payload"]["catalog"]["bundles"], "id");
    let item_ids = json_string_array(&json["payload"]["catalog"]["items"], "id");

    assert!(output.status.success());
    assert_eq!(json["payload"]["kind"], "catalog");
    assert_eq!(json["payload"]["catalog"]["required_version"], "0.1.0");
    assert_eq!(
        json["payload"]["catalog"]["distros"],
        serde_json::json!([
            "ubuntu",
            "mint",
            "popos",
            "fedora",
            "centos",
            "arch",
            "manjaro",
            "endeavouros",
            "opensuse",
            "opensuse-tumbleweed"
        ])
    );
    assert_eq!(json["payload"]["catalog"]["shell"], "bash");
    assert_eq!(
        json["payload"]["catalog"]["default_bundles"],
        serde_json::json!(["core", "terminal-tools", "observability"])
    );
    assert!(bundle_ids.contains(&"core".to_string()));
    assert!(item_ids.contains(&"essentials".to_string()));
}

#[test]
fn legacy_manifest_override_rejected() {
    let legacy_manifest = repo_path("tests/fixtures/catalog_contract_legacy_shape.toml");
    let output = Command::new(env!("CARGO_BIN_EXE_envira"))
        .args(["catalog", "--format", "json"])
        .env("ENVIRA_CATALOG_PATH", &legacy_manifest)
        .output()
        .expect("binary should run");
    let json = parse_stdout_json(&output);

    assert!(!output.status.success());
    assert_eq!(json["ok"], false);
    assert_eq!(json["error"]["code"], "catalog_invalid");
    assert!(json["error"]["message"]
        .as_str()
        .is_some_and(|message| message.contains("legacy catalog shape is no longer supported")));
    assert_eq!(
        json["error"]["context"]["catalog_path"],
        legacy_manifest.display().to_string()
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
        self.run_with_env(args, &[][..])
    }

    fn run_with_env<const N: usize>(&self, args: [&str; N], extra_env: &[(&str, &str)]) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_envira"));
        command
            .args(args)
            .env("ENVIRA_CATALOG_PATH", &self.manifest_path)
            .env("PATH", &self.path_env);

        for (key, value) in extra_env {
            command.env(key, value);
        }

        command.output().expect("binary should run")
    }

    fn write_manifest(&self, manifest: String) {
        fs::write(&self.manifest_path, manifest).expect("manifest should be updated");
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

fn stdout_string(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).expect("stdout should be valid utf-8")
}

fn test_manifest() -> String {
    test_manifest_with_required_version("0.1.0")
}

fn test_manifest_with_all_user_items() -> String {
    test_manifest()
        .replace(
            "[[items.vnc.recipes]]\nmode = \"sudo\"",
            "[[items.vnc.recipes]]\nmode = \"user\"",
        )
        .replace(
            "[[items.vnc.verifiers]]\nmode = \"sudo\"",
            "[[items.vnc.verifiers]]\nmode = \"user\"",
        )
}

fn test_manifest_with_required_version(required_version: &str) -> String {
    format!(
        r#"
required_version = "{required_version}"
distros = ["ubuntu"]
shell = "bash"
default_bundles = ["headless-tests"]

[items.headless-tool]
name = "Headless Tool"
desc = "Headless tool"
depends_on = []

[[items.headless-tool.recipes]]
mode = "user"
distros = ["ubuntu"]
cmd = "curl -fsSL https://example.com/headless-tool -o ~/.local/bin/{TEST_COMMAND} && chmod +x ~/.local/bin/{TEST_COMMAND}"

[[items.headless-tool.verifiers]]
mode = "user"
distros = ["ubuntu"]
        cmd = "{TEST_COMMAND}"

[bundles.headless-tests]
name = "Headless Tests"
desc = "Headless Tests"
items = ["headless-tool"]

[items.vnc]
name = "TigerVNC"
desc = "TigerVNC"
depends_on = []

[[items.vnc.recipes]]
mode = "sudo"
distros = ["ubuntu"]
cmd = "curl -fsSL https://example.com/vnc -o ~/.local/bin/{TEST_COMMAND} && chmod +x ~/.local/bin/{TEST_COMMAND}"

[[items.vnc.verifiers]]
mode = "sudo"
distros = ["ubuntu"]
        cmd = "{TEST_COMMAND}"

[bundles.remote-access]
name = "Remote Access"
desc = "Remote Access"
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

fn repo_path(relative_path: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(relative_path)
}

fn json_string_array(array: &Value, field: &str) -> Vec<String> {
    array
        .as_array()
        .unwrap_or_else(|| panic!("expected array for field extraction, got: {array:?}"))
        .iter()
        .map(|entry| {
            entry[field]
                .as_str()
                .unwrap_or_else(|| panic!("expected string field `{field}` in {entry:?}"))
                .to_string()
        })
        .collect()
}
