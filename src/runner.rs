//! Process execution behind a trait, so the executor and status logic can be tested with a fake.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use futures::future::BoxFuture;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::{mpsc, watch};

use crate::net;

/// Result of a captured command.
#[derive(Debug, Clone, Default)]
pub struct Probe {
    /// Exit code; -1 when the command could not run or timed out.
    pub code: i32,
    /// stdout followed by stderr.
    pub output: String,
}

impl Probe {
    pub fn ok(&self) -> bool {
        self.code == 0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamEnd {
    Exit(i32),
    Cancelled,
    SpawnError(String),
}

pub trait Runner: Send + Sync {
    /// Runs a command as the user and captures its output (detection, checks).
    fn capture<'a>(&'a self, cmd: &'a str, timeout: Duration) -> BoxFuture<'a, Probe> {
        self.capture_as(cmd, timeout, false)
    }

    /// `capture`, as root when `as_root`: directly when envira is root, else through `sudo -n`
    /// (which fails at once rather than ask for a password). The environment is the user's
    /// either way, so a check looks at the same files.
    fn capture_as<'a>(
        &'a self,
        cmd: &'a str,
        timeout: Duration,
        as_root: bool,
    ) -> BoxFuture<'a, Probe>;

    /// Runs an install or apply step, sending each output line to `lines`, until it exits or
    /// `cancel` becomes true (then the whole process group is killed). `lines` is bounded: a
    /// step that prints faster than its lines are logged waits on its pipe instead of queueing
    /// its output in memory. `as_root`: a system-scope step, which under `sudo envira` runs as
    /// root (its `{sudo}` is empty) while everything else runs as the user.
    fn stream<'a>(
        &'a self,
        cmd: &'a str,
        cwd: &'a Path,
        as_root: bool,
        lines: mpsc::Sender<String>,
        cancel: watch::Receiver<bool>,
    ) -> BoxFuture<'a, StreamEnd>;

    /// HTTP GET of a text body (latest-version queries).
    fn http_get<'a>(
        &'a self,
        url: &'a str,
        timeout: Duration,
    ) -> BoxFuture<'a, Result<String, String>>;
}

/// GET with envira's User-Agent (crates.io rejects requests without one) and `GITHUB_TOKEN`
/// for the GitHub API (higher rate limit). Transient failures are retried briefly.
async fn http_get_text(url: &str, timeout: Duration) -> Result<String, String> {
    let client = reqwest::Client::builder()
        .user_agent(concat!("envira/", env!("CARGO_PKG_VERSION")))
        .timeout(timeout)
        .build()
        .map_err(|e| e.to_string())?;
    let policy = net::Retry::QUERY;
    let mut n = 1;
    loop {
        let mut req = client.get(url);
        if let Some(t) = net::github_token(url) {
            req = req.bearer_auth(t);
        }
        let (why, after) = match req.send().await {
            Ok(resp) if resp.status().is_success() => match resp.text().await {
                Ok(t) => return Ok(t),
                Err(e) if net::transient_error(&e) => (e.to_string(), None),
                Err(e) => return Err(e.to_string()),
            },
            Ok(resp) if net::transient_status(resp.status().as_u16()) => (
                format!("HTTP {}", resp.status()),
                net::retry_after(resp.headers()),
            ),
            Ok(resp) => return Err(format!("HTTP {}", resp.status())),
            Err(e) if net::transient_error(&e) => (e.to_string(), None),
            Err(e) => return Err(e.to_string()),
        };
        match policy.delay(n, after) {
            Some(d) => {
                tokio::time::sleep(d).await;
                n += 1;
            }
            None => return Err(why),
        }
    }
}

/// Environment every command runs with.
#[derive(Debug, Clone)]
pub struct CmdEnv {
    pub path: String,
    pub home: PathBuf,
    pub user: String,
    /// Run commands as this user (envira itself runs as root via sudo).
    pub drop_to: Option<String>,
    /// HOME of system-scope steps, which stay root under `sudo envira`: root writing caches
    /// into the user's home would leave root-owned files there.
    pub root_home: PathBuf,
    pub state_dir: PathBuf,
}

pub struct RealRunner {
    pub env: CmdEnv,
}

