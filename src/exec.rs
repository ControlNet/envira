//! Executes a plan unit by unit. A failure blocks only the units that depend on it; state and
//! the run record are written after every unit.

use std::collections::BTreeSet;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::{mpsc, watch};

use crate::catalog::Catalog;
use crate::facts::Facts;
use crate::plan::{Action, Kind, Plan, Unit};
use crate::runner::{Runner, StreamEnd};
use crate::shellenv;
use crate::state::{
    self, ConfigRecord, LastResult, Paths, RunRecord, State, ToolRecord, UnitRecord,
};
use crate::status::{probe_config, probe_tool};

const CHECK_TIMEOUT: Duration = Duration::from_secs(30);
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
    let mut record = RunRecord {
        id: ctx.run_id.clone(),
        command: ctx.command.clone(),
        started: state::now_rfc3339(),
        ..Default::default()
    };
    let mut bad: BTreeSet<String> = BTreeSet::new();

    for (idx, unit) in plan.units.iter().enumerate() {
        let started = Instant::now();
        let outcome = if ctl.borrow().stop {
            Outcome::Skipped
        } else if let Some(d) = unit.deps.iter().find(|d| bad.contains(*d)) {
            Outcome::Blocked(d.clone())
        } else {
            let _ = tx.send(Event::UnitStarted(idx));
            run_unit(idx, unit, &ctx, &tx, &mut ctl, &run_dir).await
        };
        if !outcome.succeeded() {
            bad.insert(unit.id.clone());
        }
        let elapsed = started.elapsed();
        update_state(&mut state, unit, &outcome, &ctx.run_id);
        record.units.push(UnitRecord {
            id: unit.id.clone(),
            action: unit.action.as_str().into(),
            via: unit.via.clone(),
            outcome: outcome.as_str().into(),
            detail: outcome.detail(),
            seconds: (elapsed.as_secs_f64() * 10.0).round() / 10.0,
        });
        if let Err(e) = state
            .save(&ctx.paths, &ctx.facts)
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
    let _ = state.save(&ctx.paths, &ctx.facts);
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
) -> Outcome {
    let log_path = run_dir.join(format!("{}.log", unit.id));
    let log = std::fs::File::create(&log_path).ok();
    if log.is_some() {
        state::chown_to_target(&log_path, &ctx.facts);
    }
    let log = Arc::new(std::sync::Mutex::new(log));
    let emit = {
        let log = log.clone();
        let tx = tx.clone();
        move |line: String| {
            if let Some(f) = log.lock().unwrap().as_mut() {
                let _ = writeln!(f, "{line}");
            }
            let _ = tx.send(Event::Line(idx, line));
        }
    };

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
        let (ltx, mut lrx) = mpsc::unbounded_channel::<String>();
        let (ctx_cancel, crx) = watch::channel(false);
        let forward = {
            let emit = emit.clone();
            tokio::spawn(async move {
                while let Some(l) = lrx.recv().await {
                    emit(l);
                }
            })
        };
        let fut = ctx.runner.stream(cmd, tmp.path(), ltx, crx);
        tokio::pin!(fut);
        let mut watching = true;
        let end = loop {
            tokio::select! {
                end = &mut fut => break end,
                changed = ctl.changed(), if watching => {
                    if changed.is_err() {
                        watching = false;
                    } else if ctl.borrow().cancel_seq != base_seq {
                        let _ = ctx_cancel.send(true);
                    }
                }
            }
        };
        let _ = forward.await;
        match end {
            StreamEnd::Exit(0) => {}
            StreamEnd::Exit(code) => {
                emit(format!("[envira] {label} exited with code {code}"));
                return Outcome::Failed(format!("{} exited with code {code}", step.label));
            }
            StreamEnd::Cancelled => {
                emit("[envira] cancelled".into());
                return Outcome::Cancelled;
            }
            StreamEnd::SpawnError(e) => return Outcome::Failed(format!("could not start: {e}")),
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
        };
        Setup {
            _dir: dir,
            ctx,
            runner,
        }
    }

    fn plan(ctx: &ExecCtx, targets: &[&str]) -> Plan {
        let present: BTreeSet<String> = ["unzip".to_string(), "libatomic".to_string()].into();
        let t: Vec<String> = targets.iter().map(|s| s.to_string()).collect();
        Planner::new(&ctx.catalog, &ctx.facts, &present).plan(&t, false)
    }

    async fn run(s: &Setup, p: Plan) -> (RunRecord, State, Vec<Event>) {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let (_ctl_tx, ctl_rx) = watch::channel(Control::default());
        let ctx = ExecCtx {
            catalog: s.ctx.catalog.clone(),
            facts: s.ctx.facts.clone(),
            paths: s.ctx.paths.clone(),
            runner: s.runner.clone(),
            run_id: s.ctx.run_id.clone(),
            command: "test".into(),
            shell_env: false,
        };
        let (rec, st) = execute(p, ctx, State::default(), tx, ctl_rx).await;
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
            vec!["fnm", "nodejs", "codex", "herdr"]
        );
        let (rec, st, events) = run(&s, p).await;
        assert_eq!(
            outcomes(&rec),
            vec![
                ("fnm", "failed"),
                ("nodejs", "blocked"),
                ("codex", "blocked"),
                ("herdr", "ok")
            ]
        );
        assert_eq!(rec.unfinished(), vec!["fnm", "nodejs", "codex"]);
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
