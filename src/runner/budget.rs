//! Memory budget of envira's own commands (detection, checks, `installed` and latest-version
//! queries). A command starts only when envira, the commands already running and an estimate
//! of this one fit in the budget; otherwise it waits for one to end. A scan used to start eight
//! at once whatever they were, and several are Node or Bun programs at 100-160 MiB each
//! (`npm ls -g`, `opencode --version`, `agy --version`): the tree took ~300 MiB.
//!
//! The budget follows the memory available (`limit_for`): with 1 GiB or more, half of it, which
//! never holds a scan back; with less, 200 MB.
//!
//! A command never seen before is estimated from the peak of another command of its program
//! (`apt-cache policy curl` from `apt-cache policy git`: 92 MiB, the package cache it maps),
//! else from the programs it names: a script by its interpreter, a native program by its size
//! (a single-file Bun or Electron build maps most of itself even for `--version`). A program
//! never measured runs one command at a time until one has ended. Once a command has run, its
//! own peak is used, and kept in the state directory for the next envira. While one runs, its
//! process tree is sampled and its reservation grows with what it really takes.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use serde::{Deserialize, Serialize};
use tokio::sync::Notify;

use crate::facts::Facts;

const MIB: u64 = 1 << 20;

/// What envira and its commands may take at once during a scan when memory is short: 200 MB
/// (190.7 MiB), less ~15 MiB for what sampling sees late (a command growing between two
/// samples, envira's own buffers).
pub const SCAN_MEMORY: u64 = 176 * MIB;

/// Memory available from which a scan is not held back.
const PLENTY: u64 = 1 << 30;

/// How often the memory available is read again (when no command runs).
const RECHECK: Duration = Duration::from_secs(1);

/// The budget for `available` bytes (`MemAvailable`, or what a cgroup limit leaves; not the
/// machine's total): from `PLENTY` up, half of it, where a scan (~300 MiB at most) runs as
/// fast as it can; below, `SCAN_MEMORY`, and at most three quarters of what is left. Unknown
/// (no /proc): `SCAN_MEMORY`.
pub fn limit_for(available: Option<u64>) -> u64 {
    match available {
        Some(a) if a >= PLENTY => a / 2,
        Some(a) => SCAN_MEMORY.min(a / 4 * 3),
        None => SCAN_MEMORY,
    }
}

/// bash itself, around every command.
const SHELL: u64 = 4 * MIB;
/// A Node, Bun or Deno program: 40-160 MiB measured for `--version` and `npm ls -g`.
const JS_RUNTIME: u64 = 160 * MIB;
/// A Python (or other interpreter's) program: 17-57 MiB measured.
const SCRIPT: u64 = 64 * MIB;
/// A program whose file cannot be judged (a path from a variable, an unknown format).
const UNKNOWN: u64 = 64 * MIB;
/// Bounds of a native program's estimate, which follows its size.
const NATIVE_MIN: u64 = 8 * MIB;
const NATIVE_MAX: u64 = 160 * MIB;

pub struct Budget {
    /// A fixed limit (tests); else it follows the memory available.
    fixed: Option<u64>,
    /// Where the peaks are kept between runs.
    file: Option<PathBuf>,
    state: Mutex<State>,
    freed: Notify,
}

#[derive(Default)]
struct State {
    limit: u64,
    /// When `limit` was taken from the memory available.
    checked: Option<Instant>,
    next: u64,
    /// Reservation of each running command: its estimate, or more once it was seen to take more.
    running: HashMap<u64, u64>,
    /// Highest peak of each command seen by this envira.
    learned: HashMap<String, u64>,
    /// Peaks an earlier envira saw.
    saved: BTreeMap<String, u64>,
    /// Highest peak of a command of each program (`main_program`), now or earlier.
    programs: HashMap<String, u64>,
    /// Programs running for the first time, before any of their commands was measured.
    probing: HashSet<String>,
}

/// The file of peaks.
#[derive(Default, Serialize, Deserialize)]
struct Peaks {
    #[serde(default)]
    peaks: BTreeMap<String, u64>,
}

/// Commands kept in the file at most; past that, only those of the last run are.
const MAX_SAVED: usize = 1024;

