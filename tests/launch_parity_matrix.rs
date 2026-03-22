use std::{
    collections::BTreeMap,
    env, fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{self, Command, Output},
    time::{SystemTime, UNIX_EPOCH},
};

use envira::{
    catalog::{load_embedded_catalog, Catalog, CatalogItem, TargetBackend},
    engine::{
        InstallWorkflowFailure, InstallWorkflowOutcome, InstallWorkflowStatus,
        VerificationItemResult, VerificationWorkflowResult, VerificationWorkflowSummary,
    },
    executor::{
        build_execution_plan, execute_execution_plan, ExecutionDisposition, ExecutionPlan,
        ExecutionPlanReport, OperationSpec,
    },
    planner::{
        build_install_plan, classify_install_plan, ActionPlan, PlannedAction, PlannerRequest,
    },
    platform::{
        ArchitectureIdentity, ArchitectureKind, DistroIdentity, DistroKind, InvocationKind,
        PlatformContext, RuntimeScope, UserAccount,
    },
    verifier::{verify_with_context, VerificationContext, VerificationProfile, VerifierResult},
};
use serde_json::{json, Value};

const TARGET_ITEM_ID: &str = "ctop";
const TARGET_BUNDLE_ID: &str = "terminal-tools";

#[test]
fn launch_catalog_parity_success_artifact_captures_embedded_catalog_executor_and_strict_profile() {
    let scenario = EmbeddedParityScenario::run(InstallBehavior::CreatesVerifiedCommand);
    let target_action = scenario.target_action_step();
    let target_pre_verification = scenario.target_pre_verification();
    let target_post_verification = scenario.target_post_verification();

    assert!(scenario.catalog.items.len() > 1);
    assert_eq!(
        scenario.catalog_command["payload"]["catalog"]["items"]
            .as_array()
            .map(Vec::len),
        Some(scenario.catalog.items.len())
    );
    assert_eq!(target_action.action, PlannedAction::Install);
    assert!(!target_pre_verification.result.threshold_met);
    assert_eq!(scenario.execution.summary.failed_steps, 0);
    assert_eq!(scenario.outcome.status, InstallWorkflowStatus::Success);
    assert_eq!(
        scenario.outcome.threshold_met_steps,
        scenario.action_plan.steps.len()
    );
    assert!(target_post_verification.result.threshold_met);
    assert!(scenario.strict_verification.threshold_met);
    assert_eq!(
        scenario.strict_verification.requested_profile,
        VerificationProfile::Strict
    );

    write_json_evidence(
        "task-15-planner-verifier-install.json",
        &json!({
            "task": 15,
            "kind": "launch_catalog_parity_success",
            "catalog": scenario.catalog_summary(),
            "selection": {
                "item": TARGET_ITEM_ID,
                "bundle": TARGET_BUNDLE_ID,
                "request": scenario.install_plan.request,
            },
            "planner": {
                "steps": scenario.action_plan.steps,
            },
            "pre_install_verification": scenario.pre_verification,
            "execution_plan": scenario.execution_plan,
            "execution": scenario.execution,
            "install_outcome": scenario.outcome,
            "post_install_verification": scenario.post_verification,
            "strict_profile": serde_json::to_value(&scenario.strict_verification)
                .expect("strict verifier result should serialize"),
        }),
    );
}

#[test]
fn launch_catalog_parity_failure_artifact_captures_exact_embedded_catalog_regression_details() {
    let scenario = EmbeddedParityScenario::run(InstallBehavior::LeavesCommandMissing);
    let target_action = scenario.target_action_step();
    let target_pre_verification = scenario.target_pre_verification();
    let target_post_verification = scenario.target_post_verification();
    let failure = scenario.target_failure();
    let execution_step = scenario.target_execution_step();

    assert_eq!(target_action.action, PlannedAction::Install);
    assert!(!target_pre_verification.result.threshold_met);
    assert_eq!(scenario.execution.summary.failed_steps, 0);
    assert_eq!(
        scenario.outcome.status,
        InstallWorkflowStatus::VerificationFailed
    );
    assert_eq!(failure.item_id, TARGET_ITEM_ID);
    assert_eq!(failure.action, PlannedAction::Install);
    assert_eq!(failure.execution_disposition, ExecutionDisposition::Success);
    assert!(!target_post_verification.result.threshold_met);

    write_text_evidence(
        "task-15-matrix-error.txt",
        &serde_json::to_string_pretty(&json!({
            "task": 15,
            "kind": "launch_catalog_parity_failure",
            "catalog": scenario.catalog_summary(),
            "selection": {
                "item": TARGET_ITEM_ID,
                "bundle": TARGET_BUNDLE_ID,
                "depends_on": target_action.step.depends_on,
            },
            "item_id": failure.item_id,
            "planned_action": failure.action,
            "planned_scope": target_action.step.planned_scope,
            "observed_scope": target_post_verification.result.observed_scope,
            "verification_summary": target_post_verification.result.summary,
            "evidence": target_post_verification.result.evidence,
            "install_failure": failure,
            "execution_message": execution_step.message,
            "post_install_status": scenario.outcome.status,
        }))
        .expect("failure artifact should serialize"),
    );
}

