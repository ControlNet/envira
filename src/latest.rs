//! Latest-version queries for present tools, with a small on-disk cache so `status` and the TUI
//! do not hit the network on every start.

use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::Result;
use chrono::{DateTime, Local};
use futures::StreamExt;
use serde::{Deserialize, Serialize};

use crate::catalog::{Catalog, Query};
use crate::facts::Facts;
use crate::plan::{Route, bin_dir};
use crate::runner::Runner;
use crate::state::{self, Paths};
use crate::status::{Snapshot, extract_version};
use crate::template::{Ctx, Vars};

const QUERY_TIMEOUT: Duration = Duration::from_secs(20);
/// Successful answers are reused for this long.
pub const TTL: chrono::Duration = chrono::Duration::hours(6);
/// Failed queries are retried sooner.
pub const ERROR_TTL: chrono::Duration = chrono::Duration::minutes(30);

/// How much network a check may use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fetch {
    /// Only cached answers (no network).
    CacheOnly,
    /// Query what is missing from the cache or older than its TTL.
    IfStale,
    /// Query everything again.
    Force,
}

/// A query with every placeholder expanded; also the cache key.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Expanded {
    pub cmd: Option<String>,
    pub http: Option<String>,
    pub json: Option<String>,
    pub regex: Option<String>,
}

/// The `latest` query of the entry a tool lives in (the entry's own, else its method's).
pub fn query_for(
    cat: &Catalog,
    facts: &Facts,
    id: &str,
    route: &Route,
) -> Option<Result<Expanded, String>> {
    let tool = cat.tools.get(id)?;
    let entry = tool.install.get(route.entry)?;
    let method = entry.method.as_ref().and_then(|m| cat.methods.get(m));
    let q: &Query = entry
        .latest
        .as_ref()
        .or(method.and_then(|m| m.latest.as_ref()))?;
    // Queries are read-only and run without sudo; raw entries only get builtins.
    let mut vars = Vars::new(facts, id, tool.bin(id), bin_dir(cat, route.scope), false);
    if let Some(m) = method {
        vars = vars.with_params(&m.defaults).with_params(&entry.params);
    }
    let exp = |t: &Option<String>, ctx| t.as_ref().map(|t| vars.expand(t, ctx)).transpose();
    Some((|| {
        Ok(Expanded {
            cmd: exp(&q.cmd, Ctx::Shell)?,
            http: exp(&q.http, Ctx::Url)?,
            json: q.json.clone(),
            regex: exp(&q.regex, Ctx::Regex)?,
        })
    })())
}

/// Value at a dotted path (`crate.max_stable_version`, `0.version`) of a JSON text, as a
/// string. Only the value at the path is built: some answers are large (the anaconda.org
/// package API returns 11 MB for conda), and a whole `serde_json::Value` of them took ~60 MB.
pub fn json_path(text: &str, path: &str) -> serde_json::Result<Option<String>> {
    let parts: Vec<&str> = path.split('.').filter(|p| !p.is_empty()).collect();
    let mut de = serde_json::Deserializer::from_str(text);
    let v = serde::de::DeserializeSeed::deserialize(PathSeed(&parts), &mut de)?;
    de.end()?;
    Ok(v)
}

/// Walks one level of the path per JSON level and skips everything else.
struct PathSeed<'a>(&'a [&'a str]);

impl<'de> serde::de::DeserializeSeed<'de> for PathSeed<'_> {
    type Value = Option<String>;

    fn deserialize<D: serde::Deserializer<'de>>(self, d: D) -> Result<Self::Value, D::Error> {
        if !self.0.is_empty() {
            return d.deserialize_any(self);
        }
        Ok(match serde_json::Value::deserialize(d)? {
            serde_json::Value::String(s) => Some(s),
            serde_json::Value::Number(n) => Some(n.to_string()),
            _ => None,
        })
    }
}

