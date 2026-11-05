//! Per-user machine state (`~/.local/state/envira/state.toml`) and run records.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::catalog::Catalog;
use crate::facts::Facts;

#[derive(Debug, Clone)]
pub struct Paths {
    pub state_dir: PathBuf,
    pub config_dir: PathBuf,
    pub cache_dir: PathBuf,
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
        let cache_dir = std::env::var_os("ENVIRA_CACHE_DIR")
            .map(PathBuf::from)
            .or_else(|| xdg("XDG_CACHE_HOME").map(|p| p.join("envira")))
            .unwrap_or_else(|| facts.home.join(".cache/envira"));
        Paths {
            state_dir,
            config_dir,
            cache_dir,
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
        match toml::from_str(&text) {
            Ok(st) => Ok(Some(st)),
            Err(e) => {
                // Keep the broken file for inspection and let the next start begin fresh
                // instead of failing every command until someone edits it by hand.
                let aside = f.with_file_name(format!(
                    "state.toml.broken-{}",
                    chrono::Local::now().format("%Y%m%d-%H%M%S")
                ));
                std::fs::rename(&f, &aside)
                    .with_context(|| format!("{} is corrupt: {e}", f.display()))?;
                bail!(
                    "{} is corrupt and envira moved it to {}; run envira again to start with \
                     a fresh state (bundles need to be selected again): {e}",
                    f.display(),
                    aside.display()
                )
            }
        }
    }

    pub fn save(&self, paths: &Paths, facts: &Facts) -> Result<()> {
        let text = toml::to_string_pretty(self)?;
        write_atomic(&paths.state_file(), text.as_bytes(), facts)
    }

    /// Saves the results of a run. The selection (bundles, include, exclude) is taken from disk
    /// first: a run never changes it, while the TUI or `install <bundle>` may have meanwhile.
    pub fn save_run(&mut self, paths: &Paths, facts: &Facts) -> Result<()> {
        if let Ok(Some(disk)) = State::load(paths) {
            self.bundles = disk.bundles;
            self.include = disk.include;
            self.exclude = disk.exclude;
        }
        self.save(paths, facts)
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
    crate::files::replace(path, data, None)
        .with_context(|| format!("writing {}", path.display()))?;
    chown_to_target(path, facts);
    Ok(())
}

/// Exclusive lock on the state directory for the length of a run, so two envira processes (a
/// TUI and a CLI `install`, say) never install at once or overwrite each other's state. Released
/// when dropped or when the process dies. `envira fs` and `fetch-release`, which steps call,
/// never take it.
#[derive(Debug)]
pub struct RunLock {
    _file: std::fs::File,
    _exe: Option<std::fs::File>,
}

impl RunLock {
    /// For a run: also holds the envira binary shared, so `self-update` (exclusive) cannot swap
    /// it while any run uses it, whatever user or state dir that run has.
    pub fn acquire(paths: &Paths, facts: &Facts) -> Result<RunLock> {
        Self::take(paths, facts, false)
    }

    /// For `self-update`: the binary exclusively.
    pub fn acquire_exclusive(paths: &Paths, facts: &Facts) -> Result<RunLock> {
        Self::take(paths, facts, true)
    }

    fn take(paths: &Paths, facts: &Facts, exclusive: bool) -> Result<RunLock> {
        use std::os::fd::AsRawFd;
        create_dir_all_owned(&paths.state_dir, facts)?;
        let path = paths.state_dir.join("lock");
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .with_context(|| format!("opening {}", path.display()))?;
        chown_to_target(&path, facts);
        // SAFETY: plain syscall on a file descriptor owned by `file`.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            bail!(
                "another envira run is in progress (lock {}); wait for it to finish",
                path.display()
            );
        }
        // The running binary itself (/proc/self/exe also opens it after it was replaced).
        // Best effort: a binary that cannot be opened is not locked.
        let exe = std::fs::File::open("/proc/self/exe")
            .or_else(|_| std::env::current_exe().and_then(std::fs::File::open))
            .ok();
        if let Some(f) = &exe {
            let op = if exclusive {
                libc::LOCK_EX
            } else {
                libc::LOCK_SH
            };
            // SAFETY: plain syscall on a file descriptor owned by `f`.
            if unsafe { libc::flock(f.as_raw_fd(), op | libc::LOCK_NB) } != 0 {
                if exclusive {
                    bail!("an envira run is using this binary; update it when that run is done");
                }
                bail!("envira is being updated; run it again in a moment");
            }
        }
        Ok(RunLock {
            _file: file,
            _exe: exe,
        })
    }
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

