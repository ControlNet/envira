use std::{
    env, fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{self, Command, Output},
    time::{SystemTime, UNIX_EPOCH},
};

use envira::catalog::Catalog;
use serde_json::{json, Value};

const FIXTURE_PATH: &str = "tests/fixtures/launch_parity_container_catalog.toml";
const DEFAULT_BUNDLE_ID: &str = "launch-parity";
const TARGET_ITEM_ID: &str = "shell-path";
const VERIFIED_TOOL: &str = "envira-launch-parity-tool";
const EXPECTED_RECIPE_COMMAND: &str =
    "mkdir -p ~/.local/bin && install -m 755 /bin/sh ~/.local/bin/envira-launch-parity-tool";

#[test]
fn launch_parity_catalog_artifact_matches_keyed_toml_fixture() {
    let fixture = LaunchParityFixture::new(false);
    let catalog = Catalog::from_toml_str(&fixture_catalog_text())
        .expect("launch parity fixture should parse through the keyed TOML catalog runtime");
    let output = fixture.run(["catalog", "--format", "json"]);
    let json = parse_stdout_json(&output);
    let bundle_ids = json_string_array(&json["payload"]["catalog"]["bundles"], "id");
    let item_ids = json_string_array(&json["payload"]["catalog"]["items"], "id");

    assert!(output.status.success());
    assert_eq!(json["payload"]["kind"], "catalog");
    assert_eq!(json["payload"]["catalog"]["required_version"], "0.1.0");
    assert_eq!(json["payload"]["catalog"]["shell"], "bash");
    assert_eq!(
        json["payload"]["catalog"]["distros"],
        json!(["ubuntu", "fedora", "arch", "opensuse-tumbleweed"])
    );
    assert_eq!(
        json["payload"]["catalog"]["default_bundles"],
        json!([DEFAULT_BUNDLE_ID])
    );
    assert_eq!(bundle_ids, vec![DEFAULT_BUNDLE_ID.to_string()]);
    assert_eq!(item_ids, vec![TARGET_ITEM_ID.to_string()]);

    write_json_evidence(
        "task-11-launch-parity-catalog.json",
        &json!({
            "task": 11,
            "kind": "launch_parity_catalog_contract",
            "catalog_source": "envira_catalog_path",
            "catalog_path": FIXTURE_PATH,
            "catalog": {
                "required_version": catalog.required_version,
                "distros": catalog.distros,
                "shell": catalog.shell,
                "default_bundles": catalog
                    .default_bundles
                    .iter()
                    .map(|bundle_id| bundle_id.as_str().to_string())
                    .collect::<Vec<_>>(),
                "bundle_ids": bundle_ids,
                "item_ids": item_ids,
            },
        }),
    );
}