/// The peak of a command that ended before it could be measured (`node --version`, 16 MiB for
/// a few ms). It is estimated from its programs, but at most `FAST`: so short a run touches
/// little. Not its first sample's peak, taken before it even started its program.
const UNMEASURED: u64 = 0;
const FAST: u64 = 32 * MIB;

/// A running command's place in the budget, given back when dropped.
pub struct Ticket {
    budget: Arc<Budget>,
    id: u64,
    key: String,
    program: Option<String>,
    /// The first command of a program not measured yet.
    probing: bool,
    peak: u64,
    ended: bool,
}

/// What is known of a command's peak.
#[derive(Debug, PartialEq)]
enum Known {
    /// Its own peak.
    Measured(u64),
    /// It ran, too briefly to be measured.
    Fast,
    /// The peak of another command of its program.
    Program(u64),
    Nothing,
}

impl Budget {
    /// A budget that follows the memory available (`limit_for`), with the peaks kept in
    /// `file`.
    pub fn for_scans(file: PathBuf) -> Budget {
        let mut b = Budget::new(0);
        b.fixed = None;
        let s = b.state.get_mut().unwrap();
        s.limit = limit_for(super::available_memory());
        s.checked = Some(Instant::now());
        s.saved = std::fs::read_to_string(&file)
            .ok()
            .and_then(|t| toml::from_str::<Peaks>(&t).ok())
            .unwrap_or_default()
            .peaks;
        for (cmd, &p) in &s.saved {
            if let (Some(prog), true) = (main_program(cmd), p != UNMEASURED) {
                let e = s.programs.entry(prog).or_default();
                *e = (*e).max(p);
            }
        }
        b.file = Some(file);
        b
    }

    pub fn new(limit: u64) -> Budget {
        Budget {
            fixed: Some(limit),
            file: None,
            state: Mutex::new(State {
                limit,
                ..State::default()
            }),
            freed: Notify::new(),
        }
    }

    /// Estimated peak of `cmd` (as `admit` takes it).
    #[cfg(test)]
    pub fn estimate(&self, cmd: &str, path: &str, home: &Path) -> u64 {
        let s = self.state.lock().unwrap();
        let known = s.known(cmd, main_program(cmd).as_deref());
        reserve(&known, || estimate(cmd, path, home))
    }

    /// Keeps the peaks for the next envira when they changed. A cache: callers ignore errors.
    pub fn save(&self, facts: &Facts) -> Result<()> {
        let (Some(file), Some(text)) = (&self.file, self.render()) else {
            return Ok(());
        };
        crate::state::write_cache(file, text.as_bytes(), facts)
    }

    /// The file's text: the earlier peaks with this run's. The higher of the two is kept, but a
    /// peak under half the earlier one takes three quarters of it: a spike fades over runs.
    /// `None` unless that matters (peaks vary by a third from one run to the next): a command
    /// is new, outgrew the headroom of its estimate, or fades.
    fn render(&self) -> Option<String> {
        let s = self.state.lock().unwrap();
        let mut peaks = if s.saved.len() + s.learned.len() > MAX_SAVED {
            BTreeMap::new()
        } else {
            s.saved.clone()
        };
        let mut changed = false;
        for (cmd, &p) in &s.learned {
            let before = s.saved.get(cmd).copied();
            let now = match before {
                None => p,
                // Nothing new about it.
                Some(b) if p == UNMEASURED => b,
                // Far lower: an earlier spike fades.
                Some(b) if p < b / 2 => b / 4 * 3,
                Some(b) => p.max(b),
            };
            changed |= match before {
                None => true,
                Some(UNMEASURED) => now > FAST,
                Some(b) => now > headroom(b) || now < b,
            };
            peaks.insert(cmd.clone(), now);
        }
        if !changed {
            return None;
        }
        let body = toml::to_string(&Peaks { peaks }).ok()?;
        Some(format!(
            "# Peak memory (bytes) of envira's own commands when they last ran: how many run at\n\
             # once. A cache; deleting it is harmless.\n{body}"
        ))
    }

