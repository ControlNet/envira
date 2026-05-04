use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process;
use std::time::{SystemTime, UNIX_EPOCH};

use envira::catalog::{Catalog, TargetBackend};
use envira::executor::{
    build_execution_plan, execute_execution_plan, ArchiveFormat, BuiltinOperation, CommandEvent,
    CommandOperation, CommandRunner, ExecutionDisposition, ExecutionRecipe, ExecutionTarget,
    ExecutorError, OperationSpec,
};
use envira::planner::{build_install_plan, classify_install_plan, PlannerRequest};
use envira::platform::{
    ArchitectureIdentity, ArchitectureKind, DistroIdentity, DistroKind, InvocationKind,
    PlatformContext, RuntimeScope, UserAccount,
};
use envira::verifier::{
    EvidenceRecord, EvidenceStatus, ObservedScope, ProbeKind, ProbeRequirement, VerificationHealth,
    VerificationProfile, VerificationStage, VerificationSummary, VerifierCheck, VerifierEvidence,
    VerifierResult,
};
use serde_json::json;
use users::get_effective_uid;

#[test]
fn operation_serialization_includes_explicit_command_metadata() {
    let mut env = BTreeMap::new();
    env.insert("ENVIRA_MODE".to_string(), "dry-run".to_string());

    let operation = OperationSpec::Command(CommandOperation {
        program: "python3".to_string(),
        args: vec!["-c".to_string(), "print('hello')".to_string()],
        env,
        cwd: Some(PathBuf::from("/tmp/envira-executor")),
        timeout_ms: Some(2_500),
        target: ExecutionTarget::TargetUser,
    });

    let serialized = serde_json::to_value(&operation).expect("operation should serialize");

    assert_eq!(
        serialized,
        json!({
            "kind": "command",
            "program": "python3",
            "args": ["-c", "print('hello')"],
            "env": {"ENVIRA_MODE": "dry-run"},
            "cwd": "/tmp/envira-executor",
            "timeout_ms": 2500,
            "target": "target_user"
        })
    );
}

#[test]
fn builtin_operation_serialization_uses_bounded_install_families() {
    let operation = OperationSpec::Builtin(BuiltinOperation::ArchiveInstall {
        url: "https://example.com/bat.tar.gz".to_string(),
        destination_dir: PathBuf::from("/usr/local/bin"),
        format: ArchiveFormat::TarGz,
        strip_components: 1,
        checksum_sha256: Some("abc123".to_string()),
    });

    let serialized = serde_json::to_value(&operation).expect("builtin operation should serialize");

    assert_eq!(
        serialized,
        json!({
            "kind": "builtin",
            "family": "archive_install",
            "format": "tar_gz",
            "url": "https://example.com/bat.tar.gz",
            "destination_dir": "/usr/local/bin",
            "strip_components": 1,
            "checksum_sha256": "abc123"
        })
    );
}

#[test]
fn successful_command_execution_respects_env_and_cwd_metadata() {
    let temp_dir = TestDir::new("success");
    let operation = CommandOperation::new("python3")
        .with_args([
            "-c",
            "import os, pathlib; print(os.environ['ENVIRA_EXECUTOR_TEST']); print(pathlib.Path.cwd())",
        ])
        .with_env([("ENVIRA_EXECUTOR_TEST", "available")])
        .with_cwd(temp_dir.path())
        .with_target(ExecutionTarget::CurrentProcess);

    let execution = CommandRunner::default()
        .execute(&operation)
        .expect("command should execute");

    assert_eq!(execution.disposition(), ExecutionDisposition::Success);
    assert_eq!(execution.summary.exit_code, Some(0));
    assert!(!execution.summary.timed_out);
    assert!(execution.summary.message.contains("exited successfully"));
    assert!(execution.stdout.evidence.contains("available"));
    assert!(execution
        .stdout
        .evidence
        .contains(temp_dir.path().to_string_lossy().as_ref()));
    assert_eq!(execution.stderr.evidence, "");
}

