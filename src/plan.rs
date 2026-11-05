//! Planning: method choice, dependency closure, topological order and expanded commands.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use crate::catalog::{Catalog, InstallEntry, ScopeSpec};
use crate::facts::Facts;
use crate::state::{Scope, UnitRecord};
use crate::template::{Ctx, Vars, shell_quote, unmet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Tool,
    Config,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Action {
    Install,
    Update,
    Apply,
    Revert,
}

impl Action {
    pub fn as_str(self) -> &'static str {
        match self {
            Action::Install => "install",
            Action::Update => "update",
            Action::Apply => "apply",
            Action::Revert => "revert",
        }
    }
}

/// Per install that failed after some of its steps: the route it took (see [`route_id`]) and
/// those steps.
pub type InstallsDone = BTreeMap<String, (String, Vec<String>)>;

/// Identifies a route as planned: method, scope and every step in full. A catalog changed
/// since (another build command, parameter or step) gives another id, even where the method
/// name and step labels stay (raw entries are all `run`).
pub fn route_id(via: &str, scope: Scope, steps: &[UnitStep]) -> String {
    use sha2::Digest;
    let text = serde_json::to_string(&(via, scope, steps)).unwrap_or_default();
    let hash = sha2::Sha256::digest(text.as_bytes());
    hash[..16].iter().map(|b| format!("{b:02x}")).collect()
}

/// The unfinished units of a run, split by what they were doing: retrying an update must
/// update again (an install plan skips a tool still present at its old version), and retrying
/// a revert must revert again (an install plan would apply the config instead).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RetryTargets {
    pub install: Vec<String>,
    pub update: Vec<String>,
    pub revert: Vec<String>,
    /// The steps each revert got through (labels), from the run record.
    pub revert_done: BTreeMap<String, Vec<String>>,
    /// The steps each failed install got through (see `Planner::finishing`).
    pub install_done: InstallsDone,
}

impl RetryTargets {
    /// From `(id, action)` pairs (`Action::as_str` names).
    pub fn new<'a>(units: impl IntoIterator<Item = (&'a str, &'a str)>) -> RetryTargets {
        let mut t = RetryTargets::default();
        for (id, action) in units {
            match action {
                "update" => t.update.push(id.to_string()),
                "revert" => t.revert.push(id.to_string()),
                _ => t.install.push(id.to_string()),
            }
        }
        t
    }

    /// Adds how far the reverts got, from the run's records, or for a unit still `pending`
    /// (envira died during it) from the progress file in `run_dir`; and how far failed installs
    /// got.
    pub fn progress<'a>(
        mut self,
        records: impl IntoIterator<Item = &'a UnitRecord>,
        run_dir: Option<&std::path::Path>,
    ) -> Self {
        for r in records {
            // An install that started (or may have: `pending` when envira died) and did not
            // finish: its tool may be detected already (the build of a portable tool went
            // through, the copy did not; a first step that failed after creating it) and still
            // needs the rest. A record without its route (an older envira) runs it in full.
            if r.action == "install"
                && self.install.contains(&r.id)
                && matches!(r.outcome.as_str(), "failed" | "cancelled" | "pending")
            {
                self.install_done
                    .insert(r.id.clone(), (r.route.clone(), r.done_steps.clone()));
            }
            if r.action != "revert" || !self.revert.contains(&r.id) {
                continue;
            }
            // A unit that did not start has no progress file; its record has what it inherited.
            // The file starts with that too: the longer of the two is where it got.
            let journal = match run_dir {
                Some(dir) if r.outcome == "pending" => crate::state::read_progress(dir, &r.id),
                _ => Vec::new(),
            };
            let done = if journal.len() > r.done_steps.len() {
                journal
            } else {
                r.done_steps.clone()
            };
            if !done.is_empty() {
                self.revert_done.insert(r.id.clone(), done);
            }
        }
        self
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn len(&self) -> usize {
        self.install.len() + self.update.len() + self.revert.len()
    }
}

/// One command of a unit. A step with a passing `check` is skipped; `cmd = None` means there is
/// nothing to run (a revert the catalog does not describe).
#[derive(Debug, Clone, Default, Serialize)]
pub struct UnitStep {
    pub label: String,
    pub check: Option<String>,
    pub cmd: Option<String>,
    /// Needs root in a user-scope unit (the system-wide copy of a portable build): under
    /// `sudo envira` it runs as root, not as the user.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub as_root: bool,
    /// Runs the step only while this check passes, else skips it: in a retried revert, a step
    /// after the one that failed undoes its change only while the change is in place.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub only_if: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Unit {
    pub id: String,
    pub kind: Kind,
    pub action: Action,
    /// Method name, `run` for raw entries, or `config`.
    pub via: String,
    pub scope: Scope,
    pub steps: Vec<UnitStep>,
    /// Commands that must all succeed afterwards (detect for tools, checks for configs).
    pub verify: Vec<String>,
    /// Units in the same plan this one depends on.
    pub deps: Vec<String>,
    /// Why it is in the plan: `requested`, `needed by X`, ...
    pub reason: String,
    /// Version an update is expected to reach (the latest known).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expect: Option<String>,
    /// Steps an earlier run got through, left out of `steps` (a retried revert): this run's
    /// record counts them as done too, so a second retry does not run them again.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub resumed: Vec<String>,
    /// [`route_id`] of an install as planned, before steps an earlier run got through were
    /// left out; empty for other actions.
    #[serde(skip)]
    pub route: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Plan {
    pub units: Vec<Unit>,
    /// Targets left out, with the reason.
    pub skipped: Vec<(String, String)>,
}

impl Plan {
    /// Appends `other` (a plan for other ids).
    pub fn extend(&mut self, other: Plan) {
        self.units.extend(other.units);
        self.skipped.extend(other.skipped);
    }

    pub fn needs_sudo(&self, facts: &Facts) -> bool {
        !facts.is_root()
            && self.units.iter().any(|u| {
                u.steps
                    .iter()
                    .any(|s| s.cmd.as_deref().is_some_and(uses_sudo))
            })
    }

    pub fn is_empty(&self) -> bool {
        self.units.is_empty()
    }

    /// Filesystems this plan writes to that have less than [`LOW_SPACE`] free: the home, the
    /// temp dir (downloads, builds) and, with system units, `/usr/local` and `/var` (apt).
    /// Each filesystem is reported once, by the first of its directories.
    pub fn low_space(&self, facts: &Facts) -> Vec<(std::path::PathBuf, u64)> {
        if self.units.is_empty() {
            return vec![];
        }
        let mut dirs = vec![facts.home.clone(), std::env::temp_dir()];
        if self.units.iter().any(|u| u.scope == Scope::System) {
            dirs.extend(["/usr/local".into(), "/var".into()]);
        }
        let mut seen = std::collections::BTreeSet::new();
        dirs.into_iter()
            .filter_map(|d| {
                let (dev, free) = disk_free(&d)?;
                (seen.insert(dev) && free < LOW_SPACE).then_some((d, free))
            })
            .collect()
    }
}

/// Below this much free space a plan warns before it runs.
pub const LOW_SPACE: u64 = 500 * 1024 * 1024;

/// `(device, free bytes)` of the filesystem holding `path`, or of its nearest existing ancestor.
pub fn disk_free(path: &std::path::Path) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    let existing = path.ancestors().find(|p| p.exists())?;
    let dev = std::fs::metadata(existing).ok()?.dev();
    let c = std::ffi::CString::new(existing.as_os_str().as_encoded_bytes()).ok()?;
    // SAFETY: statvfs fills the zeroed struct for a valid C string path.
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c.as_ptr(), &mut st) } != 0 {
        return None;
    }
    // The field types differ between targets (u32 on some 32-bit ones).
    #[allow(clippy::unnecessary_cast)]
    let free = st.f_bavail as u64 * st.f_frsize as u64;
    Some((dev, free))
}

