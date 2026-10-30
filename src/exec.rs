//! Executes a plan unit by unit. A failure blocks only the units that depend on it; state and
//! the run record are written after every unit.

use std::collections::BTreeSet;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::{mpsc, watch};

use crate::catalog::Catalog;
use crate::diagnose::diagnose;
use crate::facts::Facts;
use crate::plan::{Action, Kind, Plan, Unit};
use crate::runner::{Runner, StreamEnd};
use crate::shellenv;
use crate::state::{
    self, ConfigRecord, LastResult, Paths, RunRecord, State, ToolRecord, UnitRecord,
};
use crate::status::{probe_config, probe_tool};

const CHECK_TIMEOUT: Duration = Duration::from_secs(30);
/// Default wall-clock limit of one step (source builds such as ripgrep or yazi take minutes).
pub const STEP_TIMEOUT: Duration = Duration::from_secs(30 * 60);
pub const RETRIES: u32 = 2;
pub const RETRY_DELAY: Duration = Duration::from_secs(5);
const KEEP_RUNS: usize = 20;

#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    Ok {
        version: Option<String>,
    },
    Failed(String),
    Blocked(String),
    Cancelled,
    /// Not started because the run was stopped.
    Skipped,
}

impl Outcome {
    pub fn as_str(&self) -> &'static str {
        match self {
            Outcome::Ok { .. } => "ok",
            Outcome::Failed(_) => "failed",
            Outcome::Blocked(_) => "blocked",
            Outcome::Cancelled => "cancelled",
            Outcome::Skipped => "skipped",
        }
    }

    pub fn detail(&self) -> Option<String> {
        match self {
            Outcome::Ok { version } => version.clone(),
            Outcome::Failed(e) => Some(e.clone()),
            Outcome::Blocked(d) => Some(format!("needs {d}")),
            Outcome::Cancelled | Outcome::Skipped => None,
        }
    }

    fn succeeded(&self) -> bool {
        matches!(self, Outcome::Ok { .. })
    }
}

#[derive(Debug, Clone)]
pub enum Event {
    UnitStarted(usize),
    /// A log line of a unit (`[envira] ...` lines come from envira itself).
    Line(usize, String),
    UnitFinished(usize, Outcome, Duration),
    /// Shell environment regenerated: the files written, or an error.
    ShellEnv(Result<Vec<String>, String>),
    Finished(RunRecord),
}

/// Control messages from the UI.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Control {
    /// Incremented to cancel the unit that is running now.
    pub cancel_seq: u32,
    /// Do not start further units.
    pub stop: bool,
}

pub struct ExecCtx {
    pub catalog: Arc<Catalog>,
    pub facts: Arc<Facts>,
    pub paths: Paths,
    pub runner: Arc<dyn Runner>,
    pub run_id: String,
    pub command: String,
    /// Regenerate the managed shell environment at the end.
    pub shell_env: bool,
    /// A step still running after this long is stopped and fails.
    pub step_timeout: Duration,
    /// Extra attempts for a step that failed for a transient reason (network, busy apt).
    pub retries: u32,
    /// Wait before the first retry; later ones wait longer.
    pub retry_delay: Duration,
}

