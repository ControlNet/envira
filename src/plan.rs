//! Planning: method choice, dependency closure, topological order and expanded commands.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use crate::catalog::{Catalog, InstallEntry, ScopeSpec};
use crate::facts::Facts;
use crate::state::Scope;
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

/// One command of a unit. A step with a passing `check` is skipped; `cmd = None` means there is
/// nothing to run (a revert the catalog does not describe).
#[derive(Debug, Clone, Serialize)]
pub struct UnitStep {
    pub label: String,
    pub check: Option<String>,
    pub cmd: Option<String>,
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
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Plan {
    pub units: Vec<Unit>,
    /// Targets left out, with the reason.
    pub skipped: Vec<(String, String)>,
}

impl Plan {
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
}

fn uses_sudo(cmd: &str) -> bool {
    cmd.split(|c: char| c.is_whitespace() || c == '(' || c == ';' || c == '&' || c == '|')
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
            if self.present.contains(d) {
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
        }];
        if entry.portable
            && scope == Scope::User
            && self.prefer_system
            && facts.privilege.can_elevate()
        {
            for b in tool.bins(id) {
                let sv = Vars::new(facts, id, b, bin_dir(cat, Scope::System), true);
                let c = sv.expand(
                    "{sudo} install -m 755 \"$(command -v {bin})\" {bin_dir}/{bin}",
                    Ctx::Shell,
                )?;
                steps.push(UnitStep {
                    label: format!("copy {b} to {}", bin_dir(cat, Scope::System)),
                    check: None,
                    cmd: Some(c),
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
        plan.units.push(Unit {
            id: id.to_string(),
            kind,
            action: if kind == Kind::Config {
                Action::Apply
            } else {
                Action::Install
            },
            via: choice.via,
            scope: choice.scope,
            steps: choice.steps,
            verify: choice.verify,
            deps: in_plan,
            reason,
            expect: None,
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
            if !self.present.contains(id) {
                plan.skipped.push((id.clone(), "not applied".into()));
                continue;
            }
            let scope = resolve_scope(cf.scope, self.prefer_system);
            if scope == Scope::System && !self.facts.privilege.can_elevate() {
                plan.skipped.push((id.clone(), "needs sudo".into()));
                continue;
            }
            match config_steps(self.cat, self.facts, id, scope) {
                Ok(cs) => plan.units.push(Unit {
                    id: id.clone(),
                    kind: Kind::Config,
                    action: Action::Revert,
                    via: "config".into(),
                    scope,
                    steps: cs
                        .into_iter()
                        .rev()
                        .map(|s| UnitStep {
                            label: format!("revert {}", s.label),
                            check: None,
                            cmd: s.revert,
                        })
                        .collect(),
                    verify: vec![],
                    deps: vec![],
                    reason: "requested".into(),
                    expect: None,
                }),
                Err(e) => plan.skipped.push((id.clone(), e)),
            }
        }
        plan
    }
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
            match (&st.check, &st.cmd) {
                (Some(c), Some(cmd)) => s.push_str(&format!("{c} || {{ {cmd}; }}\n")),
                (None, Some(cmd)) => s.push_str(&format!("{cmd}\n")),
                (_, None) => s.push_str(&format!("# {}: manual\n", shell_quote(&st.label))),
            }
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
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
    fn dependencies_come_first_and_present_ones_are_skipped() {
        let (c, f) = (cat(), Facts::sample(Path::new("/home/user")));
        let present = set(&[]);
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

        let present = set(&["unzip", "libatomic"]);
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
    fn method_choice_follows_privilege() {
        let c = cat();
        let mut f = Facts::sample(Path::new("/home/user"));
        let present = set(&[]);
        let plan = Planner::new(&c, &f, &present).plan(&["bat".into()], false);
        assert_eq!(plan.units[0].via, "apt");
        assert_eq!(plan.units[0].scope, Scope::System);
        assert!(
            plan.units[0].steps[0]
                .cmd
                .as_deref()
                .unwrap()
                .starts_with("sudo -n env DEBIAN_FRONTEND")
        );
        assert!(plan.needs_sudo(&f));

        f.privilege = Privilege::None;
        let plan = Planner::new(&c, &f, &present).plan(&["bat".into()], false);
        assert_eq!(plan.units[0].via, "github-release");
        assert_eq!(plan.units[0].scope, Scope::User);
        let cmd = plan.units[0].steps[0].cmd.clone().unwrap();
        assert!(cmd.contains("--asset 'bat-v*-x86_64-unknown-linux-musl.tar.gz' --bin bat --to /home/user/.local/bin"), "{cmd}");
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
            "sudo -n install -m 755 \"$(command -v texlab)\" /usr/local/bin/texlab"
        );
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
        assert_eq!(
            plan.skipped,
            vec![(
                "bat".to_string(),
                "only configs can be reverted".to_string()
            )]
        );
    }

    #[test]
    fn force_reinstalls_present_target() {
        let (c, f) = (cat(), Facts::sample(Path::new("/home/user")));
        let present = set(&["herdr"]);
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
