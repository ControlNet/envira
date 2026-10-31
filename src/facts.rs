//! Machine facts, probed once per run.

use std::ffi::{CStr, CString};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Privilege {
    Root,
    /// Can use sudo; `passwordless` when `sudo -n true` already works.
    Sudo {
        passwordless: bool,
    },
    None,
}

impl Privilege {
    pub fn can_elevate(self) -> bool {
        !matches!(self, Privilege::None)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Facts {
    /// `linux` or `macos` (`std::env::consts::OS`).
    pub os: String,
    pub distro_id: String,
    /// `PRETTY_NAME` or `NAME VERSION_ID`.
    pub distro_name: String,
    /// `ID` followed by `ID_LIKE` entries.
    pub families: Vec<String>,
    pub arch: String,
    pub privilege: Privilege,
    pub gpu: bool,
    pub container: bool,
    pub wsl: bool,
    pub shells: Vec<String>,
    pub hostname: String,
    /// Whose home user-scope installs, state and shell files belong to.
    pub user: String,
    pub home: PathBuf,
    pub uid: u32,
    pub gid: u32,
    /// Set when running as root on behalf of `SUDO_USER`: user-scope commands drop to this user.
    pub drop_to: Option<String>,
}

impl Facts {
    pub fn probe() -> Facts {
        let os = std::env::consts::OS.to_string();
        let (distro_id, distro_name, families) = if os == "macos" {
            let v = Command::new("sw_vers")
                .arg("-productVersion")
                .output()
                .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
                .unwrap_or_default();
            macos_release(&v)
        } else {
            parse_os_release(&std::fs::read_to_string("/etc/os-release").unwrap_or_default())
        };
        let euid = unsafe { libc::geteuid() };
        let (user, home, uid, gid, drop_to) = target_user(euid);
        let privilege = match std::env::var("ENVIRA_PRIVILEGE").ok().as_deref() {
            Some("none") => Privilege::None,
            Some("sudo") => Privilege::Sudo { passwordless: true },
            _ if euid == 0 => Privilege::Root,
            _ => probe_sudo(&user),
        };
        Facts {
            os,
            distro_id,
            distro_name,
            families,
            arch: std::env::consts::ARCH.to_string(),
            privilege,
            gpu: Path::new("/proc/driver/nvidia").exists() || which("nvidia-smi"),
            container: Path::new("/.dockerenv").exists()
                || Path::new("/run/.containerenv").exists(),
            wsl: std::fs::read_to_string("/proc/sys/kernel/osrelease")
                .map(|s| s.to_lowercase().contains("microsoft"))
                .unwrap_or(false),
            shells: ["bash", "zsh"]
                .iter()
                .filter(|s| which(s))
                .map(|s| s.to_string())
                .collect(),
            hostname: hostname(),
            user,
            home,
            uid,
            gid,
            drop_to,
        }
    }

    pub fn is_root(&self) -> bool {
        self.privilege == Privilege::Root
    }

    /// Expands a leading `~/` (or a lone `~`) to the target home.
    pub fn expand_home(&self, s: &str) -> String {
        if s == "~" {
            self.home.display().to_string()
        } else if let Some(rest) = s.strip_prefix("~/") {
            format!("{}/{rest}", self.home.display())
        } else {
            s.to_string()
        }
    }

    pub fn has_shell(&self, shell: &str) -> bool {
        self.shells.iter().any(|s| s == shell)
    }

    /// One-line summary for headers, e.g. `ubuntu 24.04 x86_64 · user · sudo ✓ · nvidia`.
    pub fn summary(&self) -> String {
        let sudo = match self.privilege {
            Privilege::Root => "root",
            Privilege::Sudo { .. } => "sudo ✓",
            Privilege::None => "no sudo",
        };
        let mut s = format!(
            "{} {} · {} · {}",
            self.distro_name, self.arch, self.user, sudo
        );
        if self.gpu {
            s.push_str(" · nvidia");
        }
        if self.container {
            s.push_str(" · container");
        }
        if self.wsl {
            s.push_str(" · wsl");
        }
        s
    }

    /// Facts for tests: Ubuntu, x86_64, sudo, no GPU, home under the given dir.
    #[cfg(test)]
    pub fn sample(home: &Path) -> Facts {
        Facts {
            os: "linux".into(),
            distro_id: "ubuntu".into(),
            distro_name: "Ubuntu 24.04".into(),
            families: vec!["ubuntu".into(), "debian".into()],
            arch: "x86_64".into(),
            privilege: Privilege::Sudo { passwordless: true },
            gpu: false,
            container: false,
            wsl: false,
            shells: vec!["bash".into(), "zsh".into()],
            hostname: "workstation".into(),
            user: "user".into(),
            home: home.to_path_buf(),
            uid: 1000,
            gid: 1000,
            drop_to: None,
        }
    }
}

pub fn parse_os_release(text: &str) -> (String, String, Vec<String>) {
    let mut id = String::from("linux");
    let mut like = Vec::new();
    let mut pretty = None;
    let mut name = None;
    let mut version = None;
    for line in text.lines() {
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let v = v.trim().trim_matches('"').trim_matches('\'').to_string();
        match k.trim() {
            "ID" => id = v.to_lowercase(),
            "ID_LIKE" => like = v.split_whitespace().map(|s| s.to_lowercase()).collect(),
            "PRETTY_NAME" => pretty = Some(v),
            "NAME" => name = Some(v),
            "VERSION_ID" => version = Some(v),
            _ => {}
        }
    }
    let display = match version {
        Some(v) => format!("{id} {v}"),
        None => pretty.or(name).unwrap_or_else(|| id.clone()),
    };
    let mut families = vec![id.clone()];
    families.extend(like.into_iter().filter(|f| *f != id));
    (id, display, families)
}

/// `(distro_id, display name, families)` of macOS from `sw_vers -productVersion`.
pub fn macos_release(version: &str) -> (String, String, Vec<String>) {
    let v = version.trim();
    let name = if v.is_empty() {
        "macOS".to_string()
    } else {
        format!("macOS {v}")
    };
    ("macos".into(), name, vec!["macos".into()])
}

fn hostname() -> String {
    let mut buf = [0u8; 256];
    // SAFETY: gethostname writes at most `len` bytes into the buffer.
    let ok = unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) } == 0;
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    match std::str::from_utf8(&buf[..end]) {
        Ok(h) if ok && !h.is_empty() => h.to_string(),
        _ => "localhost".into(),
    }
}

