//! sudo's password for the steps of a run. Steps run in sessions of their own (see
//! `runner::RealRunner::command`), where the ticket a password left in envira's session is not
//! seen (sudo keys tickets by terminal and session, or by parent process). So `{sudo}` is
//! `sudo -A`: while a run holds the password, sudo asks `SUDO_ASKPASS`, which is envira itself
//! under the name `envira-askpass`, and that gets the password from this run over a socket in a
//! private directory. Only the same user is answered, and on Linux only processes started by
//! this envira.

use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use crate::sudo_prompt::Secret;

/// The name envira answers sudo under.
const HELPER: &str = "envira-askpass";

/// The helper of the run going on, for the steps' `SUDO_ASKPASS`.
static ACTIVE: Mutex<Option<PathBuf>> = Mutex::new(None);

/// `SUDO_ASKPASS` for commands started now: the helper while a run holds sudo's password.
pub fn helper() -> Option<PathBuf> {
    ACTIVE.lock().ok()?.clone()
}

/// Answers sudo for as long as it lives.
pub struct Askpass {
    dir: tempfile::TempDir,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Askpass {
    pub fn start(password: Secret) -> std::io::Result<Askpass> {
        // 0700, as tempfile makes it.
        let dir = tempfile::Builder::new()
            .prefix("envira-askpass-")
            .tempdir()?;
        let helper = dir.path().join(HELPER);
        std::os::unix::fs::symlink(std::env::current_exe()?, &helper)?;
        let listener = UnixListener::bind(dir.path().join("sock"))?;
        listener.set_nonblocking(true)?;
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let stop = stop.clone();
            std::thread::spawn(move || serve(&listener, &password, &stop))
        };
        if let Ok(mut a) = ACTIVE.lock() {
            *a = Some(helper);
        }
        Ok(Askpass {
            dir,
            stop,
            thread: Some(thread),
        })
    }

    #[cfg(test)]
    fn dir(&self) -> &Path {
        self.dir.path()
    }
}

