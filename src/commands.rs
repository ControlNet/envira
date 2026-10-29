//! Headless commands.

use std::io::{IsTerminal, Write};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use tokio::sync::{mpsc, watch};

use crate::app::{self, Session};
use crate::catalog::{self, Catalog, validate};
use crate::cli::{CatalogCmd, FsCmd, Global, RunOpts};
use crate::exec::{self, Control, Event, ExecCtx, Outcome};
use crate::fs;
use crate::latest::Fetch;
use crate::plan::{Plan, as_script};
use crate::plan::{Route, UpdateTarget};
use crate::shellenv;
use crate::state::ToolRecord;
use crate::state::{self, Paths, RunRecord, State};
use crate::status::{ItemStatus, Label, Ownership, Snapshot};

pub fn fmt_duration(d: Duration) -> String {
    let s = d.as_secs();
    if s >= 60 {
        format!("{}:{:02}", s / 60, s % 60)
    } else {
        format!("{:.1}s", d.as_secs_f64())
    }
}

// ------------------------------------------------------------------------------------------------
// status / doctor / log
// ------------------------------------------------------------------------------------------------

pub async fn status(sess: &Session, json: bool, latest: bool, refresh: bool) -> Result<i32> {
    let state = sess.state()?;
    let mut snap = sess.snapshot(state.as_ref()).await;
    let fetch = match (latest, refresh) {
        (_, true) => Fetch::Force,
        (true, false) => Fetch::IfStale,
        _ => Fetch::CacheOnly,
    };
    sess.apply_latest(&mut snap, fetch).await;
    if json {
        println!("{}", serde_json::to_string_pretty(&snap.items)?);
        return Ok(0);
    }
    println!(
        "envira {} · {} · {}",
        env!("CARGO_PKG_VERSION"),
        sess.facts.hostname,
        sess.facts.summary()
    );
    println!("catalog: {}", sess.loaded.layers.join(" + "));
    match &state {
        None => println!("state: first run on this machine (no bundles selected yet)"),
        Some(s) if s.bundles.is_empty() => println!("bundles: none selected"),
        Some(s) => println!("bundles: {}", s.bundles.join(", ")),
    }
    println!();
    print_items(&snap);
    println!();
    println!("{}", summary_line(&snap));
    Ok(0)
}

fn print_items(snap: &Snapshot) {
    let mut last_cat = "";
    for i in &snap.items {
        if i.category != last_cat {
            println!("{}", i.category);
            last_cat = &i.category;
        }
        let detail = match i.label {
            Label::Installed => i.ownership.map(|o| o.as_str()).unwrap_or("").to_string(),
            Label::Outdated => format!(
                "{} · latest {}",
                i.ownership.map(|o| o.as_str()).unwrap_or(""),
                i.latest.as_deref().unwrap_or("?")
            ),
            Label::NotApplicable => i.reason.clone().unwrap_or_default(),
            Label::Failed => i.last_error.clone().unwrap_or_default(),
            _ => String::new(),
        };
        println!(
            "  {} {:<24} {:<12} {:<10} {:<15} {}",
            i.label.glyph(),
            i.id,
            match (i.kind, &i.version) {
                (crate::plan::Kind::Config, _) => "",
                (_, Some(v)) => v.as_str(),
                (_, None) if i.present => "?",
                _ => "—",
            },
            i.label.text(),
            i.via.as_deref().unwrap_or("—"),
            detail
        );
    }
}

pub fn summary_line(snap: &Snapshot) -> String {
    let present = snap.count(|i| i.present);
    let applicable = snap.count(|i| i.label != Label::NotApplicable || i.present);
    let missing = snap.count(|i| i.label == Label::Missing);
    let new = snap.count(|i| i.label == Label::New);
    let failed = snap.count(|i| matches!(i.label, Label::Failed | Label::Drifted));
    let outdated = snap.count(|i| i.label == Label::Outdated);
    format!(
        "{present}/{applicable} present · {outdated} outdated · {missing} missing · {new} new · {failed} failed or drifted"
    )
}

