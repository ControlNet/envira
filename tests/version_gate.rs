use std::{
    env, fs,
    io::{Read, Write},
    net::TcpListener,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{self, Command, Output},
    thread,
    time::{SystemTime, UNIX_EPOCH},
};

use serde_json::Value;

#[test]
fn satisfied_required_version_allows_normal_execution_without_update_attempt() {
    let fixture = VersionGateFixture::new(plan_manifest("0.1.0"));
    let update_marker = fixture.root.join("update-called");
    let wrapper_path = fixture.root.join("unexpected-update.sh");

    write_executable(
        &wrapper_path,
        format!(
            "#!/usr/bin/env bash\nset -euo pipefail\ntouch \"{}\"\nexit 80\n",
            update_marker.display()
        )
        .as_str(),
    );

    let output = fixture
        .command(["plan", "--format", "json"])
        .env("ENVIRA_CURRENT_VERSION", "0.1.0")
        .env("ENVIRA_UPDATE_WRAPPER_PATH", &wrapper_path)
        .output()
        .expect("binary should run");
    let json = parse_stdout_json(&output);

    assert!(output.status.success());
    assert_eq!(json["ok"], true);
    assert_eq!(json["payload"]["kind"], "plan");
    assert!(
        !update_marker.exists(),
        "version-satisfied execution must not attempt the updater"
    );
}

#[test]
fn unsatisfied_required_version_hands_off_through_wrapper() {
    let fixture = VersionGateFixture::new(plan_manifest("0.2.0"));
    let wrapper_path = repo_path("envira.sh");
    let server = TestServer::spawn(vec![
        ("/envira".to_string(), updated_binary()),
        ("/envira.sha256".to_string(), valid_checksum_manifest()),
    ]);

    let output = fixture
        .command(["plan", "--format", "json"])
        .env("ENVIRA_CURRENT_VERSION", "0.1.0")
        .env("ENVIRA_UPDATE_WRAPPER_PATH", &wrapper_path)
        .env("ENVIRA_BOOTSTRAP_BASE_URL", server.base_url())
        .output()
        .expect("binary should run");
    let stdout = parse_stdout_json(&output);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success(),
        "updated handoff should succeed: {stderr}"
    );
    assert_eq!(stdout["updated"], true);
    assert_eq!(stdout["current_version_env"], "unset");
    assert!(stderr.contains("Handing off to"));
    assert!(
        fixture.install_path().exists(),
        "wrapper should install the refreshed binary"
    );
}

#[test]
fn invalid_required_version_fails_fast() {
    let fixture = VersionGateFixture::new(plan_manifest("banana"));

    let output = fixture
        .command(["plan", "--format", "json"])
        .output()
        .expect("binary should run");
    let json = parse_stdout_json(&output);

    assert!(!output.status.success());
    assert_eq!(json["ok"], false);
    assert_eq!(json["error"]["code"], "catalog_required_version_invalid");
    assert!(json["error"]["message"]
        .as_str()
        .is_some_and(|message| message.contains("major.minor.patch")));
}

#[test]
fn catalog_prerelease_required_version_is_rejected() {
    let fixture = VersionGateFixture::new(plan_manifest("0.2.0-beta.1"));

    let output = fixture
        .command(["plan", "--format", "json"])
        .output()
        .expect("binary should run");
    let json = parse_stdout_json(&output);

    assert!(!output.status.success());
    assert_eq!(json["ok"], false);
    assert_eq!(
        json["error"]["code"],
        "catalog_required_version_prerelease_unsupported"
    );
    assert_eq!(json["error"]["context"]["required_version"], "0.2.0-beta.1");
}

#[test]
fn binary_prerelease_is_rejected() {
    let fixture = VersionGateFixture::new(plan_manifest("0.1.0"));

    let output = fixture
        .command(["plan", "--format", "json"])
        .env("ENVIRA_CURRENT_VERSION", "0.1.0-beta.1")
        .output()
        .expect("binary should run");
    let json = parse_stdout_json(&output);

    assert!(!output.status.success());
    assert_eq!(json["ok"], false);
    assert_eq!(
        json["error"]["code"],
        "envira_binary_prerelease_unsupported"
    );
    assert_eq!(json["error"]["context"]["current_version"], "0.1.0-beta.1");
}

#[test]
fn updater_failure_blocks_execution() {
    let fixture = VersionGateFixture::new(install_manifest("0.2.0", fixture_marker_path_seed()));
    let marker_path = fixture.root.join("install-ran");
    fixture.write_manifest(install_manifest("0.2.0", &marker_path));

    let output = fixture
        .command(["install", "--format", "json"])
        .env("ENVIRA_CURRENT_VERSION", "0.1.0")
        .env("ENVIRA_UPDATE_WRAPPER_PATH", repo_path("envira.sh"))
        .env("ENVIRA_BOOTSTRAP_BASE_URL", "http://127.0.0.1:9")
        .output()
        .expect("binary should run");
    let json = parse_stdout_json(&output);

    assert!(!output.status.success());
    assert_eq!(json["ok"], false);
    assert_eq!(json["error"]["code"], "envira_auto_update_failed");
    assert_eq!(json["error"]["context"]["current_version"], "0.1.0");
    assert_eq!(json["error"]["context"]["required_version"], "0.2.0");
    assert_eq!(json["error"]["context"]["exit_code"], "80");
    assert!(json["error"]["context"]["detail"]
        .as_str()
        .is_some_and(|detail| detail.starts_with("[ERROR]")));
    assert!(
        !marker_path.exists(),
        "normal catalog execution must stay blocked when the updater fails"
    );
}