pub fn fmt_bytes(n: u64) -> String {
    const MB: u64 = 1024 * 1024;
    if n >= 1024 * MB {
        format!("{:.1} GB", n as f64 / (1024 * MB) as f64)
    } else {
        format!("{} MB", n / MB)
    }
}

fn uses_sudo(cmd: &str) -> bool {
    cmd.split(|c: char| c.is_whitespace() || "(;&|'\"".contains(c))
        .any(|w| w == "sudo")
}

/// The feasible way to install or apply one item.
#[derive(Debug, Clone)]
pub struct Choice {
    pub via: String,
    pub scope: Scope,
    pub deps: Vec<String>,
    pub steps: Vec<UnitStep>,
    pub verify: Vec<String>,
}

/// A tool to update and where it lives.
#[derive(Debug, Clone)]
pub struct UpdateTarget {
    pub id: String,
    pub route: Route,
    /// Shown in the plan, e.g. `0.24.0 → 0.25.0`.
    pub reason: String,
    /// Latest known version.
    pub expect: Option<String>,
}

/// A config step with all templates expanded.
#[derive(Debug, Clone)]
pub struct ConfigStep {
    pub label: String,
    pub check: Option<String>,
    pub apply: String,
    pub revert: Option<String>,
}

fn resolve_scope(spec: Option<ScopeSpec>, prefer_system: bool) -> Scope {
    match spec {
        Some(ScopeSpec::System) => Scope::System,
        Some(ScopeSpec::Any) if prefer_system => Scope::System,
        _ => Scope::User,
    }
}

/// Where a present tool lives: the install entry that put it there and its scope. Updates run
/// that entry's update command in that scope, so the tool is replaced in place.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Route {
    pub entry: usize,
    pub via: String,
    pub scope: Scope,
}

/// Whether the item's own `when` holds here. A dependency that does not apply to this machine
/// (libatomic on macOS) is not needed by its dependents here either.
pub fn applies(cat: &Catalog, facts: &Facts, id: &str) -> bool {
    let when = match (cat.tools.get(id), cat.configs.get(id)) {
        (Some(t), _) => &t.when,
        (None, Some(c)) => &c.when,
        (None, None) => return true,
    };
    unmet(when, facts).is_none()
}

fn entry_name(e: &InstallEntry) -> &str {
    e.method.as_deref().unwrap_or("run")
}

/// The entry's own and its method's `when` hold.
fn entry_usable(cat: &Catalog, facts: &Facts, e: &InstallEntry) -> bool {
    unmet(&e.when, facts).is_none()
        && e.method.as_ref().is_none_or(|m| {
            cat.methods
                .get(m)
                .is_some_and(|m| unmet(&m.when, facts).is_none())
        })
}

fn entry_scope_spec(cat: &Catalog, e: &InstallEntry) -> Option<ScopeSpec> {
    e.scope.or(e
        .method
        .as_ref()
        .and_then(|m| cat.methods.get(m))
        .and_then(|m| m.scope))
}

/// Install template of an entry (method template or raw command).
fn install_template<'c>(cat: &'c Catalog, e: &'c InstallEntry) -> Option<&'c str> {
    match &e.method {
        Some(m) => cat.methods.get(m).and_then(|m| m.install.as_deref()),
        None => e.run.as_deref(),
    }
}

/// Route of a tool recorded (managed) or matched (adoption query) with method `via`.
pub fn route_for(
    cat: &Catalog,
    facts: &Facts,
    id: &str,
    via: &str,
    scope: Option<Scope>,
    prefer_system: bool,
) -> Option<Route> {
    let tool = cat.tools.get(id)?;
    let (idx, e) = tool
        .install
        .iter()
        .enumerate()
        .find(|(_, e)| entry_name(e) == via && entry_usable(cat, facts, e))?;
    Some(Route {
        entry: idx,
        via: via.to_string(),
        scope: scope.unwrap_or_else(|| resolve_scope(entry_scope_spec(cat, e), prefer_system)),
    })
}

/// Route of a tool found only by its binary path in the user's home: the first usable user-scope
/// entry that would install it there. Entries whose method has an `installed` query are skipped
/// (that query already said no), and entries that install into `{bin_dir}` must match the
/// binary's directory, so an update never leaves a second copy elsewhere.
pub fn path_route(cat: &Catalog, facts: &Facts, id: &str, path: &str) -> Option<Route> {
    let tool = cat.tools.get(id)?;
    let user_bin = facts.expand_home(bin_dir(cat, Scope::User));
    let parent = std::path::Path::new(path).parent()?;
    tool.install.iter().enumerate().find_map(|(idx, e)| {
        let user_ok = matches!(
            entry_scope_spec(cat, e),
            None | Some(ScopeSpec::User) | Some(ScopeSpec::Any)
        );
        let queried = e
            .method
            .as_ref()
            .and_then(|m| cat.methods.get(m))
            .is_some_and(|m| m.installed.is_some());
        let dir_ok = !queried
            && install_template(cat, e).is_some_and(|t| {
                !t.contains("{bin_dir}") || parent == std::path::Path::new(&user_bin)
            });
        (entry_usable(cat, facts, e) && user_ok && dir_ok).then(|| Route {
            entry: idx,
            via: entry_name(e).to_string(),
            scope: Scope::User,
        })
    })
}

pub fn bin_dir(cat: &Catalog, scope: Scope) -> &str {
    match scope {
        Scope::User => &cat.env.bin_dir.user,
        Scope::System => &cat.env.bin_dir.system,
    }
}

/// Detect command of a tool, expanded (`{bin} --version` by default).
pub fn detect_cmd(cat: &Catalog, facts: &Facts, id: &str) -> String {
    let Some(t) = cat.tools.get(id) else {
        return "false".into();
    };
    let tpl = t
        .detect
        .as_ref()
        .and_then(|d| d.cmd.clone())
        .unwrap_or_else(|| "{bin} --version".into());
    Vars::new(facts, id, t.bin(id), bin_dir(cat, Scope::User), false)
        .expand(&tpl, Ctx::Shell)
        .unwrap_or_else(|_| "false".into())
}

