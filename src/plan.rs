//! Planning: method choice, dependency closure, topological order and expanded commands.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use crate::catalog::{Catalog, ScopeSpec};
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
    Apply,
    Revert,
}

impl Action {
    pub fn as_str(self) -> &'static str {
        match self {
            Action::Install => "install",
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
        for entry in &tool.install {
            let name = entry.method.as_deref().unwrap_or("run");
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
            let vars = Vars::new(
                facts,
                id,
                tool.bin(id),
                bin_dir(cat, scope),
                scope == Scope::System,
            );
            let (label, tpl, vars) = match (method, &entry.run) {
                (Some(m), _) => (
                    format!("{name} install"),
                    m.install.clone().unwrap_or_else(|| "false".into()),
                    vars.with_params(&m.defaults).with_params(&entry.params),
                ),
                (None, Some(run)) => ("install".to_string(), run.clone(), vars),
                (None, None) => continue,
            };
            let cmd = match vars.expand(&tpl, Ctx::Shell) {
                Ok(c) => c,
                Err(e) => {
                    notes.push(format!("{name}: {e}"));
                    continue;
                }
            };
            let mut steps = vec![UnitStep {
                label,
                check: None,
                cmd: Some(cmd),
            }];
            // Portable binaries built as the user are copied into the system bin dir.
            if entry.portable
                && scope == Scope::User
                && self.prefer_system
                && facts.privilege.can_elevate()
            {
                for b in tool.bins(id) {
                    let sv = Vars::new(facts, id, b, bin_dir(cat, Scope::System), true);
                    let c = sv
                        .expand(
                            "{sudo} install -m 755 \"$(command -v {bin})\" {bin_dir}/{bin}",
                            Ctx::Shell,
                        )
                        .unwrap_or_default();
                    steps.push(UnitStep {
                        label: format!("copy {b} to {}", bin_dir(cat, Scope::System)),
                        check: None,
                        cmd: Some(c),
                    });
                }
            }
            return Ok(Choice {
                via: name.to_string(),
                scope,
                deps,
                steps,
                verify,
            });
        }
        Err(summarize(&notes, facts))
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
            vec!["fnm", "nodejs", "codex", "codex-sandbox-network"]
        );
        assert_eq!(plan.units[2].deps, vec!["nodejs"]);
        assert_eq!(plan.units[1].reason, "needed by codex");

        let present = set(&["fnm", "nodejs"]);
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
        let plan = Planner::new(&c, &f, &present).plan(&["yazi".into()], false);
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
        let present = set(&["rust-toolchain"]);
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
        assert_eq!(p.choose("nvitop").unwrap_err(), "when gpu is not met");
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
            check.contains("fs write --check /etc/sysctl.d/60-envira-inotify.conf"),
            "{check}"
        );
        assert!(u.steps[0].cmd.as_deref().unwrap().starts_with("sudo -n "));
        assert_eq!(
            u.steps[1].cmd.as_deref().unwrap(),
            "sudo -n sysctl --system"
        );
        assert_eq!(u.verify.len(), 1);
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
}