struct VersionGateFixture {
    root: PathBuf,
    home: PathBuf,
    manifest_path: PathBuf,
}

impl VersionGateFixture {
    fn new(manifest: String) -> Self {
        let root = unique_temp_dir("version-gate");
        let home = root.join("home");
        fs::create_dir_all(&home).expect("home directory should be created");

        let manifest_path = root.join("catalog.toml");
        fs::write(&manifest_path, manifest).expect("manifest should be written");

        Self {
            root,
            home,
            manifest_path,
        }
    }

    fn command<const N: usize>(&self, args: [&str; N]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_envira"));
        command
            .args(args)
            .env("ENVIRA_CATALOG_PATH", &self.manifest_path)
            .env("HOME", &self.home);
        command
    }

    fn install_path(&self) -> PathBuf {
        self.home.join(".local/bin/envira")
    }

    fn write_manifest(&self, manifest: String) {
        fs::write(&self.manifest_path, manifest).expect("manifest should be updated");
    }
}

impl Drop for VersionGateFixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

struct TestServer {
    base_url: String,
    join_handle: Option<thread::JoinHandle<()>>,
}

impl TestServer {
    fn spawn(routes: Vec<(String, Vec<u8>)>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("test server should bind");
        let address = listener
            .local_addr()
            .expect("test server address should exist");
        let base_url = format!("http://{}", address);

        let join_handle = thread::spawn(move || {
            for _ in 0..routes.len() {
                let (mut stream, _) = listener.accept().expect("test server should accept");
                let mut request_buffer = [0_u8; 4096];
                let bytes_read = stream
                    .read(&mut request_buffer)
                    .expect("request should be readable");
                let request = String::from_utf8_lossy(&request_buffer[..bytes_read]);
                let request_line = request.lines().next().expect("request line should exist");
                let path = request_line
                    .split_whitespace()
                    .nth(1)
                    .expect("request path should exist");

                let response_body = routes
                    .iter()
                    .find(|(candidate, _)| candidate == path)
                    .map(|(_, body)| body.as_slice())
                    .unwrap_or(b"not found");
                let status_line = if response_body == b"not found" {
                    "HTTP/1.1 404 Not Found"
                } else {
                    "HTTP/1.1 200 OK"
                };

                write!(
                    stream,
                    "{status_line}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    response_body.len()
                )
                .expect("response headers should be written");
                stream
                    .write_all(response_body)
                    .expect("response body should be written");
            }
        });

        Self {
            base_url,
            join_handle: Some(join_handle),
        }
    }

    fn base_url(&self) -> &str {
        &self.base_url
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        if let Some(join_handle) = self.join_handle.take() {
            join_handle
                .join()
                .expect("test server should shut down cleanly");
        }
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

fn plan_manifest(required_version: &str) -> String {
    format!(
        r#"
required_version = "{required_version}"
distros = ["ubuntu"]
shell = "bash"
default_bundles = ["core"]

[items.tool]
name = "Tool"
desc = "Tool"
depends_on = []

[[items.tool.recipes]]
mode = "user"
distros = ["ubuntu"]
cmd = "printf 'noop' >/dev/null"

[[items.tool.verifiers]]
mode = "user"
distros = ["ubuntu"]
cmd = "command -v definitely-missing-version-gate-tool"

[bundles.core]
name = "Core"
desc = "Core"
items = ["tool"]
"#
    )
}

fn install_manifest(required_version: &str, marker_path: &Path) -> String {
    format!(
        r#"
required_version = "{required_version}"
distros = ["ubuntu"]
shell = "bash"
default_bundles = ["core"]

[items.tool]
name = "Tool"
desc = "Tool"
depends_on = []

[[items.tool.recipes]]
mode = "user"
distros = ["ubuntu"]
cmd = "touch {}"

[[items.tool.verifiers]]
mode = "user"
distros = ["ubuntu"]
cmd = "test -f {}"

[bundles.core]
name = "Core"
desc = "Core"
items = ["tool"]
"#,
        marker_path.display(),
        marker_path.display()
    )
}

fn fixture_marker_path_seed() -> &'static Path {
    Path::new("/tmp/envira-version-gate-placeholder")
}

fn updated_binary() -> Vec<u8> {
    b"#!/usr/bin/env bash\nset -euo pipefail\nprintf '{\"updated\":true,\"current_version_env\":\"%s\"}\\n' \"${ENVIRA_CURRENT_VERSION-unset}\"\n".to_vec()
}

fn valid_checksum_manifest() -> Vec<u8> {
    let binary_path = write_temp_file("version-gate-updated-binary", &updated_binary());
    let checksum = sha256_for_file(&binary_path);
    fs::remove_file(binary_path).expect("temporary checksum input should be removable");
    format!("{checksum}  envira\n").into_bytes()
}

fn sha256_for_file(path: &Path) -> String {
    let output = Command::new("sha256sum")
        .arg(path)
        .output()
        .expect("sha256sum should be available in test environment");
    assert!(output.status.success(), "sha256sum should succeed");

    String::from_utf8_lossy(&output.stdout)
        .split_whitespace()
        .next()
        .expect("sha256sum output should contain a checksum")
        .to_string()
}

fn write_temp_file(label: &str, contents: &[u8]) -> PathBuf {
    let path = unique_temp_dir(label).join("artifact.bin");
    fs::write(&path, contents).expect("temporary artifact should be written");
    path
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