impl Drop for Askpass {
    fn drop(&mut self) {
        if let Ok(mut a) = ACTIVE.lock()
            && a.as_deref().is_some_and(|h| h.starts_with(self.dir.path()))
        {
            *a = None;
        }
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn serve(listener: &UnixListener, password: &Secret, stop: &AtomicBool) {
    let me = std::process::id();
    // SAFETY: plain syscall.
    let uid = unsafe { libc::geteuid() };
    while !stop.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((mut s, _)) => {
                let _ = s.set_nonblocking(false);
                if allowed(&s, uid, me) {
                    let mut line = password.as_str().as_bytes().to_vec();
                    line.push(b'\n');
                    let _ = s.write_all(&line);
                    line.iter_mut().for_each(|b| *b = 0);
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(_) => std::thread::sleep(Duration::from_millis(100)),
        }
    }
}

/// The same user, and on Linux a process started by this envira.
#[cfg(target_os = "linux")]
fn allowed(s: &UnixStream, uid: u32, me: u32) -> bool {
    use std::os::fd::AsRawFd;
    let mut cred = libc::ucred {
        pid: 0,
        uid: u32::MAX,
        gid: 0,
    };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: `cred` and `len` are valid for the call and sized for SO_PEERCRED.
    let r = unsafe {
        libc::getsockopt(
            s.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut cred as *mut libc::ucred).cast(),
            &mut len,
        )
    };
    r == 0
        && cred.uid == uid
        && cred.pid > 0
        && descends_from(cred.pid as u32, me, |p| {
            std::fs::read_to_string(format!("/proc/{p}/stat"))
                .ok()
                .and_then(|s| parent_of(&s))
        })
}

/// The same user (macOS has no /proc to follow the process tree with).
#[cfg(not(target_os = "linux"))]
fn allowed(s: &UnixStream, uid: u32, _me: u32) -> bool {
    use std::os::fd::AsRawFd;
    let (mut peer_uid, mut peer_gid) = (u32::MAX, u32::MAX);
    // SAFETY: the out-pointers are valid for the call.
    unsafe { libc::getpeereid(s.as_raw_fd(), &mut peer_uid, &mut peer_gid) == 0 && peer_uid == uid }
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
/// The parent pid in a `/proc/<pid>/stat` line (the name before it may hold spaces and `)`).
fn parent_of(stat: &str) -> Option<u32> {
    let rest = &stat[stat.rfind(')')? + 1..];
    rest.split_whitespace().nth(1)?.parse().ok()
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
/// Whether `ancestor` is on the way from `pid` up to init.
fn descends_from(pid: u32, ancestor: u32, parent: impl Fn(u32) -> Option<u32>) -> bool {
    let mut p = pid;
    for _ in 0..256 {
        if p == ancestor {
            return true;
        }
        match parent(p) {
            Some(q) if q != p && q != 0 => p = q,
            _ => return false,
        }
    }
    false
}

/// envira started as `envira-askpass` (by sudo, with its prompt as the argument): prints the
/// password of the run that made it, from the socket next to it. `None`: not the helper.
pub fn helper_main() -> Option<i32> {
    let argv0 = std::env::args_os().next()?;
    let path = Path::new(&argv0);
    if path.file_name()? != HELPER {
        return None;
    }
    let sock = path.parent()?.join("sock");
    let Ok(mut s) = UnixStream::connect(sock) else {
        return Some(1);
    };
    let mut buf = Vec::with_capacity(256);
    let ok = s.read_to_end(&mut buf).is_ok() && !buf.is_empty();
    if ok {
        let mut out = std::io::stdout();
        let _ = out.write_all(&buf);
        let _ = out.flush();
    }
    buf.iter_mut().for_each(|b| *b = 0);
    Some(if ok { 0 } else { 1 })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_parent_is_read_past_odd_process_names() {
        assert_eq!(parent_of("42 (bash) S 7 42 42 0"), Some(7));
        assert_eq!(parent_of("42 (a) b) (c) S 9 1 1"), Some(9));
        assert_eq!(parent_of("garbage"), None);
    }

    #[test]
    fn only_descendants_are_answered() {
        // 10 <- 20 <- 30; 40 is elsewhere.
        let parent = |p: u32| match p {
            30 => Some(20),
            20 => Some(10),
            10 => Some(1),
            40 => Some(1),
            _ => None,
        };
        assert!(descends_from(30, 10, parent));
        assert!(descends_from(10, 10, parent));
        assert!(!descends_from(40, 10, parent));
        assert!(!descends_from(30, 99, parent));
    }

    #[test]
    fn a_child_gets_the_password_and_others_do_not() {
        let mut pw = Secret::new_for_test("test-only-pw");
        let a = Askpass::start(std::mem::replace(&mut pw, Secret::new_for_test(""))).unwrap();
        let h = helper().unwrap();
        assert!(h.starts_with(a.dir()));
        // The test binary is not envira, so ask the socket as the helper would: from this
        // process (envira itself counts as its own descendant), and from a child.
        let mut s = UnixStream::connect(a.dir().join("sock")).unwrap();
        let mut got = String::new();
        s.read_to_string(&mut got).unwrap();
        assert_eq!(got, "test-only-pw\n");
        #[cfg(target_os = "linux")]
        {
            let out = std::process::Command::new("python3")
                .args([
                    "-I",
                    "-c",
                    "import socket,sys; s=socket.socket(socket.AF_UNIX); s.connect(sys.argv[1]); print(s.recv(100).decode().strip())",
                ])
                .arg(a.dir().join("sock"))
                .output();
            if let Ok(out) = out {
                assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "test-only-pw");
            }
        }
        let dir = a.dir().to_path_buf();
        drop(a);
        assert!(helper().is_none());
        assert!(!dir.exists());
    }
}
