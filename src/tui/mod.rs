//! Terminal UI: one event channel fed by input, a ticker, detection and the executor; the UI
//! only folds events into `App` and redraws, so it never blocks on work.

pub mod app;
pub mod theme;
pub mod ui;

use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{Event as TermEvent, EventStream, KeyEventKind, MouseEventKind};
use futures::StreamExt;
use ratatui::DefaultTerminal;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

use crate::app::Session;
use crate::exec::{self, Control};
use crate::latest::{self, Fetch, Latest};
use crate::plan::Plan;
use crate::state::{self, State};
use crate::status::{self, Snapshot};
use app::{App, Cmd, Screen};
use theme::Theme;

enum AppEvent {
    Term(TermEvent),
    Tick,
    Snapshot(Box<Snapshot>, Option<State>),
    Latest(BTreeMap<String, Latest>),
    Exec(exec::Event),
    Error(String),
    /// SIGTERM or SIGHUP (the terminal went away).
    Terminate,
}

fn spawn_signals(tx: mpsc::UnboundedSender<AppEvent>) {
    use tokio::signal::unix::{SignalKind, signal};
    tokio::spawn(async move {
        let (Ok(mut term), Ok(mut hup)) = (
            signal(SignalKind::terminate()),
            signal(SignalKind::hangup()),
        ) else {
            return;
        };
        loop {
            tokio::select! {
                _ = term.recv() => {}
                _ = hup.recv() => {}
            }
            if tx.send(AppEvent::Terminate).is_err() {
                break;
            }
        }
    });
}

pub async fn run(sess: Session) -> Result<i32> {
    let theme = Theme::detect();
    let mut terminal = init();
    // ratatui's hook restores the screen on a panic; the mouse must be released too.
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        mouse(false);
        hook(info);
    }));
    let res = event_loop(&mut terminal, sess, theme).await;
    restore();
    res
}

/// Clicks and the wheel are on unless `ENVIRA_MOUSE=0`.
fn mouse_on() -> bool {
    std::env::var("ENVIRA_MOUSE").map_or(true, |v| v != "0")
}

/// Button and wheel reports only (1000, SGR 1006): no motion events, so moving the pointer
/// costs nothing, even over SSH.
fn mouse(on: bool) {
    use std::io::Write;
    if !mouse_on() {
        return;
    }
    let seq: &[u8] = if on {
        b"\x1b[?1000h\x1b[?1006h"
    } else {
        b"\x1b[?1006l\x1b[?1000l"
    };
    let mut out = std::io::stdout();
    let _ = out.write_all(seq);
    let _ = out.flush();
}

fn init() -> DefaultTerminal {
    let t = ratatui::init();
    mouse(true);
    t
}

fn restore() {
    mouse(false);
    ratatui::restore();
}

fn spawn_input(tx: mpsc::UnboundedSender<AppEvent>) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut events = EventStream::new();
        while let Some(Ok(ev)) = events.next().await {
            if let TermEvent::Mouse(m) = &ev
                && matches!(m.kind, MouseEventKind::Moved | MouseEventKind::Drag(_))
            {
                continue;
            }
            if tx.send(AppEvent::Term(ev)).is_err() {
                break;
            }
        }
    })
}