#[test]
fn failing_command_execution_preserves_exit_status_and_evidence() {
    let operation = CommandOperation::new("python3").with_args([
        "-c",
        "import sys; print('before failure'); print('stderr evidence', file=sys.stderr); sys.exit(7)",
    ]);

    let execution = CommandRunner::default()
        .execute(&operation)
        .expect("command should execute even when it fails");

    assert_eq!(execution.disposition(), ExecutionDisposition::Failure);
    assert_eq!(execution.summary.exit_code, Some(7));
    assert!(!execution.summary.timed_out);
    assert!(execution.stdout.evidence.contains("before failure"));
    assert!(execution.stderr.evidence.contains("stderr evidence"));
    assert!(execution.summary.message.contains("stderr evidence"));
    assert_eq!(execution.state(), envira::executor::OperationState::Failure);
}

#[test]
fn command_streaming_emits_structured_stdout_stderr_and_finished_events() {
    let operation = CommandOperation::new("python3").with_args([
        "-c",
        "import sys; print('alpha', flush=True); print('omega', file=sys.stderr, flush=True)",
    ]);
    let mut events = Vec::new();

    let execution = CommandRunner::default()
        .execute_with_events(&operation, |event| events.push(event))
        .expect("command should execute");

    assert_eq!(execution.disposition(), ExecutionDisposition::Success);
    assert!(matches!(events.first(), Some(CommandEvent::Started(_))));
    assert!(events.iter().any(|event| {
        matches!(event, CommandEvent::Stdout(stdout) if stdout.text == "alpha" && stdout.line_number == 1)
    }));
    assert!(events.iter().any(|event| {
        matches!(event, CommandEvent::Stderr(stderr) if stderr.text == "omega" && stderr.line_number == 1)
    }));
    assert!(
        matches!(events.last(), Some(CommandEvent::Finished(finished)) if finished.summary.disposition == ExecutionDisposition::Success)
    );
}

#[test]
fn current_process_target_executes_directly_without_sudo_wrapper() {
    let temp_dir = TestDir::new("current-process");
    let sudo = temp_dir.write_executable("sudo", "#!/bin/sh\nprintf 'sudo:%s\\n' \"$*\"\n");
    let operation = CommandOperation::new("python3")
        .with_args(["-c", "print('direct:current')"])
        .with_env([("PATH", joined_path(sudo.parent().expect("sudo parent")))])
        .with_target(ExecutionTarget::CurrentProcess);

    let execution = CommandRunner::default()
        .execute(&operation)
        .expect("current process execution should succeed");

    assert_eq!(execution.disposition(), ExecutionDisposition::Success);
    assert_eq!(execution.stdout.evidence.trim(), "direct:current");
}

#[test]
fn system_target_enforces_runtime_wrapper_semantics() {
    let temp_dir = TestDir::new("system-target");
    let probe = temp_dir.write_executable("probe.sh", "#!/bin/sh\nprintf 'direct:%s\\n' \"$1\"\n");
    let sudo = temp_dir.write_executable("sudo", "#!/bin/sh\nprintf 'sudo:%s\\n' \"$*\"\n");
    let operation = CommandOperation::new(probe.to_string_lossy().into_owned())
        .with_args(["system"])
        .with_env([("PATH", joined_path(sudo.parent().expect("sudo parent")))])
        .with_target(ExecutionTarget::System);

    let execution = CommandRunner::default()
        .execute(&operation)
        .expect("system target execution should succeed");

    assert_eq!(execution.disposition(), ExecutionDisposition::Success);
    if get_effective_uid() == 0 {
        assert_eq!(execution.stdout.evidence.trim(), "direct:system");
    } else {
        assert_eq!(
            execution.stdout.evidence.trim(),
            format!("sudo:--preserve-env=PATH -- {} system", probe.display())
        );
    }
}

#[test]
fn system_target_preserves_declared_env_at_real_command_execution() {
    let temp_dir = TestDir::new("system-target-env");
    let probe = temp_dir.write_executable(
        "probe.sh",
        "#!/bin/sh\nprintf '%s\\n' \"${ENVIRA_EXECUTOR_TEST:-missing}\"\n",
    );
    let sudo = temp_dir.write_preserving_sudo("sudo");
    let operation = CommandOperation::new(probe.to_string_lossy().into_owned())
        .with_env([
            ("ENVIRA_EXECUTOR_TEST", "available".to_string()),
            ("PATH", joined_path(sudo.parent().expect("sudo parent"))),
        ])
        .with_target(ExecutionTarget::System);

    let execution = CommandRunner::default()
        .execute(&operation)
        .expect("system target execution should preserve env");

    assert_eq!(execution.disposition(), ExecutionDisposition::Success);
    assert_eq!(execution.stdout.evidence.trim(), "available");
}

