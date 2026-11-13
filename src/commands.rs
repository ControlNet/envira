//! Headless commands.

use std::io::{IsTerminal, Write};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use tokio::sync::watch;

use crate::app::{self, Session};
use crate::catalog::{self, Catalog, validate};
use crate::cli::{CatalogCmd, FsCmd, Global, RunOpts};
use crate::exec::{self, Control, Event, Outcome};
use crate::fs;
use crate::latest::Fetch;
use crate::plan::Route;
use crate::plan::{Plan, as_script};
use crate::shellenv;
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
        // Missing items show the route an install would take, behind an arrow.
        let planned = !i.present && i.label != Label::NotApplicable;
        let via = match (&i.via, planned) {
            (Some(v), true) => format!("→ {v}"),
            (Some(v), false) => v.clone(),
            (None, _) => "—".into(),
        };
        let scope = match i.scope {
            Some(s) if i.label != Label::NotApplicable => s.to_string(),
            _ => String::new(),
        };
        let line = format!(
            "  {} {:<24} {:<12} {:<10} {:<17} {:<7}{}",
            i.label.glyph(),
            i.id,
            match (i.kind, i.method_version.as_ref().or(i.version.as_ref())) {
                (crate::plan::Kind::Config, _) => "",
                (_, Some(v)) => v.as_str(),
                (_, None) if i.present => "?",
                _ => "—",
            },
            i.label.text(),
            via,
            scope,
            detail
        );
        println!("{}", line.trim_end());
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
        "{present}/{applicable} present · {outdated} outdated · {missing} wanted · {new} new · {failed} failed or drifted"
    )
}