/// Detects everything and sends the snapshot (with cached latest versions), then checks latest
/// versions per `fetch` and sends those answers.
/// `prefer_system` is the TUI's current scope choice (`S`), which can differ from `--scope`.
fn spawn_detect(
    sess: &Session,
    tx: mpsc::UnboundedSender<AppEvent>,
    fetch: Fetch,
    prefer_system: bool,
) {
    let (cat, facts, runner, paths) = (
        sess.catalog.clone(),
        sess.facts.clone(),
        sess.runner.clone(),
        sess.paths.clone(),
    );
    tokio::spawn(async move {
        let state = match State::load(&paths) {
            Ok(s) => s,
            Err(e) => {
                let _ = tx.send(AppEvent::Error(format!("{e:#}")));
                None
            }
        };
        let mut snap =
            status::snapshot(&cat, &facts, state.as_ref(), runner.as_ref(), prefer_system).await;
        let mut cache = latest::Cache::load(&paths);
        let cached = latest::check(
            &cat,
            &facts,
            &snap,
            runner.as_ref(),
            &mut cache,
            Fetch::CacheOnly,
        )
        .await;
        snap.apply_latest(&cached);
        let fresh = (fetch != Fetch::CacheOnly).then(|| snap.clone());
        if tx.send(AppEvent::Snapshot(Box::new(snap), state)).is_err() {
            return;
        }
        if let Some(snap) = fresh {
            let answers =
                latest::check(&cat, &facts, &snap, runner.as_ref(), &mut cache, fetch).await;
            let _ = cache.save(&paths, &facts);
            let _ = tx.send(AppEvent::Latest(answers));
        }
    });
}

async fn event_loop(term: &mut DefaultTerminal, mut sess: Session, theme: Theme) -> Result<i32> {
    let (tx, mut rx) = mpsc::unbounded_channel::<AppEvent>();
    let mut input = spawn_input(tx.clone());
    spawn_signals(tx.clone());
    {
        let tx = tx.clone();
        tokio::spawn(async move {
            let mut iv = tokio::time::interval(Duration::from_millis(200));
            loop {
                iv.tick().await;
                if tx.send(AppEvent::Tick).is_err() {
                    break;
                }
            }
        });
    }
    spawn_detect(&sess, tx.clone(), Fetch::IfStale, sess.prefer_system());
    let mut app = App::new(
        &sess.loaded,
        sess.catalog.clone(),
        sess.facts.clone(),
        sess.paths.clone(),
        sess.prefer_system(),
    );
    app.checking_latest = true;
    let mut keepalive: Option<JoinHandle<()>> = None;

    loop {
        // After SIGHUP the terminal is gone; keep winding the run down without drawing.
        if let Err(e) = term.draw(|f| ui::draw(f, &app, &theme))
            && !app.quitting
        {
            return Err(e.into());
        }
        let Some(first) = rx.recv().await else {
            return Ok(0);
        };
        // Fold everything already queued before the next redraw (fast installer output).
        let mut batch = vec![first];
        while let Ok(ev) = rx.try_recv() {
            batch.push(ev);
        }
        let mut cmds = Vec::new();
        for ev in batch {
            match ev {
                AppEvent::Term(TermEvent::Key(k)) if k.kind == KeyEventKind::Press => {
                    cmds.extend(app.on_key(k))
                }
                AppEvent::Term(TermEvent::Mouse(m)) => cmds.extend(app.on_mouse(m)),
                AppEvent::Term(_) => {}
                AppEvent::Tick => app.tick += 1,
                AppEvent::Snapshot(s, st) => app.on_snapshot(*s, st),
                AppEvent::Latest(a) => app.on_latest(a),
                AppEvent::Exec(e) => {
                    if matches!(e, exec::Event::Finished(_))
                        && let Some(k) = keepalive.take()
                    {
                        k.abort();
                    }
                    let finished = matches!(e, exec::Event::Finished(_));
                    app.on_exec(e);
                    if finished && app.quitting {
                        return Ok(1);
                    }
                }
                AppEvent::Terminate => {
                    if app.terminate() {
                        return Ok(1);
                    }
                }
                AppEvent::Error(e) => {
                    app.detecting = false;
                    app.notify(e);
                }
            }
        }
        for cmd in cmds {
            match cmd {
                Cmd::Quit => return Ok(0),
                Cmd::Refresh(fetch) => {
                    app.detecting = true;
                    app.checking_latest |= fetch != Fetch::CacheOnly;
                    spawn_detect(&sess, tx.clone(), fetch, app.prefer_system);
                }
                Cmd::Reload => match sess.reload() {
                    Ok(()) => {
                        app.catalog = sess.catalog.clone();
                        app.loaded_layers = sess.loaded.layers.clone();
                        app.notify("Catalog reloaded");
                        spawn_detect(&sess, tx.clone(), Fetch::CacheOnly, app.prefer_system);
                    }
                    Err(e) => {
                        app.detecting = false;
                        app.notify(format!("{e:#}").lines().next().unwrap_or("").to_string());
                    }
                },
                Cmd::SaveState => {
                    if let Some(st) = &app.state
                        && let Err(e) = st.save(&sess.paths, &sess.facts)
                    {
                        app.notify(format!("could not save state: {e:#}"));
                    }
                }
                Cmd::ExportScript(plan) => {
                    let f = sess.paths.state_dir.join("plan.sh");
                    match state::write_atomic(
                        &f,
                        crate::plan::as_script(&plan).as_bytes(),
                        &sess.facts,
                    ) {
                        Ok(()) => app.notify(format!("Saved {}", f.display())),
                        Err(e) => app.notify(format!("{e:#}")),
                    }
                }
                Cmd::StartRun { plan, command } => {
                    let lock = match state::RunLock::acquire(&sess.paths, &sess.facts) {
                        Ok(l) => l,
                        Err(e) => {
                            app.screen = Screen::Dashboard;
                            app.notify(format!("{e:#}"));
                            continue;
                        }
                    };
                    if plan.needs_sudo(&sess.facts) {
                        match authorize_sudo(term, &mut input, &tx) {
                            Ok(true) => keepalive = Some(spawn_keepalive()),
                            Ok(false) => {
                                app.notify("sudo authentication failed; nothing was run");
                                continue;
                            }
                            Err(e) => return Err(e),
                        }
                    }
                    start_run(&sess, &mut app, plan, command, lock, tx.clone());
                }
            }
        }
    }
}