/// Opens a file of a run dir (a unit's log, a progress file) to write: an existing regular file
/// is appended to or emptied, while a link, FIFO or anything else already at that name is an
/// error, not followed or waited on. Under `sudo envira` the file belongs to the user.
pub fn open_run_file(path: &Path, append: bool, facts: &Facts) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut o = std::fs::OpenOptions::new();
    o.write(true)
        .create(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    if append {
        o.append(true);
    } else {
        o.truncate(true);
    }
    let f = o.open(path)?;
    if !f.metadata()?.is_file() {
        return Err(std::io::Error::other(format!(
            "{} is not a regular file",
            path.display()
        )));
    }
    if facts.drop_to.is_some() {
        std::os::unix::fs::fchown(&f, Some(facts.uid), Some(facts.gid))?;
    }
    Ok(f)
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
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UnitRecord {
    pub id: String,
    pub action: String,
    pub via: String,
    pub outcome: String,
    pub detail: Option<String>,
    /// What the failure output suggests, e.g. `(network): ...`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    pub seconds: f64,
    /// Labels of the steps a unit that did not succeed got through, in order: a retried revert
    /// does not run them again.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub done_steps: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RunRecord {
    pub id: String,
    pub command: String,
    /// envira version and machine summary, for reading old logs.
    #[serde(default)]
    pub envira: String,
    #[serde(default)]
    pub machine: String,
    pub started: String,
    pub finished: Option<String>,
    #[serde(default)]
    pub units: Vec<UnitRecord>,
    /// Set when saving the run failed: why the record on disk (and the state) is behind this
    /// one, so `retry` may not know what the run did.
    #[serde(skip)]
    pub unrecorded: Option<String>,
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
            envira: env!("CARGO_PKG_VERSION").into(),
            machine: String::new(),
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
                    // What an earlier run got through: still known if this one dies first.
                    done_steps: u.resumed.clone(),
                    ..Default::default()
                })
                .collect(),
            unrecorded: None,
        }
    }

    /// Ids that did not succeed: failed, blocked, cancelled, skipped after a stop, or never
    /// reached because envira was killed.
    pub fn unfinished(&self) -> Vec<String> {
        self.unfinished_units().map(|u| u.id.clone()).collect()
    }

    pub fn unfinished_units(&self) -> impl Iterator<Item = &UnitRecord> {
        self.units.iter().filter(|u| {
            matches!(
                u.outcome.as_str(),
                "failed" | "blocked" | "cancelled" | "skipped" | "pending"
            )
        })
    }
}

/// Where a revert notes each step it gets through (see `exec::Progress`).
pub fn progress_file(run_dir: &Path, id: &str) -> PathBuf {
    run_dir.join(format!("{id}.steps"))
}

