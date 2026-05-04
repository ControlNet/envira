use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process;
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use envira::catalog::TargetBackend;
use envira::platform::{
    ArchitectureIdentity, ArchitectureKind, DistroIdentity, DistroKind, InvocationKind,
    PlatformContext, RuntimeScope, UserAccount,
};
use envira::verifier::{
    verify_with_context, ProbeKind, ProbeRequirement, ServiceKind, ServiceManagerScope,
    ServiceUsabilityState, ServiceVerificationSpec, VerificationContext, VerificationProfile,
    VerificationStage, VerifierCheck, VerifierSpec,
};

#[test]
fn docker_service_reports_blocked_when_socket_exists_but_access_is_denied() {
    let harness = TestHarness::new();
    let docker_socket = harness.root.join("docker.sock");
    let _socket = UnixListener::bind(&docker_socket).expect("docker socket should bind");
    harness.write_executable(
        "docker",
        "#!/bin/bash\nprintf 'permission denied while trying to connect to the docker daemon socket\n' >&2\nexit 1\n",
    );

    let result = verify_with_context(
        VerificationStage::Operational,
        &VerifierSpec {
            checks: vec![command_check("command -v docker")],
            service: Some(ServiceVerificationSpec {
                kind: ServiceKind::Docker,
                command: Some("command -v docker".to_string()),
                commands: Vec::new(),
                service_unit: None,
                service_scope: None,
                socket_paths: vec![docker_socket],
                access_group: Some("envira-missing-docker-group".to_string()),
                http_url: None,
                tcp_host: None,
                tcp_port: None,
            }),
        },
        &harness.context(),
    )
    .expect("docker verification should complete")
    .result;

    let service = result
        .service
        .as_ref()
        .expect("service assessment should be present");
    assert_eq!(service.kind, ServiceKind::Docker);
    assert_eq!(service.state, ServiceUsabilityState::Blocked);
    assert_eq!(service.achieved_stage, Some(VerificationStage::Present));
    assert!(!result.threshold_met);

    let json = serde_json::to_value(&result).expect("result should serialize");
    assert_eq!(json["service"]["state"], "blocked");
    assert_eq!(json["service"]["achieved_stage"], "present");
}

#[test]
fn plain_shell_contract_docker_verifier_still_derives_blocked_service_readiness() {
    let harness = TestHarness::new();
    let docker_socket = harness.root.join("docker.sock");
    let _socket = UnixListener::bind(&docker_socket).expect("docker socket should bind");
    harness.write_executable(
        "docker",
        "#!/bin/bash\nprintf 'permission denied while trying to connect to the docker daemon socket\n' >&2\nexit 1\n",
    );

    let result = verify_with_context(
        VerificationStage::Present,
        &VerifierSpec {
            checks: vec![command_check("command -v docker")],
            service: None,
        },
        &harness.context(),
    )
    .expect("docker verification should complete")
    .result;

    let service = result
        .service
        .as_ref()
        .expect("service assessment should be derived from the shell contract");
    assert_eq!(service.kind, ServiceKind::Docker);
    assert_eq!(service.state, ServiceUsabilityState::Blocked);
    assert_eq!(service.achieved_stage, Some(VerificationStage::Present));
    assert!(!result.threshold_met);
}