pub async fn doctor(sess: &Session) -> Result<i32> {
    let f = &sess.facts;
    println!("envira {}", env!("CARGO_PKG_VERSION"));
    println!(
        "distro     {} (families: {})",
        f.distro_name,
        f.families.join(", ")
    );
    println!("arch       {}", f.arch);
    println!("privilege  {:?}", f.privilege);
    println!("gpu        {}", if f.gpu { "nvidia" } else { "none" });
    println!("container  {}   wsl {}", f.container, f.wsl);
    println!("shells     {}", f.shells.join(", "));
    println!("user       {} · {}", f.user, f.home.display());
    if let Some(u) = &f.drop_to {
        println!("           running as root for {u}: user-scope steps run as {u}");
    }
    println!("state      {}", sess.paths.state_dir.display());
    println!("config     {}", sess.paths.config_dir.display());
    println!("catalog    {}", sess.loaded.layers.join(" + "));
    for w in &sess.loaded.warnings {
        println!("  warning: {w}");
    }
    println!("PATH used for detection and installs:");
    for d in crate::status::effective_path(&sess.catalog, f).split(':') {
        let mark = if std::path::Path::new(d).is_dir() {
            "✓"
        } else {
            "·"
        };
        println!("  {mark} {d}");
    }
    println!("shell integration:");
    for (shell, rc) in shellenv::rc_files(f) {
        let block = shellenv::rc_block(shell, &sess.paths, f);
        let b = fs::Backups::new(sess.paths.state_dir.join("backups"));
        let ok = fs::block(&b, &rc, "envira", "#", Some(&block), true).unwrap_or(false);
        println!("  {} {}", if ok { "✓" } else { "✗" }, rc.display());
    }
    let state = sess.state()?;
    let snap = sess.snapshot(state.as_ref()).await;
    let dups = shellenv::rc_duplicates(&sess.catalog, f, &snap.present());
    if !dups.is_empty() {
        println!(
            "rc lines that duplicate the managed environment (safe to remove once `envira env` ran):"
        );
        for d in dups {
            println!("  {d}");
        }
    }
    Ok(0)
}

pub fn log(sess: &Session, item: Option<&str>, run: Option<&str>) -> Result<i32> {
    let run_id = match run {
        Some(r) => r.to_string(),
        None => sess
            .state()?
            .and_then(|s| s.last_run)
            .context("no runs yet")?,
    };
    let dir = sess.paths.run_dir(&run_id);
    if let Some(item) = item {
        let f = dir.join(format!("{item}.log"));
        print!(
            "{}",
            std::fs::read_to_string(&f).with_context(|| format!("reading {}", f.display()))?
        );
        return Ok(0);
    }
    let rec = RunRecord::load(&sess.paths, &run_id)?;
    println!("run {} · {} · started {}", rec.id, rec.command, rec.started);
    for u in &rec.units {
        println!(
            "  {:<9} {:<24} {:<8} {:<15} {:>6.1}s {}",
            u.outcome,
            u.id,
            u.action,
            u.via,
            u.seconds,
            u.detail.as_deref().unwrap_or("")
        );
    }
    println!("logs: {}", dir.display());
    Ok(0)
}

// ------------------------------------------------------------------------------------------------
// catalog
// ------------------------------------------------------------------------------------------------

pub fn catalog_cmd(global: &Global, cmd: &CatalogCmd) -> Result<i32> {
    if let CatalogCmd::Schema = cmd {
        println!("{}", schema_json());
        return Ok(0);
    }
    let facts = crate::facts::Facts::probe();
    let paths = Paths::new(&facts);
    let loaded = catalog::load_catalog(&app::catalog_options(global, &paths))?;
    match cmd {
        CatalogCmd::Check => {
            let r = validate::validate(&loaded);
            for e in &r.errors {
                println!("error: {e}");
            }
            for w in &r.warnings {
                println!("warning: {w}");
            }
            let c = &loaded.catalog;
            println!(
                "{} · {} tools, {} configs, {} bundles, {} methods, {} actions · {} errors, {} warnings",
                loaded.layers.join(" + "),
                c.tools.len(),
                c.configs.len(),
                c.bundles.len(),
                c.methods.len(),
                c.actions.len(),
                r.errors.len(),
                r.warnings.len()
            );
            Ok(if r.is_ok() { 0 } else { 1 })
        }
        CatalogCmd::Show => {
            print!("{}", toml::to_string_pretty(&loaded.catalog)?);
            Ok(0)
        }
        CatalogCmd::Schema => unreachable!(),
    }
}

pub fn schema_json() -> String {
    serde_json::to_string_pretty(&schemars::schema_for!(Catalog)).expect("schema serializes")
}

// ------------------------------------------------------------------------------------------------
// fs / fetch-release (called from templates)
// ------------------------------------------------------------------------------------------------

