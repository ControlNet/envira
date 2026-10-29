//! Process execution behind a trait, so the executor and status logic can be tested with a fake.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use futures::future::BoxFuture;
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::process::Command;
use tokio::sync::{mpsc, watch};

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
    /// Runs a command and captures its output (detection, checks).
    fn capture<'a>(&'a self, cmd: &'a str, timeout: Duration) -> BoxFuture<'a, Probe>;

    /// Runs an install or apply step, sending each output line to `lines`, until it exits or
    /// `cancel` becomes true (then the whole process group is killed).
    fn stream<'a>(
        &'a self,
        cmd: &'a str,
        cwd: &'a Path,
        lines: mpsc::UnboundedSender<String>,
        cancel: watch::Receiver<bool>,
    ) -> BoxFuture<'a, StreamEnd>;
}

/// Environment every command runs with.
#[derive(Debug, Clone)]
pub struct CmdEnv {
    pub path: String,
    pub home: PathBuf,
    pub user: String,
    /// Run commands as this user (envira itself runs as root via sudo).
    pub drop_to: Option<String>,
    pub state_dir: PathBuf,
}

pub struct RealRunner {
    pub env: CmdEnv,
}

impl RealRunner {
    fn command(&self, cmd: &str) -> Command {
        let e = &self.env;
        let mut c = match &e.drop_to {
            Some(user) => {
                let mut c = Command::new("runuser");
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
            .env("HOME", &e.home)
            .env("ENVIRA_STATE_DIR", &e.state_dir)
            .env("DEBIAN_FRONTEND", "noninteractive")
            .env("NO_COLOR", "1")
            .stdin(Stdio::null())
            .kill_on_drop(true);
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

fn kill_group(pid: Option<u32>, sig: i32) {
    if let Some(pid) = pid {
        // SAFETY: plain syscall; a negative pid targets the process group created by setsid.
        unsafe {
            libc::kill(-(pid as i32), sig);
        }
    }
}

async fn pump<R: AsyncRead + Unpin>(r: R, tx: mpsc::UnboundedSender<String>) {
    let mut lines = BufReader::new(r).lines();
    while let Ok(Some(l)) = lines.next_line().await {
        let _ = tx.send(strip_ansi(&l));
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
    fn capture<'a>(&'a self, cmd: &'a str, timeout: Duration) -> BoxFuture<'a, Probe> {
        Box::pin(async move {
            let mut c = self.command(cmd);
            c.stdout(Stdio::piped()).stderr(Stdio::piped());
            let child = match c.spawn() {
                Ok(ch) => ch,
                Err(e) => {
                    return Probe {
                        code: -1,
                        output: e.to_string(),
                    };
                }
            };
            let pid = child.id();
            match tokio::time::timeout(timeout, child.wait_with_output()).await {
                Ok(Ok(out)) => {
                    let mut output = String::from_utf8_lossy(&out.stdout).into_owned();
                    output.push_str(&String::from_utf8_lossy(&out.stderr));
                    Probe {
                        code: out.status.code().unwrap_or(-1),
                        output,
                    }
                }
                Ok(Err(e)) => Probe {
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
        lines: mpsc::UnboundedSender<String>,
        mut cancel: watch::Receiver<bool>,
    ) -> BoxFuture<'a, StreamEnd> {
        Box::pin(async move {
            let mut c = self.command(cmd);
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
                            kill_group(pid, libc::SIGTERM);
                            if tokio::time::timeout(Duration::from_secs(3), child.wait()).await.is_err() {
                                kill_group(pid, libc::SIGKILL);
                                let _ = child.wait().await;
                            }
                            break StreamEnd::Cancelled;
                        }
                    }
                }
            };
            // A backgrounded grandchild may keep the pipes open; do not wait on it forever.
            let _ = tokio::time::timeout(Duration::from_secs(2), async {
                let _ = out.await;
                let _ = err.await;
            })
            .await;
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
        pub log: Mutex<Vec<String>>,
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

        pub fn ran(&self) -> Vec<String> {
            self.log.lock().unwrap().clone()
        }
    }

    impl Runner for FakeRunner {
        fn capture<'a>(&'a self, cmd: &'a str, _t: Duration) -> BoxFuture<'a, Probe> {
            let (code, output) = self.lookup(cmd);
            Box::pin(async move { Probe { code, output } })
        }

        fn stream<'a>(
            &'a self,
            cmd: &'a str,
            _cwd: &'a Path,
            lines: mpsc::UnboundedSender<String>,
            _cancel: watch::Receiver<bool>,
        ) -> BoxFuture<'a, StreamEnd> {
            self.log.lock().unwrap().push(cmd.to_string());
            let (code, output) = self.lookup(cmd);
            for l in output.lines() {
                let _ = lines.send(l.to_string());
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
        let (tx, mut rx) = mpsc::unbounded_channel();
        let (_ctx, crx) = watch::channel(false);
        let end = r
            .stream("echo a; echo -e '\\e[31mb\\e[0m'", dir.path(), tx, crx)
            .await;
        assert_eq!(end, StreamEnd::Exit(0));
        assert_eq!(rx.recv().await.unwrap(), "a");
        assert_eq!(rx.recv().await.unwrap(), "b");

        let (tx, _rx) = mpsc::unbounded_channel();
        let (ctx, crx) = watch::channel(false);
        let h = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(200)).await;
            ctx.send(true).unwrap();
            ctx
        });
        let start = std::time::Instant::now();
        let end = r.stream("sleep 30 & sleep 30", dir.path(), tx, crx).await;
        assert_eq!(end, StreamEnd::Cancelled);
        assert!(start.elapsed() < Duration::from_secs(6));
        drop(h.await);
    }

    #[test]
    fn strips_ansi_and_cr() {
        assert_eq!(strip_ansi("\x1b[1;32mok\x1b[0m"), "ok");
        assert_eq!(strip_ansi("10%\r50%\r100%"), "100%");
    }
}