impl RealRunner {
    /// `bash -c cmd` in envira's environment: as `drop_to` when envira acts for a user, unless
    /// `as_root` (a system-scope step).
    fn command(&self, cmd: &str, as_root: bool) -> Command {
        let e = &self.env;
        let home = if as_root && e.drop_to.is_some() {
            &e.root_home
        } else {
            &e.home
        };
        let mut c = match e.drop_to.as_ref().filter(|_| !as_root) {
            Some(user) => {
                let mut c = Command::new(drop_helper());
                c.args(["-u", user, "--", "env"])
                    .arg(format!("HOME={}", e.home.display()))
                    .arg(format!("USER={}", e.user))
                    .arg(format!("PATH={}", e.path))
                    .arg(format!("ENVIRA_STATE_DIR={}", e.state_dir.display()))
                    .arg("bash");
                c
            }
            None => Command::new("bash"),
        };
        c.args(["-eo", "pipefail", "-c", cmd])
            .env("PATH", &e.path)
            .env("HOME", home)
            .env("ENVIRA_STATE_DIR", &e.state_dir)
            .env("DEBIAN_FRONTEND", "noninteractive")
            .env("NO_COLOR", "1")
            .stdin(Stdio::null())
            .kill_on_drop(true);
        for (k, v) in build_env() {
            c.env(k, v);
        }
        // New session without a controlling terminal: tools that open /dev/tty (even for
        // `--version`) cannot block on the TUI's terminal, and the session id doubles as the
        // process group that cancellation kills.
        // SAFETY: setsid is async-signal-safe and touches no memory.
        unsafe {
            c.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
        c
    }
}

/// Environment that keeps builds within the machine's memory, for steps and for
/// `self-update`'s package managers. A value the user set is kept.
/// - rustup unpacks with one thread per core and buffers for them: rustup-init peaked at
///   560-670 MiB and was OOM-killed in 512 MiB. One IO thread: ~210 MiB, a few seconds slower.
/// - cargo compiles one crate per core, and one rustc can take ~0.5 GiB: a small VM with many
///   cores runs out of memory. About one job per GiB available.
pub fn build_env() -> Vec<(&'static str, String)> {
    let mut env = Vec::new();
    if std::env::var_os("RUSTUP_IO_THREADS").is_none() {
        env.push(("RUSTUP_IO_THREADS", "1".to_string()));
    }
    if std::env::var_os("CARGO_BUILD_JOBS").is_none() {
        let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
        if let Some(jobs) = build_jobs(available_memory(), cores) {
            env.push(("CARGO_BUILD_JOBS", jobs.to_string()));
        }
    }
    env
}

const GIB: u64 = 1 << 30;

/// envira's own commands (detection, `installed` and latest-version queries) to run at once:
/// about one per 192 MiB available, 2 to 8. Many are a Node or Python CLI (`--version`,
/// `npm ls`, `dnf repoquery`) at 30-300 MiB each; eight at once took a small VM's memory.
pub fn process_jobs() -> usize {
    jobs_for(available_memory())
}

fn jobs_for(available: Option<u64>) -> usize {
    available.map_or(8, |a| (a / (192 << 20)).clamp(2, 8) as usize)
}

/// Build jobs that fit in `available` bytes (one per GiB, at least one), or `None` when every
/// core fits (cargo's own default) or the memory is unknown.
fn build_jobs(available: Option<u64>, cores: usize) -> Option<usize> {
    let jobs = (available? / GIB).max(1) as usize;
    (jobs < cores).then_some(jobs)
}

/// Memory this machine can still give, in bytes: `MemAvailable`, or less when a cgroup v2
/// limits this process (a container, a systemd service or slice). `None` where there is no
/// /proc (macOS).
fn available_memory() -> Option<u64> {
    let read = |p: &Path| std::fs::read_to_string(p).ok();
    let host = mem_available(&read(Path::new("/proc/meminfo"))?)?;
    let own = read(Path::new("/proc/self/cgroup"))
        .and_then(|c| cgroup_path(&c))
        .unwrap_or_default();
    let cgroup = cgroup_left(Path::new("/sys/fs/cgroup"), &own, read);
    Some(cgroup.map_or(host, |c| c.min(host)))
}

/// This process's cgroup v2 path, relative to the mount (the `0::` line of /proc/self/cgroup).
/// A path outside this cgroup namespace (`/../..`) counts as the mount root.
fn cgroup_path(proc_cgroup: &str) -> Option<String> {
    let p = proc_cgroup
        .lines()
        .find_map(|l| l.strip_prefix("0::"))?
        .trim();
    Some(if p.split('/').any(|c| c == "..") {
        String::new()
    } else {
        p.trim_start_matches('/').to_string()
    })
}

/// The least memory left under any limit from the cgroup `own` up to the mount `root`: a limit
/// on a parent slice binds as much as one on the cgroup itself.
fn cgroup_left(root: &Path, own: &str, read: impl Fn(&Path) -> Option<String>) -> Option<u64> {
    let mut dir = root.join(own);
    let mut left: Option<u64> = None;
    while dir.starts_with(root) {
        let here = read(&dir.join("memory.max"))
            .zip(read(&dir.join("memory.current")))
            .and_then(|(max, cur)| cgroup_free(&max, &cur));
        left = match (left, here) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        if !dir.pop() {
            break;
        }
    }
    left
}

/// `MemAvailable` of /proc/meminfo, in bytes.
fn mem_available(meminfo: &str) -> Option<u64> {
    let line = meminfo.lines().find(|l| l.starts_with("MemAvailable:"))?;
    let kb: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kb * 1024)
}