    /// Waits until `cmd` (run by bash with `path`, from `home`) fits, and reserves its estimate.
    /// One always starts when nothing else runs, however big. The estimate is taken again after
    /// each wait: a command of the same program may have been measured meanwhile.
    pub async fn admit(self: &Arc<Self>, cmd: &str, path: &str, home: &Path) -> Ticket {
        let program = main_program(cmd);
        let guess = estimate(cmd, path, home);
        loop {
            let freed = self.freed.notified();
            tokio::pin!(freed);
            freed.as_mut().enable();
            {
                let mut s = self.state.lock().unwrap();
                // Memory freed or taken by others since: read again while nothing of ours runs
                // (our commands would count against themselves).
                if self.fixed.is_none()
                    && s.running.is_empty()
                    && s.checked.is_none_or(|t| t.elapsed() >= RECHECK)
                {
                    s.limit = limit_for(super::available_memory());
                    s.checked = Some(Instant::now());
                }
                let known = s.known(cmd, program.as_deref());
                let est = reserve(&known, || guess);
                // A program never measured: one of its commands at a time until one has ended.
                let first = known == Known::Nothing && program.is_some();
                let held = first && program.as_ref().is_some_and(|p| s.probing.contains(p));
                let used: u64 = s.running.values().sum();
                if !held && (s.running.is_empty() || own_rss() + used + est <= s.limit) {
                    let id = s.next;
                    s.next += 1;
                    s.running.insert(id, est);
                    if first && let Some(p) = &program {
                        s.probing.insert(p.clone());
                    }
                    return Ticket {
                        budget: self.clone(),
                        id,
                        key: cmd.to_string(),
                        program,
                        probing: first,
                        peak: 0,
                        ended: false,
                    };
                }
            }
            freed.await;
        }
    }

    #[cfg(test)]
    pub fn learned(&self, cmd: &str) -> Option<u64> {
        self.state.lock().unwrap().learned.get(cmd).copied()
    }
}

impl State {
    fn known(&self, cmd: &str, program: Option<&str>) -> Known {
        let own = match (self.learned.get(cmd), self.saved.get(cmd)) {
            (Some(&p), _) if p != UNMEASURED => Some(p),
            (_, Some(&p)) => Some(p),
            (p, None) => p.copied(),
        };
        match own {
            Some(UNMEASURED) => Known::Fast,
            Some(p) => Known::Measured(p),
            None => match program.and_then(|p| self.programs.get(p)) {
                Some(&p) => Known::Program(p),
                None => Known::Nothing,
            },
        }
    }
}

/// What to reserve for a command: its peak with some headroom; else `guess` (its programs'
/// estimate), or the peak of another command of its program when that is more; at most `FAST`
/// for a command that ran too briefly to be measured.
fn reserve(known: &Known, guess: impl FnOnce() -> u64) -> u64 {
    match *known {
        Known::Measured(p) => headroom(p),
        Known::Program(p) => headroom(p).max(guess()),
        Known::Fast => guess().min(FAST),
        Known::Nothing => guess(),
    }
}

/// A peak with the headroom its estimate gets.
fn headroom(peak: u64) -> u64 {
    peak + peak / 4 + 2 * MIB
}

/// The one program a command runs (besides builtins), whichever words it takes: the program
/// whose peak stands for other commands of it. `None` for a pipeline of several.
fn main_program(cmd: &str) -> Option<String> {
    let mut found: Option<String> = None;
    for p in programs(cmd) {
        if BUILTINS.contains(&p.as_str()) {
            continue;
        }
        match &found {
            Some(f) if *f != p => return None,
            _ => found = Some(p),
        }
    }
    found
}

impl Ticket {
    /// Records what the command's processes take now (`tree_hwm`); the reservation grows when
    /// that is more than the estimate.
    pub fn seen(&mut self, bytes: u64) {
        if bytes <= self.peak {
            return;
        }
        self.peak = bytes;
        if let Some(r) = self.budget.state.lock().unwrap().running.get_mut(&self.id) {
            *r = (*r).max(bytes);
        }
    }

    /// The command has ended (not timed out or dropped).
    pub fn ended(&mut self) {
        self.ended = true;
    }
}

