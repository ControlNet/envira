use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::os::unix::fs::symlink;
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
    aggregate_verifier_evidence, verify_with_context, AnyCommandProbe, CommandExecutionProbe,
    CommandExistsProbe, ContainsProbe, DirectoryProbe, EvidenceStatus, FileProbe,
    GroupMembershipProbe, HttpProbe, ProbeKind, ProbeRequirement, ProbeSpec, ServiceManagerScope,
    ServiceUnitCondition, ServiceUnitProbe, SymlinkTargetProbe, TcpProbe, UnixSocketProbe,
    VerificationContext, VerificationProfile, VerificationStage, VerifierCheck,
    VerifierProbeRunner, VerifierSpec,
};

#[test]
fn command_and_any_command_probes_use_verifier_search_paths() {
    let harness = TestHarness::new();
    harness.write_executable("alpha-tool", "#!/bin/sh\nprintf alpha\n");

    let context = harness.context();
    let runner = VerifierProbeRunner::default();

    let command = runner.run_probe(
        &ProbeSpec::CommandExists(CommandExistsProbe {
            command: "alpha-tool".to_string(),
        }),
        &context,
    );
    assert_eq!(command.status, EvidenceStatus::Satisfied);
    assert_eq!(
        command.observed_scope,
        envira::verifier::ObservedScope::User
    );
    assert_eq!(
        command.detail.as_deref(),
        Some(
            harness
                .bin_dir
                .join("alpha-tool")
                .to_string_lossy()
                .as_ref()
        )
    );

    let any = runner.run_probe(
        &ProbeSpec::AnyCommand(AnyCommandProbe {
            commands: vec!["missing-tool".to_string(), "alpha-tool".to_string()],
        }),
        &context,
    );
    assert_eq!(any.status, EvidenceStatus::Satisfied);
    assert!(any.detail.unwrap_or_default().contains("alpha-tool"));
}

#[test]
fn command_execution_probe_runs_read_only_subprocesses() {
    let harness = TestHarness::new();
    harness.write_executable("probe-ok", "#!/bin/sh\nprintf ready\n");

    let context = harness.context();
    let runner = VerifierProbeRunner::default();
    let record = runner.run_probe(
        &ProbeSpec::CommandExecution(CommandExecutionProbe {
            program: "probe-ok".to_string(),
            args: Vec::new(),
            timeout_ms: Some(1_000),
        }),
        &context,
    );

    assert_eq!(record.status, EvidenceStatus::Satisfied);
    assert!(record.detail.unwrap_or_default().contains("ready"));
}

#[test]
fn file_directory_and_contains_probes_distinguish_presence_from_content_mismatches() {
    let harness = TestHarness::new();
    let config_dir = harness.home_dir.join(".config/tool");
    fs::create_dir_all(&config_dir).expect("config dir should exist");
    let config_path = config_dir.join("config.toml");
    fs::write(&config_path, "enabled = true\nmode = \"strict\"\n")
        .expect("config file should exist");

    let context = harness.context();
    let runner = VerifierProbeRunner::default();

    let directory = runner.run_probe(
        &ProbeSpec::Directory(DirectoryProbe {
            path: config_dir.clone(),
        }),
        &context,
    );
    assert_eq!(directory.status, EvidenceStatus::Satisfied);

    let file = runner.run_probe(
        &ProbeSpec::File(FileProbe {
            path: config_path.clone(),
        }),
        &context,
    );
    assert_eq!(file.status, EvidenceStatus::Satisfied);

    let contains = runner.run_probe(
        &ProbeSpec::Contains(ContainsProbe {
            path: config_path.clone(),
            pattern: r#"enabled\s*=\s*true"#.to_string(),
        }),
        &context,
    );
    assert_eq!(contains.status, EvidenceStatus::Satisfied);

    let mismatch = runner.run_probe(
        &ProbeSpec::Contains(ContainsProbe {
            path: config_path,
            pattern: r#"enabled\s*=\s*false"#.to_string(),
        }),
        &context,
    );
    assert_eq!(mismatch.status, EvidenceStatus::Broken);
}

