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

#[test]
fn wrapper_downloads_verifies_and_execs_binary_with_passthrough_args() {
    let fixture = WrapperFixture::new(valid_checksum_manifest());

    let output = fixture.run_wrapper(["--run", "--", "catalog", "--format", "json"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let installed_path = fixture.install_path();

    assert!(output.status.success(), "wrapper should succeed: {stderr}");
    assert_eq!(stdout.trim(), r#"{"argv":["catalog","--format","json"]}"#);
    assert!(stderr.contains("Verifying release checksum"));
    assert!(stderr.contains("Handing off to"));
    assert!(installed_path.exists(), "binary should be installed");

    let mode = fs::metadata(installed_path)
        .expect("installed binary metadata should exist")
        .permissions()
        .mode();
    assert_ne!(mode & 0o111, 0, "installed binary should be executable");
}

#[test]
fn wrapper_stops_before_exec_when_checksum_does_not_match() {
    let fixture = WrapperFixture::new(invalid_checksum_manifest());

    let output = fixture.run_wrapper(["--run", "--", "catalog", "--format", "json"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        !output.status.success(),
        "wrapper should fail checksum validation"
    );
    assert!(
        stdout.trim().is_empty(),
        "binary stdout should stay empty on failure"
    );
    assert!(
        stderr.contains("Integrity check failed"),
        "stderr should explain the checksum failure: {stderr}"
    );
    assert!(
        !fixture.install_path().exists(),
        "binary should not be installed after integrity failure"
    );
}

struct WrapperFixture {
    root: PathBuf,
    home: PathBuf,
    server: TestServer,
}

impl WrapperFixture {
    fn new(checksum_manifest: Vec<u8>) -> Self {
        let root = unique_temp_dir("wrapper-bootstrap");
        let home = root.join("home");
        fs::create_dir_all(&home).expect("home directory should be created");

        let binary_bytes = bootstrap_binary();
        let server = TestServer::spawn(vec![
            ("/envira".to_string(), binary_bytes),
            ("/envira.sha256".to_string(), checksum_manifest),
        ]);

        Self { root, home, server }
    }

    fn install_path(&self) -> PathBuf {
        self.home.join(".local/bin/envira")
    }

    fn run_wrapper<const N: usize>(&self, args: [&str; N]) -> Output {
        Command::new("bash")
            .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("envira.sh"))
            .args(args)
            .env("HOME", &self.home)
            .env("ENVIRA_BOOTSTRAP_BASE_URL", self.server.base_url())
            .output()
            .expect("wrapper should run")
    }
}

impl Drop for WrapperFixture {
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

fn bootstrap_binary() -> Vec<u8> {
    b"#!/usr/bin/env bash\nset -euo pipefail\nprintf '{\"argv\":[\"%s\",\"%s\",\"%s\"]}\\n' \"${1:-}\" \"${2:-}\" \"${3:-}\"\n".to_vec()
}

fn valid_checksum_manifest() -> Vec<u8> {
    let binary_path = write_temp_file("wrapper-bootstrap-binary", &bootstrap_binary());
    let checksum = sha256_for_file(&binary_path);
    fs::remove_file(binary_path).expect("temporary checksum input should be removable");
    format!("{checksum}  envira\n").into_bytes()
}

fn invalid_checksum_manifest() -> Vec<u8> {
    b"0000000000000000000000000000000000000000000000000000000000000000  envira\n".to_vec()
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