/// Expanded steps of a config for the given scope.
pub fn config_steps(
    cat: &Catalog,
    facts: &Facts,
    id: &str,
    scope: Scope,
) -> Result<Vec<ConfigStep>, String> {
    let cf = cat
        .configs
        .get(id)
        .ok_or_else(|| format!("unknown config {id:?}"))?;
    let bd = bin_dir(cat, scope);
    let sys = scope == Scope::System;
    let vars = Vars::new(facts, id, id, bd, sys);
    // Checks are read-only and run without sudo.
    let check_vars = Vars::new(facts, id, id, bd, false);
    if let Some(apply) = &cf.apply {
        return Ok(vec![ConfigStep {
            label: short(apply),
            check: cf
                .check
                .as_ref()
                .map(|c| check_vars.expand(c, Ctx::Shell))
                .transpose()?,
            apply: vars.expand(apply, Ctx::Shell)?,
            revert: cf
                .revert
                .as_ref()
                .map(|r| vars.expand(r, Ctx::Shell))
                .transpose()?,
        }]);
    }
    let mut out = Vec::new();
    for s in &cf.steps {
        if let Some(name) = &s.use_ {
            let act = cat
                .actions
                .get(name)
                .ok_or_else(|| format!("unknown action {name:?}"))?;
            let v = vars
                .clone()
                .with_params(&act.defaults)
                .with_params(&s.params);
            let cv = check_vars
                .clone()
                .with_params(&act.defaults)
                .with_params(&s.params);
            let target = ["file", "path", "link", "group"]
                .iter()
                .find_map(|k| s.params.get(*k).and_then(|v| v.as_str()))
                .unwrap_or("");
            out.push(ConfigStep {
                label: format!("{name} {target}").trim().to_string(),
                check: act
                    .check
                    .as_ref()
                    .map(|c| cv.expand(c, Ctx::Shell))
                    .transpose()?,
                apply: v.expand(act.apply.as_deref().unwrap_or("false"), Ctx::Shell)?,
                revert: act
                    .revert
                    .as_ref()
                    .map(|r| v.expand(r, Ctx::Shell))
                    .transpose()?,
            });
        } else if let Some(run) = &s.run {
            out.push(ConfigStep {
                label: short(run),
                check: s
                    .check
                    .as_ref()
                    .map(|c| check_vars.expand(c, Ctx::Shell))
                    .transpose()?,
                apply: vars.expand(run, Ctx::Shell)?,
                revert: s
                    .revert
                    .as_ref()
                    .map(|r| vars.expand(r, Ctx::Shell))
                    .transpose()?,
            });
        }
    }
    Ok(out)
}

fn short(cmd: &str) -> String {
    let line = cmd.lines().next().unwrap_or("").trim();
    if line.chars().count() > 48 {
        format!("{}…", line.chars().take(47).collect::<String>())
    } else {
        line.to_string()
    }
}

pub struct Planner<'a> {
    cat: &'a Catalog,
    facts: &'a Facts,
    present: &'a BTreeSet<String>,
    pub prefer_system: bool,
    /// Retrying reverts that stopped halfway, with the steps each got through (see
    /// [`resume_revert`]); a config no longer (fully) applied is reverted too.
    pub retrying: Option<BTreeMap<String, Vec<String>>>,
    /// Retrying installs that failed after some of their steps, with those steps: a tool that
    /// is present now gets the steps it did not get through (the system-wide copy of a portable
    /// build), not "already present".
    pub finishing: InstallsDone,
    memo: BTreeMap<String, Result<Choice, String>>,
    visiting: BTreeSet<String>,
}

impl<'a> Planner<'a> {
    pub fn new(cat: &'a Catalog, facts: &'a Facts, present: &'a BTreeSet<String>) -> Self {
        Planner {
            cat,
            facts,
            present,
            prefer_system: facts.privilege.can_elevate(),
            retrying: None,
            finishing: BTreeMap::new(),
            memo: BTreeMap::new(),
            visiting: BTreeSet::new(),
        }
    }

    /// How `id` would be installed or applied, or why it cannot be on this machine.
    pub fn choose(&mut self, id: &str) -> Result<Choice, String> {
        if let Some(r) = self.memo.get(id) {
            return r.clone();
        }
        if !self.visiting.insert(id.to_string()) {
            return Err("dependency cycle".into());
        }
        let r = if self.cat.tools.contains_key(id) {
            self.choose_tool(id)
        } else if self.cat.configs.contains_key(id) {
            self.choose_config(id)
        } else {
            Err(format!("unknown item {id:?}"))
        };
        self.visiting.remove(id);
        self.memo.insert(id.to_string(), r.clone());
        r
    }

    /// Checks that every dependency is present or can be installed.
    fn deps_ok(&mut self, deps: &[String]) -> Result<(), String> {
        for d in deps {
            if self.present.contains(d) || !applies(self.cat, self.facts, d) {
                continue;
            }
            if let Err(e) = self.choose(d) {
                return Err(format!("needs {d} ({e})"));
            }
        }
        Ok(())
    }

    fn choose_tool(&mut self, id: &str) -> Result<Choice, String> {
        let (cat, facts) = (self.cat, self.facts);
        let tool = &cat.tools[id];
        if let Some(w) = unmet(&tool.when, facts) {
            return Err(format!("when {w} is not met"));
        }
        if let Some(m) = &tool.manual {
            return Err(format!("install it yourself: {m}"));
        }
        let verify = vec![detect_cmd(cat, facts, id)];
        let mut notes: Vec<String> = Vec::new();
        for (idx, entry) in tool.install.iter().enumerate() {
            let name = entry_name(entry);
            if let Some(w) = unmet(&entry.when, facts) {
                notes.push(format!("{name}: {w}"));
                continue;
            }
            let method = match &entry.method {
                Some(m) => match cat.methods.get(m) {
                    Some(method) => Some(method),
                    None => {
                        notes.push(format!("{m}: unknown method"));
                        continue;
                    }
                },
                None => None,
            };
            if let Some(w) = method.and_then(|m| unmet(&m.when, facts)) {
                notes.push(format!("{name}: {w}"));
                continue;
            }
            let scope = resolve_scope(
                entry.scope.or(method.and_then(|m| m.scope)),
                self.prefer_system,
            );
            if scope == Scope::System && !facts.privilege.can_elevate() {
                notes.push(format!("{name}: needs sudo"));
                continue;
            }
            let deps: Vec<String> = tool.depends.iter().chain(&entry.depends).cloned().collect();
            if let Err(e) = self.deps_ok(&deps) {
                notes.push(format!("{name}: {e}"));
                continue;
            }
            match self.entry_steps(id, idx, scope, false) {
                Ok(steps) => {
                    return Ok(Choice {
                        via: name.to_string(),
                        scope,
                        deps,
                        steps,
                        verify,
                    });
                }
                Err(e) => notes.push(format!("{name}: {e}")),
            }
        }
        Err(summarize(&notes, facts))
    }

