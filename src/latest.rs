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
const ERROR_TTL: chrono::Duration = chrono::Duration::minutes(30);

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

/// Value at a dotted path (`crate.max_stable_version`, `0.version`) as a string.
pub fn json_path(v: &serde_json::Value, path: &str) -> Option<String> {
    let mut cur = v;
    for part in path.split('.').filter(|p| !p.is_empty()) {
        cur = match cur {
            serde_json::Value::Array(a) => a.get(part.parse::<usize>().ok()?)?,
            other => other.get(part)?,
        };
    }
    match cur {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Number(n) => Some(n.to_string()),
        _ => None,
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
        Some(path) => {
            let v: serde_json::Value =
                serde_json::from_str(&text).map_err(|e| format!("invalid JSON: {e}"))?;
            json_path(&v, path).ok_or_else(|| format!("no value at {path:?}"))?
        }
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
    let answers: BTreeMap<Expanded, Result<String, String>> = futures::stream::iter(to_run)
        .map(|q| async move {
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
    out
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
        let v: serde_json::Value = serde_json::from_str(
            r#"{"crate":{"max_stable_version":"4.3.2"},"list":[{"version":"v1"}],"n":3}"#,
        )
        .unwrap();
        assert_eq!(
            json_path(&v, "crate.max_stable_version").as_deref(),
            Some("4.3.2")
        );
        assert_eq!(json_path(&v, "list.0.version").as_deref(), Some("v1"));
        assert_eq!(json_path(&v, "n").as_deref(), Some("3"));
        assert_eq!(json_path(&v, "crate.missing"), None);
        assert_eq!(json_path(&v, "crate"), None);
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
    }

    fn route_of(cat: &Catalog, id: &str, via: &str) -> Route {
        let entry = cat.tools[id]
            .install
            .iter()
            .position(|e| e.method.as_deref().unwrap_or("run") == via)
            .unwrap();
        Route {
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