/// What a cgroup v2 limit leaves (`memory.max` minus `memory.current`); `None` without a limit.
fn cgroup_free(max: &str, current: &str) -> Option<u64> {
    let max: u64 = max.trim().parse().ok()?;
    let current: u64 = current.trim().parse().ok()?;
    Some(max.saturating_sub(current))
}

/// How root runs a command as another user: util-linux's `runuser` where there is one (no PAM
/// password prompt, no sudoers rules), else `sudo -u` (macOS; minimal images with only
/// util-linux-core, such as Fedora's), which root may use without a password.
fn drop_helper() -> &'static str {
    static HELPER: std::sync::OnceLock<&'static str> = std::sync::OnceLock::new();
    HELPER.get_or_init(|| {
        let runuser = [
            "/usr/sbin/runuser",
            "/sbin/runuser",
            "/usr/bin/runuser",
            "/bin/runuser",
        ];
        if cfg!(target_os = "linux") && runuser.iter().any(|p| Path::new(p).exists()) {
            "runuser"
        } else {
            "sudo"
        }
    })
}

/// Whether any process of the group led by `pid` is still alive.
fn group_alive(pid: Option<u32>) -> bool {
    // SAFETY: signal 0 only checks that the group exists and may be signalled.
    pid.is_some_and(|pid| unsafe { libc::kill(-(pid as i32), 0) } == 0)
}

fn kill_group(pid: Option<u32>, sig: i32) {
    if let Some(pid) = pid {
        // SAFETY: plain syscall; a negative pid targets the process group created by setsid.
        unsafe {
            libc::kill(-(pid as i32), sig);
        }
    }
}

/// Longest line kept in memory; the rest of a longer line is dropped (the disk log gets the
/// same). Output without newlines (a progress bar, a binary on stdout) stays bounded.
const MAX_LINE: usize = 64 * 1024;

/// Sends each line of `r` (lossy UTF-8, at most `MAX_LINE` bytes) until EOF or a read error.
/// Output of a probe kept per stream. Probes print a version or a path; a broken tool can
/// print without end, and the rest is read and dropped so it does not block on its pipe.
const CAPTURE_BYTES: usize = 1 << 20;

async fn read_capped<R: AsyncRead + Unpin>(r: Option<R>) -> Vec<u8> {
    let mut buf = Vec::new();
    let Some(mut r) = r else { return buf };
    let mut chunk = [0u8; 8192];
    let mut cut = false;
    loop {
        match r.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let room = CAPTURE_BYTES - buf.len();
                buf.extend_from_slice(&chunk[..n.min(room)]);
                cut |= n > room;
            }
        }
    }
    if cut {
        buf.extend_from_slice(b"\n[envira] output cut at 1 MiB\n");
    }
    buf
}

async fn pump<R: AsyncRead + Unpin>(r: R, tx: mpsc::Sender<String>) {
    let mut r = BufReader::new(r);
    let mut line = Vec::new();
    let send = |line: &[u8]| {
        let l = line.strip_suffix(b"\r").unwrap_or(line);
        strip_ansi(&String::from_utf8_lossy(l))
    };
    loop {
        let buf = match r.fill_buf().await {
            Ok([]) | Err(_) => break,
            Ok(b) => b,
        };
        let nl = buf.iter().position(|&b| b == b'\n');
        let chunk = &buf[..nl.unwrap_or(buf.len())];
        let room = MAX_LINE.saturating_sub(line.len());
        line.extend_from_slice(&chunk[..chunk.len().min(room)]);
        let used = nl.map_or(buf.len(), |i| i + 1);
        r.consume(used);
        if nl.is_some() {
            let _ = tx.send(send(&line)).await;
            line.clear();
        }
    }
    if !line.is_empty() {
        let _ = tx.send(send(&line)).await;
    }
}