fn target_user(euid: u32) -> (String, PathBuf, u32, u32, Option<String>) {
    if euid == 0
        && let Ok(sudo_user) = std::env::var("SUDO_USER")
        && !sudo_user.is_empty()
        && sudo_user != "root"
        && let Some((home, uid, gid)) = passwd_by_name(&sudo_user)
    {
        return (sudo_user.clone(), home, uid, gid, Some(sudo_user));
    }
    let (name, pw_home, gid) = passwd_by_uid(euid).unwrap_or_else(|| {
        let name = ["USER", "LOGNAME"]
            .into_iter()
            .find_map(|v| std::env::var(v).ok().filter(|n| !n.is_empty()))
            .unwrap_or_else(|| "user".into());
        (name, PathBuf::from("/"), euid)
    });
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or(pw_home);
    (name, home, euid, gid, None)
}

/// One `/etc/passwd`-format line: name, uid, gid, home.
fn parse_passwd(line: &str) -> Option<(String, u32, u32, PathBuf)> {
    let f: Vec<&str> = line.trim_end().split(':').collect();
    if f.len() < 7 {
        return None;
    }
    Some((
        f[0].into(),
        f[2].parse().ok()?,
        f[3].parse().ok()?,
        f[5].into(),
    ))
}

/// The static (musl) build has no NSS, so libc only sees `/etc/passwd`; users from LDAP or SSSD
/// are found through the system's `getent`, which uses the system's libc.
fn getent_passwd(key: &str) -> Option<(String, u32, u32, PathBuf)> {
    let out = Command::new("getent")
        .args(["passwd", key])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| parse_passwd(String::from_utf8_lossy(&out.stdout).lines().next()?))?
}