#[test]
fn launch_parity_commands_capture_default_bundle_plan_verify_and_dry_run_install() {
    let preview_fixture = LaunchParityFixture::new(false);
    let plan_output = preview_fixture.run(["plan", "--format", "json"]);
    let plan = parse_stdout_json(&plan_output);
    let install_output = preview_fixture.run(["install", "--dry-run", "--format", "json"]);
    let install = parse_stdout_json(&install_output);

    let verified_fixture = LaunchParityFixture::new(true);
    let verify_output = verified_fixture.run(["verify", "--format", "json"]);
    let verify = parse_stdout_json(&verify_output);

    assert!(plan_output.status.success());
    assert_eq!(plan["payload"]["kind"], "plan");
    assert_eq!(
        selection_count(&plan["payload"]["request"]["selections"]),
        0
    );
    assert_eq!(plan["payload"]["summary"]["requested_items"], 1);
    assert_eq!(plan["payload"]["summary"]["install_items"], 1);
    assert_eq!(plan["payload"]["items"][0]["item_id"], TARGET_ITEM_ID);
    assert_eq!(plan["payload"]["items"][0]["action"], "install");

    assert!(verify_output.status.success());
    assert_eq!(verify["payload"]["kind"], "verify");
    assert_eq!(
        selection_count(&verify["payload"]["request"]["selections"]),
        0
    );
    assert_eq!(verify["payload"]["items"][0]["item_id"], TARGET_ITEM_ID);
    assert_eq!(verify["payload"]["items"][0]["threshold_met"], true);

    assert!(install_output.status.success());
    assert_eq!(install["payload"]["kind"], "install");
    assert_eq!(install["payload"]["install_mode"], "dry_run");
    assert_eq!(install["payload"]["outcome"]["status"], "dry_run");
    assert_eq!(
        install["payload"]["execution"]["steps"][0]["item_id"],
        TARGET_ITEM_ID
    );
    assert_eq!(
        install["payload"]["execution"]["steps"][0]["recipe"]["command"],
        EXPECTED_RECIPE_COMMAND
    );

    write_json_evidence(
        "task-11-launch-parity-commands.json",
        &json!({
            "task": 11,
            "kind": "launch_parity_command_contract",
            "catalog_source": "envira_catalog_path",
            "catalog_path": FIXTURE_PATH,
            "default_bundles": [DEFAULT_BUNDLE_ID],
            "plan": {
                "requested_selection_count": selection_count(&plan["payload"]["request"]["selections"]),
                "requested_items": plan["payload"]["summary"]["requested_items"],
                "install_items": plan["payload"]["summary"]["install_items"],
                "item_id": plan["payload"]["items"][0]["item_id"],
                "action": plan["payload"]["items"][0]["action"],
            },
            "verify": {
                "requested_selection_count": selection_count(&verify["payload"]["request"]["selections"]),
                "item_id": verify["payload"]["items"][0]["item_id"],
                "threshold_met": verify["payload"]["items"][0]["threshold_met"],
            },
            "install": {
                "install_mode": install["payload"]["install_mode"],
                "status": install["payload"]["outcome"]["status"],
                "item_id": install["payload"]["execution"]["steps"][0]["item_id"],
                "recipe_command": install["payload"]["execution"]["steps"][0]["recipe"]["command"],
            },
        }),
    );
}

struct LaunchParityFixture {
    root: PathBuf,
    home_dir: PathBuf,
    manifest_path: PathBuf,
    path_env: String,
}

impl LaunchParityFixture {
    fn new(include_verified_tool: bool) -> Self {
        let root = unique_temp_dir("task11-launch-parity");
        let home_dir = root.join("home/alice");
        let bin_dir = root.join("bin");
        fs::create_dir_all(&home_dir).expect("home directory should be created");
        fs::create_dir_all(&bin_dir).expect("bin directory should be created");

        if include_verified_tool {
            write_executable(&bin_dir.join(VERIFIED_TOOL), "#!/bin/sh\nexit 0\n");
        }

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
            home_dir,
            manifest_path: repo_path(FIXTURE_PATH),
            path_env,
        }
    }

    fn run<const N: usize>(&self, args: [&str; N]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_envira"))
            .args(args)
            .env("ENVIRA_CATALOG_PATH", &self.manifest_path)
            .env("PATH", &self.path_env)
            .env("HOME", &self.home_dir)
            .env("USER", "alice")
            .output()
            .expect("envira command should run")
    }
}

impl Drop for LaunchParityFixture {
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

fn json_string_array(value: &Value, key: &str) -> Vec<String> {
    value
        .as_array()
        .expect("expected array value")
        .iter()
        .map(|entry| {
            entry[key]
                .as_str()
                .expect("expected string field in array entry")
                .to_string()
        })
        .collect()
}

fn selection_count(value: &Value) -> usize {
    value.as_array().map(Vec::len).unwrap_or_default()
}

fn fixture_catalog_text() -> String {
    fs::read_to_string(repo_path(FIXTURE_PATH)).expect("launch parity fixture should be readable")
}

fn write_executable(path: &Path, contents: &str) {
    fs::write(path, contents).expect("executable should be written");
    let mut permissions = fs::metadata(path)
        .expect("executable should exist")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("executable permissions should be set");
}

fn write_json_evidence(file_name: &str, value: &Value) {
    let path = evidence_path(file_name);
    fs::write(
        &path,
        format!(
            "{}\n",
            serde_json::to_string_pretty(value).expect("evidence should serialize")
        ),
    )
    .expect("evidence file should be written");
}

fn evidence_path(file_name: &str) -> PathBuf {
    let path = repo_path(".sisyphus/evidence").join(file_name);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("evidence directory should exist");
    }
    path
}

fn repo_path(relative_path: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(relative_path)
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