pub fn fs_cmd(cmd: &FsCmd) -> Result<i32> {
    let b = fs::Backups::from_env();
    let code = |ok: bool| if ok { 0 } else { 1 };
    Ok(match cmd {
        FsCmd::Write {
            path,
            content,
            mode,
            check,
        } => {
            let mode = mode
                .as_deref()
                .map(|m| u32::from_str_radix(m, 8))
                .transpose()
                .context("--mode must be octal")?;
            code(fs::write(&b, &fs::expand(path), content, mode, *check)?)
        }
        FsCmd::Block {
            path,
            id,
            comment,
            content,
            remove,
            check,
        } => {
            let content = if *remove { None } else { content.as_deref() };
            code(fs::block(
                &b,
                &fs::expand(path),
                id,
                comment,
                content,
                *check,
            )?)
        }
        FsCmd::TomlSet {
            file,
            key,
            value,
            check,
        } => code(fs::toml_set(&b, &fs::expand(file), key, value, *check)?),
        FsCmd::IniSet {
            file,
            section,
            key,
            value,
            check,
        } => code(fs::ini_set(
            &b,
            &fs::expand(file),
            section,
            key,
            value,
            *check,
        )?),
        FsCmd::Link {
            target,
            link,
            check,
        } => code(fs::link(&b, target, &fs::expand(link), *check)?),
        FsCmd::Restore { path, key } => {
            fs::restore(&b, &fs::expand(path), key.as_deref())?;
            0
        }
    })
}

// ------------------------------------------------------------------------------------------------
// sync / install / retry / config / plan / env
// ------------------------------------------------------------------------------------------------