#[test]
fn jupyter_service_reports_on_demand_when_user_unit_exists_but_http_is_down() {
    let harness = TestHarness::new();
    let port = allocate_unused_port();
    harness.write_executable("jupyter", "#!/bin/bash\nexit 0\n");
    let units: [(&str, ServiceManagerScope, &str, &str, &str, &str); 1] = [(
        "jupyter.service",
        ServiceManagerScope::User,
        "loaded",
        "inactive",
        "enabled",
        "dead",
    )];
    harness.write_systemctl_fixture(&units[..]);

    let result = verify_with_context(
        VerificationStage::Operational,
        &VerifierSpec {
            checks: vec![command_check("command -v jupyter")],
            service: Some(ServiceVerificationSpec {
                kind: ServiceKind::Jupyter,
                command: Some("command -v jupyter".to_string()),
                commands: Vec::new(),
                service_unit: Some("jupyter.service".to_string()),
                service_scope: Some(ServiceManagerScope::User),
                socket_paths: Vec::new(),
                access_group: None,
                http_url: Some(format!("http://127.0.0.1:{port}/")),
                tcp_host: None,
                tcp_port: None,
            }),
        },
        &harness.context_with_systemctl(),
    )
    .expect("jupyter verification should complete")
    .result;

    let service = result
        .service
        .as_ref()
        .expect("service assessment should be present");
    assert_eq!(service.kind, ServiceKind::Jupyter);
    assert_eq!(service.state, ServiceUsabilityState::OnDemand);
    assert_eq!(service.achieved_stage, Some(VerificationStage::Configured));
    assert!(!result.threshold_met);
    assert!(result
        .service_evidence
        .iter()
        .any(|entry| entry.id == "unit"
            && entry.record.observed_scope == envira::verifier::ObservedScope::User));
}

#[test]
fn jupyter_service_reaches_operational_when_user_unit_and_http_endpoint_are_live() {
    let harness = TestHarness::new();
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("http listener should bind");
    let port = listener
        .local_addr()
        .expect("listener addr should exist")
        .port();
    harness.write_executable("jupyter", "#!/bin/bash\nexit 0\n");
    let units: [(&str, ServiceManagerScope, &str, &str, &str, &str); 1] = [(
        "jupyter.service",
        ServiceManagerScope::User,
        "loaded",
        "active",
        "enabled",
        "running",
    )];
    harness.write_systemctl_fixture(&units[..]);

    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("http client should connect");
        let mut buffer = [0_u8; 1024];
        let _ = stream.read(&mut buffer);
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .expect("http response should be written");
    });

    let result = verify_with_context(
        VerificationStage::Operational,
        &VerifierSpec {
            checks: vec![command_check("command -v jupyter")],
            service: Some(ServiceVerificationSpec {
                kind: ServiceKind::Jupyter,
                command: Some("command -v jupyter".to_string()),
                commands: Vec::new(),
                service_unit: Some("jupyter.service".to_string()),
                service_scope: Some(ServiceManagerScope::User),
                socket_paths: Vec::new(),
                access_group: None,
                http_url: Some(format!("http://127.0.0.1:{port}/")),
                tcp_host: None,
                tcp_port: None,
            }),
        },
        &harness.context_with_systemctl(),
    )
    .expect("jupyter verification should complete")
    .result;
    server.join().expect("http server should complete");

    let service = result
        .service
        .as_ref()
        .expect("service assessment should be present");
    assert_eq!(service.state, ServiceUsabilityState::Operational);
    assert_eq!(service.achieved_stage, Some(VerificationStage::Operational));
    assert!(result.threshold_met);
}

#[test]
fn jupyter_service_does_not_claim_configuration_when_the_user_unit_is_missing() {
    let harness = TestHarness::new();
    let port = allocate_unused_port();
    harness.write_executable("jupyter", "#!/bin/bash\nexit 0\n");
    let units: [(&str, ServiceManagerScope, &str, &str, &str, &str); 0] = [];
    harness.write_systemctl_fixture(&units[..]);

    let result = verify_with_context(
        VerificationStage::Operational,
        &VerifierSpec {
            checks: vec![command_check("command -v jupyter")],
            service: Some(ServiceVerificationSpec {
                kind: ServiceKind::Jupyter,
                command: Some("command -v jupyter".to_string()),
                commands: Vec::new(),
                service_unit: Some("jupyter.service".to_string()),
                service_scope: Some(ServiceManagerScope::User),
                socket_paths: Vec::new(),
                access_group: None,
                http_url: Some(format!("http://127.0.0.1:{port}/")),
                tcp_host: None,
                tcp_port: None,
            }),
        },
        &harness.context_with_systemctl(),
    )
    .expect("jupyter verification should complete")
    .result;

    let service = result
        .service
        .as_ref()
        .expect("service assessment should be present");
    assert_eq!(service.kind, ServiceKind::Jupyter);
    assert_eq!(service.state, ServiceUsabilityState::Missing);
    assert_eq!(service.achieved_stage, Some(VerificationStage::Present));
    assert!(!result.threshold_met);
}