#[derive(Clone, Copy)]
enum InstallBehavior {
    CreatesVerifiedCommand,
    LeavesCommandMissing,
}

struct EmbeddedParityScenario {
    _fixture: LaunchParityFixture,
    catalog: Catalog,
    catalog_command: Value,
    install_plan: envira::planner::InstallPlan,
    pre_verification: VerificationWorkflowResult,
    action_plan: ActionPlan,
    execution_plan: ExecutionPlan,
    execution: ExecutionPlanReport,
    post_verification: VerificationWorkflowResult,
    outcome: InstallWorkflowOutcome,
    strict_verification: VerifierResult,
}

impl EmbeddedParityScenario {
    fn run(install_behavior: InstallBehavior) -> Self {
        let fixture = LaunchParityFixture::new(install_behavior);
        let catalog = load_embedded_catalog().expect("embedded catalog should parse");
        let target_item = catalog_item(&catalog);
        let terminal_tools_bundle = catalog
            .bundle(TARGET_BUNDLE_ID)
            .expect("terminal-tools bundle should exist");

        assert!(
            terminal_tools_bundle
                .items
                .iter()
                .any(|item_id| item_id.as_str() == TARGET_ITEM_ID),
            "target item should stay in the embedded terminal-tools bundle"
        );

        let catalog_command = parse_stdout_json(&fixture.run_catalog());
        assert_eq!(
            catalog_command["payload"]["catalog"]["items"]
                .as_array()
                .map(Vec::len),
            Some(catalog.items.len())
        );

        let platform = platform_context(&fixture.home_dir);
        let request = PlannerRequest::item(TARGET_ITEM_ID);
        let install_plan = build_install_plan(&catalog, &platform, &request)
            .expect("embedded item plan should build");
        let pre_verification = verify_install_plan(
            &catalog,
            &install_plan,
            &platform,
            VerificationProfile::Quick,
            &fixture.search_paths(),
        );
        let action_plan = classify_install_plan(
            &install_plan,
            &verification_results_by_item(&pre_verification),
        )
        .expect("action plan should classify");
        let mut execution_plan = build_execution_plan(&catalog, &platform, &action_plan)
            .expect("execution plan should build");
        inject_command_env(&mut execution_plan, &fixture.path_env, &fixture.home_dir);
        let execution = execute_execution_plan(&execution_plan);
        let post_verification = verify_install_plan(
            &catalog,
            &install_plan,
            &platform,
            VerificationProfile::Quick,
            &fixture.search_paths(),
        );
        let strict_verification = verify_item(
            target_item,
            &platform,
            VerificationProfile::Strict,
            &fixture.search_paths(),
        );
        let outcome = summarize_install_outcome(&action_plan, &execution, &post_verification);

        Self {
            _fixture: fixture,
            catalog,
            catalog_command,
            install_plan,
            pre_verification,
            action_plan,
            execution_plan,
            execution,
            post_verification,
            outcome,
            strict_verification,
        }
    }

    fn catalog_summary(&self) -> Value {
        let target_item = catalog_item(&self.catalog);
        json!({
            "manifest_source": "embedded",
            "schema_version": self.catalog.schema_version,
            "items": self.catalog.items.len(),
            "bundles": self.catalog.bundles.len(),
            "default_bundles": self.catalog.default_bundles,
            "default_install_ids": self
                .catalog
                .default_install_ids()
                .expect("default bundles should expand")
                .into_iter()
                .map(str::to_string)
                .collect::<Vec<_>>(),
            "selected_item": {
                "id": target_item.id.as_str(),
                "display_name": target_item.display_name,
                "depends_on": target_item
                    .depends_on
                    .iter()
                    .map(|dependency| dependency.as_str().to_string())
                    .collect::<Vec<_>>(),
                "bundle": TARGET_BUNDLE_ID,
            },
            "catalog_command": {
                "items": self.catalog_command["payload"]["catalog"]["items"]
                    .as_array()
                    .map(Vec::len)
                    .unwrap_or_default(),
                "bundles": self.catalog_command["payload"]["catalog"]["bundles"]
                    .as_array()
                    .map(Vec::len)
                    .unwrap_or_default(),
            },
        })
    }