/// Removes ANSI escape sequences and carriage-return progress noise from a log line.
pub fn strip_ansi(s: &str) -> String {
    let s = s.rsplit('\r').find(|p| !p.is_empty()).unwrap_or("");
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for c in chars.by_ref() {
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

impl Runner for RealRunner {
    fn http_get<'a>(
        &'a self,
        url: &'a str,
        timeout: Duration,
    ) -> BoxFuture<'a, Result<String, String>> {
        Box::pin(http_get_text(url, timeout))
    }

    fn capture_as<'a>(
        &'a self,
        cmd: &'a str,
        timeout: Duration,
        as_root: bool,
    ) -> BoxFuture<'a, Probe> {
        Box::pin(async move {
            let e = &self.env;
            // SAFETY: geteuid has no preconditions.
            let root = unsafe { libc::geteuid() } == 0;
            let through_sudo;
            let cmd = if as_root && !root {
                let q = |s: &str| crate::template::shell_quote(s);
                through_sudo = format!(
                    "sudo -n -- env HOME={} PATH={} ENVIRA_STATE_DIR={} bash -eo pipefail -c {}",
                    q(&e.home.display().to_string()),
                    q(&e.path),
                    q(&e.state_dir.display().to_string()),
                    q(cmd)
                );
                through_sudo.as_str()
            } else {
                cmd
            };
            let mut c = self.command(cmd, as_root && root);
            // Detection must not depend on where envira was started (rust-toolchain.toml,
            // .nvmrc and similar files change what `rustc` / `node` report). As root too, `~`
            // is the user's: a check looks at the user's files.
            c.env("HOME", &e.home)
                .current_dir(&e.home)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            let mut child = match c.spawn() {
                Ok(ch) => ch,
                Err(e) => {
                    return Probe {
                        code: -1,
                        output: e.to_string(),
                    };
                }
            };
            let pid = child.id();
            let (out, err) = (child.stdout.take(), child.stderr.take());
            let work = async {
                let (out, err) = tokio::join!(read_capped(out), read_capped(err));
                (out, err, child.wait().await)
            };
            match tokio::time::timeout(timeout, work).await {
                Ok((out, err, Ok(status))) => {
                    let mut output = String::from_utf8_lossy(&out).into_owned();
                    output.push_str(&String::from_utf8_lossy(&err));
                    Probe {
                        code: status.code().unwrap_or(-1),
                        output,
                    }
                }
                Ok((_, _, Err(e))) => Probe {
                    code: -1,
                    output: e.to_string(),
                },
                Err(_) => {
                    kill_group(pid, libc::SIGKILL);
                    Probe {
                        code: -1,
                        output: format!("timed out after {}s", timeout.as_secs()),
                    }
                }
            }
        })
    }

    fn stream<'a>(
        &'a self,
        cmd: &'a str,
        cwd: &'a Path,
        as_root: bool,
        lines: mpsc::Sender<String>,
        mut cancel: watch::Receiver<bool>,
    ) -> BoxFuture<'a, StreamEnd> {
        Box::pin(async move {
            let mut c = self.command(cmd, as_root);
            c.current_dir(cwd)
                .env("ENVIRA_TMP", cwd)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            let mut child = match c.spawn() {
                Ok(ch) => ch,
                Err(e) => return StreamEnd::SpawnError(e.to_string()),
            };
            let pid = child.id();
            let out = tokio::spawn(pump(child.stdout.take().expect("piped"), lines.clone()));
            let err = tokio::spawn(pump(child.stderr.take().expect("piped"), lines));
            let end = loop {
                tokio::select! {
                    status = child.wait() => {
                        break match status {
                            Ok(s) => StreamEnd::Exit(s.code().unwrap_or(-1)),
                            Err(e) => StreamEnd::SpawnError(e.to_string()),
                        };
                    }
                    changed = cancel.changed() => {
                        if changed.is_err() {
                            // Controller gone: keep waiting for the process without cancellation.
                            let s = child.wait().await;
                            break match s {
                                Ok(s) => StreamEnd::Exit(s.code().unwrap_or(-1)),
                                Err(e) => StreamEnd::SpawnError(e.to_string()),
                            };
                        }
                        if *cancel.borrow() {
                            // The whole group gets the grace period, not just the shell: a
                            // shell that exits at once on TERM may leave a child that ignores
                            // it, still changing the machine after the step "stopped".
                            kill_group(pid, libc::SIGTERM);
                            let grace = tokio::time::Instant::now() + Duration::from_secs(3);
                            let _ = tokio::time::timeout_at(grace, child.wait()).await;
                            while group_alive(pid) && tokio::time::Instant::now() < grace {
                                tokio::time::sleep(Duration::from_millis(50)).await;
                            }
                            if group_alive(pid) {
                                kill_group(pid, libc::SIGKILL);
                            }
                            let _ = child.wait().await;
                            break StreamEnd::Cancelled;
                        }
                    }
                }
            };
            // A backgrounded grandchild may keep the pipes open; do not wait on it forever. The
            // pumps hold senders of `lines`, which the executor drains until every sender is
            // gone: stop them, not just stop waiting for them.
            let (abort_out, abort_err) = (out.abort_handle(), err.abort_handle());
            let drained = tokio::time::timeout(Duration::from_secs(2), async {
                let _ = out.await;
                let _ = err.await;
            })
            .await;
            if drained.is_err() {
                abort_out.abort();
                abort_err.abort();
            }
            end
        })
    }
}