pub async fn execute(
    plan: Plan,
    ctx: ExecCtx,
    mut state: State,
    tx: mpsc::UnboundedSender<Event>,
    mut ctl: watch::Receiver<Control>,
) -> (RunRecord, State) {
    let run_dir = ctx.paths.run_dir(&ctx.run_id);
    let _ = state::create_dir_all_owned(&run_dir, &ctx.facts);
    // Logs can hold whatever installers print; keep them private to the user.
    let _ = std::fs::set_permissions(
        ctx.paths.logs_dir(),
        std::os::unix::fs::PermissionsExt::from_mode(0o700),
    );
    let mut record = RunRecord::planned(&plan, &ctx.run_id, &ctx.command);
    record.machine = ctx.facts.summary();
    let _ = record.save(&ctx.paths, &ctx.facts);
    // Point state at this run now: if envira dies midway, `retry` resumes this run.
    state.last_run = Some(ctx.run_id.clone());
    let _ = state.save_run(&ctx.paths, &ctx.facts);
    let mut bad: BTreeSet<String> = BTreeSet::new();

    for (idx, unit) in plan.units.iter().enumerate() {
        let started = Instant::now();
        let (outcome, hint) = if ctl.borrow().stop {
            (Outcome::Skipped, None)
        } else if let Some(d) = unit.deps.iter().find(|d| bad.contains(*d)) {
            (Outcome::Blocked(d.clone()), None)
        } else {
            let _ = tx.send(Event::UnitStarted(idx));
            run_unit(idx, unit, &ctx, &tx, &mut ctl, &run_dir).await
        };
        if !outcome.succeeded() {
            bad.insert(unit.id.clone());
        }
        let elapsed = started.elapsed();
        update_state(&mut state, unit, &outcome, &ctx.run_id);
        record.units[idx] = UnitRecord {
            id: unit.id.clone(),
            action: unit.action.as_str().into(),
            via: unit.via.clone(),
            outcome: outcome.as_str().into(),
            detail: outcome.detail(),
            hint: hint.filter(|_| !outcome.succeeded()),
            seconds: (elapsed.as_secs_f64() * 10.0).round() / 10.0,
        };
        if let Err(e) = state
            .save_run(&ctx.paths, &ctx.facts)
            .and_then(|_| record.save(&ctx.paths, &ctx.facts))
        {
            let _ = tx.send(Event::Line(
                idx,
                format!("[envira] could not save state: {e:#}"),
            ));
        }
        let _ = tx.send(Event::UnitFinished(idx, outcome, elapsed));
    }

    if ctx.shell_env {
        let r = regenerate_shell_env(&ctx).await;
        let _ = tx.send(Event::ShellEnv(r));
    }
    state.mark_all_seen(&ctx.catalog);
    state.last_run = Some(ctx.run_id.clone());
    record.finished = Some(state::now_rfc3339());
    let _ = state.save_run(&ctx.paths, &ctx.facts);
    let _ = record.save(&ctx.paths, &ctx.facts);
    state::prune_runs(&ctx.paths, KEEP_RUNS);
    let _ = tx.send(Event::Finished(record.clone()));
    (record, state)
}

async fn run_unit(
    idx: usize,
    unit: &Unit,
    ctx: &ExecCtx,
    tx: &mpsc::UnboundedSender<Event>,
    ctl: &mut watch::Receiver<Control>,
    run_dir: &std::path::Path,
) -> (Outcome, Option<String>) {
    let log_path = run_dir.join(format!("{}.log", unit.id));
    let log = std::fs::File::create(&log_path).ok();
    if log.is_some() {
        state::chown_to_target(&log_path, &ctx.facts);
    }
    let log = Arc::new(std::sync::Mutex::new(log));
    // The last diagnosis hint, kept for the run record.
    let hint: Arc<std::sync::Mutex<Option<String>>> = Default::default();
    let emit = {
        let log = log.clone();
        let hint = hint.clone();
        let tx = tx.clone();
        move |line: String| {
            let line = redact(&line, secrets());
            if let Some(h) = line.strip_prefix("[envira] hint ") {
                *hint.lock().unwrap() = Some(h.to_string());
            }
            if let Some(f) = log.lock().unwrap().as_mut() {
                let _ = writeln!(f, "{} {line}", chrono::Local::now().format("%H:%M:%S"));
            }
            let _ = tx.send(Event::Line(idx, line));
        }
    };
    if let Some(f) = log.lock().unwrap().as_mut() {
        let _ = writeln!(
            f,
            "# envira {} · run {} ({}) · {}\n# started {}",
            env!("CARGO_PKG_VERSION"),
            ctx.run_id,
            ctx.command,
            ctx.facts.summary(),
            state::now_rfc3339()
        );
    }
    let started = Instant::now();
    let outcome = run_steps(unit, ctx, &emit, ctl).await;
    if let Some(f) = log.lock().unwrap().as_mut() {
        let _ = writeln!(
            f,
            "# finished {}{} in {}",
            outcome.as_str(),
            outcome
                .detail()
                .map(|d| format!(": {}", redact(&d, secrets())))
                .unwrap_or_default(),
            crate::commands::fmt_duration(started.elapsed())
        );
    }
    let hint = hint.lock().unwrap().clone();
    (outcome, hint)
}