    fn target_action_step(&self) -> &envira::planner::ActionPlanStep {
        self.action_plan
            .steps
            .iter()
            .find(|step| step.step.item_id == TARGET_ITEM_ID)
            .expect("target action step should exist")
    }

    fn target_pre_verification(&self) -> &VerificationItemResult {
        self.pre_verification
            .result_for(TARGET_ITEM_ID)
            .expect("target pre-verification result should exist")
    }

    fn target_post_verification(&self) -> &VerificationItemResult {
        self.post_verification
            .result_for(TARGET_ITEM_ID)
            .expect("target post-verification result should exist")
    }

    fn target_failure(&self) -> &InstallWorkflowFailure {
        self.outcome
            .failures
            .iter()
            .find(|failure| failure.item_id == TARGET_ITEM_ID)
            .expect("target failure should exist")
    }

    fn target_execution_step(&self) -> &envira::executor::ExecutionStepReport {
        self.execution
            .steps
            .iter()
            .find(|step| step.step.action_step.step.item_id == TARGET_ITEM_ID)
            .expect("target execution step should exist")
    }
}

struct LaunchParityFixture {
    root: PathBuf,
    home_dir: PathBuf,
    bin_dir: PathBuf,
    path_env: String,
}

impl LaunchParityFixture {
    fn new(install_behavior: InstallBehavior) -> Self {
        let root = unique_temp_dir("task15-launch-parity");
        let home_dir = root.join("home/alice");
        let bin_dir = root.join("shims/bin");
        fs::create_dir_all(&bin_dir).expect("bin directory should be created");

        for command in ["git", "wget", "ncdu", "mkdir", "chmod"] {
            write_executable(&bin_dir.join(command), "#!/bin/sh\nexit 0\n");
        }
        write_executable(&bin_dir.join("curl"), &curl_script());
        write_executable(&bin_dir.join("install"), &install_script(install_behavior));

        let path_env = env::join_paths(
            std::iter::once(bin_dir.clone()).chain(
                env::var_os("PATH")
                    .as_deref()
                    .map(env::split_paths)
                    .into_iter()
                    .flatten(),
            ),
        )
        .expect("PATH should join")
        .to_string_lossy()
        .into_owned();

        Self {
            root,
            home_dir,
            bin_dir,
            path_env,
        }
    }

    fn run_catalog(&self) -> Output {
        Command::new(env!("CARGO_BIN_EXE_envira"))
            .args(["catalog", "--format", "json"])
            .env_remove("ENVIRA_CATALOG_PATH")
            .env("PATH", &self.path_env)
            .env("HOME", &self.home_dir)
            .env("USER", "alice")
            .output()
            .expect("envira catalog should run")
    }

    fn search_paths(&self) -> Vec<PathBuf> {
        vec![self.home_dir.join(".local/bin"), self.bin_dir.clone()]
    }
}

impl Drop for LaunchParityFixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn catalog_item(catalog: &Catalog) -> &CatalogItem {
    catalog
        .item(TARGET_ITEM_ID)
        .expect("target item should exist in the embedded catalog")
}

fn verify_install_plan(
    catalog: &Catalog,
    install_plan: &envira::planner::InstallPlan,
    platform: &PlatformContext,
    profile: VerificationProfile,
    search_paths: &[PathBuf],
) -> VerificationWorkflowResult {
    let context = VerificationContext::new(platform.clone(), profile)
        .with_search_paths(search_paths.to_vec());
    let results = install_plan
        .steps
        .iter()
        .map(|step| VerificationItemResult {
            step: step.clone(),
            result: verify_with_context(
                step.success_threshold,
                &catalog
                    .item(step.item_id.as_str())
                    .expect("planned item should exist in catalog")
                    .verifier,
                &context,
            )
            .expect("verification should complete")
            .result,
        })
        .collect::<Vec<_>>();

    VerificationWorkflowResult {
        request: install_plan.request.clone(),
        profile,
        platform: platform.clone(),
        summary: summarize_verification(&results),
        results,
    }
}

fn verify_item(
    item: &CatalogItem,
    platform: &PlatformContext,
    profile: VerificationProfile,
    search_paths: &[PathBuf],
) -> VerifierResult {
    verify_with_context(
        item.success_threshold,
        &item.verifier,
        &VerificationContext::new(platform.clone(), profile)
            .with_search_paths(search_paths.to_vec()),
    )
    .expect("item verification should complete")
    .result
}