#[test]
fn symlink_group_membership_and_unix_socket_probes_use_local_fixtures() {
    let harness = TestHarness::new();
    let target_path = harness.home_dir.join("target.txt");
    fs::write(&target_path, "fixture\n").expect("target file should exist");

    let symlink_path = harness.home_dir.join("target-link");
    symlink(&target_path, &symlink_path).expect("symlink should be created");

    let socket_path = harness.home_dir.join("probe.sock");
    let _listener = UnixListener::bind(&socket_path).expect("unix socket should bind");

    let context = harness.context();
    let runner = VerifierProbeRunner::default();

    let symlink_record = runner.run_probe(
        &ProbeSpec::SymlinkTarget(SymlinkTargetProbe {
            path: symlink_path,
            target: target_path,
        }),
        &context,
    );
    assert_eq!(symlink_record.status, EvidenceStatus::Satisfied);

    let socket_record = runner.run_probe(
        &ProbeSpec::UnixSocket(UnixSocketProbe { path: socket_path }),
        &context,
    );
    assert_eq!(socket_record.status, EvidenceStatus::Satisfied);

    let group_context =
        VerificationContext::new(root_platform_context(), VerificationProfile::Quick)
            .with_search_paths(vec![harness.bin_dir.clone()]);
    let group_record = runner.run_probe(
        &ProbeSpec::GroupMembership(GroupMembershipProbe {
            group: "root".to_string(),
            username: Some("root".to_string()),
        }),
        &group_context,
    );
    assert_eq!(group_record.status, EvidenceStatus::Satisfied);
}

#[test]
fn tcp_http_and_service_unit_probes_are_testable_without_external_dependencies() {
    let harness = TestHarness::new();
    let context = harness.context();
    let runner = VerifierProbeRunner::default();

    let tcp_listener = TcpListener::bind(("127.0.0.1", 0)).expect("tcp listener should bind");
    let tcp_port = tcp_listener
        .local_addr()
        .expect("tcp addr should be available")
        .port();
    let tcp_record = runner.run_probe(
        &ProbeSpec::Tcp(TcpProbe {
            host: "127.0.0.1".to_string(),
            port: tcp_port,
            timeout_ms: Some(500),
        }),
        &context,
    );
    assert_eq!(tcp_record.status, EvidenceStatus::Satisfied);
    drop(tcp_listener);

    let http_listener = TcpListener::bind(("127.0.0.1", 0)).expect("http listener should bind");
    let http_port = http_listener
        .local_addr()
        .expect("http addr should be available")
        .port();
    let server = thread::spawn(move || {
        let (mut stream, _) = http_listener.accept().expect("http client should connect");
        let mut buffer = [0_u8; 1024];
        let _ = stream.read(&mut buffer);
        stream
            .write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .expect("http response should be written");
    });
    let http_record = runner.run_probe(
        &ProbeSpec::Http(HttpProbe {
            url: format!("http://127.0.0.1:{http_port}/health"),
            expected_status: Some(204),
            timeout_ms: Some(500),
        }),
        &context,
    );
    server.join().expect("http server thread should finish");
    assert_eq!(http_record.status, EvidenceStatus::Satisfied);

    let no_systemctl_context = VerificationContext::new(
        platform_context(&harness.home_dir),
        VerificationProfile::Quick,
    )
    .with_search_paths(vec![harness.bin_dir.clone()]);
    let service_record = runner.run_probe(
        &ProbeSpec::ServiceUnit(ServiceUnitProbe {
            unit: "envira-test.service".to_string(),
            scope: ServiceManagerScope::System,
            condition: ServiceUnitCondition::Exists,
            timeout_ms: Some(500),
        }),
        &no_systemctl_context,
    );
    assert_eq!(service_record.status, EvidenceStatus::Unknown);
    assert_eq!(
        service_record.detail.as_deref(),
        Some("systemctl not found")
    );
}