/// Refreshes sudo credentials, prompting on the normal screen when a password is needed.
fn authorize_sudo(
    term: &mut DefaultTerminal,
    input: &mut JoinHandle<()>,
    tx: &mpsc::UnboundedSender<AppEvent>,
) -> Result<bool> {
    if crate::app::sudo_without_password() {
        return Ok(true);
    }
    // Stop reading keys so the password prompt gets them, then hand the terminal back.
    input.abort();
    restore();
    println!("envira: this plan needs sudo (asked once; kept alive during the run).");
    let ok = std::process::Command::new("sudo")
        .arg("-v")
        .status()
        .is_ok_and(|s| s.success());
    *term = init();
    term.clear()?;
    *input = spawn_input(tx.clone());
    Ok(ok)
}

fn spawn_keepalive() -> JoinHandle<()> {
    tokio::spawn(async {
        loop {
            tokio::time::sleep(Duration::from_secs(60)).await;
            let _ = tokio::process::Command::new("sudo")
                .args(["-n", "-v"])
                .stdin(std::process::Stdio::null())
                .status()
                .await;
        }
    })
}

fn start_run(
    sess: &Session,
    app: &mut App,
    plan: Plan,
    command: String,
    lock: state::RunLock,
    tx: mpsc::UnboundedSender<AppEvent>,
) {
    let (ctl_tx, ctl_rx) = watch::channel(Control::default());
    let (etx, mut erx) = mpsc::unbounded_channel();
    let ctx = sess.exec_ctx(command, true);
    let run_id = ctx.run_id.clone();
    let st = app.state.clone().unwrap_or_default();
    let run = plan.clone();
    tokio::spawn(async move {
        let _lock = lock;
        exec::execute(run, ctx, st, etx, ctl_rx).await
    });
    tokio::spawn(async move {
        while let Some(e) = erx.recv().await {
            if tx.send(AppEvent::Exec(e)).is_err() {
                break;
            }
        }
    });
    app.start_run(plan, run_id, Some(ctl_tx));
}
