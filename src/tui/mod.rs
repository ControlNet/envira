//! Terminal UI: one event channel fed by input, a ticker, detection and the executor; the UI
//! only folds events into `App` and redraws, so it never blocks on work.

pub mod app;
pub mod theme;
pub mod ui;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
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
use crate::selfupdate;
use crate::state::{self, State};
use crate::status::{self, Snapshot};
use app::{App, Cmd, Screen};
use theme::Theme;

enum AppEvent {
    Term(TermEvent),
    Tick,
    Snapshot(Box<Snapshot>, Option<State>),
    Latest(BTreeMap<String, Latest>),
    /// A newer envira release, if any.
    SelfLatest(Option<String>),
    Exec(exec::Event),
    Error(String),
    /// SIGTERM or SIGHUP (the terminal went away).
    Terminate,
    /// What sudo said to the password typed in the dialog, with the password when it took it.
    Sudo(
        crate::sudo_prompt::Check,
        Option<crate::sudo_prompt::Secret>,
    ),
}

/// What SIGTERM/SIGHUP need besides the UI loop, which can be stuck writing to a terminal that
/// stopped reading (flow control, a stalled SSH connection): the running plan's control, and
/// whether its record is final.
#[derive(Default)]
struct Shutdown {
    run: std::sync::Mutex<Option<(watch::Sender<Control>, Arc<AtomicBool>)>>,
    /// Self-update has the terminal (and may be replacing the binary): wait for it.
    away: AtomicBool,
}

impl Shutdown {
    fn settled(&self) -> bool {
        let run_done = self
            .run
            .lock()
            .unwrap()
            .as_ref()
            .is_none_or(|(_, done)| done.load(Ordering::SeqCst));
        run_done && !self.away.load(Ordering::SeqCst)
    }
}