async fn run_steps<F>(
    unit: &Unit,
    ctx: &ExecCtx,
    emit: &F,
    ctl: &mut watch::Receiver<Control>,
) -> Outcome
where
    F: Fn(String) + Clone + Send + 'static,
{
    emit(format!(
        "[envira] {} {} via {} · scope {} · {}",
        unit.action.as_str(),
        unit.id,
        unit.via,
        unit.scope,
        unit.reason
    ));
    let tmp = match tempfile::Builder::new()
        .prefix(&format!("envira-{}-", unit.id))
        .tempdir()
    {
        Ok(t) => t,
        Err(e) => return Outcome::Failed(format!("cannot create temp dir: {e}")),
    };
    state::chown_to_target(tmp.path(), &ctx.facts);
    let base_seq = ctl.borrow().cancel_seq;

    for (n, step) in unit.steps.iter().enumerate() {
        let label = format!("step {}/{}", n + 1, unit.steps.len());
        if let Some(check) = &step.check
            && ctx.runner.capture(check, CHECK_TIMEOUT).await.ok()
        {
            emit(format!(
                "[envira] {label} · {} · already in place",
                step.label
            ));
            continue;
        }
        let Some(cmd) = &step.cmd else {
            emit(format!(
                "[envira] {label} · {} · not described by the catalog, revert manually",
                step.label
            ));
            continue;
        };
        emit(format!("$ {cmd}"));
        let mut attempt = 1;
        loop {
            let (end, timed_out, lines) =
                attempt_step(cmd, tmp.path(), ctx, emit, ctl, base_seq).await;
            match end {
                StreamEnd::Exit(0) => break,
                StreamEnd::Exit(code) => {
                    emit(format!("[envira] {label} exited with code {code}"));
                    let diag = diagnose(&lines);
                    if let Some(d) = &diag {
                        emit(format!("[envira] hint ({}): {}", d.kind, d.hint));
                    }
                    let kind = diag.as_ref().map(|d| format!(" ({})", d.kind));
                    if let Some(d) = diag.filter(|d| d.transient)
                        && attempt <= ctx.retries
                        && !ctl.borrow().stop
                    {
                        let wait = retry_wait(ctx.retry_delay, attempt, d.kind);
                        emit(format!(
                            "[envira] {label} failed ({}); retrying in {} (attempt {}/{})",
                            d.kind,
                            crate::commands::fmt_duration(wait),
                            attempt + 1,
                            ctx.retries + 1
                        ));
                        if !pause(wait, ctl, base_seq).await {
                            emit("[envira] cancelled".into());
                            return Outcome::Cancelled;
                        }
                        attempt += 1;
                        continue;
                    }
                    return Outcome::Failed(format!(
                        "{} exited with code {code}{}",
                        step.label,
                        kind.unwrap_or_default()
                    ));
                }
                StreamEnd::Cancelled if timed_out => {
                    let limit = crate::commands::fmt_duration(ctx.step_timeout);
                    emit(format!(
                        "[envira] {label} timed out after {limit}; stopped it"
                    ));
                    return Outcome::Failed(format!("{} timed out after {limit}", step.label));
                }
                StreamEnd::Cancelled => {
                    emit("[envira] cancelled".into());
                    return Outcome::Cancelled;
                }
                StreamEnd::SpawnError(e) => {
                    return Outcome::Failed(format!("could not start: {e}"));
                }
            }
        }
    }

    // Post-condition: the item must now be detected.
    match (unit.kind, unit.action) {
        (_, Action::Revert) => Outcome::Ok { version: None },
        (Kind::Tool, _) => {
            let (present, version) =
                probe_tool(&ctx.catalog, &ctx.facts, &unit.id, ctx.runner.as_ref()).await;
            if present {
                emit(format!(
                    "[envira] verified {}{}",
                    unit.id,
                    version
                        .as_deref()
                        .map(|v| format!(" {v}"))
                        .unwrap_or_default()
                ));
                if let (Some(want), Some(got)) = (&unit.expect, &version)
                    && crate::version::is_newer(want, got)
                {
                    emit(format!(
                        "[envira] note: {} is still {got}; the latest is {want} (the source may lag behind)",
                        unit.id
                    ));
                }
                Outcome::Ok { version }
            } else {
                let cmd = unit.verify.first().cloned().unwrap_or_default();
                emit(format!("[envira] installed, but `{cmd}` still fails"));
                Outcome::Failed(format!("not detected after install (`{cmd}`)"))
            }
        }
        (Kind::Config, _) => {
            if probe_config(&ctx.catalog, &ctx.facts, &unit.id, ctx.runner.as_ref()).await {
                emit(format!("[envira] verified {}", unit.id));
                Outcome::Ok { version: None }
            } else {
                Outcome::Failed("applied, but its check still fails".into())
            }
        }
    }
}