impl Drop for Ticket {
    fn drop(&mut self) {
        {
            let mut s = self.budget.state.lock().unwrap();
            s.running.remove(&self.id);
            if let Some(prog) = self.program.take() {
                if self.peak > 0 {
                    let p = s.programs.entry(prog.clone()).or_default();
                    *p = (*p).max(self.peak);
                }
                if self.probing {
                    s.probing.remove(&prog);
                }
            }
            if self.peak > 0 || (self.ended && procfs()) {
                let p = s.learned.entry(std::mem::take(&mut self.key)).or_default();
                *p = (*p).max(self.peak);
            }
        }
        self.budget.freed.notify_waiters();
    }
}

/// Whether processes can be measured here (Linux).
fn procfs() -> bool {
    static PROCFS: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *PROCFS.get_or_init(|| Path::new("/proc/self/status").exists())
}

/// envira's own resident memory (0 where there is no /proc).
fn own_rss() -> u64 {
    std::fs::read_to_string("/proc/self/statm")
        .ok()
        .and_then(|s| s.split_whitespace().nth(1)?.parse::<u64>().ok())
        .map_or(0, |pages| pages * page_size())
}

fn page_size() -> u64 {
    // SAFETY: sysconf has no preconditions.
    let p = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    if p > 0 { p as u64 } else { 4096 }
}

/// Peak resident memory (`VmHWM`) summed over `pid` and its live descendants, found through
/// `/proc/<pid>/task/<tid>/children` (only `pid` where the kernel does not list children).
/// `VmHWM` only grows, so a spike between two samples still counts.
pub fn tree_hwm(pid: u32) -> u64 {
    let mut total = 0;
    let mut todo = vec![pid];
    let mut seen = 0;
    while let Some(p) = todo.pop() {
        seen += 1;
        if seen > 256 {
            break;
        }
        let Ok(status) = std::fs::read_to_string(format!("/proc/{p}/status")) else {
            continue;
        };
        total += status
            .lines()
            .find_map(|l| l.strip_prefix("VmHWM:"))
            .and_then(|v| v.split_whitespace().next()?.parse::<u64>().ok())
            .map_or(0, |kb| kb * 1024);
        let Ok(tasks) = std::fs::read_dir(format!("/proc/{p}/task")) else {
            continue;
        };
        for t in tasks.flatten() {
            let Ok(kids) = std::fs::read_to_string(t.path().join("children")) else {
                continue;
            };
            todo.extend(
                kids.split_whitespace()
                    .filter_map(|k| k.parse::<u32>().ok()),
            );
        }
    }
    total
}

/// Estimated peak of `cmd` run by bash with `path`, from the programs it names.
pub fn estimate(cmd: &str, path: &str, home: &Path) -> u64 {
    SHELL
        + programs(cmd)
            .iter()
            .map(|p| program_estimate(p, path, home))
            .sum::<u64>()
}

/// Shell words that run no program of their own.
const BUILTINS: &[&str] = &[
    "!", "[", "[[", ":", "{", "}", "case", "esac", "in", "if", "then", "else", "elif", "fi", "for",
    "while", "until", "do", "done", "command", "test", "true", "false", "echo", "printf", "type",
    "exit", "return", "cd", "export", "local", "read", "set", "shift", "unset", "hash",
];

/// Words after which the next word is still the command's program.
const PREFIXES: &[&str] = &[
    "!", "{", "if", "then", "else", "elif", "while", "until", "do", "exec", "env", "time",
];

fn program_estimate(word: &str, path: &str, home: &Path) -> u64 {
    if BUILTINS.contains(&word) {
        return 0;
    }
    let word = expand_home(word, home);
    if word.contains('$') || word.contains('`') {
        return UNKNOWN;
    }
    let Some(file) = resolve(&word, path, home) else {
        return 0; // not installed: bash reports it and exits
    };
    let name = file.file_name().and_then(|n| n.to_str()).unwrap_or("");
    if is_js(name) {
        return JS_RUNTIME;
    }
    if is_interpreter(name) {
        return SCRIPT;
    }
    let mut head = [0u8; 256];
    let n = std::fs::File::open(&file)
        .and_then(|mut f| f.read(&mut head))
        .unwrap_or(0);
    let head = &head[..n];
    if let Some(line) = head.strip_prefix(b"#!") {
        let line = line.split(|&b| b == b'\n').next().unwrap_or(line);
        let line = String::from_utf8_lossy(line);
        let js = line
            .split(|c: char| c.is_whitespace() || c == '/')
            .any(is_js);
        return if js { JS_RUNTIME } else { SCRIPT };
    }
    if head.starts_with(b"\x7fELF") {
        let size = std::fs::metadata(&file).map_or(0, |m| m.len());
        return size.clamp(NATIVE_MIN, NATIVE_MAX);
    }
    UNKNOWN
}