#[test]
fn target_user_target_uses_sudo_with_selected_user_context() {
    let temp_dir = TestDir::new("target-user");
    let probe = temp_dir.write_executable("probe.sh", "#!/bin/sh\nprintf 'direct:%s\\n' \"$1\"\n");
    let sudo = temp_dir.write_executable("sudo", "#!/bin/sh\nprintf 'sudo:%s\\n' \"$*\"\n");
    let operation = CommandOperation::new(probe.to_string_lossy().into_owned())
        .with_args(["target"])
        .with_env([
            ("PATH", joined_path(sudo.parent().expect("sudo parent"))),
            ("SUDO_USER", "alice".to_string()),
        ])
        .with_target(ExecutionTarget::TargetUser);

    let execution = CommandRunner::default()
        .execute(&operation)
        .expect("target-user execution should succeed");

    assert_eq!(execution.disposition(), ExecutionDisposition::Success);
    assert_eq!(
        execution.stdout.evidence.trim(),
        format!(
            "sudo:--preserve-env=PATH,SUDO_USER -u alice -- {} target",
            probe.display()
        )
    );
}

#[test]
fn target_user_target_preserves_declared_env_at_real_command_execution() {
    let temp_dir = TestDir::new("target-user-env");
    let probe = temp_dir.write_executable(
        "probe.sh",
        "#!/bin/sh\nprintf '%s:%s\\n' \"${ENVIRA_EXECUTOR_TEST:-missing}\" \"${ENVIRA_TARGET_MARKER:-missing}\"\n",
    );
    let sudo = temp_dir.write_preserving_sudo("sudo");
    let operation = CommandOperation::new(probe.to_string_lossy().into_owned())
        .with_env([
            ("ENVIRA_EXECUTOR_TEST", "available".to_string()),
            ("ENVIRA_TARGET_MARKER", "wrapped".to_string()),
            ("PATH", joined_path(sudo.parent().expect("sudo parent"))),
            ("SUDO_USER", "alice".to_string()),
        ])
        .with_target(ExecutionTarget::TargetUser);

    let execution = CommandRunner::default()
        .execute(&operation)
        .expect("target-user execution should preserve env");

    assert_eq!(execution.disposition(), ExecutionDisposition::Success);
    assert_eq!(execution.stdout.evidence.trim(), "available:wrapped");
}

#[test]
fn target_user_target_requires_explicit_user_context() {
    let operation = CommandOperation::new("python3")
        .with_args(["-c", "print('unused')"])
        .with_env([("SUDO_USER", "")])
        .with_target(ExecutionTarget::TargetUser);

    let error = CommandRunner::default()
        .execute(&operation)
        .expect_err("target-user execution without user context should fail");

    match error {
        ExecutorError::MissingTargetUser { program } => assert_eq!(program, "python3"),
        other => panic!("expected missing target user error, got {other}"),
    }
}

#[test]
fn execution_plan_uses_shell_recipe_contract_and_preserves_quotes_and_spaces() {
    let execution_plan = execution_plan_for_manifest(
        shell_preview_manifest(),
        "shell-preview",
        RuntimeScope::User,
    );
    let step = &execution_plan.steps[0];

    assert_eq!(step.execution_target, ExecutionTarget::CurrentProcess);
    assert_eq!(
        step.recipe,
        Some(ExecutionRecipe::Shell {
            shell: "bash".to_string(),
            command: shell_preview_command().to_string(),
        })
    );
    assert_eq!(
        step.operations,
        vec![OperationSpec::Command(
            CommandOperation::shell("bash", shell_preview_command())
                .with_target(ExecutionTarget::CurrentProcess)
        )]
    );

    let execution = execute_execution_plan(&execution_plan);
    let operation = execution.steps[0].operations[0]
        .command
        .as_ref()
        .expect("shell operation should execute");

    assert_eq!(
        execution.steps[0].disposition,
        ExecutionDisposition::Success
    );
    assert_eq!(operation.summary.exit_code, Some(0));
    assert_eq!(operation.stdout.evidence.trim(), "quoted value with spaces");
}

