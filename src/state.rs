//! Per-user machine state (`~/.local/state/envira/state.toml`) and run records.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::catalog::Catalog;
use crate::facts::Facts;

#[derive(Debug, Clone)]
pub struct Paths {
    pub state_dir: PathBuf,
    pub config_dir: PathBuf,
}

impl Paths {
    pub fn new(facts: &Facts) -> Paths {
        // XDG variables belong to the caller; when acting for SUDO_USER use that user's defaults.
        let xdg = |var: &str| {
            if facts.drop_to.is_none() {
                std::env::var_os(var).map(PathBuf::from)
            } else {
                None
            }
        };
        let state_dir = std::env::var_os("ENVIRA_STATE_DIR")
            .map(PathBuf::from)
            .or_else(|| xdg("XDG_STATE_HOME").map(|p| p.join("envira")))
            .unwrap_or_else(|| facts.home.join(".local/state/envira"));
        let config_dir = std::env::var_os("ENVIRA_CONFIG_DIR")
            .map(PathBuf::from)
            .or_else(|| xdg("XDG_CONFIG_HOME").map(|p| p.join("envira")))
            .unwrap_or_else(|| facts.home.join(".config/envira"));
        Paths {
            state_dir,
            config_dir,
        }
    }

    pub fn state_file(&self) -> PathBuf {
        self.state_dir.join("state.toml")
    }
    pub fn logs_dir(&self) -> PathBuf {
        self.state_dir.join("logs")
    }
    pub fn run_dir(&self, run_id: &str) -> PathBuf {
        self.logs_dir().join(run_id)
    }
    pub fn overlay_dir(&self) -> PathBuf {
        self.config_dir.join("catalog.d")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    User,
    System,
}

impl std::fmt::Display for Scope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Scope::User => "user",
            Scope::System => "system",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LastResult {
    Ok,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolRecord {
    pub method: Option<String>,
    pub scope: Option<Scope>,
    pub version: Option<String>,
    /// Set once envira installed the tool successfully: it is then `managed`.
    pub installed_at: Option<String>,
    pub last_result: LastResult,
    pub last_error: Option<String>,
    pub last_run: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigRecord {
    pub applied: bool,
    pub applied_at: Option<String>,
    pub last_result: LastResult,
    pub last_error: Option<String>,
    pub last_run: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct State {
    /// Bundles this machine keeps in sync.
    #[serde(default)]
    pub bundles: Vec<String>,
    #[serde(default)]
    pub include: Vec<String>,
    #[serde(default)]
    pub exclude: Vec<String>,
    /// Catalog ids already shown to the user; others are `new`.
    #[serde(default)]
    pub seen: BTreeSet<String>,
    pub last_run: Option<String>,
    #[serde(default)]
    pub tools: BTreeMap<String, ToolRecord>,
    #[serde(default)]
    pub configs: BTreeMap<String, ConfigRecord>,
}

impl State {
    /// Loads state; `Ok(None)` when this machine has never run envira.
    pub fn load(paths: &Paths) -> Result<Option<State>> {
        let f = paths.state_file();
        if !f.exists() {
            return Ok(None);
        }
        let text =
            std::fs::read_to_string(&f).with_context(|| format!("reading {}", f.display()))?;
        Ok(Some(
            toml::from_str(&text).with_context(|| format!("parsing {}", f.display()))?,
        ))
    }

    pub fn save(&self, paths: &Paths, facts: &Facts) -> Result<()> {
        let text = toml::to_string_pretty(self)?;
        write_atomic(&paths.state_file(), text.as_bytes(), facts)
    }

    /// Desired ids: bundle members plus `include`, minus `exclude`.
    pub fn desired(&self, cat: &Catalog) -> BTreeSet<String> {
        let mut d: BTreeSet<String> = self
            .bundles
            .iter()
            .flat_map(|b| cat.bundle_items(b))
            .chain(self.include.iter().cloned())
            .collect();
        for x in &self.exclude {
            d.remove(x);
        }
        d
    }

    pub fn is_managed(&self, id: &str) -> bool {
        self.tools.get(id).is_some_and(|r| r.installed_at.is_some())
    }

    pub fn mark_all_seen(&mut self, cat: &Catalog) {
        self.seen.extend(cat.item_ids());
    }
}

/// Writes a file atomically, creating parents; hands files to the target user when running as
/// root on their behalf.
pub fn write_atomic(path: &Path, data: &[u8], facts: &Facts) -> Result<()> {
    let dir = path.parent().context("path without parent")?;
    create_dir_all_owned(dir, facts)?;
    let tmp = dir.join(format!(
        ".{}.envira-tmp",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("f")
    ));
    std::fs::write(&tmp, data).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))?;
    chown_to_target(path, facts);
    Ok(())
}

pub fn create_dir_all_owned(dir: &Path, facts: &Facts) -> Result<()> {
    let mut missing = Vec::new();
    let mut cur = Some(dir);
    while let Some(d) = cur {
        if d.exists() {
            break;
        }
        missing.push(d.to_path_buf());
        cur = d.parent();
    }
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    for d in missing {
        chown_to_target(&d, facts);
    }
    Ok(())
}

pub fn chown_to_target(path: &Path, facts: &Facts) {
    if facts.drop_to.is_none() {
        return;
    }
    use std::os::unix::ffi::OsStrExt;
    if let Ok(c) = std::ffi::CString::new(path.as_os_str().as_bytes()) {
        // SAFETY: plain syscall on a valid C string.
        unsafe {
            libc::lchown(c.as_ptr(), facts.uid, facts.gid);
        }
    }
}

/// Outcome of one unit in a run record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnitRecord {
    pub id: String,
    pub action: String,
    pub via: String,
    pub outcome: String,
    pub detail: Option<String>,
    pub seconds: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RunRecord {
    pub id: String,
    pub command: String,
    pub started: String,
    pub finished: Option<String>,
    #[serde(default)]
    pub units: Vec<UnitRecord>,
}

impl RunRecord {
    pub fn load(paths: &Paths, run_id: &str) -> Result<RunRecord> {
        let f = paths.run_dir(run_id).join("run.toml");
        let text =
            std::fs::read_to_string(&f).with_context(|| format!("reading {}", f.display()))?;
        Ok(toml::from_str(&text)?)
    }

    pub fn save(&self, paths: &Paths, facts: &Facts) -> Result<()> {
        write_atomic(
            &paths.run_dir(&self.id).join("run.toml"),
            toml::to_string_pretty(self)?.as_bytes(),
            facts,
        )
    }

    /// A record that lists every unit of `plan` as `pending`. Outcomes replace the entries as
    /// units finish, so a run that dies midway still shows (and retries) what it never reached.
    pub fn planned(plan: &crate::plan::Plan, id: &str, command: &str) -> RunRecord {
        RunRecord {
            id: id.into(),
            command: command.into(),
            started: now_rfc3339(),
            finished: None,
            units: plan
                .units
                .iter()
                .map(|u| UnitRecord {
                    id: u.id.clone(),
                    action: u.action.as_str().into(),
                    via: u.via.clone(),
                    outcome: "pending".into(),
                    detail: None,
                    seconds: 0.0,
                })
                .collect(),
        }
    }

    /// Ids that did not succeed: failed, blocked, cancelled, skipped after a stop, or never
    /// reached because envira was killed.
    pub fn unfinished(&self) -> Vec<String> {
        self.units
            .iter()
            .filter(|u| {
                matches!(
                    u.outcome.as_str(),
                    "failed" | "blocked" | "cancelled" | "skipped" | "pending"
                )
            })
            .map(|u| u.id.clone())
            .collect()
    }
}

/// Run ids sorted newest first.
pub fn list_runs(paths: &Paths) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(paths.logs_dir())
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter_map(|e| e.file_name().into_string().ok())
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v.reverse();
    v
}

/// Removes all but the newest `keep` run directories.
pub fn prune_runs(paths: &Paths, keep: usize) {
    for old in list_runs(paths).into_iter().skip(keep) {
        let _ = std::fs::remove_dir_all(paths.run_dir(&old));
    }
}

pub fn now_rfc3339() -> String {
    chrono::Local::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, false)
}