    /// Expanded steps of one install entry in `scope`: its install (or update) command, plus
    /// copies into the system bin dir for portable binaries built as the user.
    fn entry_steps(
        &self,
        id: &str,
        idx: usize,
        scope: Scope,
        update: bool,
    ) -> Result<Vec<UnitStep>, String> {
        let (cat, facts) = (self.cat, self.facts);
        let tool = &cat.tools[id];
        let entry = tool
            .install
            .get(idx)
            .ok_or_else(|| format!("no install entry {idx}"))?;
        let name = entry_name(entry);
        let verb = if update { "update" } else { "install" };
        let vars = Vars::new(
            facts,
            id,
            tool.bin(id),
            bin_dir(cat, scope),
            scope == Scope::System,
        );
        let (label, tpl, vars) = match (&entry.method, &entry.run) {
            (Some(m), _) => {
                let method = cat
                    .methods
                    .get(m)
                    .ok_or_else(|| format!("unknown method {m:?}"))?;
                let install = method.install.clone().unwrap_or_else(|| "false".into());
                let tpl = if update {
                    entry
                        .update
                        .clone()
                        .or(method.update.clone())
                        .unwrap_or(install)
                } else {
                    install
                };
                (
                    format!("{name} {verb}"),
                    tpl,
                    vars.with_params(&method.defaults)
                        .with_params(&entry.params),
                )
            }
            (None, Some(run)) => {
                let tpl = if update {
                    entry.update.clone().unwrap_or_else(|| run.clone())
                } else {
                    run.clone()
                };
                (verb.to_string(), tpl, vars)
            }
            (None, None) => return Err("needs `method` or `run`".into()),
        };
        let mut steps = vec![UnitStep {
            label,
            check: None,
            cmd: Some(vars.expand(&tpl, Ctx::Shell)?),
            as_root: false,
            only_if: None,
        }];
        if entry.portable
            && scope == Scope::User
            && self.prefer_system
            && facts.privilege.can_elevate()
        {
            for b in tool.bins(id) {
                let sv = Vars::new(facts, id, b, bin_dir(cat, Scope::System), true);
                // Next to the target, then renamed over it: a failed copy leaves the old one,
                // and no staged copy. `mv` onto a directory (or a link to one) would move the
                // copy into it and succeed, with no binary at the target: refused, checked as
                // the identity that moves (the user may not see behind the link). The path is
                // printed as data.
                let c = sv.expand(
                    "if {sudo} test -d {bin_dir}/{bin}; then printf '%s is a directory\\n' {bin_dir}/{bin} >&2; false; \
                     else {sudo} install -m 755 \"$(command -v {bin})\" {bin_dir}/.{bin}.envira-new \
                     && {sudo} mv -f {bin_dir}/.{bin}.envira-new {bin_dir}/{bin} \
                     || {{ {sudo} rm -f {bin_dir}/.{bin}.envira-new; false; }}; fi",
                    Ctx::Shell,
                )?;
                steps.push(UnitStep {
                    label: format!("copy {b} to {}", bin_dir(cat, Scope::System)),
                    check: None,
                    cmd: Some(c),
                    as_root: true,
                    only_if: None,
                });
            }
        }
        Ok(steps)
    }

    /// Updates present tools in place along their routes. Targets that depend on each other
    /// are ordered dependencies first; nothing else is added.
    pub fn update(&mut self, targets: &[UpdateTarget]) -> Plan {
        let mut plan = Plan::default();
        let ids: BTreeSet<&str> = targets.iter().map(|t| t.id.as_str()).collect();
        let mut order: Vec<&UpdateTarget> = Vec::new();
        fn visit<'t>(
            t: &'t UpdateTarget,
            all: &'t [UpdateTarget],
            cat: &Catalog,
            order: &mut Vec<&'t UpdateTarget>,
            seen: &mut BTreeSet<String>,
        ) {
            if !seen.insert(t.id.clone()) {
                return;
            }
            let tool = &cat.tools[&t.id];
            let entry_deps = tool.install.get(t.route.entry).map(|e| &e.depends);
            for d in tool.depends.iter().chain(entry_deps.into_iter().flatten()) {
                if let Some(dt) = all.iter().find(|x| &x.id == d) {
                    visit(dt, all, cat, order, seen);
                }
            }
            order.push(t);
        }
        let mut seen = BTreeSet::new();
        for t in targets {
            if !self.cat.tools.contains_key(&t.id) {
                plan.skipped
                    .push((t.id.clone(), "only tools can be updated".into()));
                continue;
            }
            visit(t, targets, self.cat, &mut order, &mut seen);
        }
        for t in order {
            if t.route.scope == Scope::System && !self.facts.privilege.can_elevate() {
                plan.skipped.push((t.id.clone(), "needs sudo".into()));
                continue;
            }
            match self.entry_steps(&t.id, t.route.entry, t.route.scope, true) {
                Ok(steps) => {
                    let tool = &self.cat.tools[&t.id];
                    let entry_deps = tool.install.get(t.route.entry).map(|e| &e.depends);
                    let deps = tool
                        .depends
                        .iter()
                        .chain(entry_deps.into_iter().flatten())
                        .filter(|d| {
                            ids.contains(d.as_str()) && plan.units.iter().any(|u| &u.id == *d)
                        })
                        .cloned()
                        .collect();
                    plan.units.push(Unit {
                        id: t.id.clone(),
                        kind: Kind::Tool,
                        action: Action::Update,
                        via: t.route.via.clone(),
                        scope: t.route.scope,
                        steps,
                        verify: vec![detect_cmd(self.cat, self.facts, &t.id)],
                        deps,
                        reason: t.reason.clone(),
                        expect: t.expect.clone(),
                        resumed: vec![],
                        route: String::new(),
                    });
                }
                Err(e) => plan.skipped.push((t.id.clone(), e)),
            }
        }
        plan
    }

    fn choose_config(&mut self, id: &str) -> Result<Choice, String> {
        let (cat, facts) = (self.cat, self.facts);
        let cf = &cat.configs[id];
        if let Some(w) = unmet(&cf.when, facts) {
            return Err(format!("when {w} is not met"));
        }
        let scope = resolve_scope(cf.scope, self.prefer_system);
        if scope == Scope::System && !facts.privilege.can_elevate() {
            return Err("needs sudo".into());
        }
        self.deps_ok(&cf.depends)?;
        let cs = config_steps(cat, facts, id, scope)?;
        let verify = cs.iter().filter_map(|s| s.check.clone()).collect();
        let steps = cs
            .into_iter()
            .map(|s| UnitStep {
                label: s.label,
                check: s.check,
                cmd: Some(s.apply),
                as_root: false,
                only_if: None,
            })
            .collect();
        Ok(Choice {
            via: "config".into(),
            scope,
            deps: cf.depends.clone(),
            steps,
            verify,
        })
    }

    /// Installs/applies `targets` and whatever they need, dependencies first.
    pub fn plan(&mut self, targets: &[String], force: bool) -> Plan {
        let mut plan = Plan::default();
        for t in targets {
            if plan.units.iter().any(|u| u.id == *t) {
                continue;
            }
            if self.present.contains(t) && !force {
                if self.finish(t, &mut plan) {
                    continue;
                }
                plan.skipped.push((t.clone(), "already present".into()));
                continue;
            }
            match self.choose(t) {
                Ok(_) => self.commit(t, "requested".into(), &mut plan),
                Err(e) => plan.skipped.push((t.clone(), e)),
            }
        }
        plan
    }

    /// Plans the rest of a failed install of `id` (see `finishing`): the steps it did not get
    /// through, on the very route that ran; the whole route when that is another now (the
    /// catalog or the machine changed since), as what the earlier steps did cannot be told.
    /// False when the route that ran got through every step.
    fn finish(&mut self, id: &str, plan: &mut Plan) -> bool {
        let Some((route, done)) = self.finishing.get(id).cloned() else {
            return false;
        };
        let Ok(choice) = self.choose(id) else {
            return false;
        };
        // Labels such as `install` are shared by routes: the id tells them apart.
        let same = route_id(&choice.via, choice.scope, &choice.steps) == route
            && choice.steps.len() >= done.len()
            && choice.steps.iter().zip(&done).all(|(s, d)| &s.label == d);
        if same && choice.steps.len() == done.len() {
            return false;
        }
        self.commit(id, "requested".into(), plan);
        if same && let Some(u) = plan.units.iter_mut().find(|u| u.id == id) {
            u.steps.drain(..done.len());
            u.resumed = done;
        }
        true
    }

    fn commit(&mut self, id: &str, reason: String, plan: &mut Plan) {
        if plan.units.iter().any(|u| u.id == id) {
            return;
        }
        let Ok(choice) = self.choose(id) else { return };
        for d in &choice.deps {
            if !self.present.contains(d) {
                self.commit(d, format!("needed by {id}"), plan);
            }
        }
        let in_plan: Vec<String> = choice
            .deps
            .iter()
            .filter(|d| plan.units.iter().any(|u| &u.id == *d))
            .cloned()
            .collect();
        let kind = if self.cat.configs.contains_key(id) {
            Kind::Config
        } else {
            Kind::Tool
        };
        let route = route_id(&choice.via, choice.scope, &choice.steps);
        plan.units.push(Unit {
            id: id.to_string(),
            kind,
            action: if kind == Kind::Config {
                Action::Apply
            } else {
                Action::Install
            },
            route,
            via: choice.via,
            scope: choice.scope,
            steps: choice.steps,
            verify: choice.verify,
            deps: in_plan,
            reason,
            expect: None,
            resumed: vec![],
        });
    }

    /// Reverts configs (tool uninstall is out of scope).
    pub fn revert(&mut self, ids: &[String]) -> Plan {
        let mut plan = Plan::default();
        for id in ids {
            let Some(cf) = self.cat.configs.get(id) else {
                plan.skipped
                    .push((id.clone(), "only configs can be reverted".into()));
                continue;
            };
            if !self.present.contains(id) && self.retrying.is_none() {
                plan.skipped.push((id.clone(), "not applied".into()));
                continue;
            }
            let scope = resolve_scope(cf.scope, self.prefer_system);
            if scope == Scope::System && !self.facts.privilege.can_elevate() {
                plan.skipped.push((id.clone(), "needs sudo".into()));
                continue;
            }
            let cs = match config_steps(self.cat, self.facts, id, scope) {
                Ok(cs) => cs,
                Err(e) => {
                    plan.skipped.push((id.clone(), e));
                    continue;
                }
            };
            let steps: Vec<UnitStep> = cs
                .into_iter()
                .rev()
                .map(|s| UnitStep {
                    label: format!("revert {}", s.label),
                    cmd: s.revert,
                    only_if: s.check,
                    ..Default::default()
                })
                .collect();
            let (steps, resumed) = match &self.retrying {
                Some(done) => {
                    resume_revert(steps, done.get(id).map(Vec::as_slice).unwrap_or_default())
                }
                None => (
                    steps
                        .into_iter()
                        .map(|s| UnitStep { only_if: None, ..s })
                        .collect(),
                    vec![],
                ),
            };
            plan.units.push(Unit {
                id: id.clone(),
                kind: Kind::Config,
                action: Action::Revert,
                via: "config".into(),
                scope,
                steps,
                verify: vec![],
                deps: vec![],
                reason: "requested".into(),
                expect: None,
                resumed,
                route: String::new(),
            });
        }
        plan
    }
}