fn summarize_verification(results: &[VerificationItemResult]) -> VerificationWorkflowSummary {
    let threshold_met_steps = results
        .iter()
        .filter(|result| result.result.threshold_met)
        .count();

    VerificationWorkflowSummary {
        total_steps: results.len(),
        threshold_met_steps,
        threshold_unmet_steps: results.len().saturating_sub(threshold_met_steps),
    }
}

fn verification_results_by_item(
    verification: &VerificationWorkflowResult,
) -> BTreeMap<String, VerifierResult> {
    verification
        .results
        .iter()
        .map(|result| (result.step.item_id.clone(), result.result.clone()))
        .collect()
}

fn inject_command_env(execution_plan: &mut ExecutionPlan, path_env: &str, home_dir: &Path) {
    for step in &mut execution_plan.steps {
        for operation in &mut step.operations {
            if let OperationSpec::Command(command) = operation {
                command.env.insert("PATH".to_string(), path_env.to_string());
                command
                    .env
                    .insert("HOME".to_string(), home_dir.display().to_string());
                command.env.insert("USER".to_string(), "alice".to_string());
            }
        }
    }
}

fn summarize_install_outcome(
    action_plan: &ActionPlan,
    execution: &ExecutionPlanReport,
    post_verification: &VerificationWorkflowResult,
) -> InstallWorkflowOutcome {
    let execution_by_item = execution
        .steps
        .iter()
        .map(|step| (step.step.action_step.step.item_id.clone(), step.disposition))
        .collect::<BTreeMap<_, _>>();

    let mut failures = Vec::new();
    let mut actionable_steps = 0usize;
    let mut blocked_steps = 0usize;
    let mut threshold_met_steps = 0usize;

    for step in &action_plan.steps {
        if step.action == PlannedAction::Blocked {
            blocked_steps += 1;
            continue;
        }

        actionable_steps += 1;

        if let Some(result) = post_verification.result_for(step.step.item_id.as_str()) {
            if result.result.threshold_met {
                threshold_met_steps += 1;
            } else {
                failures.push(InstallWorkflowFailure {
                    item_id: step.step.item_id.clone(),
                    action: step.action,
                    execution_disposition: execution_by_item
                        .get(step.step.item_id.as_str())
                        .copied()
                        .unwrap_or(ExecutionDisposition::Skipped),
                    verifier: result.result.clone(),
                });
            }
        }
    }

    let status = if !failures.is_empty() {
        InstallWorkflowStatus::VerificationFailed
    } else if blocked_steps > 0 {
        InstallWorkflowStatus::Blocked
    } else {
        InstallWorkflowStatus::Success
    };

    InstallWorkflowOutcome {
        status,
        execution_succeeded: execution.summary.failed_steps == 0,
        actionable_steps,
        blocked_steps,
        threshold_met_steps,
        failures,
    }
}

fn curl_script() -> String {
    "#!/bin/sh\noutput=''\nwhile [ \"$#\" -gt 0 ]; do\n  case \"$1\" in\n    -o)\n      output=\"$2\"\n      shift 2\n      ;;
    *)
      shift
      ;;
  esac
done
if [ -n \"$output\" ]; then
  /bin/mkdir -p \"$(dirname \"$output\")\"
  printf '#!/bin/sh\\nexit 0\\n' > \"$output\"
fi
exit 0
"
    .to_string()
}

fn install_script(install_behavior: InstallBehavior) -> String {
    match install_behavior {
        InstallBehavior::CreatesVerifiedCommand => "#!/bin/sh\ndestination=''\nfor last; do destination=\"$last\"; done\n/bin/mkdir -p \"$(dirname \"$destination\")\"\nprintf '#!/bin/sh\\nexit 0\\n' > \"$destination\"\n/bin/chmod 755 \"$destination\"\nexit 0\n"
            .to_string(),
        InstallBehavior::LeavesCommandMissing => "#!/bin/sh\nexit 0\n".to_string(),
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

fn parse_stdout_json(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "stdout should contain parseable JSON: {error}\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        )
    })
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

fn write_text_evidence(file_name: &str, contents: &str) {
    let path = evidence_path(file_name);
    fs::write(&path, format!("{contents}\n")).expect("evidence file should be written");
}

fn evidence_path(file_name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join(".sisyphus/evidence");
    fs::create_dir_all(&dir).expect("evidence directory should exist");
    dir.join(file_name)
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