/// Runs one attempt of a step: streams its output through `emit`, enforces the step timeout and
/// cancels it on a new cancel request. Returns how it ended, whether it timed out, and its last
/// output lines (for diagnosis).
async fn attempt_step<F>(
    cmd: &str,
    dir: &std::path::Path,
    ctx: &ExecCtx,
    emit: &F,
    ctl: &mut watch::Receiver<Control>,
    base_seq: u32,
) -> (StreamEnd, bool, Vec<String>)
where
    F: Fn(String) + Clone + Send + 'static,
{
    let (ltx, mut lrx) = mpsc::unbounded_channel::<String>();
    let (ctx_cancel, crx) = watch::channel(false);
    let forward = {
        let emit = emit.clone();
        tokio::spawn(async move {
            let mut tail: std::collections::VecDeque<String> = Default::default();
            while let Some(l) = lrx.recv().await {
                if tail.len() == 200 {
                    tail.pop_front();
                }
                tail.push_back(l.clone());
                emit(l);
            }
            Vec::from(tail)
        })
    };
    let fut = ctx.runner.stream(cmd, dir, ltx, crx);
    tokio::pin!(fut);
    let deadline = tokio::time::sleep(ctx.step_timeout);
    tokio::pin!(deadline);
    let mut watching = true;
    let mut timed_out = false;
    let end = loop {
        tokio::select! {
            end = &mut fut => break end,
            _ = &mut deadline, if !timed_out => {
                timed_out = true;
                let _ = ctx_cancel.send(true);
            }
            changed = ctl.changed(), if watching => {
                if changed.is_err() {
                    watching = false;
                } else if ctl.borrow().cancel_seq != base_seq {
                    let _ = ctx_cancel.send(true);
                }
            }
        }
    };
    let lines = forward.await.unwrap_or_default();
    (end, timed_out, lines)
}

/// Wait before retry `attempt + 1`: `base`, then 3x longer each time; a busy package manager
/// (unattended-upgrades) gets six times as long.
fn retry_wait(base: Duration, attempt: u32, kind: &str) -> Duration {
    let w = base * 3u32.pow(attempt - 1);
    if kind == "package manager busy" {
        w * 6
    } else {
        w
    }
}

/// Sleeps unless the step is cancelled or the run stopped meanwhile; false when interrupted.
async fn pause(d: Duration, ctl: &mut watch::Receiver<Control>, base_seq: u32) -> bool {
    let sleep = tokio::time::sleep(d);
    tokio::pin!(sleep);
    loop {
        tokio::select! {
            _ = &mut sleep => return true,
            changed = ctl.changed() => {
                if changed.is_err() {
                    (&mut sleep).await;
                    return true;
                }
                let c = *ctl.borrow();
                if c.stop || c.cancel_seq != base_seq {
                    return false;
                }
            }
        }
    }
}

/// Values of secret-looking environment variables (`*TOKEN*`, `*SECRET*`, `*PASSWORD*`,
/// `*API_KEY*`, ...), longest first; installers that echo their environment must not put them
/// into logs.
pub fn secrets() -> &'static [String] {
    static SECRETS: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
    SECRETS.get_or_init(|| secret_values(std::env::vars()))
}

fn secret_values(vars: impl Iterator<Item = (String, String)>) -> Vec<String> {
    const MARKERS: [&str; 6] = [
        "TOKEN",
        "SECRET",
        "PASSWORD",
        "PASSWD",
        "API_KEY",
        "PRIVATE_KEY",
    ];
    let mut v: Vec<String> = vars
        .filter(|(k, val)| {
            let k = k.to_ascii_uppercase();
            val.len() >= 8 && MARKERS.iter().any(|m| k.contains(m))
        })
        .map(|(_, val)| val)
        .collect();
    v.sort_by_key(|s| std::cmp::Reverse(s.len()));
    v.dedup();
    v
}