#[test]
fn non_zero_recipe_exit_propagates() {
    let execution_plan =
        execution_plan_for_manifest(shell_failure_manifest(), "shell-fail", RuntimeScope::User);
    let execution = execute_execution_plan(&execution_plan);
    let step = &execution.steps[0];
    let operation = step.operations[0]
        .command
        .as_ref()
        .expect("shell operation should execute");

    assert_eq!(step.disposition, ExecutionDisposition::Failure);
    assert_eq!(operation.summary.exit_code, Some(23));
    assert!(operation.stdout.evidence.contains("before failure"));
    assert!(operation.stderr.evidence.contains("stderr evidence"));
    assert!(step.message.contains("stderr evidence"));
}

fn execution_plan_for_manifest(
    manifest: &str,
    item_id: &str,
    runtime_scope: RuntimeScope,
) -> envira::executor::ExecutionPlan {
    let catalog = Catalog::from_toml_str(manifest).expect("fixture catalog should parse");
    let platform = platform_context(TargetBackend::Apt, runtime_scope);
    let install_plan = build_install_plan(&catalog, &platform, &PlannerRequest::item(item_id))
        .expect("install plan should build");
    let action_plan = classify_install_plan(
        &install_plan,
        &std::collections::BTreeMap::from([(
            item_id.to_string(),
            missing_result(VerificationStage::Present),
        )]),
    )
    .expect("action plan should classify");

    build_execution_plan(&catalog, &platform, &action_plan)
        .expect("execution plan should use the shell contract")
}

fn platform_context(native_backend: TargetBackend, runtime_scope: RuntimeScope) -> PlatformContext {
    PlatformContext {
        distro: DistroIdentity {
            kind: DistroKind::Ubuntu,
            id: "ubuntu".to_string(),
            name: "Ubuntu".to_string(),
            pretty_name: Some("Ubuntu".to_string()),
            version_id: Some("latest".to_string()),
        },
        arch: ArchitectureIdentity {
            kind: ArchitectureKind::X86_64,
            raw: "x86_64".to_string(),
        },
        native_backend: Some(native_backend),
        invocation: match runtime_scope {
            RuntimeScope::System => InvocationKind::Root,
            RuntimeScope::User | RuntimeScope::Unknown => InvocationKind::User,
            RuntimeScope::Both => InvocationKind::Sudo,
        },
        effective_user: match runtime_scope {
            RuntimeScope::System => user("root", "/root", 0, 0),
            RuntimeScope::User | RuntimeScope::Both | RuntimeScope::Unknown => {
                user("alice", "/home/alice", 1000, 1000)
            }
        },
        target_user: match runtime_scope {
            RuntimeScope::Both => Some(user("alice", "/home/alice", 1000, 1000)),
            RuntimeScope::User => Some(user("alice", "/home/alice", 1000, 1000)),
            RuntimeScope::System | RuntimeScope::Unknown => None,
        },
        runtime_scope,
    }
}

fn user(username: &str, home_dir: &str, uid: u32, gid: u32) -> UserAccount {
    UserAccount {
        username: username.to_string(),
        home_dir: PathBuf::from(home_dir),
        uid: Some(uid),
        gid: Some(gid),
    }
}

fn missing_result(required_stage: VerificationStage) -> VerifierResult {
    let evidence = vec![VerifierEvidence {
        check: VerifierCheck {
            stage: VerificationStage::Present,
            requirement: ProbeRequirement::Required,
            min_profile: VerificationProfile::Quick,
            kind: ProbeKind::Command,
            command: Some("fixture-command".to_string()),
            commands: None,
            path: None,
            pattern: None,
        },
        record: EvidenceRecord {
            status: EvidenceStatus::Missing,
            observed_scope: ObservedScope::Unknown,
            summary: "required command is missing".to_string(),
            detail: None,
        },
        participates: true,
    }];

    VerifierResult {
        requested_profile: VerificationProfile::Quick,
        required_stage,
        achieved_stage: None,
        threshold_met: false,
        health: VerificationHealth::Missing,
        observed_scope: ObservedScope::Unknown,
        summary: VerificationSummary {
            total_checks: 1,
            participating_checks: 1,
            skipped_checks: 0,
            satisfied_checks: 0,
            missing_checks: 1,
            broken_checks: 0,
            unknown_checks: 0,
            not_applicable_checks: 0,
            required_failures: 1,
        },
        evidence,
        service_evidence: Vec::new(),
        service: None,
    }
}