/// The steps noted in a progress file, in order.
pub fn read_progress(run_dir: &Path, id: &str) -> Vec<String> {
    std::fs::read_to_string(progress_file(run_dir, id))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

/// The last `n` lines of a log, reading at most the last `max` bytes of it: a long build log
/// is never loaded whole to show its end.
pub fn tail_lines(path: &Path, n: usize, max: u64) -> std::io::Result<Vec<String>> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path)?;
    let len = f.metadata()?.len();
    let start = len.saturating_sub(max);
    f.seek(SeekFrom::Start(start))?;
    let mut buf = Vec::new();
    f.take(max).read_to_end(&mut buf)?;
    let text = String::from_utf8_lossy(&buf);
    let mut lines: Vec<&str> = text.lines().collect();
    if start > 0 && !lines.is_empty() {
        // Starts mid-line.
        lines.remove(0);
    }
    Ok(lines[lines.len().saturating_sub(n)..]
        .iter()
        .map(|l| l.to_string())
        .collect())
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
    fn run_files_are_not_opened_through_what_is_already_there() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let facts = Facts::sample(dir.path());
        let other = dir.path().join("other");
        std::fs::write(&other, "keep").unwrap();
        let link = dir.path().join("u.log");
        std::os::unix::fs::symlink(&other, &link).unwrap();
        assert!(open_run_file(&link, false, &facts).is_err());
        assert!(open_run_file(&link, true, &facts).is_err());
        assert_eq!(std::fs::read_to_string(&other).unwrap(), "keep");
        // A FIFO nobody reads: an error at once, not a wait.
        let fifo = dir.path().join("f.steps");
        let c = std::ffi::CString::new(fifo.to_str().unwrap()).unwrap();
        // SAFETY: plain syscall on a valid C string.
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
        assert!(open_run_file(&fifo, true, &facts).is_err());
        // A regular file: emptied for a log, appended to for progress.
        let f = dir.path().join("x.log");
        std::fs::write(&f, "old\n").unwrap();
        open_run_file(&f, true, &facts)
            .unwrap()
            .write_all(b"more\n")
            .unwrap();
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "old\nmore\n");
        drop(open_run_file(&f, false, &facts).unwrap());
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "");
    }

    #[test]
    fn tails_read_only_the_end() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("x.log");
        let text: String = (0..10_000).map(|i| format!("line {i}\n")).collect();
        std::fs::write(&f, &text).unwrap();
        assert_eq!(
            tail_lines(&f, 2, 1 << 20).unwrap(),
            ["line 9998", "line 9999"]
        );
        // 100 bytes from the end: the cut first line is dropped.
        let t = tail_lines(&f, 1000, 100).unwrap();
        assert_eq!(t.last().unwrap(), "line 9999");
        assert!(t.iter().all(|l| l.starts_with("line ")) && t.len() < 12);
        assert_eq!(tail_lines(&f, 3, 1 << 20).unwrap().len(), 3);
        assert!(tail_lines(&d.path().join("none"), 3, 100).is_err());
    }

    #[test]
    fn round_trip_and_desired() {
        let dir = tempfile::tempdir().unwrap();
        let facts = Facts::sample(dir.path());
        let paths = Paths {
            state_dir: dir.path().join("s"),
            config_dir: dir.path().join("c"),
            cache_dir: dir.path().join("k"),
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
    fn run_saves_keep_selection_changes_made_meanwhile() {
        let dir = tempfile::tempdir().unwrap();
        let facts = Facts::sample(dir.path());
        let paths = Paths {
            state_dir: dir.path().join("s"),
            config_dir: dir.path().join("c"),
            cache_dir: dir.path().join("k"),
        };
        let mut run = State {
            bundles: vec!["base".into()],
            ..Default::default()
        };
        run.save(&paths, &facts).unwrap();
        // Another envira subscribes a bundle while the run is going.
        let mut other = State::load(&paths).unwrap().unwrap();
        other.bundles.push("terminal".into());
        other.save(&paths, &facts).unwrap();
        run.last_run = Some("r1".into());
        run.save_run(&paths, &facts).unwrap();
        let disk = State::load(&paths).unwrap().unwrap();
        assert_eq!(disk.bundles, vec!["base", "terminal"]);
        assert_eq!(disk.last_run.as_deref(), Some("r1"));
    }

    #[test]
    fn corrupt_state_is_moved_aside() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths {
            state_dir: dir.path().join("s"),
            config_dir: dir.path().join("c"),
            cache_dir: dir.path().join("k"),
        };
        std::fs::create_dir_all(&paths.state_dir).unwrap();
        std::fs::write(paths.state_file(), "bundles = [\"base\"\ntools = {").unwrap();
        let err = format!("{:#}", State::load(&paths).unwrap_err());
        assert!(err.contains("moved it to"), "{err}");
        assert!(!paths.state_file().exists());
        let kept: Vec<_> = std::fs::read_dir(&paths.state_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().into_string().unwrap())
            .filter(|n| n.starts_with("state.toml.broken-"))
            .collect();
        assert_eq!(kept.len(), 1, "{kept:?}");
        // The next start begins fresh.
        assert!(State::load(&paths).unwrap().is_none());
    }

    #[test]
    fn only_one_run_holds_the_lock() {
        let dir = tempfile::tempdir().unwrap();
        let facts = Facts::sample(dir.path());
        let paths = Paths {
            state_dir: dir.path().join("s"),
            config_dir: dir.path().join("c"),
            cache_dir: dir.path().join("k"),
        };
        let held = RunLock::acquire(&paths, &facts).unwrap();
        let err = format!("{:#}", RunLock::acquire(&paths, &facts).unwrap_err());
        assert!(err.contains("another envira run"), "{err}");
        // self-update with another state dir: the binary is still in use.
        let other = Paths {
            state_dir: dir.path().join("s2"),
            ..paths.clone()
        };
        let err = format!(
            "{:#}",
            RunLock::acquire_exclusive(&other, &facts).unwrap_err()
        );
        assert!(err.contains("using this binary"), "{err}");
        drop(held);
        // Tests running in parallel fork children; between fork and exec a child briefly holds
        // a copy of the descriptor (O_CLOEXEC closes it only at exec), so allow a short wait.
        let free = (0..50).any(|_| {
            RunLock::acquire(&paths, &facts).is_ok()
                || (
                    std::thread::sleep(std::time::Duration::from_millis(10)),
                    false,
                )
                    .1
        });
        assert!(free, "lock still held after drop");
    }

    #[test]
    fn run_ids_within_one_second_are_unique_and_ordered() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths {
            state_dir: dir.path().join("s"),
            config_dir: dir.path().join("c"),
            cache_dir: dir.path().join("k"),
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
            cache_dir: dir.path().into(),
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
                seconds: 1.0,
                ..Default::default()
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