#[cfg(test)]
pub mod fake {
    //! Scripted runner for executor and status tests.

    use super::*;
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    /// Commands are matched by substring; the first matching rule wins.
    #[derive(Default)]
    pub struct FakeRunner {
        pub rules: Mutex<Vec<(String, i32, String)>>,
        /// Substrings of commands that flip to succeed once a matching stream command ran,
        /// e.g. detect of a tool after its install.
        pub installs: Mutex<BTreeMap<String, String>>,
        /// Substrings of streamed commands that never exit on their own (until cancelled).
        pub hangs: Mutex<Vec<String>>,
        /// `(needle, failures left, output)`: matching streamed commands fail with `output`
        /// that many times before the normal rules apply.
        pub flaky: Mutex<Vec<(String, u32, String)>>,
        pub log: Mutex<Vec<String>>,
        /// Streamed commands asked to run as root (system scope).
        pub as_root: Mutex<Vec<String>>,
        /// Substrings of captured commands that pass only as root (a private system file).
        pub root_only: Mutex<Vec<String>>,
        /// Captures asked to run as root.
        pub root_captures: Mutex<Vec<String>>,
    }

    impl FakeRunner {
        pub fn rule(&self, needle: &str, code: i32, output: &str) -> &Self {
            self.rules
                .lock()
                .unwrap()
                .push((needle.into(), code, output.into()));
            self
        }

        /// After a streamed command containing `install_needle` succeeds, captures containing
        /// `detect_needle` succeed with `output`.
        pub fn on_install(&self, install_needle: &str, detect_needle: &str, output: &str) -> &Self {
            self.installs.lock().unwrap().insert(
                install_needle.into(),
                format!("{detect_needle}\u{0}{output}"),
            );
            self
        }

        fn lookup(&self, cmd: &str) -> (i32, String) {
            self.rules
                .lock()
                .unwrap()
                .iter()
                .find(|(n, _, _)| cmd.contains(n.as_str()))
                .map(|(_, c, o)| (*c, o.clone()))
                .unwrap_or((1, String::new()))
        }

        pub fn fail_then(&self, needle: &str, times: u32, output: &str) -> &Self {
            self.flaky
                .lock()
                .unwrap()
                .push((needle.into(), times, output.into()));
            self
        }

        pub fn hang(&self, needle: &str) -> &Self {
            self.hangs.lock().unwrap().push(needle.into());
            self
        }

        pub fn ran(&self) -> Vec<String> {
            self.log.lock().unwrap().clone()
        }
    }

