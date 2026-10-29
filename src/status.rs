//! Detection and per-item status.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use futures::StreamExt;
use regex::Regex;
use serde::Serialize;

use crate::catalog::Catalog;
use crate::facts::Facts;
use crate::plan::{Kind, Planner, bin_dir, config_steps, detect_cmd};
use crate::runner::Runner;
use crate::state::{LastResult, Scope, State};
use crate::template::{Ctx, Vars, shell_quote, unmet};

const DETECT_TIMEOUT: Duration = Duration::from_secs(15);
const QUERY_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_VERSION_RE: &str = r"(\d+(?:\.\d+)+[0-9A-Za-z.+~-]*)";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Ownership {
    /// Installed by envira.
    Managed,
    /// Present, not installed by envira, but envira can take it over.
    Adoptable,
    /// Present and owned by someone else (system package without sudo, admin install).
    External,
}

impl Ownership {
    pub fn as_str(self) -> &'static str {
        match self {
            Ownership::Managed => "managed",
            Ownership::Adoptable => "adoptable",
            Ownership::External => "external",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Label {
    Installed,
    Applied,
    Missing,
    New,
    Available,
    Failed,
    Drifted,
    NotApplicable,
}

impl Label {
    pub fn glyph(self) -> &'static str {
        match self {
            Label::Installed | Label::Applied => "✓",
            Label::Missing => "✗",
            Label::New => "◆",
            Label::Available => "·",
            Label::Failed | Label::Drifted => "!",
            Label::NotApplicable => "–",
        }
    }

    pub fn text(self) -> &'static str {
        match self {
            Label::Installed => "installed",
            Label::Applied => "applied",
            Label::Missing => "missing",
            Label::New => "new",
            Label::Available => "available",
            Label::Failed => "failed",
            Label::Drifted => "drifted",
            Label::NotApplicable => "n/a",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ItemStatus {
    pub id: String,
    pub kind: Kind,
    pub category: String,
    pub label: Label,
    pub present: bool,
    pub version: Option<String>,
    pub ownership: Option<Ownership>,
    /// Resolved executable for present tools.
    pub path: Option<String>,
    pub desired: bool,
    pub seen: bool,
    /// Method used (managed), matched (adoptable) or planned (absent).
    pub via: Option<String>,
    pub scope: Option<Scope>,
    /// Why the item cannot be installed here.
    pub reason: Option<String>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub items: Vec<ItemStatus>,
    /// No state yet on this machine.
    pub first_run: bool,
}

impl Snapshot {
    pub fn get(&self, id: &str) -> Option<&ItemStatus> {
        self.items.iter().find(|i| i.id == id)
    }

    pub fn present(&self) -> BTreeSet<String> {
        self.items
            .iter()
            .filter(|i| i.present)
            .map(|i| i.id.clone())
            .collect()
    }

    pub fn count(&self, f: impl Fn(&ItemStatus) -> bool) -> usize {
        self.items.iter().filter(|i| f(i)).count()
    }

    /// Desired and not present (what `sync` installs), excluding items that cannot be installed.
    pub fn to_sync(&self) -> Vec<String> {
        self.items
            .iter()
            .filter(|i| i.desired && !i.present && i.label != Label::NotApplicable)
            .map(|i| i.id.clone())
            .collect()
    }
}

/// PATH used for detection and execution: catalog dirs, tool shell dirs, the caller's PATH and
/// the standard system dirs, de-duplicated.
pub fn effective_path(cat: &Catalog, facts: &Facts) -> String {
    let mut dirs: Vec<String> = Vec::new();
    let mut push = |d: String| {
        if !d.is_empty() && !dirs.contains(&d) {
            dirs.push(d);
        }
    };
    for d in &cat.env.path {
        push(facts.expand_home(d));
    }
    for t in cat.tools.values() {
        for d in t.shell.iter().flat_map(|s| &s.path) {
            push(facts.expand_home(d));
        }
    }
    if let Some(p) = std::env::var_os("PATH") {
        for d in std::env::split_paths(&p) {
            push(d.display().to_string());
        }
    }
    for d in [
        "/usr/local/sbin",
        "/usr/local/bin",
        "/usr/sbin",
        "/usr/bin",
        "/sbin",
        "/bin",
    ] {
        push(d.to_string());
    }
    dirs.join(":")
}

pub fn extract_version(output: &str, re: Option<&str>) -> Option<String> {
    let re = Regex::new(re.unwrap_or(DEFAULT_VERSION_RE)).ok()?;
    re.captures(output)
        .and_then(|c| c.get(1).or_else(|| c.get(0)))
        .map(|m| m.as_str().to_string())
}

#[derive(Debug, Clone, Default)]
struct ToolProbe {
    present: bool,
    version: Option<String>,
    path: Option<String>,
}

pub async fn probe_tool(
    cat: &Catalog,
    facts: &Facts,
    id: &str,
    runner: &dyn Runner,
) -> (bool, Option<String>) {
    let p = probe_tool_full(cat, facts, id, runner).await;
    (p.present, p.version)
}

async fn probe_tool_full(cat: &Catalog, facts: &Facts, id: &str, runner: &dyn Runner) -> ToolProbe {
    let tool = &cat.tools[id];
    let cmd = detect_cmd(cat, facts, id);
    let p = runner.capture(&cmd, DETECT_TIMEOUT).await;
    if !p.ok() {
        return ToolProbe::default();
    }
    let re = tool.detect.as_ref().and_then(|d| d.version.as_deref());
    let version = extract_version(&p.output, re);
    let lookup = tool
        .bins(id)
        .iter()
        .map(|b| format!("command -v {}", shell_quote(b)))
        .collect::<Vec<_>>()
        .join(" || ");
    let w = runner.capture(&lookup, DETECT_TIMEOUT).await;
    let path = w
        .ok()
        .then(|| w.output.lines().next().unwrap_or("").trim().to_string())
        .filter(|s| !s.is_empty());
    ToolProbe {
        present: true,
        version,
        path,
    }
}

/// True when every check of a config passes.
pub async fn probe_config(cat: &Catalog, facts: &Facts, id: &str, runner: &dyn Runner) -> bool {
    let scope = if cat.configs[id].scope == Some(crate::catalog::ScopeSpec::System) {
        Scope::System
    } else {
        Scope::User
    };
    let Ok(steps) = config_steps(cat, facts, id, scope) else {
        return false;
    };
    let checks: Vec<String> = steps.into_iter().filter_map(|s| s.check).collect();
    if checks.is_empty() {
        return false;
    }
    for c in checks {
        if !runner.capture(&c, DETECT_TIMEOUT).await.ok() {
            return false;
        }
    }
    true
}

/// Detects every item and derives its status.
pub async fn snapshot(
    cat: &Catalog,
    facts: &Facts,
    state: Option<&State>,
    runner: &dyn Runner,
) -> Snapshot {
    let ids = cat.item_ids();
    let probes: BTreeMap<String, ToolProbe> = futures::stream::iter(ids.iter().cloned())
        .map(|id| async move {
            let p = if cat.tools.contains_key(&id) {
                probe_tool_full(cat, facts, &id, runner).await
            } else {
                ToolProbe {
                    present: probe_config(cat, facts, &id, runner).await,
                    ..Default::default()
                }
            };
            (id, p)
        })
        .buffer_unordered(8)
        .collect()
        .await;

    let present: BTreeSet<String> = probes
        .iter()
        .filter(|(_, p)| p.present)
        .map(|(id, _)| id.clone())
        .collect();
    let adoptable = adoption_queries(cat, facts, state, &probes, runner).await;
    let empty = State::default();
    let st = state.unwrap_or(&empty);
    let desired = st.desired(cat);
    let mut planner = Planner::new(cat, facts, &present);

    let items =
        ids.iter()
            .map(|id| {
                let p = &probes[id];
                let kind = if cat.tools.contains_key(id) {
                    Kind::Tool
                } else {
                    Kind::Config
                };
                let when = if kind == Kind::Tool {
                    &cat.tools[id].when
                } else {
                    &cat.configs[id].when
                };
                let choice = planner.choose(id);
                let (mut via, mut scope) = match &choice {
                    Ok(c) => (Some(c.via.clone()), Some(c.scope)),
                    Err(_) => (None, None),
                };
                let reason = match &choice {
                    Err(e) if !p.present => Some(e.clone()),
                    _ if !p.present => unmet(when, facts).map(|w| format!("when {w} is not met")),
                    _ => None,
                };
                let rec = st.tools.get(id);
                let crec = st.configs.get(id);
                let last_error = match kind {
                    Kind::Tool => rec
                        .filter(|r| r.last_result == LastResult::Failed)
                        .and_then(|r| r.last_error.clone()),
                    Kind::Config => crec
                        .filter(|r| r.last_result == LastResult::Failed)
                        .and_then(|r| r.last_error.clone()),
                };
                let ownership =
                    (p.present && kind == Kind::Tool).then(|| {
                        if st.is_managed(id) {
                            if let Some(r) = rec {
                                via = r.method.clone().or(via.clone());
                                scope = r.scope.or(scope);
                            }
                            Ownership::Managed
                        } else if let Some(m) = adoptable.get(id) {
                            via = Some(m.clone());
                            Ownership::Adoptable
                        } else if p.path.as_deref().is_some_and(|path| {
                            path.starts_with(&format!("{}/", facts.home.display()))
                        }) {
                            Ownership::Adoptable
                        } else {
                            Ownership::External
                        }
                    });
                let drifted = kind == Kind::Config && !p.present && crec.is_some_and(|r| r.applied);
                let seen = state.is_none() || st.seen.contains(id);
                let is_desired = desired.contains(id);
                let label = if p.present {
                    if kind == Kind::Tool {
                        Label::Installed
                    } else {
                        Label::Applied
                    }
                } else if reason.is_some() {
                    Label::NotApplicable
                } else if last_error.is_some() {
                    Label::Failed
                } else if drifted {
                    Label::Drifted
                } else if !seen {
                    Label::New
                } else if is_desired {
                    Label::Missing
                } else {
                    Label::Available
                };
                ItemStatus {
                    id: id.clone(),
                    kind,
                    category: cat.category(id).to_string(),
                    label,
                    present: p.present,
                    version: p.version.clone(),
                    ownership,
                    path: p.path.clone(),
                    desired: is_desired,
                    seen,
                    via,
                    scope,
                    reason,
                    last_error,
                }
            })
            .collect();
    Snapshot {
        items,
        first_run: state.is_none(),
    }
}

/// For present tools not managed by envira: the first install method whose `installed` query
/// finds the tool. Queries shared by many tools (e.g. `pipx list`) run once.
async fn adoption_queries(
    cat: &Catalog,
    facts: &Facts,
    state: Option<&State>,
    probes: &BTreeMap<String, ToolProbe>,
    runner: &dyn Runner,
) -> BTreeMap<String, String> {
    let mut wanted: Vec<(String, String, String, String)> = Vec::new(); // id, method, cmd, regex
    for (id, p) in probes {
        if !p.present || state.is_some_and(|s| s.is_managed(id)) {
            continue;
        }
        let Some(tool) = cat.tools.get(id) else {
            continue;
        };
        for e in &tool.install {
            let Some(mname) = &e.method else { continue };
            let Some(m) = cat.methods.get(mname) else {
                continue;
            };
            if unmet(&m.when, facts).is_some() || unmet(&e.when, facts).is_some() {
                continue;
            }
            let Some(q) = &m.installed else { continue };
            let (Some(cmd), Some(re)) = (&q.cmd, &q.regex) else {
                continue;
            };
            let vars = Vars::new(facts, id, tool.bin(id), bin_dir(cat, Scope::User), false)
                .with_params(&m.defaults)
                .with_params(&e.params);
            if let (Ok(c), Ok(r)) = (vars.expand(cmd, Ctx::Shell), vars.expand(re, Ctx::Regex)) {
                wanted.push((id.clone(), mname.clone(), c, r));
            }
        }
    }
    let cmds: BTreeSet<String> = wanted.iter().map(|w| w.2.clone()).collect();
    let outputs: BTreeMap<String, String> = futures::stream::iter(cmds)
        .map(|c| async move {
            let p = runner.capture(&c, QUERY_TIMEOUT).await;
            (c, if p.ok() { p.output } else { String::new() })
        })
        .buffer_unordered(4)
        .collect()
        .await;
    let mut found = BTreeMap::new();
    for (id, method, cmd, re) in wanted {
        if found.contains_key(&id) {
            continue;
        }
        let out = &outputs[&cmd];
        if !out.is_empty() && Regex::new(&re).is_ok_and(|r| r.is_match(out)) {
            found.insert(id, method);
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::load::{embedded_layer, load};
    use crate::runner::fake::FakeRunner;
    use crate::state::ToolRecord;
    use std::path::Path;

    fn cat() -> Catalog {
        load(&[embedded_layer()]).unwrap().catalog
    }

    #[test]
    fn version_extraction() {
        assert_eq!(
            extract_version("bat 0.24.0 (fc95468)", Some(r"bat (\S+)")).as_deref(),
            Some("0.24.0")
        );
        assert_eq!(
            extract_version("Docker version 27.3.1, build ce12230", None).as_deref(),
            Some("27.3.1")
        );
        assert_eq!(
            extract_version("v22.11.0\n", Some(r"v(\S+)")).as_deref(),
            Some("22.11.0")
        );
    }

    #[tokio::test]
    async fn labels_and_ownership() {
        let c = cat();
        let f = Facts::sample(Path::new("/home/user"));
        let r = FakeRunner::default();
        r.rule("bat --version", 0, "bat 0.24.0\n")
            .rule("command -v bat", 0, "/usr/bin/batcat\n")
            .rule("dpkg-query -W -f='${Version}' bat", 0, "0.24.0-1build1")
            .rule("herdr --version", 0, "herdr 0.3.1\n")
            .rule("command -v herdr", 0, "/home/user/.local/bin/herdr\n")
            .rule("yazi --version", 0, "Yazi 25.5.31\n")
            .rule("command -v yazi", 0, "/opt/yazi/yazi\n")
            .rule("codex --version", 0, "codex-cli 0.46.0\n")
            .rule(
                "command -v codex",
                0,
                "/home/user/.local/share/fnm/aliases/default/bin/codex\n",
            );
        let mut st = State {
            bundles: vec!["terminal".into(), "ai-agents".into()],
            ..Default::default()
        };
        st.mark_all_seen(&c);
        st.seen.remove("texlab");
        st.tools.insert(
            "codex".into(),
            ToolRecord {
                method: Some("npm".into()),
                scope: Some(Scope::User),
                version: Some("0.46.0".into()),
                installed_at: Some("t".into()),
                last_result: LastResult::Ok,
                last_error: None,
                last_run: None,
            },
        );
        st.tools.insert(
            "fnm".into(),
            ToolRecord {
                method: None,
                scope: None,
                version: None,
                installed_at: None,
                last_result: LastResult::Failed,
                last_error: Some("exit 1".into()),
                last_run: None,
            },
        );
        st.configs.insert(
            "codex-sandbox-network".into(),
            crate::state::ConfigRecord {
                applied: true,
                applied_at: None,
                last_result: LastResult::Ok,
                last_error: None,
                last_run: None,
            },
        );
        let snap = snapshot(&c, &f, Some(&st), &r).await;
        let get = |id: &str| snap.get(id).unwrap().clone();

        let bat = get("bat");
        assert_eq!(
            (bat.label, bat.ownership, bat.via.as_deref()),
            (Label::Installed, Some(Ownership::Adoptable), Some("apt"))
        );
        assert_eq!(bat.version.as_deref(), Some("0.24.0"));
        assert_eq!(get("herdr").ownership, Some(Ownership::Adoptable));
        assert_eq!(get("yazi").ownership, Some(Ownership::External));
        assert_eq!(get("codex").ownership, Some(Ownership::Managed));
        assert_eq!(get("codex").via.as_deref(), Some("npm"));
        assert_eq!(get("texlab").label, Label::New);
        assert_eq!(get("fnm").label, Label::Failed);
        assert_eq!(get("yazi-theme").label, Label::Missing);
        assert_eq!(get("codex-sandbox-network").label, Label::Drifted);
        assert_eq!(get("nvitop").label, Label::NotApplicable);
        assert_eq!(get("pipx").label, Label::Available);
        assert!(snap.to_sync().contains(&"yazi-theme".to_string()));
        assert!(!snap.to_sync().contains(&"bat".to_string()));
    }

    #[tokio::test]
    async fn first_run_has_nothing_new_or_desired() {
        let c = cat();
        let f = Facts::sample(Path::new("/home/user"));
        let snap = snapshot(&c, &f, None, &FakeRunner::default()).await;
        assert!(snap.first_run);
        assert_eq!(snap.get("herdr").unwrap().label, Label::Available);
        assert!(snap.to_sync().is_empty());
    }

    #[test]
    fn path_has_catalog_dirs_first() {
        let c = cat();
        let f = Facts::sample(Path::new("/home/user"));
        let p = effective_path(&c, &f);
        assert!(
            p.starts_with("/home/user/.local/bin:/home/user/.cargo/bin:"),
            "{p}"
        );
        assert!(p.contains("/home/user/.local/share/fnm/aliases/default/bin"));
        assert_eq!(p.matches("/home/user/.cargo/bin:").count(), 1);
    }
}