pub async fn doctor(sess: &Session) -> Result<i32> {
    let f = &sess.facts;
    println!("envira {}", env!("CARGO_PKG_VERSION"));
    let (paths, facts) = (sess.paths.clone(), sess.facts.clone());
    let install = tokio::task::spawn_blocking(move || {
        let (exe, channel) = crate::selfupdate::this_install(&facts).ok()?;
        // Asks the network only where notices would (not in CI, not when opted out, not for a
        // source build); a cached answer is still shown.
        let fetch = if crate::selfupdate::check_enabled()
            && channel != crate::selfupdate::Channel::Source
        {
            crate::latest::Fetch::IfStale
        } else {
            crate::latest::Fetch::CacheOnly
        };
        let latest =
            crate::selfupdate::latest(&paths, &facts, fetch, crate::selfupdate::NOTICE_TIMEOUT);
        Some((exe, channel, latest))
    })
    .await
    .ok()
    .flatten();
    if let Some((exe, channel, latest)) = install {
        let newer = latest
            .filter(|v| crate::version::is_newer(v, env!("CARGO_PKG_VERSION")))
            .map(|v| format!(" · {v} is out: envira self-update"))
            .unwrap_or_default();
        println!("install    {} · {}{newer}", channel.label(), exe.display());
    }
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
    let disks: Vec<String> = [f.home.clone(), std::env::temp_dir(), "/usr/local".into()]
        .iter()
        .filter_map(|d| {
            crate::plan::disk_free(d)
                .map(|(_, free)| format!("{} {}", d.display(), crate::plan::fmt_bytes(free)))
        })
        .collect();
    println!("free disk  {}", disks.join(" · "));
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

/// Prints a command's exit code and the first lines of its output, indented.
fn print_probe(cmd: &str, p: &crate::runner::Probe) {
    println!("  $ {cmd}");
    println!("    exit {}", p.code);
    let lines: Vec<&str> = p.output.lines().collect();
    for l in lines.iter().take(6) {
        println!("    │ {}", exec::redact(l, exec::secrets()));
    }
    if lines.len() > 6 {
        println!("    │ … {} more lines", lines.len() - 6);
    }
}

pub async fn explain(sess: &Session, id: &str) -> Result<i32> {
    let cat = &sess.catalog;
    let f = &sess.facts;
    let state = sess.state()?;
    let snap = sess.snapshot(state.as_ref()).await;
    let Some(item) = snap.get(id) else {
        bail!("unknown item {id:?} (see `envira status`)");
    };
    println!(
        "{id} · {} · {} · {}",
        match item.kind {
            crate::plan::Kind::Tool => "tool",
            crate::plan::Kind::Config => "config",
        },
        item.category,
        item.label.text()
    );
    let desc = cat
        .tools
        .get(id)
        .and_then(|t| t.desc.clone())
        .or_else(|| cat.configs.get(id).and_then(|c| c.desc.clone()));
    if let Some(d) = desc {
        println!("  {d}");
    }
    if let Some(r) = &item.reason {
        println!("  not applicable here: {r}");
    }
    let timeout = Duration::from_secs(15);
    match item.kind {
        crate::plan::Kind::Tool => {
            println!("detect:");
            let cmd = crate::plan::detect_cmd(cat, f, id);
            print_probe(&cmd, &sess.runner.capture(&cmd, timeout).await);
            if let Some(re) = cat.tools[id]
                .detect
                .as_ref()
                .and_then(|d| d.version.as_deref())
            {
                println!("  version regex {re}");
            }
            println!(
                "  version {} · path {}",
                item.version.as_deref().unwrap_or("—"),
                item.path.as_deref().unwrap_or("—")
            );
            if let Some(o) = item.ownership {
                let via = item
                    .via
                    .as_deref()
                    .map(|v| format!(" via {v}"))
                    .unwrap_or_default();
                let scope = item.scope.map(|s| format!(" ({s})")).unwrap_or_default();
                println!("  {}{via}{scope}", o.as_str());
            }
        }
        crate::plan::Kind::Config => {
            let scope = if cat.configs[id].scope == Some(crate::catalog::ScopeSpec::System) {
                state::Scope::System
            } else {
                state::Scope::User
            };
            println!("checks (all must pass):");
            match crate::plan::config_steps(cat, f, id, scope) {
                Ok(steps) => {
                    for s in steps {
                        let Some(c) = &s.check else {
                            println!("  · {} (no check)", s.label);
                            continue;
                        };
                        let p = sess.runner.capture(c, timeout).await;
                        print_probe(c, &p);
                        // As status does: a system file only root can read.
                        if !p.ok() && scope == state::Scope::System && f.root_without_asking() {
                            println!("    again as root:");
                            print_probe(c, &sess.runner.capture_as(c, timeout, true).await);
                        }
                    }
                }
                Err(e) => println!("  cannot expand: {e}"),
            }
        }
    }
    let rec = state.as_ref().and_then(|s| {
        s.tools
            .get(id)
            .map(|r| (r.last_result, r.last_error.clone(), r.last_run.clone()))
            .or_else(|| {
                s.configs
                    .get(id)
                    .map(|r| (r.last_result, r.last_error.clone(), r.last_run.clone()))
            })
    });
    if let Some((result, error, run)) = rec {
        println!(
            "last run: {result:?}{}{}",
            run.map(|r| format!(" in {r}")).unwrap_or_default(),
            error.map(|e| format!(" · {e}")).unwrap_or_default()
        );
        if result == state::LastResult::Failed {
            println!("  `envira log {id}` shows its log");
        }
    }
    let plan = sess.plan(&snap.present(), &[id.to_string()], true);
    let verb = match (item.kind, item.present) {
        (crate::plan::Kind::Config, true) => "re-applying",
        (crate::plan::Kind::Config, false) => "applying",
        (_, true) => "reinstalling",
        (_, false) => "installing",
    };
    println!("{verb} would run:");
    print_plan(&plan, sess);
    Ok(0)
}

/// One line per kept run, newest first.
pub fn log_list(sess: &Session) -> Result<i32> {
    let runs = state::list_runs(&sess.paths);
    if runs.is_empty() {
        println!("No runs yet.");
        return Ok(0);
    }
    let locked = state::RunLock::acquire(&sess.paths, &sess.facts).is_err();
    for (n, id) in runs.iter().enumerate() {
        let Ok(rec) = RunRecord::load(&sess.paths, id) else {
            println!("{id}  (no readable run.toml)");
            continue;
        };
        let how = match &rec.finished {
            Some(_) => "finished",
            None if n == 0 && locked => "running",
            None => "interrupted",
        };
        let ok = rec.units.iter().filter(|u| u.outcome == "ok").count();
        let left = rec.unfinished().len();
        println!(
            "{id:<18}  {:<14} {:<11} {ok:>3} ok  {left:>3} unfinished  of {}",
            rec.command,
            how,
            rec.units.len()
        );
    }
    println!("`envira log --run <id>` shows one run; `envira log <item>` the last log of an item.");
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
        let mut file =
            std::fs::File::open(&f).with_context(|| format!("reading {}", f.display()))?;
        // Streamed: a build log can be large.
        match std::io::copy(&mut file, &mut std::io::stdout().lock()) {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => {}
            Err(e) => return Err(e).with_context(|| format!("reading {}", f.display())),
        }
        return Ok(0);
    }
    let rec = RunRecord::load(&sess.paths, &run_id)?;
    println!(
        "run {} · {} · started {}{}",
        rec.id,
        rec.command,
        rec.started,
        match &rec.finished {
            Some(f) => format!(" · finished {f}"),
            None if state::RunLock::acquire(&sess.paths, &sess.facts).is_err() => {
                " · still running".into()
            }
            None => " · did not finish (envira was stopped or killed)".into(),
        }
    );
    if !rec.machine.is_empty() {
        println!("envira {} · {}", rec.envira, rec.machine);
    }
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
        if let Some(h) = &u.hint {
            println!("            hint {h}");
        }
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
    let loaded = catalog::load_catalog(&app::catalog_options(global, &paths, &facts))?;
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
        FsCmd::Line {
            file,
            pattern,
            line,
            after,
            check,
        } => code(fs::line(
            &b,
            &fs::expand(file),
            pattern,
            line,
            after.as_deref(),
            *check,
        )?),
        FsCmd::Link {
            target,
            link,
            check,
        } => code(fs::link(&b, target, &fs::expand(link), *check)?),
        FsCmd::Keep { path } => {
            fs::keep(&b, &fs::expand(path))?;
            0
        }
        FsCmd::Restore { path, key } => {
            fs::restore(&b, &fs::expand(path), key.as_deref())?;
            0
        }
    })
}