/// The steps of a revert still to run after a run that stopped halfway got through `done`
/// (labels, in order), and the labels of those left out. The one that failed runs again as it
/// is (it may have undone part of its change), the ones after it only while their change is in
/// place.
pub fn resume_revert(steps: Vec<UnitStep>, done: &[String]) -> (Vec<UnitStep>, Vec<String>) {
    let skip = steps
        .iter()
        .zip(done)
        .take_while(|(s, d)| &s.label == *d)
        .count();
    let resumed = done[..skip].to_vec();
    let left = steps
        .into_iter()
        .skip(skip)
        .enumerate()
        .map(|(i, s)| {
            if i == 0 {
                UnitStep { only_if: None, ..s }
            } else {
                s
            }
        })
        .collect();
    (left, resumed)
}

fn summarize(notes: &[String], facts: &Facts) -> String {
    if notes.is_empty() {
        return "no install method".into();
    }
    if notes
        .iter()
        .all(|n| n.ends_with("needs sudo") || n.contains(": family:") || n.contains(": distro:"))
    {
        if notes.iter().any(|n| n.ends_with("needs sudo")) {
            return "needs sudo".into();
        }
        return format!("no install method for {}", facts.distro_id);
    }
    // Prefer the most specific note: one that is not about the distro family.
    notes
        .iter()
        .find(|n| !n.contains(": family:") && !n.contains(": distro:"))
        .cloned()
        .unwrap_or_else(|| notes[0].clone())
}