fn passwd_by_name(name: &str) -> Option<(PathBuf, u32, u32)> {
    libc_passwd_by_name(name).or_else(|| {
        let (_, uid, gid, home) = getent_passwd(name)?;
        Some((home, uid, gid))
    })
}

fn passwd_by_uid(uid: u32) -> Option<(String, PathBuf, u32)> {
    libc_passwd_by_uid(uid).or_else(|| {
        let (name, _, gid, home) = getent_passwd(&uid.to_string())?;
        Some((name, home, gid))
    })
}

fn libc_passwd_by_name(name: &str) -> Option<(PathBuf, u32, u32)> {
    let c = CString::new(name).ok()?;
    // SAFETY: getpwnam returns a pointer to static storage or null; fields are read immediately.
    unsafe {
        let pw = libc::getpwnam(c.as_ptr());
        if pw.is_null() {
            return None;
        }
        let home = CStr::from_ptr((*pw).pw_dir).to_string_lossy().into_owned();
        Some((PathBuf::from(home), (*pw).pw_uid, (*pw).pw_gid))
    }
}

fn libc_passwd_by_uid(uid: u32) -> Option<(String, PathBuf, u32)> {
    // SAFETY: as above.
    unsafe {
        let pw = libc::getpwuid(uid);
        if pw.is_null() {
            return None;
        }
        let name = CStr::from_ptr((*pw).pw_name).to_string_lossy().into_owned();
        let home = CStr::from_ptr((*pw).pw_dir).to_string_lossy().into_owned();
        Some((name, PathBuf::from(home), (*pw).pw_gid))
    }
}

fn probe_sudo(user: &str) -> Privilege {
    if !which("sudo") {
        return Privilege::None;
    }
    let ok = Command::new("sudo")
        .args(["-n", "true"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    if ok {
        return Privilege::Sudo { passwordless: true };
    }
    let groups = Command::new("id")
        .args(["-nG", user])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    if groups
        .split_whitespace()
        .any(|g| matches!(g, "sudo" | "wheel" | "admin"))
    {
        Privilege::Sudo {
            passwordless: false,
        }
    } else {
        Privilege::None
    }
}

/// True when `name` is an executable on the caller's PATH.
pub fn which(name: &str) -> bool {
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|d| is_executable(&d.join(name))))
        .unwrap_or(false)
}

pub fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    p.metadata()
        .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passwd_lines() {
        assert_eq!(
            parse_passwd("alice:*:12345:100:Alice:/home/alice:/bin/zsh\n"),
            Some(("alice".into(), 12345, 100, "/home/alice".into()))
        );
        assert_eq!(parse_passwd("broken:x:1"), None);
        assert_eq!(parse_passwd("x:x:uid:1:g:/h:/bin/sh"), None);
        // The current user resolves either way (libc here, getent in the static build).
        let uid = unsafe { libc::geteuid() };
        assert!(passwd_by_uid(uid).is_some());
        #[cfg(target_os = "linux")]
        assert_eq!(getent_passwd(&uid.to_string()).map(|p| p.1), Some(uid));
    }

    #[test]
    fn macos_identity() {
        assert_eq!(
            macos_release("15.1\n"),
            ("macos".into(), "macOS 15.1".into(), vec!["macos".into()])
        );
        assert_eq!(macos_release("").1, "macOS");
    }

    #[test]
    fn os_release_families() {
        let (id, name, fam) = parse_os_release(
            "NAME=\"Linux Mint\"\nVERSION_ID=\"22\"\nID=linuxmint\nID_LIKE=\"ubuntu debian\"\n",
        );
        assert_eq!(id, "linuxmint");
        assert_eq!(name, "linuxmint 22");
        assert_eq!(fam, vec!["linuxmint", "ubuntu", "debian"]);
    }

    #[test]
    fn os_release_ubuntu_display() {
        let (_, name, _) =
            parse_os_release("NAME=\"Ubuntu\"\nVERSION_ID=\"24.04\"\nID=ubuntu\nID_LIKE=debian\n");
        assert_eq!(name, "ubuntu 24.04");
    }
}