/// A timestamp id; runs started within the same second get `-02`, `-03`, ... so the ids
/// stay unique and still sort by start time.
pub fn new_run_id(paths: &Paths) -> String {
    let base = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
    std::iter::once(base.clone())
        .chain((2..).map(|n| format!("{base}-{n:02}")))
        .find(|id| !paths.run_dir(id).exists())
        .expect("unbounded candidates")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_desired() {
        let dir = tempfile::tempdir().unwrap();
        let facts = Facts::sample(dir.path());
        let paths = Paths {
            state_dir: dir.path().join("s"),
            config_dir: dir.path().join("c"),
        };
        assert!(State::load(&paths).unwrap().is_none());

        let mut st = State {
            bundles: vec!["terminal".into()],
            exclude: vec!["herdr".into()],
            ..Default::default()
        };
        st.include.push("codex".into());
        st.tools.insert(
            "bat".into(),
            ToolRecord {
                method: Some("apt".into()),
                scope: Some(Scope::System),
                version: Some("0.24.0".into()),
                installed_at: Some(now_rfc3339()),
                last_result: LastResult::Ok,
                last_error: None,
                last_run: None,
            },
        );
        st.save(&paths, &facts).unwrap();
        let back = State::load(&paths).unwrap().unwrap();
        assert!(back.is_managed("bat"));
        assert!(!back.is_managed("yazi"));

        let cat: Catalog =
            toml::from_str("[bundles.terminal]\nitems=[\"bat\",\"herdr\"]\n").unwrap();
        let d = back.desired(&cat);
        assert_eq!(d.into_iter().collect::<Vec<_>>(), vec!["bat", "codex"]);
    }

    #[test]
    fn run_ids_within_one_second_are_unique_and_ordered() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths {
            state_dir: dir.path().join("s"),
            config_dir: dir.path().join("c"),
        };
        let mut ids = Vec::new();
        for _ in 0..3 {
            let id = new_run_id(&paths);
            std::fs::create_dir_all(paths.run_dir(&id)).unwrap();
            ids.push(id);
        }
        // Possible second boundary between calls; uniqueness and order hold either way.
        let mut sorted = ids.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted, ids);
    }

    #[test]
    fn run_records_and_pruning() {
        let dir = tempfile::tempdir().unwrap();
        let facts = Facts::sample(dir.path());
        let paths = Paths {
            state_dir: dir.path().into(),
            config_dir: dir.path().into(),
        };
        for id in ["20260101-000000", "20260102-000000", "20260103-000000"] {
            let mut r = RunRecord {
                id: id.into(),
                ..Default::default()
            };
            r.units.push(UnitRecord {
                id: "gh".into(),
                action: "install".into(),
                via: "github-release".into(),
                outcome: "failed".into(),
                detail: None,
                seconds: 1.0,
            });
            r.save(&paths, &facts).unwrap();
        }
        assert_eq!(list_runs(&paths)[0], "20260103-000000");
        prune_runs(&paths, 2);
        assert_eq!(list_runs(&paths).len(), 2);
        assert_eq!(
            RunRecord::load(&paths, "20260103-000000")
                .unwrap()
                .unfinished(),
            vec!["gh"]
        );
    }
}