pub fn redact(line: &str, secrets: &[String]) -> String {
    let mut out = line.to_string();
    for s in secrets {
        if out.contains(s.as_str()) {
            out = out.replace(s.as_str(), "***");
        }
    }
    out
}

fn update_state(state: &mut State, unit: &Unit, outcome: &Outcome, run_id: &str) {
    if matches!(outcome, Outcome::Skipped | Outcome::Blocked(_)) {
        return;
    }
    let ok = outcome.succeeded();
    let result = if ok {
        LastResult::Ok
    } else {
        LastResult::Failed
    };
    let error = outcome.detail().filter(|_| !ok);
    match unit.kind {
        Kind::Tool => {
            let prev = state.tools.get(&unit.id).cloned();
            let installed_at = if ok {
                Some(state::now_rfc3339())
            } else {
                prev.as_ref().and_then(|p| p.installed_at.clone())
            };
            state.tools.insert(
                unit.id.clone(),
                ToolRecord {
                    method: Some(unit.via.clone()),
                    scope: Some(unit.scope),
                    version: match outcome {
                        Outcome::Ok { version } => version.clone(),
                        _ => prev.as_ref().and_then(|p| p.version.clone()),
                    },
                    installed_at,
                    last_result: result,
                    last_error: error,
                    last_run: Some(run_id.into()),
                },
            );
        }
        Kind::Config => {
            let prev = state.configs.get(&unit.id).cloned();
            let applied = match (unit.action, ok) {
                (Action::Revert, true) => false,
                (_, true) => true,
                _ => prev.as_ref().is_some_and(|p| p.applied),
            };
            state.configs.insert(
                unit.id.clone(),
                ConfigRecord {
                    applied,
                    applied_at: if applied && ok {
                        Some(state::now_rfc3339())
                    } else {
                        prev.and_then(|p| p.applied_at)
                    },
                    last_result: result,
                    last_error: error,
                    last_run: Some(run_id.into()),
                },
            );
        }
    }
}