impl<'de> serde::de::Visitor<'de> for PathSeed<'_> {
    type Value = Option<String>;

    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("a JSON value")
    }

    fn visit_bool<E>(self, _: bool) -> Result<Self::Value, E> {
        Ok(None)
    }
    fn visit_i64<E>(self, _: i64) -> Result<Self::Value, E> {
        Ok(None)
    }
    fn visit_u64<E>(self, _: u64) -> Result<Self::Value, E> {
        Ok(None)
    }
    fn visit_f64<E>(self, _: f64) -> Result<Self::Value, E> {
        Ok(None)
    }
    fn visit_str<E>(self, _: &str) -> Result<Self::Value, E> {
        Ok(None)
    }
    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(None)
    }

    fn visit_map<A: serde::de::MapAccess<'de>>(self, mut m: A) -> Result<Self::Value, A::Error> {
        let mut out = None;
        while let Some(k) = m.next_key::<std::borrow::Cow<str>>()? {
            if k == self.0[0] {
                // The last of duplicate keys wins, as in `serde_json::Value`.
                out = m.next_value_seed(PathSeed(&self.0[1..]))?;
            } else {
                m.next_value::<serde::de::IgnoredAny>()?;
            }
        }
        Ok(out)
    }

    fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut s: A) -> Result<Self::Value, A::Error> {
        let want = self.0[0].parse::<usize>().ok();
        let mut out = None;
        for i in 0.. {
            if want == Some(i) {
                match s.next_element_seed(PathSeed(&self.0[1..]))? {
                    Some(v) => out = v,
                    None => break,
                }
            } else if s.next_element::<serde::de::IgnoredAny>()?.is_none() {
                break;
            }
        }
        Ok(out)
    }
}

/// Runs a query and extracts the version.
pub async fn run_query(q: &Expanded, runner: &dyn Runner) -> Result<String, String> {
    let text = match (&q.cmd, &q.http) {
        (Some(cmd), _) => {
            let p = runner.capture(cmd, QUERY_TIMEOUT).await;
            if !p.ok() {
                let first = p.output.lines().next().unwrap_or("").trim();
                return Err(format!("`{cmd}` failed (exit {}): {first}", p.code));
            }
            p.output
        }
        (None, Some(url)) => runner.http_get(url, QUERY_TIMEOUT).await?,
        (None, None) => return Err("query has no source".into()),
    };
    let text = match &q.json {
        Some(path) => json_path(&text, path)
            .map_err(|e| format!("invalid JSON: {e}"))?
            .ok_or_else(|| format!("no value at {path:?}"))?,
        None => text,
    };
    extract_version(&text, q.regex.as_deref()).ok_or_else(|| "no version in the answer".into())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheEntry {
    pub query: Expanded,
    pub version: Option<String>,
    pub error: Option<String>,
    pub checked_at: String,
}

impl CacheEntry {
    fn fresh(&self, now: DateTime<Local>) -> bool {
        let ttl = if self.version.is_some() {
            TTL
        } else {
            ERROR_TTL
        };
        DateTime::parse_from_rfc3339(&self.checked_at)
            .is_ok_and(|t| now.signed_duration_since(t) < ttl)
    }
}

/// `latest.toml` in the state dir, keyed by tool id.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Cache {
    #[serde(default)]
    pub tools: BTreeMap<String, CacheEntry>,
}

impl Cache {
    fn file(paths: &Paths) -> std::path::PathBuf {
        paths.state_dir.join("latest.toml")
    }