#[test]
fn pm2_service_reports_non_usable_when_daemon_artifacts_exist_but_ping_fails() {
    let harness = TestHarness::new();
    let pm2_dir = harness.home_dir.join(".pm2");
    fs::create_dir_all(&pm2_dir).expect("pm2 dir should exist");
    let rpc_socket = pm2_dir.join("rpc.sock");
    let pub_socket = pm2_dir.join("pub.sock");
    let _rpc = UnixListener::bind(&rpc_socket).expect("pm2 rpc socket should bind");
    let _pub = UnixListener::bind(&pub_socket).expect("pm2 pub socket should bind");
    harness.write_executable(
        "pm2",
        "#!/bin/bash\nprintf 'daemon unreachable\n' >&2\nexit 1\n",
    );

    let result = verify_with_context(
        VerificationStage::Operational,
        &VerifierSpec {
            checks: vec![command_check("command -v pm2")],
            service: Some(ServiceVerificationSpec {
                kind: ServiceKind::Pm2,
                command: Some("command -v pm2".to_string()),
                commands: Vec::new(),
                service_unit: None,
                service_scope: None,
                socket_paths: vec![rpc_socket, pub_socket],
                access_group: None,
                http_url: None,
                tcp_host: None,
                tcp_port: None,
            }),
        },
        &harness.context(),
    )
    .expect("pm2 verification should complete")
    .result;

    let service = result
        .service
        .as_ref()
        .expect("service assessment should be present");
    assert_eq!(service.kind, ServiceKind::Pm2);
    assert_eq!(service.state, ServiceUsabilityState::NonUsable);
    assert_eq!(service.achieved_stage, Some(VerificationStage::Configured));
    assert!(!result.threshold_met);
}

#[test]
fn vnc_service_requires_a_live_endpoint_for_operational_success() {
    let harness = TestHarness::new();
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("vnc listener should bind");
    let port = listener.local_addr().expect("vnc addr should exist").port();
    harness.write_executable("vncserver", "#!/bin/bash\nexit 0\n");

    let result = verify_with_context(
        VerificationStage::Operational,
        &VerifierSpec {
            checks: vec![command_check("command -v vncserver")],
            service: Some(ServiceVerificationSpec {
                kind: ServiceKind::Vnc,
                command: Some("command -v vncserver".to_string()),
                commands: Vec::new(),
                service_unit: None,
                service_scope: None,
                socket_paths: Vec::new(),
                access_group: None,
                http_url: None,
                tcp_host: Some("127.0.0.1".to_string()),
                tcp_port: Some(port),
            }),
        },
        &harness.context(),
    )
    .expect("vnc verification should complete")
    .result;

    drop(listener);

    let service = result
        .service
        .as_ref()
        .expect("service assessment should be present");
    assert_eq!(service.kind, ServiceKind::Vnc);
    assert_eq!(service.state, ServiceUsabilityState::Operational);
    assert_eq!(service.achieved_stage, Some(VerificationStage::Operational));
    assert!(result.threshold_met);
}