fn shell_preview_command() -> &'static str {
    r#"printf "%s\n" "quoted value with spaces""#
}

fn shell_preview_manifest() -> &'static str {
    r#"
required_version = "0.1.0"
distros = ["ubuntu"]
shell = "bash"
default_bundles = ["shell-tests"]

[items.shell-preview]
name = "Shell Preview"
desc = "Shell Preview"
depends_on = []

[[items.shell-preview.recipes]]
mode = "user"
distros = ["ubuntu"]
cmd = 'printf "%s\n" "quoted value with spaces"'

[[items.shell-preview.verifiers]]
mode = "user"
distros = ["ubuntu"]
cmd = "command -v shell-preview"

[bundles.shell-tests]
name = "Shell Tests"
desc = "Shell Tests"
items = ["shell-preview"]
"#
}

fn shell_failure_manifest() -> &'static str {
    r#"
required_version = "0.1.0"
distros = ["ubuntu"]
shell = "bash"
default_bundles = ["shell-tests"]

[items.shell-fail]
name = "Shell Fail"
desc = "Shell Fail"
depends_on = []

[[items.shell-fail.recipes]]
mode = "user"
distros = ["ubuntu"]
cmd = 'printf "%s\n" "before failure"; printf "%s\n" "stderr evidence" >&2; exit 23'

[[items.shell-fail.verifiers]]
mode = "user"
distros = ["ubuntu"]
cmd = "command -v shell-fail"

[bundles.shell-tests]
name = "Shell Tests"
desc = "Shell Tests"
items = ["shell-fail"]
"#
}

struct TestDir {
    path: PathBuf,
}

impl TestDir {
    fn new(label: &str) -> Self {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should be after unix epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "envira-executor-{label}-{}-{unique}",
            process::id()
        ));
        fs::create_dir_all(&path).expect("temporary test directory should be created");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn write_executable(&self, name: &str, contents: &str) -> PathBuf {
        let path = self.path.join(name);
        fs::write(&path, contents).expect("test helper script should be written");
        let mut permissions = fs::metadata(&path)
            .expect("test helper script metadata should be readable")
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&path, permissions).expect("test helper script should be executable");
        path
    }

    fn write_preserving_sudo(&self, name: &str) -> PathBuf {
        self.write_executable(
            name,
            "#!/bin/sh
preserve=''
while [ \"$#\" -gt 0 ]; do
  case \"$1\" in
    --preserve-env=*)
      preserve=\"${1#--preserve-env=}\"
      shift
      ;;
    -u)
      shift 2
      ;;
    --)
      shift
      break
      ;;
    *)
      printf 'unexpected sudo arg:%s\\n' \"$1\" >&2
      exit 97
      ;;
  esac
done

saved_envira_executor_test=\"${ENVIRA_EXECUTOR_TEST-}\"
saved_envira_target_marker=\"${ENVIRA_TARGET_MARKER-}\"
saved_sudo_user=\"${SUDO_USER-}\"

unset ENVIRA_EXECUTOR_TEST
unset ENVIRA_TARGET_MARKER
unset SUDO_USER

case \",$preserve,\" in
  *,ENVIRA_EXECUTOR_TEST,*) export ENVIRA_EXECUTOR_TEST=\"$saved_envira_executor_test\" ;;
esac
case \",$preserve,\" in
  *,ENVIRA_TARGET_MARKER,*) export ENVIRA_TARGET_MARKER=\"$saved_envira_target_marker\" ;;
esac
case \",$preserve,\" in
  *,SUDO_USER,*) export SUDO_USER=\"$saved_sudo_user\" ;;
esac

exec \"$@\"
",
        )
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn joined_path(prefix: &Path) -> String {
    let original = env::var("PATH").unwrap_or_default();
    if original.is_empty() {
        prefix.to_string_lossy().into_owned()
    } else {
        format!("{}:{}", prefix.display(), original)
    }
}