    impl Runner for FakeRunner {
        /// URLs are matched against the same rules as commands; exit 0 means HTTP 200.
        fn http_get<'a>(
            &'a self,
            url: &'a str,
            _t: Duration,
        ) -> BoxFuture<'a, Result<String, String>> {
            self.log.lock().unwrap().push(format!("GET {url}"));
            let (code, output) = self.lookup(url);
            Box::pin(async move {
                if code == 0 {
                    Ok(output)
                } else {
                    Err("HTTP 404 Not Found".into())
                }
            })
        }

        fn capture_as<'a>(
            &'a self,
            cmd: &'a str,
            _t: Duration,
            as_root: bool,
        ) -> BoxFuture<'a, Probe> {
            if as_root {
                self.root_captures.lock().unwrap().push(cmd.to_string());
            }
            let root_only = self
                .root_only
                .lock()
                .unwrap()
                .iter()
                .any(|n| cmd.contains(n.as_str()));
            let (code, output) = if root_only {
                (if as_root { 0 } else { 1 }, String::new())
            } else {
                self.lookup(cmd)
            };
            Box::pin(async move { Probe { code, output } })
        }

        fn stream<'a>(
            &'a self,
            cmd: &'a str,
            _cwd: &'a Path,
            as_root: bool,
            lines: mpsc::Sender<String>,
            mut cancel: watch::Receiver<bool>,
        ) -> BoxFuture<'a, StreamEnd> {
            self.log.lock().unwrap().push(cmd.to_string());
            if as_root {
                self.as_root.lock().unwrap().push(cmd.to_string());
            }
            let flaky = self
                .flaky
                .lock()
                .unwrap()
                .iter_mut()
                .find(|(n, left, _)| *left > 0 && cmd.contains(n.as_str()))
                .map(|(_, left, out)| {
                    *left -= 1;
                    out.clone()
                });
            if let Some(out) = flaky {
                for l in out.lines() {
                    let _ = lines.try_send(l.to_string());
                }
                return Box::pin(async { StreamEnd::Exit(1) });
            }
            if self.hangs.lock().unwrap().iter().any(|h| cmd.contains(h)) {
                return Box::pin(async move {
                    while !*cancel.borrow() {
                        if cancel.changed().await.is_err() {
                            std::future::pending::<()>().await;
                        }
                    }
                    StreamEnd::Cancelled
                });
            }
            let (code, output) = self.lookup(cmd);
            // Scripted outputs are a few lines, far below the executor's channel capacity.
            for l in output.lines() {
                let _ = lines.try_send(l.to_string());
            }
            if code == 0 {
                let flips: Vec<String> = self
                    .installs
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|(k, _)| cmd.contains(k.as_str()))
                    .map(|(_, v)| v.clone())
                    .collect();
                for f in flips {
                    let (needle, out) = f.split_once('\u{0}').unwrap();
                    self.rules
                        .lock()
                        .unwrap()
                        .insert(0, (needle.into(), 0, out.into()));
                }
            }
            Box::pin(async move { StreamEnd::Exit(code) })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn runner() -> RealRunner {
        RealRunner {
            env: CmdEnv {
                path: std::env::var("PATH").unwrap(),
                home: std::env::temp_dir(),
                user: "user".into(),
                drop_to: None,
                root_home: "/root".into(),
                state_dir: std::env::temp_dir(),
            },
        }
    }

    #[tokio::test]
    async fn capture_and_exit_codes() {
        let r = runner();
        let p = r
            .capture("echo out; echo err >&2; exit 3", Duration::from_secs(5))
            .await;
        assert_eq!(p.code, 3);
        assert_eq!(p.output, "out\nerr\n");
        let p = r.capture("false | true", Duration::from_secs(5)).await;
        assert_eq!(p.code, 1, "pipefail is on");
    }

    #[tokio::test]
    async fn children_have_no_controlling_terminal() {
        let p = runner()
            .capture("exec 3</dev/tty && echo has-tty", Duration::from_secs(5))
            .await;
        assert_ne!(p.code, 0, "{}", p.output);
        assert!(!p.output.contains("has-tty"));
    }

    #[tokio::test]
    async fn capture_runs_in_home() {
        let r = runner();
        let p = r.capture("pwd -P", Duration::from_secs(5)).await;
        // macOS: the temp dir is under /var, a symlink to /private/var.
        let home = std::fs::canonicalize(&r.env.home).unwrap();
        assert_eq!(p.output.trim(), home.display().to_string());
    }

    #[test]
    fn cargo_builds_fit_in_memory() {
        let meminfo = "MemTotal:       16303580 kB\nMemFree:  1 kB\nMemAvailable:    3670016 kB\n";
        assert_eq!(mem_available(meminfo), Some(3670016 * 1024));
        assert_eq!(mem_available("MemTotal: 1 kB\n"), None);
        assert_eq!(cgroup_free("max\n", "123\n"), None);
        assert_eq!(cgroup_free("1073741824\n", "268435456\n"), Some(805306368));
        assert_eq!(cgroup_free("100\n", "200\n"), Some(0));
        // 3.5 GiB on 16 cores: 3 jobs; under 1 GiB: still one; plenty: cargo's default.
        assert_eq!(build_jobs(Some(3670016 * 1024), 16), Some(3));
        assert_eq!(build_jobs(Some(GIB / 2), 4), Some(1));
        assert_eq!(build_jobs(Some(64 * GIB), 16), None);
        assert_eq!(build_jobs(None, 16), None);
    }

    #[test]
    fn own_commands_fit_in_memory() {
        let mib = |m: u64| Some(m << 20);
        assert_eq!(jobs_for(mib(300)), 2);
        assert_eq!(jobs_for(mib(700)), 3);
        assert_eq!(jobs_for(mib(1024)), 5);
        assert_eq!(jobs_for(mib(64 << 10)), 8);
        assert_eq!(jobs_for(None), 8);
    }

    #[test]
    fn the_tightest_cgroup_limit_counts() {
        assert_eq!(
            cgroup_path("0::/system.slice/envira.service\n").as_deref(),
            Some("system.slice/envira.service")
        );
        assert_eq!(cgroup_path("0::/\n").as_deref(), Some(""));
        assert_eq!(cgroup_path("0::/../../x\n").as_deref(), Some(""));
        assert_eq!(cgroup_path("12:memory:/x\n"), None);

        let root = Path::new("/sys/fs/cgroup");
        // The mount root has no limit, the slice 2 GiB (1.5 used), the service none.
        let files = |p: &Path| -> Option<String> {
            let rel = p.strip_prefix(root).ok()?.to_str()?;
            match rel {
                "system.slice/memory.max" => Some(format!("{}\n", 2 * GIB)),
                "system.slice/memory.current" => Some(format!("{}\n", 3 * GIB / 2)),
                "system.slice/envira.service/memory.max" => Some("max\n".into()),
                "system.slice/envira.service/memory.current" => Some("1\n".into()),
                _ => None,
            }
        };
        assert_eq!(
            cgroup_left(root, "system.slice/envira.service", files),
            Some(GIB / 2)
        );
        assert_eq!(cgroup_left(root, "", files), None);
        assert_eq!(cgroup_left(root, "user.slice", files), None);
    }

    /// Under `sudo envira` (root acting for a user), steps run as the user except system-scope
    /// ones, which keep root with root's HOME.
    #[test]
    fn sudo_envira_runs_system_steps_as_root() {
        let mut r = runner();
        r.env.drop_to = Some("alice".into());
        r.env.home = "/home/alice".into();
        let words = |c: &Command| {
            let c = c.as_std();
            let mut w = vec![c.get_program().to_string_lossy().into_owned()];
            w.extend(c.get_args().map(|a| a.to_string_lossy().into_owned()));
            let home = c
                .get_envs()
                .find(|(k, _)| *k == "HOME")
                .and_then(|(_, v)| v)
                .map(|v| v.to_string_lossy().into_owned());
            (w, home)
        };
        let (user, home) = words(&r.command("id -u", false));
        let drop = drop_helper();
        assert_eq!(user[..3], [drop, "-u", "alice"]);
        assert!(user.contains(&"HOME=/home/alice".to_string()), "{user:?}");
        assert_eq!(home.as_deref(), Some("/home/alice"));

        let (root, home) = words(&r.command("apt-get install -y x", true));
        assert_eq!(root[0], "bash");
        assert_eq!(home.as_deref(), Some("/root"));

        // envira run as root itself (no SUDO_USER) or as a user: nobody to switch to.
        r.env.drop_to = None;
        let (plain, home) = words(&r.command("x", true));
        assert_eq!(plain[0], "bash");
        assert_eq!(home.as_deref(), Some("/home/alice"));
    }

    #[tokio::test]
    async fn rustup_gets_one_io_thread() {
        if std::env::var_os("RUSTUP_IO_THREADS").is_some() {
            return;
        }
        let p = runner()
            .capture("echo \"$RUSTUP_IO_THREADS\"", Duration::from_secs(5))
            .await;
        assert_eq!(p.output.trim(), "1");
    }

    #[tokio::test]
    async fn noisy_probes_keep_a_bounded_output() {
        // 8 MiB on stdout and a line on stderr: the first MiB, a note, and stderr; exit 3.
        let p = runner()
            .capture(
                "head -c 8388608 /dev/zero | tr '\\0' x; echo err >&2; exit 3",
                Duration::from_secs(20),
            )
            .await;
        assert_eq!(p.code, 3);
        assert!(p.output.len() < CAPTURE_BYTES + 100, "{}", p.output.len());
        assert!(p.output.starts_with("xxxx"));
        assert!(p.output.ends_with("[envira] output cut at 1 MiB\nerr\n"));
    }

    #[tokio::test]
    async fn capture_times_out() {
        let p = runner()
            .capture("sleep 5", Duration::from_millis(200))
            .await;
        assert_eq!(p.code, -1);
        assert!(p.output.contains("timed out"));
    }

    #[tokio::test]
    async fn stream_lines_and_cancel() {
        let r = runner();
        let dir = tempfile::tempdir().unwrap();
        let (tx, mut rx) = mpsc::channel(16);
        let (_ctx, crx) = watch::channel(false);
        let end = r
            .stream(
                "echo a; printf '\\033[31mb\\033[0m\\n'",
                dir.path(),
                false,
                tx,
                crx,
            )
            .await;
        assert_eq!(end, StreamEnd::Exit(0));
        assert_eq!(rx.recv().await.unwrap(), "a");
        assert_eq!(rx.recv().await.unwrap(), "b");

        let (tx, _rx) = mpsc::channel(16);
        let (ctx, crx) = watch::channel(false);
        let h = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(200)).await;
            ctx.send(true).unwrap();
            ctx
        });
        let start = std::time::Instant::now();
        let end = r
            .stream("sleep 30 & sleep 30", dir.path(), false, tx, crx)
            .await;
        assert_eq!(end, StreamEnd::Cancelled);
        assert!(start.elapsed() < Duration::from_secs(6));
        drop(h.await);
    }

    /// Cancelling kills the whole group, also a child that ignores TERM after its shell exited.
    #[tokio::test]
    async fn cancel_kills_children_that_ignore_term() {
        let r = runner();
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("after-cancel");
        let cmd = format!(
            "bash -c 'trap \"\" TERM; sleep 4; touch {}' & sleep 30",
            marker.display()
        );
        let (tx, _rx) = mpsc::channel(16);
        let (ctx, crx) = watch::channel(false);
        let h = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            ctx.send(true).unwrap();
            ctx
        });
        let end = r.stream(&cmd, dir.path(), false, tx, crx).await;
        assert_eq!(end, StreamEnd::Cancelled);
        drop(h.await);
        // The child would touch the marker 4 s after it started.
        tokio::time::sleep(Duration::from_secs(2)).await;
        assert!(!marker.exists(), "a child outlived the cancellation");
    }

    /// A step that leaves a background process holding its stdout: the step ends when its shell
    /// exits, and the line channel closes soon after (the executor waits for that).
    #[tokio::test]
    async fn a_background_child_does_not_hold_the_step_open() {
        let r = runner();
        let dir = tempfile::tempdir().unwrap();
        let (tx, mut rx) = mpsc::channel(16);
        let (_ctx, crx) = watch::channel(false);
        let start = std::time::Instant::now();
        let end = r
            .stream("sleep 8 & echo hi", dir.path(), false, tx, crx)
            .await;
        assert_eq!(end, StreamEnd::Exit(0));
        let closed = tokio::time::timeout(Duration::from_secs(1), async {
            let mut got = Vec::new();
            while let Some(l) = rx.recv().await {
                got.push(l);
            }
            got
        })
        .await;
        assert_eq!(closed.as_deref(), Ok(&["hi".to_string()][..]));
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "{:?}",
            start.elapsed()
        );
    }

    #[tokio::test]
    async fn long_or_invalid_lines_stay_bounded() {
        let mut input = vec![b'x'; 3 * MAX_LINE];
        input.extend_from_slice(b"\nbad \xff byte\r\nlast");
        let (tx, mut rx) = mpsc::channel(16);
        pump(&input[..], tx).await;
        let first = rx.recv().await.unwrap();
        assert_eq!(first.len(), MAX_LINE);
        assert_eq!(rx.recv().await.unwrap(), "bad \u{fffd} byte");
        assert_eq!(rx.recv().await.unwrap(), "last");
        assert_eq!(rx.recv().await, None);
    }

    #[test]
    fn strips_ansi_and_cr() {
        assert_eq!(strip_ansi("\x1b[1;32mok\x1b[0m"), "ok");
        assert_eq!(strip_ansi("10%\r50%\r100%"), "100%");
    }
}