    /// A missing or unreadable cache is empty.
    pub fn load(paths: &Paths) -> Cache {
        std::fs::read_to_string(Self::file(paths))
            .ok()
            .and_then(|t| toml::from_str(&t).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, paths: &Paths, facts: &Facts) -> Result<()> {
        state::write_atomic(
            &Self::file(paths),
            toml::to_string_pretty(self)?.as_bytes(),
            facts,
        )
    }
}

/// Answer for one tool.
#[derive(Debug, Clone, PartialEq)]
pub enum Latest {
    Version(String),
    Error(String),
}

/// Latest versions of the present, updatable tools in `snap`. Identical queries run once.
/// Returns the answers; the cache is updated in place.
pub async fn check(
    cat: &Catalog,
    facts: &Facts,
    snap: &Snapshot,
    runner: &dyn Runner,
    cache: &mut Cache,
    fetch: Fetch,
) -> BTreeMap<String, Latest> {
    let now = Local::now();
    let mut wanted: Vec<(String, Expanded)> = Vec::new();
    let mut out = BTreeMap::new();
    for i in &snap.items {
        let Some(route) = i.route.as_ref().filter(|_| i.present) else {
            continue;
        };
        match query_for(cat, facts, &i.id, route) {
            None => {}
            Some(Err(e)) => {
                out.insert(i.id.clone(), Latest::Error(e));
            }
            Some(Ok(q)) => wanted.push((i.id.clone(), q)),
        }
    }
    let mut to_run: Vec<Expanded> = Vec::new();
    for (id, q) in &wanted {
        let cached = cache.tools.get(id).filter(|e| &e.query == q);
        let use_cache = match fetch {
            Fetch::CacheOnly => true,
            Fetch::IfStale => cached.is_some_and(|e| e.fresh(now)),
            Fetch::Force => false,
        };
        if use_cache {
            if let Some(e) = cached {
                out.insert(
                    id.clone(),
                    match (&e.version, &e.error) {
                        (Some(v), _) => Latest::Version(v.clone()),
                        (None, err) => Latest::Error(err.clone().unwrap_or_default()),
                    },
                );
            }
        } else if !to_run.contains(q) {
            to_run.push(q.clone());
        }
    }
    // HTTP queries are cheap; command queries (dnf, zypper, apt-cache, rustup) are processes
    // that can take hundreds of MiB each, so fewer of those run at once on a small machine.
    let procs = tokio::sync::Semaphore::new(crate::runner::process_jobs().min(6));
    let procs = &procs;
    let answers: BTreeMap<Expanded, Result<String, String>> = futures::stream::iter(to_run)
        .map(|q| async move {
            let _slot = match q.cmd {
                Some(_) => procs.acquire().await.ok(),
                None => None,
            };
            let r = run_query(&q, runner).await;
            (q, r)
        })
        .buffer_unordered(6)
        .collect()
        .await;
    let stamp = state::now_rfc3339();
    for (id, q) in wanted {
        let Some(r) = answers.get(&q) else { continue };
        cache.tools.insert(
            id.clone(),
            CacheEntry {
                query: q.clone(),
                version: r.as_ref().ok().cloned(),
                error: r.as_ref().err().cloned(),
                checked_at: stamp.clone(),
            },
        );
        out.insert(
            id,
            match r {
                Ok(v) => Latest::Version(v.clone()),
                Err(e) => Latest::Error(e.clone()),
            },
        );
    }
    flag_stale_apt(snap, &mut out, apt_lists_behind);
    out
}

/// apt's candidate comes from the local package lists: when they are behind (`apt::decide`),
/// a candidate no newer than the installed package does not show the package is current.
/// That answer becomes an error saying so; a newer candidate still counts.
fn flag_stale_apt(
    snap: &Snapshot,
    out: &mut BTreeMap<String, Latest>,
    behind: impl FnOnce() -> Option<String>,
) {
    let current: Vec<&str> = snap
        .items
        .iter()
        .filter(|i| i.package_versions && i.route.as_ref().is_some_and(|r| r.via == "apt"))
        .filter(|i| match (out.get(&i.id), &i.method_version) {
            (Some(Latest::Version(l)), Some(v)) => !crate::version::is_newer_package(l, v),
            _ => false,
        })
        .map(|i| i.id.as_str())
        .collect();
    if current.is_empty() {
        return;
    }
    let Some(why) = behind() else { return };
    for id in current {
        out.insert(
            id.to_string(),
            Latest::Error(format!(
                "apt's package lists may be behind ({why}); `sudo apt update` to check"
            )),
        );
    }
}

/// Why apt's package lists on this machine may be behind the archive, or None when fresh.
fn apt_lists_behind() -> Option<String> {
    let now = std::time::SystemTime::now();
    match crate::apt::decide(&crate::apt::Paths::default(), now, crate::apt::MAX_AGE) {
        crate::apt::Decision::Refresh(why) => Some(why),
        crate::apt::Decision::Fresh(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::load::{embedded_layer, load};
    use crate::runner::fake::FakeRunner;
    use crate::state::Scope;
    use std::path::Path;

    #[test]
    fn json_paths() {
        let v = r#"{"skip":{"a":[1,{"b":null}],"s":"x"},"crate":{"max_stable_version":"4.3.2"},
            "list":[{"version":"v0"},{"version":"v1"}],"n":3,"f":1.5,"dup":"a","dup":"b"}"#;
        let get = |p| json_path(v, p).unwrap();
        assert_eq!(get("crate.max_stable_version").as_deref(), Some("4.3.2"));
        assert_eq!(get("list.1.version").as_deref(), Some("v1"));
        assert_eq!(get("n").as_deref(), Some("3"));
        assert_eq!(get("f").as_deref(), Some("1.5"));
        assert_eq!(get("dup").as_deref(), Some("b"));
        assert_eq!(get("crate.missing"), None);
        assert_eq!(get("crate"), None);
        assert_eq!(get("list.7.version"), None);
        assert_eq!(get("list.x"), None);
        assert_eq!(get("n.deeper"), None);
        assert!(json_path("{\"a\":", "a").is_err());
        assert!(json_path("{} trailing", "a").is_err());
    }

    #[test]
    fn method_and_entry_queries_expand() {
        let cat = load(&[embedded_layer()]).unwrap().catalog;
        let f = Facts::sample(Path::new("/home/user"));
        let codex = route_of(&cat, "codex", "npm");
        let q = query_for(&cat, &f, "codex", &codex).unwrap().unwrap();
        assert_eq!(
            q.http.as_deref(),
            Some("https://registry.npmjs.org/@openai/codex/latest")
        );
        assert_eq!(q.json.as_deref(), Some("version"));
        let bun = route_of(&cat, "bun", "run");
        let q = query_for(&cat, &f, "bun", &bun).unwrap().unwrap();
        assert!(q.http.unwrap().contains("oven-sh/bun"));
        assert_eq!(q.json.as_deref(), Some("tag_name"));
        let gcc = route_of(&cat, "build-tools", "dnf");
        let q = query_for(&cat, &f, "build-tools", &gcc).unwrap().unwrap();
        assert_eq!(
            q.cmd.as_deref(),
            Some("dnf -q -C repoquery --latest-limit=1 --qf '%{evr}\\n' gcc make perl")
        );
        let tmux = route_of(&cat, "tmux", "zypper");
        let q = query_for(&cat, &f, "tmux", &tmux).unwrap().unwrap();
        assert_eq!(q.cmd.as_deref(), Some("zypper -n --no-refresh info tmux"));
    }

    #[tokio::test]
    async fn dnf_and_zypper_answers_parse() {
        let cat = load(&[embedded_layer()]).unwrap().catalog;
        let f = Facts::sample(Path::new("/home/user"));
        let r = FakeRunner::default();
        // Real output from Fedora and openSUSE Leap (a warning first, then the candidate).
        r.rule("repoquery", 0, "15.2.1-3.fc43\n1:4.4.1-10.fc43\n4:5.40.2-512.fc43\n")
            .rule(
                "info tmux",
                0,
                "Warning: Repository 'Main Update Repository' metadata expired.\nInformation for package tmux:\n-------------------------------\nRepository     : Main Repository\nName           : tmux\nVersion        : 3.3a-150300.3.6.1\nStatus         : not installed\n",
            )
            .rule("info git", 104, "package 'git' not found.\n");
        let q = |id: &str, via: &str| {
            query_for(&cat, &f, id, &route_of(&cat, id, via))
                .unwrap()
                .unwrap()
        };
        let gcc = run_query(&q("build-tools", "dnf"), &r).await;
        assert_eq!(gcc.as_deref(), Ok("15.2.1-3.fc43"));
        assert_eq!(
            run_query(&q("tmux", "zypper"), &r).await.as_deref(),
            Ok("3.3a-150300.3.6.1")
        );
        assert!(run_query(&q("git", "zypper"), &r).await.is_err());
    }

    #[test]
    fn apt_lists_behind_do_not_show_a_package_is_current() {
        let item = |id: &str, installed: &str| {
            let mut i = crate::tui::app::tests::item(
                id,
                crate::plan::Kind::Tool,
                crate::status::Label::Installed,
                true,
            );
            i.package_versions = true;
            i.method_version = Some(installed.into());
            i.route = Some(Route {
                key: String::new(),
                entry: 0,
                via: "apt".into(),
                scope: Scope::System,
            });
            i
        };
        let snap = Snapshot {
            items: vec![item("bat", "0.24.0-1"), item("fd", "9.0.0-1")],
            first_run: false,
        };
        let answers = || {
            BTreeMap::from([
                ("bat".to_string(), Latest::Version("0.24.0-1".into())),
                ("fd".to_string(), Latest::Version("9.0.0-2".into())),
            ])
        };
        // Fresh lists: as answered.
        let mut out = answers();
        flag_stale_apt(&snap, &mut out, || None);
        assert_eq!(out, answers());
        // Behind: the same version says nothing; a newer one still counts.
        let mut out = answers();
        flag_stale_apt(&snap, &mut out, || {
            Some("the lists were refreshed 9 days ago".into())
        });
        assert!(matches!(&out["bat"], Latest::Error(e) if e.contains("9 days ago")));
        assert_eq!(out["fd"], Latest::Version("9.0.0-2".into()));
    }

    fn route_of(cat: &Catalog, id: &str, via: &str) -> Route {
        let entry = cat.tools[id]
            .install
            .iter()
            .position(|e| e.method.as_deref().unwrap_or("run") == via)
            .unwrap();
        Route {
            key: String::new(),
            entry,
            via: via.into(),
            scope: Scope::User,
        }
    }

    #[tokio::test]
    async fn query_runs_json_then_regex() {
        let r = FakeRunner::default();
        r.rule("api.github.com/repos/oven-sh/bun", 0, r#"{"tag_name":"bun-v1.4.4"}"#)
            .rule("rustup check", 0, "stable-x86_64-unknown-linux-gnu - update available: 1.93.0 (x 2026-01-19) -> 1.99.1 (y 2026-10-13)\n");
        let q = Expanded {
            cmd: None,
            http: Some("https://api.github.com/repos/oven-sh/bun/releases/latest".into()),
            json: Some("tag_name".into()),
            regex: Some(r"bun-v(\S+)".into()),
        };
        assert_eq!(run_query(&q, &r).await.as_deref(), Ok("1.4.4"));
        let q = Expanded {
            cmd: Some("rustup check".into()),
            http: None,
            json: None,
            regex: Some(
                r"(?m)^stable-\S+ - (?:update available: \S+ \([^)]*\) -> |up to date: )(\S+)"
                    .into(),
            ),
        };
        assert_eq!(run_query(&q, &r).await.as_deref(), Ok("1.99.1"));
        let q = Expanded {
            http: Some("https://example.invalid/x".into()),
            ..q
        };
        let q = Expanded { cmd: None, ..q };
        assert!(run_query(&q, &r).await.is_err());
    }
}