/// Renders shell files for the tools present now and ensures the rc blocks exist.
pub async fn regenerate_shell_env(ctx: &ExecCtx) -> Result<Vec<String>, String> {
    let mut present = BTreeSet::new();
    for (id, t) in &ctx.catalog.tools {
        if t.shell.is_some()
            && probe_tool(&ctx.catalog, &ctx.facts, id, ctx.runner.as_ref())
                .await
                .0
        {
            present.insert(id.clone());
        }
    }
    shellenv::install(&ctx.catalog, &ctx.facts, &ctx.paths, &present)
        .map(|files| {
            files
                .into_iter()
                .map(|p: PathBuf| p.display().to_string())
                .collect()
        })
        .map_err(|e| format!("{e:#}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::load::{embedded_layer, load};
    use crate::plan::Planner;
    use crate::runner::fake::FakeRunner;
    use std::collections::BTreeSet;

    struct Setup {
        _dir: tempfile::TempDir,
        ctx: ExecCtx,
        runner: Arc<FakeRunner>,
    }

    fn setup() -> Setup {
        let dir = tempfile::tempdir().unwrap();
        let facts = Facts::sample(dir.path());
        let paths = Paths {
            state_dir: dir.path().join("state"),
            config_dir: dir.path().join("config"),
        };
        let runner = Arc::new(FakeRunner::default());
        let ctx = ExecCtx {
            catalog: Arc::new(load(&[embedded_layer()]).unwrap().catalog),
            facts: Arc::new(facts),
            paths,
            runner: runner.clone(),
            run_id: "20261029-140500".into(),
            command: "test".into(),
            shell_env: false,
            step_timeout: STEP_TIMEOUT,
            retries: RETRIES,
            retry_delay: Duration::from_millis(5),
        };
        Setup {
            _dir: dir,
            ctx,
            runner,
        }
    }

    fn plan(ctx: &ExecCtx, targets: &[&str]) -> Plan {
        let present: BTreeSet<String> = ["curl", "unzip", "libatomic"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let t: Vec<String> = targets.iter().map(|s| s.to_string()).collect();
        Planner::new(&ctx.catalog, &ctx.facts, &present).plan(&t, false)
    }

    fn ctx_of(s: &Setup) -> ExecCtx {
        ExecCtx {
            catalog: s.ctx.catalog.clone(),
            facts: s.ctx.facts.clone(),
            paths: s.ctx.paths.clone(),
            runner: s.runner.clone(),
            run_id: s.ctx.run_id.clone(),
            command: "test".into(),
            shell_env: false,
            step_timeout: s.ctx.step_timeout,
            retries: s.ctx.retries,
            retry_delay: s.ctx.retry_delay,
        }
    }

    async fn run(s: &Setup, p: Plan) -> (RunRecord, State, Vec<Event>) {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let (_ctl_tx, ctl_rx) = watch::channel(Control::default());
        let (rec, st) = execute(p, ctx_of(s), State::default(), tx, ctl_rx).await;
        let mut events = Vec::new();
        while let Ok(e) = rx.try_recv() {
            events.push(e);
        }
        (rec, st, events)
    }

    fn outcomes(r: &RunRecord) -> Vec<(&str, &str)> {
        r.units
            .iter()
            .map(|u| (u.id.as_str(), u.outcome.as_str()))
            .collect()
    }

    #[tokio::test]
    async fn failure_blocks_only_dependents() {
        let s = setup();
        s.runner
            .rule(
                "fnm.vercel.app",
                1,
                "curl: (22) The requested URL returned error: 403",
            )
            .rule("herdr.dev", 0, "installed")
            .on_install("herdr.dev", "herdr --version", "herdr 0.3.1");
        let p = plan(&s.ctx, &["codex", "herdr"]);
        assert_eq!(
            p.units.iter().map(|u| u.id.as_str()).collect::<Vec<_>>(),
            vec!["fnm", "nodejs", "npm-prefix", "codex", "herdr"]
        );
        let (rec, st, events) = run(&s, p).await;
        assert_eq!(
            outcomes(&rec),
            vec![
                ("fnm", "failed"),
                ("nodejs", "blocked"),
                ("npm-prefix", "blocked"),
                ("codex", "blocked"),
                ("herdr", "ok")
            ]
        );
        assert_eq!(
            rec.unfinished(),
            vec!["fnm", "nodejs", "npm-prefix", "codex"]
        );
        assert_eq!(st.tools["herdr"].version.as_deref(), Some("0.3.1"));
        assert!(st.is_managed("herdr"));
        assert_eq!(st.tools["fnm"].last_result, LastResult::Failed);
        assert!(
            !st.tools.contains_key("nodejs"),
            "blocked units leave no record"
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Event::Line(0, l) if l.contains("403")))
        );
        // State and the run record are on disk.
        let disk = State::load(&s.ctx.paths).unwrap().unwrap();
        assert!(disk.is_managed("herdr"));
        assert_eq!(disk.last_run.as_deref(), Some("20261029-140500"));
        let log = std::fs::read_to_string(s.ctx.paths.run_dir("20261029-140500").join("fnm.log"))
            .unwrap();
        assert!(log.contains("403"), "{log}");
        assert!(RunRecord::load(&s.ctx.paths, "20261029-140500").is_ok());
    }

    #[tokio::test]
    async fn stopped_runs_leave_skipped_units_for_retry() {
        let s = setup();
        let p = plan(&s.ctx, &["herdr", "bun"]);
        let (tx, _rx) = mpsc::unbounded_channel();
        let (_ctl_tx, ctl_rx) = watch::channel(Control {
            stop: true,
            ..Default::default()
        });
        let (rec, _) = execute(p, ctx_of(&s), State::default(), tx, ctl_rx).await;
        assert_eq!(
            outcomes(&rec),
            vec![("herdr", "skipped"), ("bun", "skipped")]
        );
        assert_eq!(rec.unfinished(), vec!["herdr", "bun"]);
    }

    #[test]
    fn secret_values_are_redacted() {
        let vars = [
            ("GITHUB_TOKEN", "ghp_abcdefghijklmnop"),
            ("INFISICAL_CLIENT_SECRET", "s3cr3t-value"),
            ("db_password", "hunter2hunter2"),
            ("SHORT_TOKEN", "abc"),
            ("PATH", "/usr/bin:/bin"),
        ]
        .map(|(k, v)| (k.to_string(), v.to_string()));
        let s = secret_values(vars.into_iter());
        assert_eq!(s.len(), 3, "{s:?}");
        assert_eq!(
            redact("Authorization: Bearer ghp_abcdefghijklmnop at /usr/bin", &s),
            "Authorization: Bearer *** at /usr/bin"
        );
    }

    #[tokio::test]
    async fn unit_logs_have_a_header_timestamps_and_a_trailer() {
        let s = setup();
        s.runner
            .rule("herdr.dev", 0, "fetching\ninstalled")
            .on_install("herdr.dev", "herdr --version", "herdr 0.3.1");
        run(&s, plan(&s.ctx, &["herdr"])).await;
        let log =
            std::fs::read_to_string(s.ctx.paths.run_dir(&s.ctx.run_id).join("herdr.log")).unwrap();
        let lines: Vec<&str> = log.lines().collect();
        assert!(lines[0].starts_with("# envira "), "{log}");
        assert!(lines[0].contains("Ubuntu 24.04"), "{log}");
        assert!(lines[1].starts_with("# started "), "{log}");
        let ts = regex::Regex::new(r"^\d\d:\d\d:\d\d ").unwrap();
        assert!(
            lines[2..lines.len() - 1].iter().all(|l| ts.is_match(l)),
            "{log}"
        );
        assert!(log.contains(" installed\n"), "{log}");
        assert!(
            lines
                .last()
                .unwrap()
                .starts_with("# finished ok: 0.3.1 in "),
            "{log}"
        );
    }

    #[tokio::test]
    async fn transient_failures_are_retried() {
        let s = setup();
        s.runner
            .fail_then("herdr.dev", 1, "curl: (56) Connection reset by peer")
            .rule("herdr.dev", 0, "installed")
            .on_install("herdr.dev", "herdr --version", "herdr 0.3.1");
        let (rec, _, events) = run(&s, plan(&s.ctx, &["herdr"])).await;
        assert_eq!(outcomes(&rec), vec![("herdr", "ok")]);
        let ran = s.runner.ran();
        assert_eq!(ran.iter().filter(|c| c.contains("herdr.dev")).count(), 2);
        let lines: Vec<&str> = events
            .iter()
            .filter_map(|e| match e {
                Event::Line(_, l) => Some(l.as_str()),
                _ => None,
            })
            .collect();
        assert!(
            lines
                .iter()
                .any(|l| l.starts_with("[envira] hint (network)")),
            "{lines:?}"
        );
        assert!(
            lines
                .iter()
                .any(|l| l.contains("retrying in") && l.contains("attempt 2/3")),
            "{lines:?}"
        );
    }

    #[tokio::test]
    async fn retries_are_bounded_and_only_for_transient_causes() {
        let s = setup();
        s.runner
            .fail_then(
                "herdr.dev",
                9,
                "curl: (7) Failed to connect: Connection refused",
            )
            .fail_then(
                "bun.sh",
                9,
                "curl: (22) The requested URL returned error: 404",
            );
        let (rec, _, _) = run(&s, plan(&s.ctx, &["herdr", "bun"])).await;
        assert_eq!(outcomes(&rec), vec![("herdr", "failed"), ("bun", "failed")]);
        let ran = s.runner.ran();
        assert_eq!(ran.iter().filter(|c| c.contains("herdr.dev")).count(), 3);
        assert_eq!(ran.iter().filter(|c| c.contains("bun.sh")).count(), 1);
        assert!(
            rec.units[0]
                .detail
                .as_deref()
                .unwrap()
                .ends_with("(network)")
        );
        assert!(
            rec.units[1]
                .detail
                .as_deref()
                .unwrap()
                .ends_with("(not found)")
        );
        assert!(
            rec.units[1]
                .hint
                .as_deref()
                .unwrap()
                .starts_with("(not found): ")
        );
        assert!(
            rec.units[0]
                .hint
                .as_deref()
                .unwrap()
                .starts_with("(network): ")
        );
    }

    #[tokio::test]
    async fn retry_waits_end_on_stop() {
        let (tx, mut rx) = watch::channel(Control::default());
        let h = tokio::spawn(async move { pause(Duration::from_secs(30), &mut rx, 0).await });
        tokio::time::sleep(Duration::from_millis(20)).await;
        tx.send_modify(|c| c.stop = true);
        assert!(!h.await.unwrap());
        assert_eq!(
            retry_wait(Duration::from_secs(5), 2, "network"),
            Duration::from_secs(15)
        );
        assert_eq!(
            retry_wait(Duration::from_secs(5), 1, "package manager busy"),
            Duration::from_secs(30)
        );
    }

    #[tokio::test]
    async fn hung_steps_time_out_and_fail() {
        let mut s = setup();
        s.ctx.step_timeout = Duration::from_millis(100);
        s.runner.hang("herdr.dev").rule("bun.sh", 0, "").on_install(
            "bun.sh",
            "bun --version",
            "1.3.0",
        );
        let (rec, _, _) = run(&s, plan(&s.ctx, &["herdr", "bun"])).await;
        assert_eq!(outcomes(&rec), vec![("herdr", "failed"), ("bun", "ok")]);
        let detail = rec.units[0].detail.as_deref().unwrap();
        assert!(detail.contains("timed out after"), "{detail}");
    }

    #[test]
    fn interrupted_runs_keep_pending_units() {
        // A record saved before a crash lists every planned unit; the ones never reached
        // are `pending` and retried.
        let s = setup();
        let p = plan(&s.ctx, &["herdr", "bun"]);
        let rec = RunRecord::planned(&p, "20261029-140500", "sync");
        assert_eq!(
            outcomes(&rec),
            vec![("herdr", "pending"), ("bun", "pending")]
        );
        assert_eq!(rec.unfinished(), vec!["herdr", "bun"]);
    }

    #[tokio::test]
    async fn install_that_is_not_detected_fails() {
        let s = setup();
        s.runner.rule("herdr.dev", 0, "");
        let (rec, _, _) = run(&s, plan(&s.ctx, &["herdr"])).await;
        assert_eq!(outcomes(&rec), vec![("herdr", "failed")]);
        assert!(
            rec.units[0]
                .detail
                .as_deref()
                .unwrap()
                .contains("not detected")
        );
    }

    #[tokio::test]
    async fn config_steps_skip_when_check_passes() {
        let s = setup();
        // First step's check passes, second step applies and its check then passes.
        s.runner
            .rule("test -d", 0, "")
            .rule("fs toml-set /", 0, "")
            .on_install("fs toml-set /", "fs toml-set --check", "");
        let present: BTreeSet<String> = ["yazi".to_string()].into();
        let p = Planner::new(&s.ctx.catalog, &s.ctx.facts, &present)
            .plan(&["yazi-theme".into()], false);
        let (rec, st, _) = run(&s, p).await;
        assert_eq!(outcomes(&rec), vec![("yazi-theme", "ok")]);
        assert!(st.configs["yazi-theme"].applied);
        let ran = s.runner.ran();
        assert_eq!(ran.len(), 1, "{ran:?}");
        assert!(ran[0].contains("toml-set"));
    }

    #[tokio::test]
    async fn update_adopts_and_records_the_new_version() {
        let s = setup();
        s.runner
            .rule("codex --version", 0, "codex-cli 0.46.0")
            .rule(
                "npm install -g @openai/codex@latest",
                0,
                "changed 1 package",
            )
            .on_install(
                "@openai/codex@latest",
                "codex --version",
                "codex-cli 0.48.0",
            );
        let present: BTreeSet<String> = ["codex".to_string()].into();
        let p = Planner::new(&s.ctx.catalog, &s.ctx.facts, &present).update(&[
            crate::plan::UpdateTarget {
                id: "codex".into(),
                route: crate::plan::Route {
                    entry: 0,
                    via: "npm".into(),
                    scope: crate::state::Scope::User,
                },
                reason: "0.46.0 → 0.50.0".into(),
                expect: Some("0.50.0".into()),
            },
        ]);
        let (rec, st, events) = run(&s, p).await;
        assert_eq!(outcomes(&rec), vec![("codex", "ok")]);
        assert_eq!(rec.units[0].action, "update");
        // Not in state before: the update makes it managed, with the method it lives in.
        assert!(st.is_managed("codex"));
        assert_eq!(st.tools["codex"].method.as_deref(), Some("npm"));
        assert_eq!(st.tools["codex"].version.as_deref(), Some("0.48.0"));
        assert!(events.iter().any(
            |e| matches!(e, Event::Line(0, l) if l.contains("still 0.48.0; the latest is 0.50.0"))
        ));
    }
}