// ------------------------------------------------------------------------------------------------
// install / retry / config / plan / env
// ------------------------------------------------------------------------------------------------

/// Without items: whatever the bundles this machine keeps miss. With items: those tools, configs
/// and bundles with their dependencies; bundles named here join the kept ones.
pub async fn install(sess: &Session, items: &[String], force: bool, opts: &RunOpts) -> Result<i32> {
    let disk = sess.state()?;
    let mut state = disk.clone().unwrap_or_default();
    if items.is_empty() {
        if force {
            bail!("--force reinstalls the items it is given; name them");
        }
        if state.bundles.is_empty() && state.include.is_empty() {
            bail!(
                "no bundles followed on this machine yet; use `envira install <bundle>` (available: {}) or pick bundles in the TUI",
                sess.catalog
                    .bundles
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        let snap = sess.snapshot(Some(&state)).await;
        let (present, missing) = (snap.present(), snap.to_install());
        let build = || sess.plan(&present, &missing, false);
        return run_plan(sess, &build, state, "install", opts).await;
    }
    let targets = sess.expand_targets(items)?;
    let mut added = false;
    for b in items
        .iter()
        .filter(|n| sess.catalog.bundles.contains_key(*n))
    {
        if !state.bundles.contains(b) {
            state.bundles.push(b.clone());
            added = true;
        }
    }
    if added && !opts.dry_run {
        let kept = state.bundles.clone();
        state = State::update(&sess.paths, &sess.facts, |st| {
            for b in kept {
                if !st.bundles.contains(&b) {
                    st.bundles.push(b);
                }
            }
        })?;
    }
    let known = disk.is_some() || added;
    let snap = sess.snapshot(known.then_some(&state)).await;
    let present = snap.present();
    let build = || sess.plan(&present, &targets, force);
    run_plan(sess, &build, state, "install", opts).await
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
        match i.update_target(explicit) {
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
    if targets.is_empty() {
        // Only explicit items left out: up to date is success, anything else is an error.
        for (id, why) in &skipped {
            println!("  skip {id}: {why}");
        }
        let all_current = skipped.iter().all(|(_, w)| w.starts_with("up to date"));
        if all_current {
            println!("Up to date.");
        }
        return Ok(if all_current { 0 } else { 1 });
    }
    let present = snap.present();
    let build = || {
        let mut plan = sess.update_plan(&present, &targets);
        plan.skipped.extend(skipped.clone());
        plan
    };
    run_plan(sess, &build, state.unwrap_or_default(), "update", opts).await
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
    let _lock = (!dry_run)
        .then(|| state::RunLock::acquire(&sess.paths, &sess.facts))
        .transpose()?;
    let state = sess.state()?.unwrap_or_default();
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
    let mut records = Vec::new();
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
                if !dry_run && let Some(rec) = i.adoption() {
                    records.push((id.clone(), rec));
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
        // A change to the state on disk: the TUI may have changed the selection meanwhile.
        State::update(&sess.paths, &sess.facts, |st| {
            st.tools.extend(records);
            st.mark_all_seen(&sess.catalog);
        })?;
        println!("{adopted} adopted; `envira update` can now update them in place.");
    }
    Ok(0)
}

pub async fn retry(sess: &Session, opts: &RunOpts) -> Result<i32> {
    let state = sess.state()?.unwrap_or_default();
    let run_id = state.last_run.clone().context("no previous run to retry")?;
    let rec = RunRecord::load(&sess.paths, &run_id)?;
    let mut t = crate::plan::RetryTargets::new(
        rec.unfinished_units()
            .map(|u| (u.id.as_str(), u.action.as_str())),
    )
    .progress(&rec.units, Some(&sess.paths.run_dir(&run_id)));
    if t.is_empty() {
        // Only the shell environment was left behind: bring it up to date.
        if let Some(e) = &rec.shell_env_error {
            println!("Run {run_id} left the shell environment behind ({e}).");
            let code = env_cmd(sess).await?;
            let mut rec = rec;
            rec.shell_env_error = None;
            rec.save(&sess.paths, &sess.facts)?;
            return Ok(code);
        }
        println!("Nothing failed in run {run_id}.");
        return Ok(0);
    }
    let mut snap = sess.snapshot(Some(&state)).await;
    let present = snap.present();
    let mut installs = t.install.clone();
    installs.extend(t.take_gone(&present));
    let (updates, update_skipped) = if t.update.is_empty() {
        Default::default()
    } else {
        sess.apply_latest(&mut snap, Fetch::CacheOnly).await;
        snap.update_targets(&t.update)
    };
    let build = || {
        // Updates first: an install may depend on a tool being updated.
        let mut plan = Plan::default();
        if !t.update.is_empty() {
            let mut p = sess.update_plan(&present, &updates);
            p.skipped.extend(update_skipped.clone());
            plan.extend(p);
        }
        if !installs.is_empty() {
            let p = sess.retry_install_plan(&present, &installs, &t.install_done);
            plan.extend_after(p, &sess.catalog);
        }
        if !t.revert.is_empty() {
            plan.extend(sess.revert_plan(&present, &t.revert, Some(&t.revert_done)));
        }
        plan
    };
    run_plan(sess, &build, state, "retry", opts).await
}

pub async fn config(sess: &Session, items: &[String], revert: bool, opts: &RunOpts) -> Result<i32> {
    let state = sess.state()?;
    let snap = sess.snapshot(state.as_ref()).await;
    if !revert {
        for i in items {
            if !sess.catalog.configs.contains_key(i) {
                bail!("{i:?} is not a config");
            }
        }
    }
    let present = snap.present();
    let build = || {
        if revert {
            sess.revert_plan(&present, items, None)
        } else {
            sess.plan(&present, items, false)
        }
    };
    run_plan(
        sess,
        &build,
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
        snap.to_install()
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
    let ctx = sess.exec_ctx("env".into(), true);
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
    for (dir, free) in plan.low_space(&sess.facts) {
        println!(
            "warning: only {} free on {}; installs may fail with \"No space left on device\"",
            crate::plan::fmt_bytes(free),
            dir.display()
        );
    }
}

/// The last `n` lines of a unit log, without its `#` header and trailer.
fn log_tail(path: &std::path::Path, n: usize) -> Vec<String> {
    // A few more for the header and trailer lines left out.
    let lines = crate::state::tail_lines(path, n + 8, 1 << 20).unwrap_or_default();
    let lines: Vec<String> = lines.into_iter().filter(|l| !l.starts_with("# ")).collect();
    lines[lines.len().saturating_sub(n)..].to_vec()
}

/// Plans with `build`, shows the plan and runs it once confirmed. When the plan needs sudo and
/// sudo wants a password, it is asked on the terminal (also with `-y`, and with stdin or stdout
/// piped), and typing it is the confirmation; user mode is offered instead: `build` again
/// without sudo. Where there is no terminal to ask on, the plan is made in user mode.
/// `--scope system` asks without offering user mode, and fails where it cannot ask.
async fn run_plan(
    sess: &Session,
    build: &dyn Fn() -> Plan,
    state: State,
    command: &str,
    opts: &RunOpts,
) -> Result<i32> {
    let mut plan = build();
    let mut confirmed = false;
    let mut password = None;
    if !opts.dry_run && plan.needs_sudo(&sess.facts) && !app::sudo_without_password() {
        let offer = sess.global.scope.is_none();
        if crate::sudo_prompt::can_ask() {
            print_plan(&plan, sess);
            println!();
            let n = plan.sudo_units(&sess.facts);
            let answer =
                tokio::task::spawn_blocking(move || crate::sudo_prompt::ask_on_terminal(n, offer))
                    .await??;
            match answer {
                Some(crate::sudo_prompt::Answer::Sudo(pw)) => {
                    password = Some(pw);
                    confirmed = true;
                }
                Some(crate::sudo_prompt::Answer::UserMode) => {
                    sess.use_user_mode();
                    plan = build();
                    println!("In user mode:");
                }
                None => {
                    println!("Aborted.");
                    return Ok(1);
                }
            }
        } else if offer {
            println!(
                "sudo needs a password, which this run cannot ask for: planning in user mode (--scope system uses sudo)."
            );
            sess.use_user_mode();
            plan = build();
        } else {
            print_plan(&plan, sess);
            bail!("sudo needs a password, which this run cannot ask for: run it on a terminal");
        }
    }
    if !confirmed {
        print_plan(&plan, sess);
    }
    if plan.is_empty() || opts.dry_run {
        return Ok(
            if plan.skipped.iter().any(|(_, w)| w != "already present") && plan.is_empty() {
                1
            } else {
                0
            },
        );
    }
    if !opts.yes && !confirmed {
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
    let _lock = state::RunLock::acquire(&sess.paths, &sess.facts)?;
    // The state on disk, read under the run lock: `state` was read before it, and a run that
    // finished meanwhile (another terminal, the TUI) would lose its records when this one saves.
    // The bundles this command added are kept.
    let state = match sess.state()? {
        Some(mut disk) => {
            for b in state.bundles {
                if !disk.bundles.contains(&b) {
                    disk.bundles.push(b);
                }
            }
            disk
        }
        None => state,
    };
    // The password goes to the steps' sudo through `askpass`; envira's own sudo (stopping
    // root processes on cancel) uses the ticket it left, kept fresh.
    let askpass = password
        .map(crate::askpass::Askpass::start)
        .transpose()
        .context("cannot hand sudo's password to the steps")?;
    let keepalive = plan.needs_sudo(&sess.facts).then(app::sudo_keepalive);

    let ids: Vec<String> = plan.units.iter().map(|u| u.id.clone()).collect();
    let actions: Vec<String> = plan
        .units
        .iter()
        .map(|u| format!("{} {} via {}", u.action.as_str(), u.id, u.via))
        .collect();
    let (tx, rx) = exec::events();
    let (ctl_tx, ctl_rx) = watch::channel(Control::default());
    let ctx = sess.exec_ctx(command.into(), true);
    let printer = Printer {
        // Output of units running side by side is told apart by the unit's id.
        tagged: ctx.jobs > 1,
        quiet: opts.quiet,
        ids,
        actions,
        run_dir: sess.paths.run_dir(&ctx.run_id),
    };
    let paths = sess.paths.clone();
    let mut handle = tokio::spawn(exec::execute(plan, ctx, state, tx.clone(), ctl_rx));
    // Output is written by its own thread: a reader that stalls (`| less` left alone) holds up
    // only that thread, never the run or Ctrl-C, and lines wait for it in a bounded queue.
    let printer = std::thread::spawn(move || printer.print(rx));

    let mut interrupts = 0;
    // SIGTERM/SIGHUP: cancel the running step and stop, so the run record is completed and no
    // installer keeps running on its own (steps run in their own sessions).
    use tokio::signal::unix::{SignalKind, signal};
    let mut term = signal(SignalKind::terminate())?;
    let mut hup = signal(SignalKind::hangup())?;
    let mut int = signal(SignalKind::interrupt())?;
    let record = loop {
        tokio::select! {
            r = &mut handle => break r?.0,
            _ = async { tokio::select! { _ = term.recv() => {}, _ = hup.recv() => {} } } => {
                ctl_tx.send_modify(|c| {
                    c.stop = true;
                    c.cancel_seq += 1;
                });
                tx.send(Event::Notice("terminated: cancelling the running steps and stopping".into()));
            }
            _ = int.recv() => {
                interrupts += 1;
                let mut c = *ctl_tx.borrow();
                if interrupts == 1 {
                    c.stop = true;
                } else {
                    c.cancel_seq += 1;
                }
                let _ = ctl_tx.send(c);
                if interrupts == 1 {
                    tx.send(Event::Notice("stopping after the running steps (Ctrl-C again cancels them)".into()));
                }
            }
        }
    };
    drop(tx);
    if let Some(k) = keepalive {
        k.abort();
    }
    drop(askpass);
    let ok = record.units.iter().filter(|u| u.outcome == "ok").count();
    let failed = record.units.len() - ok;
    let unrecorded = record.unrecorded.clone();
    let shell_env_failed = record.shell_env_error.is_some();
    // The run is recorded; what is left is showing the end of it. A reader that stalls must not
    // keep envira from ending on a signal now: nothing is left to cancel or record.
    let show = tokio::task::spawn_blocking(move || {
        let _ = printer.join();
        say(format_args!(
            "{ok}/{} succeeded · run {} · {}",
            record.units.len(),
            record.id,
            paths.run_dir(&record.id).display()
        ));
        if let Some(e) = &record.unrecorded {
            say(format_args!(
                "The run could not be recorded ({e}): `envira retry` may not know what it did."
            ));
        } else if failed > 0 || record.shell_env_error.is_some() {
            say(format_args!(
                "Retry what did not finish with `envira retry`."
            ));
        }
    });
    tokio::select! {
        _ = show => {}
        _ = hup.recv() => std::process::exit(128 + libc::SIGHUP),
        _ = int.recv() => std::process::exit(128 + libc::SIGINT),
        _ = term.recv() => std::process::exit(128 + libc::SIGTERM),
    }
    Ok(if failed > 0 || unrecorded.is_some() || shell_env_failed {
        1
    } else {
        0
    })
}

/// Writes a line to stdout. Once a run started, a closed stdout (`envira install … | head`) must
/// not end it halfway: write errors are ignored, and the run finishes and records its outcome.
fn say(line: std::fmt::Arguments) {
    let _ = writeln!(std::io::stdout(), "{line}");
}

/// Prints a run's events on the command line.
struct Printer {
    tagged: bool,
    quiet: bool,
    ids: Vec<String>,
    actions: Vec<String>,
    run_dir: std::path::PathBuf,
}

impl Printer {
    fn print(self, mut rx: exec::EventRx) {
        let n = self.ids.len();
        while let Some(ev) = rx.blocking_recv() {
            match ev {
                Event::UnitStarted(i) => {
                    say(format_args!("==> [{}/{n}] {}", i + 1, self.actions[i]))
                }
                Event::Line(i, l) if !self.quiet || l.starts_with("[envira]") => {
                    if self.tagged {
                        say(format_args!("    │ {} │ {l}", self.ids[i]));
                    } else {
                        say(format_args!("    │ {l}"));
                    }
                }
                Event::Line(..) => {}
                Event::UnitFinished(i, o, d) => {
                    let id = &self.ids[i];
                    match &o {
                        Outcome::Ok { version } => say(format_args!(
                            "  ✓ {id} {} ({})",
                            version.as_deref().unwrap_or(""),
                            fmt_duration(d)
                        )),
                        Outcome::Blocked(dep) => say(format_args!("  ⊘ {id} blocked: needs {dep}")),
                        Outcome::Skipped => say(format_args!("  · {id} skipped (run stopped)")),
                        Outcome::Cancelled => say(format_args!("  ✗ {id} cancelled")),
                        Outcome::Failed(e) => {
                            let log = self.run_dir.join(format!("{id}.log"));
                            say(format_args!("  ✗ {id} failed: {e} · log {}", log.display()));
                            if self.quiet {
                                // Step output was hidden: show the end of the log.
                                for l in log_tail(&log, 15) {
                                    say(format_args!("    │ {l}"));
                                }
                            }
                        }
                    }
                }
                Event::ShellEnv(Ok(files)) if !files.is_empty() => say(format_args!(
                    "==> shell environment updated ({} files); open a new shell to use it",
                    files.len()
                )),
                Event::ShellEnv(Ok(_)) => {}
                Event::ShellEnv(Err(e)) => {
                    say(format_args!("==> shell environment not updated: {e}"))
                }
                Event::Notice(m) => say(format_args!("\n==> {m}")),
                Event::Finished(_) => {}
            }
        }
    }
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
