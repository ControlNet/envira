use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use users::os::unix::UserExt;
use users::{get_effective_gid, get_effective_uid, get_user_by_name, get_user_by_uid};

use crate::catalog::TargetBackend;

const DEFAULT_OS_RELEASE_PATHS: [&str; 2] = ["/etc/os-release", "/usr/lib/os-release"];

#[derive(Debug, Error)]
pub enum PlatformError {
    #[error("failed to read os-release from `{path}`: {source}")]
    ReadOsRelease {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PlatformContext {
    pub distro: DistroIdentity,
    pub arch: ArchitectureIdentity,
    pub native_backend: Option<TargetBackend>,
    pub invocation: InvocationKind,
    pub effective_user: UserAccount,
    pub target_user: Option<UserAccount>,
    pub runtime_scope: RuntimeScope,
}

impl PlatformContext {
    pub fn detect() -> Result<Self, PlatformError> {
        let os_release = OsRelease::from_default_paths()?;
        let snapshot = DetectionSnapshot::capture(os_release);
        Ok(Self::from_snapshot(&snapshot, &SystemUserDirectory))
    }

    pub fn from_snapshot(snapshot: &DetectionSnapshot, user_directory: &dyn UserDirectory) -> Self {
        let distro = DistroIdentity::from_os_release(&snapshot.os_release);
        let arch = ArchitectureIdentity::from_machine(&snapshot.architecture);
        let native_backend = distro.native_backend();
        let invocation = InvocationKind::from_snapshot(snapshot);
        let effective_user = resolve_effective_user(snapshot, user_directory);
        let target_user =
            resolve_target_user(snapshot, user_directory, invocation, &effective_user);
        let runtime_scope = RuntimeScope::from_context(invocation, target_user.as_ref());

        Self {
            distro,
            arch,
            native_backend,
            invocation,
            effective_user,
            target_user,
            runtime_scope,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DetectionSnapshot {
    pub os_release: OsRelease,
    pub architecture: String,
    pub effective_uid: u32,
    pub effective_gid: u32,
    pub username_env: Option<String>,
    pub home_dir_env: Option<PathBuf>,
    pub sudo_user: Option<String>,
    pub user_env: Option<String>,
}

impl DetectionSnapshot {
    pub fn capture(os_release: OsRelease) -> Self {
        Self {
            os_release,
            architecture: env::consts::ARCH.to_string(),
            effective_uid: get_effective_uid(),
            effective_gid: get_effective_gid(),
            username_env: env::var("USER").ok(),
            home_dir_env: env::var_os("HOME").map(PathBuf::from),
            sudo_user: env::var("SUDO_USER").ok(),
            user_env: env::var("USER").ok(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct OsRelease {
    pub id: Option<String>,
    pub id_like: Vec<String>,
    pub name: Option<String>,
    pub pretty_name: Option<String>,
    pub version_id: Option<String>,
    pub fields: BTreeMap<String, String>,
}

impl Default for OsRelease {
    fn default() -> Self {
        Self {
            id: None,
            id_like: Vec::new(),
            name: None,
            pretty_name: None,
            version_id: None,
            fields: BTreeMap::new(),
        }
    }
}

impl OsRelease {
    pub fn from_default_paths() -> Result<Self, PlatformError> {
        let mut last_error = None;

        for raw_path in DEFAULT_OS_RELEASE_PATHS {
            let path = PathBuf::from(raw_path);
            match fs::read_to_string(&path) {
                Ok(contents) => return Ok(Self::parse(&contents)),
                Err(source) => last_error = Some(PlatformError::ReadOsRelease { path, source }),
            }
        }

        Err(last_error.expect("os-release lookup always attempts at least one path"))
    }

    pub fn parse(raw: &str) -> Self {
        let mut fields = BTreeMap::new();

        for line in raw.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }

            let Some((key, value)) = trimmed.split_once('=') else {
                continue;
            };

            fields.insert(key.trim().to_string(), parse_os_release_value(value.trim()));
        }

        let id_like = fields
            .get("ID_LIKE")
            .map(|value| {
                value
                    .split_whitespace()
                    .map(normalize_token)
                    .filter(|value| !value.is_empty())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        Self {
            id: fields.get("ID").map(|value| normalize_token(value)),
            id_like,
            name: fields.get("NAME").cloned(),
            pretty_name: fields.get("PRETTY_NAME").cloned(),
            version_id: fields.get("VERSION_ID").cloned(),
            fields,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DistroKind {
    Ubuntu,
    Debian,
    LinuxMint,
    Arch,
    Manjaro,
    Fedora,
    Rhel,
    CentOs,
    OpenSuse,
    Unknown,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DistroIdentity {
    pub kind: DistroKind,
    pub id: String,
    pub name: String,
    pub pretty_name: Option<String>,
    pub version_id: Option<String>,
}

impl DistroIdentity {
    pub fn from_os_release(os_release: &OsRelease) -> Self {
        let id = os_release
            .id
            .clone()
            .unwrap_or_else(|| "unknown".to_string());
        let name = os_release
            .name
            .clone()
            .or_else(|| os_release.pretty_name.clone())
            .unwrap_or_else(|| id.clone());
        let candidates = std::iter::once(id.as_str())
            .chain(os_release.id_like.iter().map(String::as_str))
            .collect::<Vec<_>>();

        Self {
            kind: DistroKind::from_candidates(&candidates),
            id,
            name,
            pretty_name: os_release.pretty_name.clone(),
            version_id: os_release.version_id.clone(),
        }
    }

    pub fn native_backend(&self) -> Option<TargetBackend> {
        match self.kind {
            DistroKind::Ubuntu | DistroKind::Debian | DistroKind::LinuxMint => {
                Some(TargetBackend::Apt)
            }
            DistroKind::Arch | DistroKind::Manjaro => Some(TargetBackend::Pacman),
            DistroKind::Fedora | DistroKind::Rhel | DistroKind::CentOs => Some(TargetBackend::Dnf),
            DistroKind::OpenSuse => Some(TargetBackend::Zypper),
            DistroKind::Unknown => None,
        }
    }
}

impl DistroKind {
    fn from_candidates(candidates: &[&str]) -> Self {
        for candidate in candidates {
            match normalize_token(candidate).as_str() {
                "ubuntu" => return Self::Ubuntu,
                "debian" => return Self::Debian,
                "linuxmint" | "mint" => return Self::LinuxMint,
                "arch" | "archlinux" => return Self::Arch,
                "manjaro" => return Self::Manjaro,
                "fedora" => return Self::Fedora,
                "rhel" | "redhat" | "redhatenterpriseserver" => return Self::Rhel,
                "centos" => return Self::CentOs,
                "opensuse" | "opensuse-leap" | "opensuse-tumbleweed" | "sles" => {
                    return Self::OpenSuse
                }
                _ => {}
            }
        }

        Self::Unknown
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArchitectureKind {
    X86_64,
    X86,
    Aarch64,
    Armv7,
    Unknown,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ArchitectureIdentity {
    pub kind: ArchitectureKind,
    pub raw: String,
}

impl ArchitectureIdentity {
    pub fn from_machine(machine: &str) -> Self {
        let raw = normalize_token(machine);
        let kind = match raw.as_str() {
            "x8664" | "amd64" | "x64" => ArchitectureKind::X86_64,
            "x86" | "i386" | "i486" | "i586" | "i686" => ArchitectureKind::X86,
            "aarch64" | "arm64" => ArchitectureKind::Aarch64,
            "armv7" | "armv7l" | "armhf" => ArchitectureKind::Armv7,
            _ => ArchitectureKind::Unknown,
        };

        Self { kind, raw }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InvocationKind {
    User,
    Sudo,
    Root,
}

impl InvocationKind {
    fn from_snapshot(snapshot: &DetectionSnapshot) -> Self {
        if snapshot.effective_uid != 0 {
            return Self::User;
        }

        if snapshot
            .sudo_user
            .as_deref()
            .map(str::trim)
            .is_some_and(|value| !value.is_empty())
        {
            Self::Sudo
        } else {
            Self::Root
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeScope {
    System,
    User,
    Both,
    Unknown,
}

impl RuntimeScope {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::User => "user",
            Self::Both => "both",
            Self::Unknown => "unknown",
        }
    }

    fn from_context(invocation: InvocationKind, target_user: Option<&UserAccount>) -> Self {
        match invocation {
            InvocationKind::User => Self::User,
            InvocationKind::Root => Self::System,
            InvocationKind::Sudo => {
                if target_user.is_some() {
                    Self::Both
                } else {
                    Self::System
                }
            }
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct UserAccount {
    pub username: String,
    pub home_dir: PathBuf,
    pub uid: Option<u32>,
    pub gid: Option<u32>,
}

impl UserAccount {
    pub fn is_root(&self) -> bool {
        self.uid == Some(0) || self.username == "root"
    }
}

pub trait UserDirectory {
    fn by_uid(&self, uid: u32) -> Option<UserAccount>;
    fn by_name(&self, username: &str) -> Option<UserAccount>;
}

pub struct SystemUserDirectory;

impl UserDirectory for SystemUserDirectory {
    fn by_uid(&self, uid: u32) -> Option<UserAccount> {
        get_user_by_uid(uid).map(user_account_from_system)
    }

    fn by_name(&self, username: &str) -> Option<UserAccount> {
        get_user_by_name(username).map(user_account_from_system)
    }
}

fn resolve_effective_user(
    snapshot: &DetectionSnapshot,
    user_directory: &dyn UserDirectory,
) -> UserAccount {
    user_directory
        .by_uid(snapshot.effective_uid)
        .unwrap_or_else(|| fallback_effective_user(snapshot))
}

fn resolve_target_user(
    snapshot: &DetectionSnapshot,
    user_directory: &dyn UserDirectory,
    invocation: InvocationKind,
    effective_user: &UserAccount,
) -> Option<UserAccount> {
    match invocation {
        InvocationKind::User => Some(effective_user.clone()),
        InvocationKind::Root => None,
        InvocationKind::Sudo => snapshot
            .sudo_user
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty() && *value != "root")
            .and_then(|username| user_directory.by_name(username)),
    }
}

fn fallback_effective_user(snapshot: &DetectionSnapshot) -> UserAccount {
    let username = snapshot.username_env.clone().unwrap_or_else(|| {
        if snapshot.effective_uid == 0 {
            "root".to_string()
        } else {
            format!("uid-{}", snapshot.effective_uid)
        }
    });
    let home_dir = snapshot
        .home_dir_env
        .clone()
        .or_else(|| default_home_dir(&username, snapshot.effective_uid))
        .unwrap_or_else(|| PathBuf::from("/"));

    UserAccount {
        username,
        home_dir,
        uid: Some(snapshot.effective_uid),
        gid: Some(snapshot.effective_gid),
    }
}

fn default_home_dir(username: &str, uid: u32) -> Option<PathBuf> {
    if uid == 0 {
        return Some(PathBuf::from("/root"));
    }

    if username.is_empty() {
        None
    } else {
        Some(Path::new("/home").join(username))
    }
}

fn user_account_from_system(user: users::User) -> UserAccount {
    UserAccount {
        username: user.name().to_string_lossy().into_owned(),
        home_dir: user.home_dir().to_path_buf(),
        uid: Some(user.uid()),
        gid: Some(user.primary_group_id()),
    }
}

fn parse_os_release_value(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.len() >= 2 {
        let bytes = trimmed.as_bytes();
        let first = bytes[0];
        let last = bytes[trimmed.len() - 1];
        if (first == b'"' && last == b'"') || (first == b'\'' && last == b'\'') {
            return trimmed[1..trimmed.len() - 1]
                .replace("\\\\", "\\")
                .replace("\\\"", "\"")
                .replace("\\'", "'")
                .replace("\\$", "$");
        }
    }

    trimmed.to_string()
}

fn normalize_token(value: &str) -> String {
    value
        .trim()
        .to_ascii_lowercase()
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .collect()
}