#[test]
fn vnc_service_does_not_claim_configuration_when_the_service_unit_is_missing() {
    let harness = TestHarness::new();
    let port = allocate_unused_port();
    harness.write_executable("vncserver", "#!/bin/bash\nexit 0\n");
    let units: [(&str, ServiceManagerScope, &str, &str, &str, &str); 0] = [];
    harness.write_systemctl_fixture(&units[..]);

    let result = verify_with_context(
        VerificationStage::Operational,
        &VerifierSpec {
            checks: vec![command_check("command -v vncserver")],
            service: Some(ServiceVerificationSpec {
                kind: ServiceKind::Vnc,
                command: Some("command -v vncserver".to_string()),
                commands: Vec::new(),
                service_unit: Some("vncserver@:1.service".to_string()),
                service_scope: Some(ServiceManagerScope::System),
                socket_paths: Vec::new(),
                access_group: None,
                http_url: None,
                tcp_host: Some("127.0.0.1".to_string()),
                tcp_port: Some(port),
            }),
        },
        &harness.context_with_systemctl(),
    )
    .expect("vnc verification should complete")
    .result;

    let service = result
        .service
        .as_ref()
        .expect("service assessment should be present");
    assert_eq!(service.kind, ServiceKind::Vnc);
    assert_eq!(service.state, ServiceUsabilityState::Missing);
    assert_eq!(service.achieved_stage, Some(VerificationStage::Present));
    assert!(!result.threshold_met);
}

fn command_check(command: &str) -> VerifierCheck {
    VerifierCheck {
        stage: VerificationStage::Present,
        requirement: ProbeRequirement::Required,
        min_profile: VerificationProfile::Quick,
        kind: ProbeKind::Command,
        command: Some(command.to_string()),
        commands: None,
        path: None,
        pattern: None,
    }
}

struct TestHarness {
    root: PathBuf,
    home_dir: PathBuf,
    bin_dir: PathBuf,
}

impl TestHarness {
    fn new() -> Self {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be available")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "envira-verifier-services-{}-{}",
            process::id(),
            unique
        ));
        let home_dir = root.join("home/alice");
        let bin_dir = home_dir.join(".local/bin");
        fs::create_dir_all(&bin_dir).expect("bin dir should be created");

        Self {
            root,
            home_dir,
            bin_dir,
        }
    }

    fn context(&self) -> VerificationContext {
        VerificationContext::new(platform_context(&self.home_dir), VerificationProfile::Quick)
            .with_search_paths(vec![self.bin_dir.clone()])
    }

    fn context_with_systemctl(&self) -> VerificationContext {
        self.context()
    }

    fn write_executable(&self, name: &str, contents: &str) {
        let path = self.bin_dir.join(name);
        fs::write(&path, contents).expect("script should be written");
        let mut permissions = fs::metadata(&path)
            .expect("script metadata should exist")
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&path, permissions).expect("script should be executable");
    }

    fn write_systemctl_fixture(
        &self,
        units: &[(&str, ServiceManagerScope, &str, &str, &str, &str)],
    ) {
        let mut script = String::from("#!/bin/bash\nunit=\"${@: -1}\"\nmode=system\nif [[ \"$1\" == \"--user\" ]]; then\n  mode=user\nfi\ncase \"$unit:$mode\" in\n");
        for (unit, scope, load_state, active_state, unit_file_state, sub_state) in units {
            let mode = match scope {
                ServiceManagerScope::System => "system",
                ServiceManagerScope::User => "user",
            };
            script.push_str(&format!(
                "  \"{unit}:{mode}\")\n    printf '{load_state}\\n{active_state}\\n{unit_file_state}\\n{sub_state}\\n'\n    ;;\n"
            ));
        }
        script.push_str(
            "  *)\n    printf 'not-found\\ninactive\\ndisabled\\ndead\\n'\n    ;;\nesac\n",
        );
        self.write_executable("systemctl", &script);
    }
}

impl Drop for TestHarness {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn allocate_unused_port() -> u16 {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("temp port should bind");
    listener
        .local_addr()
        .expect("temp port should have an address")
        .port()
}

fn platform_context(home_dir: &Path) -> PlatformContext {
    let user = UserAccount {
        username: "alice".to_string(),
        home_dir: home_dir.to_path_buf(),
        uid: Some(1000),
        gid: Some(1000),
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
        native_backend: Some(TargetBackend::Apt),
        invocation: InvocationKind::User,
        effective_user: user.clone(),
        target_user: Some(user),
        runtime_scope: RuntimeScope::User,
    }
}