fn is_js(name: &str) -> bool {
    matches!(name, "node" | "nodejs" | "bun" | "bunx" | "deno")
}

fn is_interpreter(name: &str) -> bool {
    name.starts_with("python") || matches!(name, "perl" | "ruby" | "php")
}

fn expand_home(word: &str, home: &Path) -> String {
    let h = home.display().to_string();
    let w = match word.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => format!("{h}{rest}"),
        _ => word.to_string(),
    };
    w.replace("${HOME}", &h).replace("$HOME", &h)
}

/// The file bash would run for `word`: a path as given (relative to `home`, the probes' working
/// directory), else the first match on `path` or in the sbin directories some checks add.
fn resolve(word: &str, path: &str, home: &Path) -> Option<PathBuf> {
    let runnable = |p: &Path| p.is_file();
    if word.contains('/') {
        let p = home.join(word);
        return runnable(&p).then_some(p);
    }
    path.split(':')
        .chain(["/usr/sbin", "/sbin"])
        .filter(|d| !d.is_empty())
        .map(|d| Path::new(d).join(word))
        .find(|p| runnable(p))
}

/// The program of each simple command in `cmd` (after `VAR=value` assignments and words such
/// as `exec` or `then`), quotes removed. A rough reading of shell syntax, enough for envira's
/// one-line checks: `;` `&` `|` `(` `)` and newlines end a simple command, also within `"$( )"`.
pub fn programs(cmd: &str) -> Vec<String> {
    let mut p = Parse {
        head: true,
        ..Parse::default()
    };
    let mut it = cmd.chars().peekable();
    while let Some(c) = it.next() {
        if p.single {
            if c == '\'' {
                p.single = false;
            } else {
                p.word.push(c);
            }
            continue;
        }
        match c {
            '\\' => {
                if let Some(n) = it.next() {
                    p.word.push(n);
                    p.started = true;
                }
            }
            '\'' if !p.double => {
                p.single = true;
                p.started = true;
            }
            '"' => {
                p.double = !p.double;
                p.started = true;
            }
            '$' if it.peek() == Some(&'(') => {
                it.next();
                p.end_word();
                p.nested.push(p.double);
                p.double = false;
                p.head = true;
            }
            '`' => {
                p.end_word();
                p.head = true;
            }
            ')' if !p.double && !p.nested.is_empty() => {
                p.end_word();
                p.double = p.nested.pop().unwrap_or(false);
                p.head = false;
            }
            _ if p.double => {
                p.word.push(c);
                p.started = true;
            }
            ';' | '&' | '|' | '(' | ')' | '\n' => {
                p.end_word();
                p.head = true;
            }
            c if c.is_whitespace() => p.end_word(),
            c => {
                p.word.push(c);
                p.started = true;
            }
        }
    }
    p.end_word();
    p.out
}

#[derive(Default)]
struct Parse {
    out: Vec<String>,
    word: String,
    /// `word` has begun (it may be an empty `""`).
    started: bool,
    /// The next word is a command's program.
    head: bool,
    /// Within '…' / "…".
    single: bool,
    double: bool,
    /// The quoting each open `$(` will return to.
    nested: Vec<bool>,
}