/// Plan as a shell script (TUI "copy as script", `--dry-run`).
pub fn as_script(plan: &Plan) -> String {
    let mut s = String::from("#!/usr/bin/env bash\nset -eo pipefail\n");
    for u in &plan.units {
        s.push_str(&format!(
            "\n# {} {} via {} ({})\n",
            u.action.as_str(),
            u.id,
            u.via,
            u.reason
        ));
        for st in &u.steps {
            match (&st.check, &st.only_if, &st.cmd) {
                (_, Some(c), Some(cmd)) => s.push_str(&format!("if {c}; then {cmd}; fi\n")),
                (Some(c), None, Some(cmd)) => s.push_str(&format!("{c} || {{ {cmd}; }}\n")),
                (None, None, Some(cmd)) => s.push_str(&format!("{cmd}\n")),
                (_, _, None) => s.push_str(&format!("# {}: manual\n", shell_quote(&st.label))),
            }
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retries_keep_what_each_unit_was_doing() {
        let t = RetryTargets::new([
            ("bat", "install"),
            ("zsh-rc", "apply"),
            ("yazi", "update"),
            ("codex-sandbox-network", "revert"),
        ]);
        assert_eq!(t.install, ["bat", "zsh-rc"]);
        assert_eq!(t.update, ["yazi"]);
        assert_eq!(t.revert, ["codex-sandbox-network"]);
        assert!(RetryTargets::new([]).is_empty());
    }
    use crate::catalog::load::{embedded_layer, load};
    use crate::facts::Privilege;
    use std::path::Path;

    fn cat() -> Catalog {
        load(&[embedded_layer()]).unwrap().catalog
    }

    fn set(ids: &[&str]) -> BTreeSet<String> {
        ids.iter().map(|s| s.to_string()).collect()
    }

    fn ids(p: &Plan) -> Vec<&str> {
        p.units.iter().map(|u| u.id.as_str()).collect()
    }

    #[test]
    fn free_space_is_measured_on_existing_ancestors() {
        let dir = tempfile::tempdir().unwrap();
        let (dev, free) = disk_free(&dir.path().join("not/yet/created")).unwrap();
        assert_eq!(Some(dev), disk_free(dir.path()).map(|d| d.0));
        assert!(free > 0);
        assert_eq!(fmt_bytes(300 * 1024 * 1024), "300 MB");
        assert_eq!(fmt_bytes(3 * 1024 * 1024 * 1024 / 2), "1.5 GB");
        assert!(
            Plan::default()
                .low_space(&Facts::sample(dir.path()))
                .is_empty()
        );
    }

    #[test]
    fn dependencies_come_first_and_present_ones_are_skipped() {
        let (c, f) = (cat(), Facts::sample(Path::new("/home/user")));
        let present = set(&["curl"]);
        let mut p = Planner::new(&c, &f, &present);
        let plan = p.plan(&["codex-sandbox-network".into()], false);
        assert_eq!(
            ids(&plan),
            vec![
                "unzip",
                "fnm",
                "libatomic",
                "nodejs",
                "npm-prefix",
                "codex",
                "codex-sandbox-network"
            ]
        );
        assert_eq!(plan.units[1].reason, "needed by nodejs");

        let present = set(&["curl", "unzip", "libatomic"]);
        let mut p = Planner::new(&c, &f, &present);
        let plan = p.plan(&["codex-sandbox-network".into()], false);
        assert_eq!(
            ids(&plan),
            vec![
                "fnm",
                "nodejs",
                "npm-prefix",
                "codex",
                "codex-sandbox-network"
            ]
        );
        assert_eq!(plan.units[3].deps, vec!["nodejs", "npm-prefix"]);
        assert_eq!(plan.units[1].reason, "needed by codex");

        let present = set(&["fnm", "nodejs", "npm-prefix"]);
        let mut p = Planner::new(&c, &f, &present);
        let plan = p.plan(&["codex".into()], false);
        assert_eq!(ids(&plan), vec!["codex"]);
        assert!(plan.units[0].deps.is_empty());
    }

    #[test]
    fn system_release_downloads_as_the_user_and_copies_with_sudo() {
        let c = cat();
        let f = Facts::sample(Path::new("/home/user"));
        let plan = Planner::new(&c, &f, &set(&[])).plan(&["lazygit".into()], false);
        assert_eq!(plan.units[0].via, "github-release");
        assert_eq!(plan.units[0].scope, Scope::System);
        let cmd = plan.units[0].steps[0].cmd.clone().unwrap();
        // fetch-release itself is not run with sudo: sudo would drop GITHUB_TOKEN.
        assert!(!cmd.starts_with("sudo"), "{cmd}");
        assert!(
            cmd.ends_with(" --to /usr/local/bin --install-with 'sudo -n'"),
            "{cmd}"
        );
        assert!(plan.needs_sudo(&f));
    }

    fn mac() -> Facts {
        let mut f = Facts::sample(Path::new("/Users/user"));
        f.os = "macos".into();
        f.distro_id = "macos".into();
        f.distro_name = "macOS 15.1".into();
        f.families = vec!["macos".into()];
        f.arch = "aarch64".into();
        f
    }

    #[test]
    fn macos_prefers_homebrew_and_skips_linux_only_items() {
        let c = cat();
        let f = mac();
        // Every Mac has curl and unzip; Homebrew is installed by hand.
        let base = set(&["curl", "unzip", "homebrew"]);
        let plan = Planner::new(&c, &f, &base).plan(&["bat".into(), "codex".into()], false);
        let via: Vec<(&str, &str)> = plan
            .units
            .iter()
            .map(|u| (u.id.as_str(), u.via.as_str()))
            .collect();
        assert_eq!(
            via,
            [
                ("bat", "brew"),
                ("fnm", "brew"),
                ("nodejs", "run"),
                ("npm-prefix", "config"),
                ("codex", "npm")
            ],
            "{:?}",
            plan.skipped
        );
        let bat = &plan.units[0];
        assert_eq!(bat.scope, Scope::User, "brew runs as the user");
        assert_eq!(
            bat.steps[0].cmd.as_deref(),
            Some("HOMEBREW_NO_ENV_HINTS=1 brew install bat")
        );
        // libatomic (Linux only) is not needed for nodejs on a Mac.
        assert!(plan.units.iter().all(|u| u.id != "libatomic"));
        assert!(!plan.needs_sudo(&f));

        // Release tables without macos-* keys are not used on a Mac, and Linux-only items say so.
        let mut p = Planner::new(&c, &f, &base);
        assert_eq!(p.choose("lazygit").unwrap().via, "brew");
        let docker = p.choose("docker").unwrap_err();
        assert!(docker.contains("os:linux"), "{docker}");
        assert!(p.choose("docker-group").is_err());
        assert!(p.choose("default-shell").is_err());
    }

    #[test]
    fn without_homebrew_macos_falls_back_or_explains() {
        let c = cat();
        let f = mac();
        let base = set(&["curl", "unzip"]);
        let mut p = Planner::new(&c, &f, &base);
        // envira does not install Homebrew: it says where to get it.
        let brew = p.choose("homebrew").unwrap_err();
        assert!(brew.contains("https://brew.sh"), "{brew}");
        // fnm falls back to its own install script; lazygit has no other route on a Mac.
        assert_eq!(p.choose("fnm").unwrap().via, "script");
        let lazygit = p.choose("lazygit").unwrap_err();
        assert!(lazygit.contains("needs homebrew"), "{lazygit}");
    }

    #[test]
    fn linux_never_picks_homebrew() {
        let c = cat();
        let f = Facts::sample(Path::new("/home/user"));
        let none = set(&[]);
        let mut p = Planner::new(&c, &f, &none);
        assert_eq!(p.choose("lazygit").unwrap().via, "github-release");
        assert_eq!(p.choose("bat").unwrap().via, "apt");
        assert!(p.choose("homebrew").is_err());
    }

    #[test]
    fn sudo_is_found_inside_quotes() {
        assert!(uses_sudo("envira fetch-release --install-with 'sudo -n'"));
        assert!(uses_sudo("x --install-with \"sudo -n\""));
        assert!(!uses_sudo("envira fetch-release --install-with ''"));
        assert!(!uses_sudo("echo pseudo"));
    }

    #[test]
    fn method_choice_follows_privilege() {
        let c = cat();
        let mut f = Facts::sample(Path::new("/home/user"));
        let present = set(&[]);
        let plan = Planner::new(&c, &f, &present).plan(&["bat".into()], false);
        assert_eq!(plan.units[0].via, "apt");
        assert_eq!(plan.units[0].scope, Scope::System);
        assert!(plan.units[0].steps[0].cmd.as_deref().unwrap().contains(
            " apt-refresh && { sudo -n env DEBIAN_FRONTEND=noninteractive apt-get install -y bat "
        ));
        assert!(plan.needs_sudo(&f));

        f.privilege = Privilege::None;
        let plan = Planner::new(&c, &f, &present).plan(&["bat".into()], false);
        assert_eq!(plan.units[0].via, "github-release");
        assert_eq!(plan.units[0].scope, Scope::User);
        let cmd = plan.units[0].steps[0].cmd.clone().unwrap();
        assert!(cmd.contains("--asset 'bat-v*-x86_64-unknown-linux-musl.tar.gz' --bin bat --to /home/user/.local/bin --install-with ''"), "{cmd}");
        assert!(!plan.needs_sudo(&f));
    }

    #[test]
    fn github_release_goes_system_wide_with_sudo() {
        let (c, f) = (cat(), Facts::sample(Path::new("/home/user")));
        let present = set(&[]);
        let plan = Planner::new(&c, &f, &present).plan(&["lazygit".into()], false);
        let u = &plan.units[0];
        assert_eq!((u.via.as_str(), u.scope), ("github-release", Scope::System));
        assert!(
            u.steps[0]
                .cmd
                .as_deref()
                .unwrap()
                .contains("--to /usr/local/bin")
        );
    }

    #[test]
    fn portable_cargo_tool_is_copied_system_wide() {
        let (c, f) = (cat(), Facts::sample(Path::new("/home/user")));
        let present = set(&["rust-toolchain", "cargo-binstall"]);
        let plan = Planner::new(&c, &f, &present).plan(&["texlab".into()], false);
        let u = &plan.units[0];
        assert_eq!(u.steps.len(), 2);
        assert_eq!(
            u.steps[1].cmd.as_deref().unwrap(),
            "if sudo -n test -d /usr/local/bin/texlab; then printf '%s is a directory\\n' /usr/local/bin/texlab >&2; \
             false; else sudo -n install -m 755 \"$(command -v texlab)\" /usr/local/bin/.texlab.envira-new \
             && sudo -n mv -f /usr/local/bin/.texlab.envira-new /usr/local/bin/texlab \
             || { sudo -n rm -f /usr/local/bin/.texlab.envira-new; false; }; fi"
        );
        // The build runs as the user, the copy as root (under `sudo envira`).
        assert!(!u.steps[0].as_root && u.steps[1].as_root);
    }

    #[test]
    fn a_route_is_told_apart_by_its_steps() {
        let step = |cmd: &str| UnitStep {
            label: "run".into(),
            cmd: Some(cmd.into()),
            ..Default::default()
        };
        let a = route_id("run", Scope::User, &[step("make OLD")]);
        assert_eq!(a, route_id("run", Scope::User, &[step("make OLD")]));
        // Same method and labels, another command, scope or method: another route.
        assert_ne!(a, route_id("run", Scope::User, &[step("make NEW")]));
        assert_ne!(a, route_id("run", Scope::System, &[step("make OLD")]));
        assert_ne!(a, route_id("script", Scope::User, &[step("make OLD")]));
    }

    #[test]
    fn a_retry_finishes_the_copy_of_a_portable_build() {
        let (c, f) = (cat(), Facts::sample(Path::new("/home/user")));
        // The build went through and the copy failed: texlab is present for the user now.
        let present = set(&["rust-toolchain", "cargo-binstall", "texlab"]);
        let full = Planner::new(&c, &f, &set(&["rust-toolchain", "cargo-binstall"]))
            .plan(&["texlab".into()], false);
        let build = full.units[0].steps[0].label.clone();
        let mut p = Planner::new(&c, &f, &present);
        let route = full.units[0].route.clone();
        assert_eq!(route.len(), 32);
        p.finishing =
            BTreeMap::from([("texlab".to_string(), (route.clone(), vec![build.clone()]))]);
        let plan = p.plan(&["texlab".into()], false);
        let u = &plan.units[0];
        assert_eq!(u.steps.len(), 1);
        assert!(
            u.steps[0].label.starts_with("copy texlab to "),
            "{:?}",
            u.steps
        );
        assert_eq!(u.resumed, std::slice::from_ref(&build));
        // Every step got through (it failed afterwards): nothing is left to do.
        let mut p = Planner::new(&c, &f, &present);
        let all: Vec<String> = full.units[0]
            .steps
            .iter()
            .map(|s| s.label.clone())
            .collect();
        p.finishing = BTreeMap::from([("texlab".to_string(), (route.clone(), all))]);
        assert_eq!(
            p.plan(&["texlab".into()], false).skipped[0].1,
            "already present"
        );
        // Steps that do not match what ran, the same labels by another route (or this one
        // changed in the catalog since), a record without its route (an older envira), or
        // another route on this machine now (no sudo, no copy): what the steps that ran did
        // cannot be told, so the route runs in full.
        let mut nf = f.clone();
        nf.privilege = Privilege::None;
        let in_full = [
            (&f, route.clone(), vec![], 2),
            (&f, route.clone(), vec!["other".to_string()], 2),
            (&f, "other route".to_string(), vec![build.clone()], 2),
            (&f, String::new(), vec![build.clone()], 2),
            (&nf, route, vec![build], 1),
        ];
        for (facts, route, done, steps) in in_full {
            let mut p = Planner::new(&c, facts, &present);
            p.finishing = BTreeMap::from([("texlab".to_string(), (route, done))]);
            let plan = p.plan(&["texlab".into()], false);
            assert_eq!(plan.units.len(), 1, "{:?}", plan.skipped);
            assert_eq!(plan.units[0].steps.len(), steps);
            assert!(plan.units[0].resumed.is_empty());
        }
    }

    #[test]
    fn not_applicable_reasons() {
        let c = cat();
        let mut f = Facts::sample(Path::new("/home/user"));
        let present = set(&[]);
        let mut p = Planner::new(&c, &f, &present);
        assert_eq!(p.choose("yay").unwrap_err(), "when family:arch is not met");
        f.privilege = Privilege::None;
        let mut p = Planner::new(&c, &f, &present);
        assert_eq!(p.choose("docker").unwrap_err(), "needs sudo");
        assert_eq!(p.choose("docker-group").unwrap_err(), "needs sudo");
        let plan = p.plan(&["docker-group".into(), "bat".into()], false);
        assert_eq!(ids(&plan), vec!["bat"]);
        assert_eq!(plan.skipped[0].0, "docker-group");
    }

    #[test]
    fn config_steps_expand_and_check_without_sudo() {
        let (c, f) = (cat(), Facts::sample(Path::new("/home/user")));
        let present = set(&[]);
        let plan = Planner::new(&c, &f, &present).plan(&["inotify-watches".into()], false);
        let u = &plan.units[0];
        assert_eq!(u.steps.len(), 2);
        let check = u.steps[0].check.as_deref().unwrap();
        assert!(!check.contains("sudo"), "{check}");
        assert!(
            check.contains("fs line --check /etc/sysctl.conf --match"),
            "{check}"
        );
        assert!(u.steps[0].cmd.as_deref().unwrap().starts_with("sudo -n "));
        assert_eq!(
            u.steps[1].cmd.as_deref().unwrap(),
            "sudo -n sysctl -w fs.inotify.max_user_watches=524288"
        );
        assert_eq!(u.verify.len(), 2);
    }

    #[test]
    fn revert_runs_steps_backwards() {
        let (c, f) = (cat(), Facts::sample(Path::new("/home/user")));
        let present = set(&["yazi-theme"]);
        let plan = Planner::new(&c, &f, &present).revert(&["yazi-theme".into(), "bat".into()]);
        let u = &plan.units[0];
        assert!(
            u.steps[0]
                .cmd
                .as_deref()
                .unwrap()
                .contains("fs restore /home/user/.config/yazi/theme.toml --key flavor.dark")
        );
        assert!(u.steps[1].cmd.is_none());
        assert!(u.steps.iter().all(|s| s.only_if.is_none()));
        assert_eq!(
            plan.skipped,
            vec![(
                "bat".to_string(),
                "only configs can be reverted".to_string()
            )]
        );
        // A revert that stopped halfway leaves the config not (fully) applied: only a retry
        // reverts it again.
        let none = set(&[]);
        let plan = Planner::new(&c, &f, &none).revert(&["yazi-theme".into()]);
        assert!(plan.units.is_empty());
        let mut p = Planner::new(&c, &f, &none);
        p.retrying = Some(BTreeMap::new());
        let plan = p.revert(&["yazi-theme".into()]);
        assert_eq!(plan.units.len(), 1);
        // Nothing recorded: the first step runs as it is, the next one if still in place.
        let steps = &plan.units[0].steps;
        assert!(steps[0].only_if.is_none());
        assert!(
            steps[1]
                .only_if
                .as_deref()
                .unwrap()
                .contains("flavors/onedark.yazi")
        );
    }

    #[test]
    fn a_retried_revert_starts_at_the_step_that_failed() {
        let step = |l: &str| UnitStep {
            label: l.into(),
            cmd: Some(format!("undo {l}")),
            only_if: Some(format!("check {l}")),
            ..Default::default()
        };
        let steps = vec![step("a"), step("b"), step("c")];
        let labels = |v: &[UnitStep]| {
            v.iter()
                .map(|s| (s.label.clone(), s.only_if.is_some()))
                .collect::<Vec<_>>()
        };
        let (left, resumed) = resume_revert(steps.clone(), &["a".into()]);
        assert_eq!(labels(&left), [("b".into(), false), ("c".into(), true)]);
        assert_eq!(resumed, ["a"]);
        let script = as_script(&Plan {
            units: vec![Unit {
                id: "cfg".into(),
                kind: Kind::Config,
                action: Action::Revert,
                via: "config".into(),
                scope: Scope::User,
                steps: left,
                verify: vec![],
                deps: vec![],
                reason: "retry".into(),
                expect: None,
                resumed: vec![],
                route: String::new(),
            }],
            skipped: vec![],
        });
        assert!(
            script.ends_with("undo b\nif check c; then undo c; fi\n"),
            "{script}"
        );
        // Steps that changed since are not skipped past their first difference.
        let (left, resumed) = resume_revert(steps.clone(), &["x".into(), "b".into()]);
        assert_eq!(labels(&left).len(), 3);
        assert!(resumed.is_empty());
        assert!(
            resume_revert(steps, &["a".into(), "b".into(), "c".into()])
                .0
                .is_empty()
        );
        let records = [
            UnitRecord {
                id: "cfg".into(),
                action: "revert".into(),
                outcome: "failed".into(),
                done_steps: vec!["a".into()],
                ..Default::default()
            },
            UnitRecord {
                id: "bat".into(),
                action: "install".into(),
                outcome: "failed".into(),
                done_steps: vec!["x".into()],
                route: "r1".into(),
                ..Default::default()
            },
            // From an envira that did not record routes: installed again in full.
            UnitRecord {
                id: "old".into(),
                action: "install".into(),
                outcome: "failed".into(),
                done_steps: vec!["x".into()],
                ..Default::default()
            },
            // Its first step failed (after creating the tool, maybe): the route runs again.
            UnitRecord {
                id: "first".into(),
                action: "install".into(),
                outcome: "failed".into(),
                route: "r2".into(),
                ..Default::default()
            },
            // Never started: present now means installed some other way.
            UnitRecord {
                id: "blocked".into(),
                action: "install".into(),
                outcome: "blocked".into(),
                route: "r3".into(),
                ..Default::default()
            },
            // envira died during it: the record has nothing, the progress file has a.
            UnitRecord {
                id: "died".into(),
                action: "revert".into(),
                outcome: "pending".into(),
                ..Default::default()
            },
            // envira died before it started: no progress file, the record has what it
            // inherited from an earlier run.
            UnitRecord {
                id: "waiting".into(),
                action: "revert".into(),
                outcome: "pending".into(),
                done_steps: vec!["x".into()],
                ..Default::default()
            },
            // envira died while its progress file got what it inherited: the record has more.
            UnitRecord {
                id: "short".into(),
                action: "revert".into(),
                outcome: "pending".into(),
                done_steps: vec!["x".into(), "y".into()],
                ..Default::default()
            },
        ];
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(crate::state::progress_file(dir.path(), "short"), "\"x\"\n").unwrap();
        std::fs::write(
            crate::state::progress_file(dir.path(), "died"),
            "\"a\"\n\"b\\nc\"\n",
        )
        .unwrap();
        let t = RetryTargets::new([
            ("cfg", "revert"),
            ("bat", "install"),
            ("died", "revert"),
            ("waiting", "revert"),
            ("short", "revert"),
            ("old", "install"),
            ("first", "install"),
            ("blocked", "install"),
        ])
        .progress(&records, Some(dir.path()));
        assert_eq!(
            t.install_done,
            BTreeMap::from([
                ("bat".to_string(), ("r1".to_string(), vec!["x".to_string()])),
                ("old".to_string(), (String::new(), vec!["x".to_string()])),
                ("first".to_string(), ("r2".to_string(), vec![])),
            ])
        );
        assert_eq!(t.revert_done.len(), 4);
        assert_eq!(t.revert_done["short"], ["x", "y"]);
        assert_eq!(t.revert_done["waiting"], ["x"]);
        assert_eq!(t.revert_done["cfg"], ["a"]);
        assert_eq!(t.revert_done["died"], ["a", "b\nc"]);
    }

    #[test]
    fn force_reinstalls_present_target() {
        let (c, f) = (cat(), Facts::sample(Path::new("/home/user")));
        let present = set(&["herdr", "curl"]);
        let mut p = Planner::new(&c, &f, &present);
        assert!(p.plan(&["herdr".into()], false).is_empty());
        let plan = p.plan(&["herdr".into()], true);
        assert_eq!(
            plan.units[0].steps[0].cmd.as_deref().unwrap(),
            "curl -fsSL https://herdr.dev/install.sh | sh -s --"
        );
        assert!(as_script(&plan).contains("# install herdr via script (requested)"));
    }

    /// Index of the first install entry of `id` using `via` (a method name or `run`).
    fn entry_of(c: &Catalog, id: &str, via: &str) -> usize {
        c.tools[id]
            .install
            .iter()
            .position(|e| e.method.as_deref().unwrap_or("run") == via)
            .unwrap()
    }

    fn target(id: &str, entry: usize, via: &str, scope: Scope) -> UpdateTarget {
        UpdateTarget {
            id: id.into(),
            route: Route {
                entry,
                via: via.into(),
                scope,
            },
            reason: "outdated".into(),
            expect: None,
        }
    }

    #[test]
    fn update_uses_the_route_and_orders_dependencies() {
        let (c, f) = (cat(), Facts::sample(Path::new("/home/user")));
        let present = set(&["fnm", "nodejs", "codex", "bat", "herdr", "bun"]);
        let mut p = Planner::new(&c, &f, &present);
        let plan = p.update(&[
            target("codex", 0, "npm", Scope::User),
            target("nodejs", 0, "run", Scope::User),
            target("fnm", 0, "script", Scope::User),
            // bat lives in ~/.local/bin: github-release in user scope, not apt.
            target(
                "bat",
                entry_of(&c, "bat", "github-release"),
                "github-release",
                Scope::User,
            ),
            target("herdr", 0, "script", Scope::User),
            target("bun", 0, "run", Scope::User),
        ]);
        // codex's npm entry depends on nodejs, which depends on fnm.
        assert_eq!(
            ids(&plan),
            vec!["fnm", "nodejs", "codex", "bat", "herdr", "bun"]
        );
        let cmd = |id: &str| {
            plan.units.iter().find(|u| u.id == id).unwrap().steps[0]
                .cmd
                .clone()
                .unwrap()
        };
        assert_eq!(cmd("codex"), "npm install -g @openai/codex@latest");
        assert_eq!(cmd("herdr"), "herdr update");
        assert_eq!(cmd("bun"), "bun upgrade");
        assert!(
            cmd("bat").contains("--to /home/user/.local/bin"),
            "{}",
            cmd("bat")
        );
        assert!(!cmd("bat").contains("sudo"));
        // No update template: the install command is run again.
        assert!(cmd("nodejs").starts_with("fnm install --lts"));
        let node = plan.units.iter().find(|u| u.id == "nodejs").unwrap();
        assert_eq!(node.deps, vec!["fnm"]);
        assert_eq!(plan.units[2].deps, vec!["nodejs"]);
        assert_eq!(node.action, Action::Update);
        assert!(plan.units.iter().all(|u| u.reason == "outdated"));
    }

    #[test]
    fn system_updates_need_sudo() {
        let c = cat();
        let mut f = Facts::sample(Path::new("/home/user"));
        f.privilege = Privilege::None;
        let present = set(&["bat"]);
        let plan = Planner::new(&c, &f, &present).update(&[target("bat", 0, "apt", Scope::System)]);
        assert!(plan.is_empty());
        assert_eq!(plan.skipped[0].1, "needs sudo");
    }
}