/// SIGTERM/SIGHUP: cancels the running steps here, then tells the UI loop. Once nothing is left
/// to record, the UI loop gets a few seconds to restore the terminal and leave; if it is stuck
/// on the terminal, envira leaves without it (128 + signal).
fn spawn_signals(tx: mpsc::UnboundedSender<AppEvent>, shutdown: Arc<Shutdown>) {
    use tokio::signal::unix::{SignalKind, signal};
    tokio::spawn(async move {
        let (Ok(mut term), Ok(mut hup)) = (
            signal(SignalKind::terminate()),
            signal(SignalKind::hangup()),
        ) else {
            return;
        };
        loop {
            let sig = tokio::select! {
                _ = term.recv() => libc::SIGTERM,
                _ = hup.recv() => libc::SIGHUP,
            };
            if let Some((ctl, _)) = shutdown.run.lock().unwrap().as_ref() {
                ctl.send_modify(|c| {
                    c.stop = true;
                    c.cancel_seq += 1;
                });
            }
            let _ = tx.send(AppEvent::Terminate);
            let shutdown = shutdown.clone();
            tokio::spawn(async move {
                while !shutdown.settled() {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                tokio::time::sleep(Duration::from_secs(3)).await;
                std::process::exit(128 + sig);
            });
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

/// Looks for a newer envira release (`selfupdate::newer`), off the UI thread.
fn spawn_self_check(sess: &Session, tx: mpsc::UnboundedSender<AppEvent>, fetch: Fetch) {
    let (paths, facts) = (sess.paths.clone(), sess.facts.clone());
    tokio::task::spawn_blocking(move || {
        let _ = tx.send(AppEvent::SelfLatest(selfupdate::newer(
            &paths, &facts, fetch,
        )));
    });
}

async fn event_loop(term: &mut DefaultTerminal, mut sess: Session, theme: Theme) -> Result<i32> {
    let (tx, mut rx) = mpsc::unbounded_channel::<AppEvent>();
    let mut input = spawn_input(tx.clone());
    let shutdown = Arc::new(Shutdown::default());
    spawn_signals(tx.clone(), shutdown.clone());
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
    spawn_self_check(&sess, tx.clone(), Fetch::IfStale);
    let mut app = App::new(
        &sess.loaded,
        sess.catalog.clone(),
        sess.facts.clone(),
        sess.paths.clone(),
        sess.prefer_system(),
    );
    app.checking_latest = true;
    app.scope_fixed = sess.global.scope.is_some();
    let mut keepalive: Option<JoinHandle<()>> = None;
    // sudo's password, from the dialog to the run that needs it, and its helper during the run.
    let mut password: Option<crate::sudo_prompt::Secret> = None;
    let mut askpass: Option<crate::askpass::Askpass> = None;
    // The running plan's events, read here and not forwarded: while the screen is busy they
    // wait in their own bounded queue.
    let mut exec_rx: Option<exec::EventRx> = None;
    // The terminal failed while a run was going: it is being cancelled, and envira leaves with
    // this error once the run is recorded.
    let mut broken: Option<std::io::Error> = None;

    loop {
        // After SIGHUP the terminal is gone; keep winding the run down without drawing.
        if let Err(e) = term.draw(|f| ui::draw(f, &app, &theme))
            && !app.quitting
        {
            // Leaving now would drop the run halfway, its steps still running: stop it first.
            if app.terminate() {
                return Err(e.into());
            }
            broken = Some(e);
        }
        let first = tokio::select! {
            ev = rx.recv() => match ev {
                Some(ev) => ev,
                None => return Ok(0),
            },
            Some(ev) = next_exec(&mut exec_rx) => AppEvent::Exec(ev),
        };
        // Fold what is already queued into one redraw (fast installer output), up to a limit
        // so keys and redraws keep coming during a flood.
        let mut batch = vec![first];
        while batch.len() < BATCH
            && let Ok(ev) = rx.try_recv()
        {
            batch.push(ev);
        }
        if let Some(erx) = &mut exec_rx {
            while batch.len() < BATCH
                && let Ok(ev) = erx.try_recv()
            {
                batch.push(AppEvent::Exec(ev));
            }
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
                AppEvent::SelfLatest(v) => app.on_self_latest(v),
                AppEvent::Exec(e) => {
                    let finished = matches!(e, exec::Event::Finished(_));
                    if finished {
                        exec_rx = None;
                        if let Some(k) = keepalive.take() {
                            k.abort();
                        }
                        drop(askpass.take());
                    }
                    app.on_exec(e);
                    if finished && app.quitting {
                        return match broken.take() {
                            Some(e) => Err(e.into()),
                            None => Ok(1),
                        };
                    }
                }
                AppEvent::Sudo(check, pw) => {
                    password = pw;
                    cmds.extend(app.on_sudo(check));
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
                    if fetch != Fetch::CacheOnly {
                        spawn_self_check(&sess, tx.clone(), fetch);
                    }
                }
                Cmd::Reload => match sess.reload() {
                    Ok(()) => {
                        app.catalog = sess.catalog.clone();
                        app.catalog_warnings = sess.loaded.warnings.clone();
                        match app::catalog_warning(&app.catalog_warnings) {
                            Some(w) => app.warn(w),
                            None => app.notify("Catalog reloaded"),
                        }
                        spawn_detect(&sess, tx.clone(), Fetch::CacheOnly, app.prefer_system);
                    }
                    Err(e) => {
                        app.detecting = false;
                        app.notify(format!("{e:#}").lines().next().unwrap_or("").to_string());
                    }
                },
                Cmd::SelfUpdate => {
                    shutdown.away.store(true, Ordering::SeqCst);
                    let r = self_update(term, &mut input, &tx).await;
                    shutdown.away.store(false, Ordering::SeqCst);
                    let (msg, updated) = r?;
                    if updated {
                        // This process is still the old version; the next start is the new one.
                        app.self_latest = None;
                        app.self_updated = true;
                    }
                    if let Some(msg) = msg {
                        app.warn(msg);
                    }
                }
                Cmd::SaveBundles { bundles, seen } => {
                    match State::update(&sess.paths, &sess.facts, |st| {
                        st.bundles = bundles;
                        st.seen.extend(seen);
                    }) {
                        Ok(st) => app.state = Some(st),
                        Err(e) => app.notify(format!("could not save state: {e:#}")),
                    }
                }
                Cmd::Adopt(records) => {
                    // Not while a run goes on: it saves the tools it read when it started.
                    let saved =
                        state::RunLock::acquire(&sess.paths, &sess.facts).and_then(|_lock| {
                            State::update(&sess.paths, &sess.facts, |st| st.tools.extend(records))
                        });
                    match saved {
                        Ok(st) => app.state = Some(st),
                        Err(e) => app.notify(format!("not adopted: {e:#}")),
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
                Cmd::Sudo(password) => {
                    let tx = tx.clone();
                    tokio::task::spawn_blocking(move || {
                        let check = crate::sudo_prompt::check(&password);
                        let keep = (check == crate::sudo_prompt::Check::Ok).then_some(password);
                        let _ = tx.send(AppEvent::Sudo(check, keep));
                    });
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
                        // The password typed in the dialog goes to the steps' sudo.
                        match password.take() {
                            Some(pw) => match crate::askpass::Askpass::start(pw) {
                                Ok(a) => askpass = Some(a),
                                Err(e) => {
                                    app.screen = Screen::Dashboard;
                                    app.notify(format!(
                                        "cannot hand sudo's password to the steps: {e}"
                                    ));
                                    continue;
                                }
                            },
                            // sudo wants a password after all (a ticket from this terminal,
                            // which steps do not see, or one that ran out): ask in the dialog.
                            None if !crate::app::sudo_without_password() => {
                                drop(lock);
                                app.ask_sudo(plan, command);
                                continue;
                            }
                            None => {}
                        }
                        keepalive = Some(crate::app::sudo_keepalive());
                    }
                    exec_rx = Some(start_run(&sess, &mut app, plan, command, lock, &shutdown));
                }
            }
        }
    }
}

/// `envira self-update` on the normal screen (it asks before changing anything), then offers
/// to restart into the new binary. Returns a toast for when it comes back to the TUI, and
/// whether the binary was replaced.
async fn self_update(
    term: &mut DefaultTerminal,
    input: &mut JoinHandle<()>,
    tx: &mpsc::UnboundedSender<AppEvent>,
) -> Result<(Option<String>, bool)> {
    input.abort();
    restore();
    let opts = selfupdate::Opts {
        check: false,
        version: None,
        yes: false,
    };
    let res = tokio::task::spawn_blocking(move || selfupdate::run(&opts)).await?;
    let updated = matches!(res, Ok(selfupdate::Outcome::Updated(_)));
    let toast = match res {
        Ok(selfupdate::Outcome::Updated(exe)) => {
            if ask("Restart envira now? [Y/n] ", true) {
                use std::os::unix::process::CommandExt;
                // Only returns when the exec failed.
                let e = std::process::Command::new(&exe)
                    .args(std::env::args_os().skip(1))
                    .exec();
                Some(format!("could not restart {}: {e}", exe.display()))
            } else {
                Some("envira was updated: restart it to use the new version".into())
            }
        }
        Ok(selfupdate::Outcome::Unchanged(_)) => {
            ask("Press enter to go back to envira ", true);
            None
        }
        Err(e) => {
            eprintln!("envira: {e:#}");
            ask("Press enter to go back to envira ", true);
            Some("envira was not updated".into())
        }
    };
    *term = init();
    term.clear()?;
    *input = spawn_input(tx.clone());
    Ok((toast, updated))
}

/// Reads a yes/no answer from the terminal; `default` on a bare enter.
fn ask(prompt: &str, default: bool) -> bool {
    use std::io::Write;
    print!("{prompt}");
    let _ = std::io::stdout().flush();
    let mut answer = String::new();
    if std::io::stdin().read_line(&mut answer).is_err() {
        return default;
    }
    match answer.trim() {
        "" => default,
        a => matches!(a, "y" | "Y" | "yes"),
    }
}

/// Events handled before the next redraw, at most.
const BATCH: usize = 2048;

async fn next_exec(rx: &mut Option<exec::EventRx>) -> Option<exec::Event> {
    match rx {
        Some(rx) => rx.recv().await,
        None => std::future::pending().await,
    }
}

/// Starts the plan; returns its events.
fn start_run(
    sess: &Session,
    app: &mut App,
    plan: Plan,
    command: String,
    lock: state::RunLock,
    shutdown: &Shutdown,
) -> exec::EventRx {
    let (ctl_tx, ctl_rx) = watch::channel(Control::default());
    let done = Arc::new(AtomicBool::new(false));
    *shutdown.run.lock().unwrap() = Some((ctl_tx.clone(), done.clone()));
    let (etx, erx) = exec::events();
    let ctx = sess.exec_ctx(command, true);
    let run_id = ctx.run_id.clone();
    // The state on disk, read under the run lock: `app.state` is from the last scan, and a
    // run finished since (retry with `r` right away) would lose that run's records.
    let st = match sess.state() {
        Ok(Some(st)) => st,
        _ => app.state.clone().unwrap_or_default(),
    };
    app.state = Some(st.clone());
    let run = plan.clone();
    tokio::spawn(async move {
        let _lock = lock;
        let r = exec::execute(run, ctx, st, etx, ctl_rx).await;
        done.store(true, Ordering::SeqCst);
        r
    });
    app.start_run(plan, run_id, Some(ctl_tx));
    erx
}