impl Parse {
    fn end_word(&mut self) {
        if !self.started && self.word.is_empty() {
            return;
        }
        let w = std::mem::take(&mut self.word);
        self.started = false;
        if !self.head {
            return;
        }
        let assignment = w.split_once('=').is_some_and(|(k, _)| {
            !k.is_empty() && k.chars().all(|c| c == '_' || c.is_ascii_alphanumeric())
        });
        if assignment || PREFIXES.contains(&w.as_str()) {
            return;
        }
        self.out.push(w);
        self.head = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn programs_of_one_line_checks() {
        let p = |c: &str| programs(c);
        assert_eq!(p("agy --version"), ["agy"]);
        assert_eq!(p("command -v opencode"), ["command"]);
        assert_eq!(
            p(r#""$HOME/miniconda3/bin/jupyter-lab" --version"#),
            ["$HOME/miniconda3/bin/jupyter-lab"]
        );
        assert_eq!(
            p(r#"PATH="$PATH:/sbin:/usr/sbin" ldconfig -p | grep -F libfuse.so.2 >/dev/null"#),
            ["ldconfig", "grep"]
        );
        assert_eq!(
            p("gcc --version && make --version >/dev/null && perl -v >/dev/null"),
            ["gcc", "make", "perl"]
        );
        assert_eq!(
            p("(screen -v || true) 2>&1 | grep 'Screen version'")[..3],
            ["screen", "true", "2>"]
        );
        assert_eq!(
            p(r#"case "$(getent passwd user | cut -d: -f7)" in */zsh) true ;; *) false ;; esac"#),
            ["case", "getent", "cut", "true", "*", "false", "esac"]
        );
        assert_eq!(
            p(r#"p=$(command -v pm2) && p=$(readlink -f "$p") && grep -m1 '"version"' "$p""#),
            ["command", "readlink", "grep"]
        );
        assert_eq!(
            p("if test -x a; then exec b --version; fi"),
            ["test", "b", "fi"]
        );
        assert_eq!(p(r"echo 'a | b' \| c"), ["echo"]);
    }

    fn file(dir: &Path, name: &str, head: &[u8], len: u64) {
        let p = dir.join(name);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        let f = std::fs::File::create(&p).unwrap();
        std::io::Write::write_all(&mut &f, head).unwrap();
        f.set_len(len.max(head.len() as u64)).unwrap();
    }

    #[test]
    fn programs_are_judged_from_their_files() {
        let t = tempfile::tempdir().unwrap();
        let bin = t.path().join("bin");
        file(&bin, "jscli", b"#!/usr/bin/env node\nrequire('x')\n", 0);
        file(&bin, "bunx-cli", b"#!/home/user/.bun/bin/bun\n", 0);
        file(&bin, "pycli", b"#!/usr/bin/python3\nimport x\n", 0);
        file(&bin, "shcli", b"#!/bin/sh\nexec x\n", 0);
        file(&bin, "node", b"\x7fELF", 1 << 20);
        file(&bin, "big", b"\x7fELF", 100 * MIB);
        file(&bin, "huge", b"\x7fELF", 300 * MIB);
        file(&bin, "tiny", b"\x7fELF", 1024);
        file(&bin, "data", b"PK\x03\x04", 1024);
        file(t.path(), "opt/tool", b"#!/usr/bin/env python3\n", 0);
        let path = bin.display().to_string();
        let e = |c: &str| (estimate(c, &path, t.path()) - SHELL) / MIB;
        assert_eq!(e("jscli --version"), 160);
        assert_eq!(e("bunx-cli --version"), 160);
        assert_eq!(e("node --version"), 160, "by name, whatever its size");
        assert_eq!(e("pycli --version"), 64);
        assert_eq!(e("shcli --version"), 64);
        assert_eq!(e("big --version"), 100);
        assert_eq!(e("huge --version"), 160);
        assert_eq!(e("tiny --version"), 8);
        assert_eq!(e("data --version"), 64);
        assert_eq!(e("missing --version"), 0, "not installed: bash alone");
        assert_eq!(e("command -v jscli || command -v huge"), 0);
        assert_eq!(e(r#""$HOME/opt/tool" --version"#), 64);
        assert_eq!(e("~/opt/tool --version"), 64);
        assert_eq!(e("big --version && tiny -v | huge"), 100 + 8 + 160);
        assert_eq!(e(r#"case "$(jscli x | tiny)" in *) true ;; esac"#), 160 + 8);
        assert_eq!(e(r#""$c" --version"#), 64, "a program from a variable");
    }

    const ROOT: &str = "/";

    /// Admits `cmd`, whose programs are not installed: estimated at `SHELL`.
    async fn admit(b: &Arc<Budget>, cmd: &str) -> Ticket {
        b.admit(cmd, "", Path::new(ROOT)).await
    }

    async fn pending(f: impl std::future::Future) -> bool {
        tokio::time::timeout(Duration::from_millis(100), f)
            .await
            .is_err()
    }

    #[tokio::test]
    async fn a_command_waits_while_the_budget_is_taken() {
        let b = Arc::new(Budget::new(own_rss() + 100 * MIB));
        let mut first = admit(&b, "first").await;
        let second = admit(&b, "second").await;
        drop(second);
        // The first takes more than it was estimated at: the second no longer fits.
        first.seen(150 * MIB);
        let waiting = admit(&b, "second");
        tokio::pin!(waiting);
        assert!(
            pending(&mut waiting).await,
            "admitted while the budget was taken"
        );
        drop(first);
        let _second = tokio::time::timeout(Duration::from_secs(5), waiting)
            .await
            .expect("admitted once the first ended");
        assert_eq!(b.learned("first"), Some(150 * MIB));
        assert_eq!(
            b.estimate("first", "", Path::new(ROOT)),
            150 * MIB + 150 * MIB / 4 + 2 * MIB
        );
    }

    #[test]
    fn the_budget_follows_the_memory_available() {
        const GIB: u64 = 1 << 30;
        // Short of memory: 200 MB, and no more than three quarters of what is left.
        assert_eq!(limit_for(Some(800 * MIB)), SCAN_MEMORY);
        assert_eq!(limit_for(Some(500 * MIB)), SCAN_MEMORY);
        assert_eq!(limit_for(Some(200 * MIB)), 150 * MIB);
        // Plenty: a scan is not held back.
        assert_eq!(limit_for(Some(GIB)), GIB / 2);
        assert_eq!(limit_for(Some(3 * GIB)), 3 * GIB / 2);
        // Unknown: careful.
        assert_eq!(limit_for(None), SCAN_MEMORY);
    }

    #[test]
    fn the_program_of_a_command() {
        assert_eq!(
            main_program("apt-cache policy curl").as_deref(),
            Some("apt-cache")
        );
        assert_eq!(
            main_program("python3 --version && python3 -m pip --version >/dev/null").as_deref(),
            Some("python3")
        );
        assert_eq!(main_program("command -v x || command -v y"), None);
        assert_eq!(main_program("ldconfig -p | grep -F x"), None);
    }

    #[tokio::test]
    async fn a_program_is_measured_once_before_its_other_commands_run() {
        let t = tempfile::tempdir().unwrap();
        file(t.path(), "pm", b"\x7fELF", 1024);
        let path = t.path().display().to_string();
        let b = Arc::new(Budget::new(u64::MAX / 4));
        let run = |cmd: &'static str| {
            let (b, path, home) = (b.clone(), path.clone(), t.path().to_path_buf());
            async move { b.admit(cmd, &path, &home).await }
        };
        assert_eq!(
            b.estimate("pm query a", &path, t.path()),
            SHELL + NATIVE_MIN
        );
        let mut first = run("pm query a").await;
        // Its other commands wait for its first measure; other programs do not.
        let second = run("pm query b");
        tokio::pin!(second);
        assert!(
            pending(&mut second).await,
            "ran before the program was measured"
        );
        drop(admit(&b, "other").await);
        first.seen(92 * MIB);
        first.ended();
        drop(first);
        let _second = tokio::time::timeout(Duration::from_secs(5), second)
            .await
            .expect("admitted once the first ended");
        // The program's peak stands for its commands not seen yet.
        assert_eq!(
            b.estimate("pm query c", &path, t.path()),
            92 * MIB + 92 * MIB / 4 + 2 * MIB
        );
        // A first command dropped before it ended (cancelled) lets the next one go.
        drop(admit(&b, "pq a").await);
        assert!(!pending(admit(&b, "pq b")).await);
    }

    #[tokio::test]
    async fn peaks_are_kept_for_the_next_envira() {
        let t = tempfile::tempdir().unwrap();
        let file = t.path().join("probes.toml");
        let cmd = "x --version \"quoted\" 'a'\nnext";
        let b = Arc::new(Budget::for_scans(file.clone()));
        assert_eq!(b.render(), None, "nothing ran");
        let mut t1 = admit(&b, cmd).await;
        t1.seen(40 * MIB);
        drop(t1);
        let mut t2 = admit(&b, "spiky").await;
        t2.seen(100 * MIB);
        drop(t2);
        std::fs::write(&file, b.render().unwrap()).unwrap();

        let b = Arc::new(Budget::for_scans(file.clone()));
        let home = Path::new("/");
        assert_eq!(b.estimate(cmd, "", home), 40 * MIB + 10 * MIB + 2 * MIB);
        // Lower this time: the earlier spike fades.
        let mut t2 = admit(&b, "spiky").await;
        t2.seen(20 * MIB);
        drop(t2);
        std::fs::write(&file, b.render().unwrap()).unwrap();
        let b = Budget::for_scans(file.clone());
        assert_eq!(
            b.estimate("spiky", "", home),
            75 * MIB + 75 * MIB / 4 + 2 * MIB
        );
        assert_eq!(b.estimate(cmd, "", home), 52 * MIB, "kept");

        // Too fast to measure this time: the peak measured before stays.
        let b = Arc::new(Budget::for_scans(file.clone()));
        let mut t = admit(&b, cmd).await;
        t.ended();
        drop(t);
        assert_eq!(b.estimate(cmd, "", home), 52 * MIB);
        assert_eq!(b.render(), None);

        // Within the headroom of its estimate: not written again.
        let b = Arc::new(Budget::for_scans(file.clone()));
        let mut t = admit(&b, cmd).await;
        t.seen(51 * MIB);
        drop(t);
        assert_eq!(b.render(), None);
        let mut t = admit(&b, cmd).await;
        t.seen(53 * MIB);
        drop(t);
        assert!(b.render().is_some(), "outgrew its estimate");

        // An unreadable file is no peaks.
        std::fs::write(&file, "peaks = 3").unwrap();
        let b = Budget::for_scans(file);
        assert_eq!(b.estimate("spiky", "", home), estimate("spiky", "", home));
    }

    // Needs /proc: elsewhere nothing is measured, so nothing is learned.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn a_command_too_fast_to_measure_keeps_a_bounded_estimate() {
        let t = tempfile::tempdir().unwrap();
        file(t.path(), "jscli", b"#!/usr/bin/env node\n", 0);
        file(t.path(), "tiny", b"\x7fELF", 1024);
        let path = t.path().display().to_string();
        let b = Arc::new(Budget::new(u64::MAX / 4));
        for cmd in ["jscli --version", "tiny --version"] {
            let mut ticket = b.admit(cmd, &path, t.path()).await;
            ticket.ended();
            drop(ticket);
        }
        assert_eq!(b.estimate("jscli --version", &path, t.path()), FAST);
        assert_eq!(
            b.estimate("tiny --version", &path, t.path()),
            SHELL + NATIVE_MIN
        );
        // Dropped before it ended (cancelled): nothing learned.
        drop(admit(&b, "dropped").await);
        assert_eq!(b.learned("dropped"), None);
    }

    #[tokio::test]
    async fn one_command_runs_whatever_its_size() {
        let b = Arc::new(Budget::new(1));
        let t = tokio::time::timeout(Duration::from_secs(5), admit(&b, "big --version")).await;
        assert!(t.is_ok());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_process_tree_is_measured() {
        let mut c = std::process::Command::new("bash")
            .args(["-c", "sleep 2 & sleep 2 & wait"])
            .spawn()
            .unwrap();
        std::thread::sleep(Duration::from_millis(300));
        let all = tree_hwm(c.id());
        let own = tree_hwm(std::process::id());
        c.kill().unwrap();
        let _ = c.wait();
        // bash and two sleeps, each at least a few hundred KiB.
        assert!(all > 3 * 256 * 1024, "{all}");
        assert!(own > 0);
    }

    use std::time::Duration;
}