pub async fn sync(sess: &Session, bundles: &[String], opts: &RunOpts) -> Result<i32> {
    let mut state = sess.state()?.unwrap_or_default();
    for b in bundles {
        if !sess.catalog.bundles.contains_key(b) {
            bail!(
                "unknown bundle {b:?} (available: {})",
                sess.catalog
                    .bundles
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        if !state.bundles.contains(b) {
            state.bundles.push(b.clone());
        }
    }
    if state.bundles.is_empty() && state.include.is_empty() {
        bail!("no bundles selected; use `envira sync --bundle <name>` or pick bundles in the TUI");
    }
    if !opts.dry_run && !bundles.is_empty() {
        state.save(&sess.paths, &sess.facts)?;
    }
    let snap = sess.snapshot(Some(&state)).await;
    let plan = sess.plan(&snap.present(), &snap.to_sync(), false);
    run_plan(sess, plan, state, "sync", opts).await
}

pub async fn install(sess: &Session, items: &[String], force: bool, opts: &RunOpts) -> Result<i32> {
    let targets = sess.expand_targets(items)?;
    let state = sess.state()?;
    let snap = sess.snapshot(state.as_ref()).await;
    let plan = sess.plan(&snap.present(), &targets, force);
    run_plan(sess, plan, state.unwrap_or_default(), "install", opts).await
}

/// Why a present item is not updated, or the update target for it.
fn update_target(i: &ItemStatus, explicit: bool) -> Result<UpdateTarget, String> {
    if i.kind != crate::plan::Kind::Tool {
        return Err("configs are applied, not updated".into());
    }
    if !i.present {
        return Err("not installed (use `envira install`)".into());
    }
    let Some(route) = i.route.clone() else {
        return Err(format!(
            "installed outside envira's reach{}",
            i.path
                .as_deref()
                .map(|p| format!(" ({p})"))
                .unwrap_or_default()
        ));
    };
    let current = i
        .method_version
        .as_deref()
        .or(i.version.as_deref())
        .unwrap_or("?");
    let reason = match (&i.latest, i.label) {
        (Some(l), Label::Outdated) => format!("{current} → {l}"),
        (Some(l), _) if explicit => return Err(format!("up to date ({current}, latest {l})")),
        (Some(_), _) => return Err("up to date".into()),
        (None, _) if explicit => format!("{current} → latest (unknown)"),
        (None, _) => return Err("latest version unknown".into()),
    };
    Ok(UpdateTarget {
        id: i.id.clone(),
        route,
        reason,
        expect: i.latest.clone(),
    })
}

pub async fn update(
    sess: &Session,
    items: &[String],
    check: bool,
    refresh: bool,
    offline: bool,
    opts: &RunOpts,
) -> Result<i32> {
    let state = sess.state()?;
    let mut snap = sess.snapshot(state.as_ref()).await;
    let fetch = if offline {
        Fetch::CacheOnly
    } else if refresh {
        Fetch::Force
    } else {
        Fetch::IfStale
    };
    sess.apply_latest(&mut snap, fetch).await;
    let explicit = !items.is_empty();
    let ids = if explicit {
        sess.expand_targets(items)?
    } else {
        snap.outdated()
    };
    let mut targets = Vec::new();
    let mut skipped = Vec::new();
    for id in &ids {
        let Some(i) = snap.get(id) else { continue };
        match update_target(i, explicit) {
            Ok(t) => targets.push(t),
            Err(why) if explicit => skipped.push((id.clone(), why)),
            Err(_) => {}
        }
    }
    if check {
        print_outdated(&snap);
        return Ok(0);
    }
    if targets.is_empty() && skipped.is_empty() {
        println!("Everything envira can update is up to date.");
        let unknown = snap.count(|i| i.route.is_some() && i.latest.is_none());
        if unknown > 0 {
            println!(
                "({unknown} without a known latest version; `envira update <id>` runs their updater.)"
            );
        }
        return Ok(0);
    }
    let mut plan = sess.update_plan(&targets);
    plan.skipped.extend(skipped);
    run_plan(sess, plan, state.unwrap_or_default(), "update", opts).await
}

fn print_outdated(snap: &Snapshot) {
    let tools: Vec<&ItemStatus> = snap
        .items
        .iter()
        .filter(|i| i.present && i.kind == crate::plan::Kind::Tool)
        .collect();
    let outdated: Vec<&&ItemStatus> = tools
        .iter()
        .filter(|i| i.label == Label::Outdated)
        .collect();
    if outdated.is_empty() {
        println!("Nothing is outdated.");
    } else {
        println!("Outdated ({}):", outdated.len());
        for i in &outdated {
            println!(
                "  ↑ {:<24} {:<12} → {:<12} via {:<15} {}",
                i.id,
                i.method_version
                    .as_deref()
                    .or(i.version.as_deref())
                    .unwrap_or("?"),
                i.latest.as_deref().unwrap_or("?"),
                i.via.as_deref().unwrap_or("—"),
                i.ownership.map(|o| o.as_str()).unwrap_or("")
            );
        }
    }
    let unknown: Vec<String> = tools
        .iter()
        .filter(|i| i.route.is_some() && i.latest.is_none())
        .map(|i| match &i.latest_error {
            Some(e) => format!("{} ({e})", i.id),
            None => i.id.clone(),
        })
        .collect();
    if !unknown.is_empty() {
        println!("Latest version unknown: {}", unknown.join(", "));
    }
    let external: Vec<&str> = tools
        .iter()
        .filter(|i| i.ownership == Some(Ownership::External))
        .map(|i| i.id.as_str())
        .collect();
    if !external.is_empty() {
        println!("Not updated by envira (external): {}", external.join(", "));
    }
}

/// Records adoptable tools as managed so later updates and `status` treat them as envira's.
pub async fn adopt(sess: &Session, items: &[String], dry_run: bool) -> Result<i32> {
    let mut state = sess.state()?.unwrap_or_default();
    let snap = sess.snapshot(Some(&state)).await;
    let explicit = !items.is_empty();
    let ids = if explicit {
        sess.expand_targets(items)?
    } else {
        snap.items
            .iter()
            .filter(|i| i.ownership == Some(Ownership::Adoptable))
            .map(|i| i.id.clone())
            .collect()
    };
    let mut adopted = 0;
    for id in &ids {
        let Some(i) = snap.get(id) else { continue };
        let route: Option<&Route> = i.route.as_ref();
        match (i.ownership, route) {
            (Some(Ownership::Adoptable), Some(r)) => {
                println!(
                    "  adopt {:<24} {:<12} via {} ({})",
                    id,
                    i.method_version
                        .as_deref()
                        .or(i.version.as_deref())
                        .unwrap_or("?"),
                    r.via,
                    r.scope
                );
                if !dry_run {
                    state.tools.insert(
                        id.clone(),
                        ToolRecord {
                            method: Some(r.via.clone()),
                            scope: Some(r.scope),
                            version: i.method_version.clone().or(i.version.clone()),
                            installed_at: Some(state::now_rfc3339()),
                            last_result: state::LastResult::Ok,
                            last_error: None,
                            last_run: None,
                        },
                    );
                }
                adopted += 1;
            }
            (Some(Ownership::Managed), _) if explicit => println!("  skip {id}: already managed"),
            (Some(Ownership::External), _) if explicit => {
                println!("  skip {id}: external (not installed where envira would install it)")
            }
            _ if explicit => println!("  skip {id}: not installed"),
            _ => {}
        }
    }
    if adopted == 0 {
        println!("Nothing to adopt.");
    } else if dry_run {
        println!("{adopted} would be adopted (dry run).");
    } else {
        state.mark_all_seen(&sess.catalog);
        state.save(&sess.paths, &sess.facts)?;
        println!("{adopted} adopted; `envira update` can now update them in place.");
    }
    Ok(0)
}

pub async fn retry(sess: &Session, opts: &RunOpts) -> Result<i32> {
    let state = sess.state()?.unwrap_or_default();
    let run_id = state.last_run.clone().context("no previous run to retry")?;
    let targets = RunRecord::load(&sess.paths, &run_id)?.unfinished();
    if targets.is_empty() {
        println!("Nothing failed in run {run_id}.");
        return Ok(0);
    }
    let snap = sess.snapshot(Some(&state)).await;
    let plan = sess.plan(&snap.present(), &targets, false);
    run_plan(sess, plan, state, "retry", opts).await
}

pub async fn config(sess: &Session, items: &[String], revert: bool, opts: &RunOpts) -> Result<i32> {
    let state = sess.state()?;
    let snap = sess.snapshot(state.as_ref()).await;
    let plan = if revert {
        sess.revert_plan(&snap.present(), items)
    } else {
        for i in items {
            if !sess.catalog.configs.contains_key(i) {
                bail!("{i:?} is not a config");
            }
        }
        sess.plan(&snap.present(), items, false)
    };
    run_plan(
        sess,
        plan,
        state.unwrap_or_default(),
        if revert {
            "config revert"
        } else {
            "config apply"
        },
        opts,
    )
    .await
}

pub async fn plan_cmd(sess: &Session, items: &[String], script: bool) -> Result<i32> {
    let state = sess.state()?;
    let snap = sess.snapshot(state.as_ref()).await;
    let targets = if items.is_empty() {
        snap.to_sync()
    } else {
        sess.expand_targets(items)?
    };
    let plan = sess.plan(&snap.present(), &targets, false);
    if script {
        print!("{}", as_script(&plan));
    } else {
        print_plan(&plan, sess);
    }
    Ok(0)
}

pub async fn env_cmd(sess: &Session) -> Result<i32> {
    let ctx = exec_ctx(sess, "env".into(), true);
    match exec::regenerate_shell_env(&ctx).await {
        Ok(files) if files.is_empty() => println!(
            "Shell environment is up to date ({}).",
            sess.paths.config_dir.display()
        ),
        Ok(files) => {
            for f in files {
                println!("updated {f}");
            }
            println!("Open a new shell (or `exec $SHELL`) to pick up the changes.");
        }
        Err(e) => bail!(e),
    }
    Ok(0)
}

fn exec_ctx(sess: &Session, command: String, shell_env: bool) -> ExecCtx {
    ExecCtx {
        catalog: sess.catalog.clone(),
        facts: sess.facts.clone(),
        paths: sess.paths.clone(),
        runner: sess.runner.clone(),
        run_id: state::new_run_id(&sess.paths),
        command,
        shell_env,
    }
}

pub fn print_plan(plan: &Plan, sess: &Session) {
    if plan.units.is_empty() {
        println!("Nothing to do.");
    } else {
        println!(
            "Plan ({} steps{}):",
            plan.units.len(),
            if plan.needs_sudo(&sess.facts) {
                ", needs sudo"
            } else {
                ""
            }
        );
        for (n, u) in plan.units.iter().enumerate() {
            println!(
                "  {:>2}. {:<8} {:<24} via {:<15} {:<7} {}",
                n + 1,
                u.action.as_str(),
                u.id,
                u.via,
                u.scope.to_string(),
                u.reason
            );
            for s in &u.steps {
                match &s.cmd {
                    Some(c) => println!("        $ {}", c.lines().next().unwrap_or("")),
                    None => println!("        · {} (manual)", s.label),
                }
            }
        }
    }
    for (id, why) in &plan.skipped {
        println!("  skip {id}: {why}");
    }
}

async fn run_plan(
    sess: &Session,
    plan: Plan,
    state: State,
    command: &str,
    opts: &RunOpts,
) -> Result<i32> {
    print_plan(&plan, sess);
    if plan.is_empty() || opts.dry_run {
        return Ok(
            if plan.skipped.iter().any(|(_, w)| w != "already present") && plan.is_empty() {
                1
            } else {
                0
            },
        );
    }
    if !opts.yes {
        if !std::io::stdin().is_terminal() {
            bail!("refusing to run without confirmation; pass -y");
        }
        print!("Proceed? [y/N] ");
        std::io::stdout().flush()?;
        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer)?;
        if !matches!(answer.trim(), "y" | "Y" | "yes") {
            println!("Aborted.");
            return Ok(1);
        }
    }
    let keepalive = if plan.needs_sudo(&sess.facts) {
        app::ensure_sudo(&sess.facts)?
    } else {
        None
    };

    let n = plan.units.len();
    let ids: Vec<String> = plan.units.iter().map(|u| u.id.clone()).collect();
    let actions: Vec<String> = plan
        .units
        .iter()
        .map(|u| format!("{} {} via {}", u.action.as_str(), u.id, u.via))
        .collect();
    let (tx, mut rx) = mpsc::unbounded_channel();
    let (ctl_tx, ctl_rx) = watch::channel(Control::default());
    let ctx = exec_ctx(sess, command.into(), true);
    let run_id = ctx.run_id.clone();
    let paths = sess.paths.clone();
    let handle = tokio::spawn(exec::execute(plan, ctx, state, tx, ctl_rx));

    let quiet = opts.quiet;
    let mut interrupts = 0;
    let mut failed = 0;
    loop {
        tokio::select! {
            ev = rx.recv() => {
                let Some(ev) = ev else { break };
                match ev {
                    Event::UnitStarted(i) => println!("==> [{}/{n}] {}", i + 1, actions[i]),
                    Event::Line(_, l) if !quiet || l.starts_with("[envira]") => println!("    │ {l}"),
                    Event::Line(..) => {}
                    Event::UnitFinished(i, o, d) => {
                        let id = &ids[i];
                        match &o {
                            Outcome::Ok { version } => println!("  ✓ {id} {} ({})", version.as_deref().unwrap_or(""), fmt_duration(d)),
                            Outcome::Blocked(dep) => { failed += 1; println!("  ⊘ {id} blocked: needs {dep}") }
                            Outcome::Skipped => println!("  · {id} skipped (run stopped)"),
                            Outcome::Cancelled => { failed += 1; println!("  ✗ {id} cancelled") }
                            Outcome::Failed(e) => {
                                failed += 1;
                                println!("  ✗ {id} failed: {e} · log {}", paths.run_dir(&run_id).join(format!("{id}.log")).display());
                            }
                        }
                    }
                    Event::ShellEnv(Ok(files)) if !files.is_empty() => {
                        println!("==> shell environment updated ({} files); open a new shell to use it", files.len())
                    }
                    Event::ShellEnv(Ok(_)) => {}
                    Event::ShellEnv(Err(e)) => println!("==> shell environment not updated: {e}"),
                    Event::Finished(_) => {}
                }
            }
            _ = tokio::signal::ctrl_c() => {
                interrupts += 1;
                let mut c = *ctl_tx.borrow();
                if interrupts == 1 {
                    c.stop = true;
                    println!("\n==> stopping after the current step (Ctrl-C again cancels it)");
                } else {
                    c.cancel_seq += 1;
                }
                let _ = ctl_tx.send(c);
            }
        }
    }
    let (record, _) = handle.await?;
    if let Some(k) = keepalive {
        k.abort();
    }
    let ok = record.units.iter().filter(|u| u.outcome == "ok").count();
    println!(
        "{ok}/{} succeeded · run {} · {}",
        record.units.len(),
        record.id,
        paths.run_dir(&record.id).display()
    );
    if failed > 0 {
        println!("Retry what failed with `envira retry`.");
    }
    Ok(if failed > 0 { 1 } else { 0 })
}

#[cfg(test)]
mod tests {
    #[test]
    fn committed_schema_is_current() {
        let committed = include_str!("../catalog/schema.json");
        assert_eq!(
            committed.trim_end(),
            super::schema_json(),
            "run `cargo run -- catalog schema > catalog/schema.json`"
        );
    }
}