#[test]
fn evidence_aggregation_collects_all_probe_records_before_reduction() {
    let harness = TestHarness::new();
    harness.write_executable("alpha-tool", "#!/bin/sh\nprintf alpha\n");
    let config_path = harness.home_dir.join(".config/tool.toml");
    fs::create_dir_all(config_path.parent().expect("config parent should exist"))
        .expect("config parent should be created");
    fs::write(&config_path, "enabled = false\n").expect("config should be written");

    let spec = VerifierSpec {
        checks: vec![
            command_check("alpha-tool", ProbeRequirement::Required),
            contains_check(
                &config_path,
                r#"enabled\s*=\s*true"#,
                ProbeRequirement::Optional,
            ),
        ],
        service: None,
    };
    let context = harness.context();
    let aggregation =
        aggregate_verifier_evidence(&spec, &context).expect("evidence should aggregate");

    assert_eq!(aggregation.collected.len(), 2);
    assert_eq!(aggregation.collected[0].check_index, 0);
    assert!(matches!(
        aggregation.collected[0].probe,
        ProbeSpec::CommandExists(_)
    ));
    assert_eq!(
        aggregation.collected[0].record.status,
        EvidenceStatus::Satisfied
    );
    assert_eq!(aggregation.collected[1].check_index, 1);
    assert!(matches!(
        aggregation.collected[1].probe,
        ProbeSpec::Contains(_)
    ));
    assert_eq!(
        aggregation.collected[1].record.status,
        EvidenceStatus::Broken
    );
}

#[test]
fn optional_failures_remain_visible_when_required_stage_passes() {
    let harness = TestHarness::new();
    harness.write_executable("alpha-tool", "#!/bin/sh\nprintf alpha\n");
    let config_path = harness.home_dir.join(".config/tool.toml");
    fs::create_dir_all(config_path.parent().expect("config parent should exist"))
        .expect("config parent should be created");
    fs::write(&config_path, "enabled = false\n").expect("config should be written");

    let spec = VerifierSpec {
        checks: vec![
            command_check("alpha-tool", ProbeRequirement::Required),
            contains_check(
                &config_path,
                r#"enabled\s*=\s*true"#,
                ProbeRequirement::Optional,
            ),
        ],
        service: None,
    };
    let context = harness.context();
    let run = verify_with_context(VerificationStage::Present, &spec, &context)
        .expect("verification should complete");

    assert!(run.result.threshold_met);
    assert_eq!(run.result.achieved_stage, Some(VerificationStage::Present));
    assert_eq!(run.result.summary.required_failures, 0);
    assert_eq!(run.result.evidence.len(), 2);
    assert_eq!(run.result.evidence[1].record.status, EvidenceStatus::Broken);
    assert_eq!(
        run.result.evidence[1].check.requirement,
        ProbeRequirement::Optional
    );
}

fn command_check(command: &str, requirement: ProbeRequirement) -> VerifierCheck {
    VerifierCheck {
        stage: VerificationStage::Present,
        requirement,
        min_profile: VerificationProfile::Quick,
        kind: ProbeKind::Command,
        command: Some(command.to_string()),
        commands: None,
        path: None,
        pattern: None,
    }
}

fn contains_check(path: &Path, pattern: &str, requirement: ProbeRequirement) -> VerifierCheck {
    VerifierCheck {
        stage: VerificationStage::Configured,
        requirement,
        min_profile: VerificationProfile::Quick,
        kind: ProbeKind::Contains,
        command: None,
        commands: None,
        path: Some(path.display().to_string()),
        pattern: Some(pattern.to_string()),
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
            "envira-verifier-probes-{}-{}",
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

    fn write_executable(&self, name: &str, contents: &str) {
        let path = self.bin_dir.join(name);
        fs::write(&path, contents).expect("script should be written");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let mut permissions = fs::metadata(&path)
                .expect("script metadata should exist")
                .permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(&path, permissions).expect("script should be executable");
        }
    }
}

impl Drop for TestHarness {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
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

fn root_platform_context() -> PlatformContext {
    let user = UserAccount {
        username: "root".to_string(),
        home_dir: PathBuf::from("/root"),
        uid: Some(0),
        gid: Some(0),
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
        invocation: InvocationKind::Root,
        effective_user: user.clone(),
        target_user: Some(user),
        runtime_scope: RuntimeScope::System,
    }
}
