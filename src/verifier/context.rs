use std::collections::BTreeSet;
use std::env;
use std::path::{Path, PathBuf};

use crate::platform::{InvocationKind, PlatformContext};
use crate::verifier::{ObservedScope, VerificationProfile};

const DEFAULT_USER_BIN_SUFFIXES: [&str; 10] = [
    ".local/bin",
    ".cargo/bin",
    "go/bin",
    ".go/bin",
    ".fzf/bin",
    ".local/share/fnm",
    "miniconda3/bin",
    ".pixi/bin",
    ".bun/bin",
    ".opencode/bin",
];

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerificationContext {
    pub platform: PlatformContext,
    pub requested_profile: VerificationProfile,
    pub search_paths: Vec<PathBuf>,
}

impl VerificationContext {
    pub fn new(platform: PlatformContext, requested_profile: VerificationProfile) -> Self {
        Self {
            search_paths: collect_search_paths(&platform),
            platform,
            requested_profile,
        }
    }

    pub fn with_search_paths(mut self, search_paths: Vec<PathBuf>) -> Self {
        self.search_paths = search_paths;
        self
    }

    pub fn command_timeout_ms(&self) -> u64 {
        match self.requested_profile {
            VerificationProfile::Quick => 1_000,
            VerificationProfile::Standard => 3_000,
            VerificationProfile::Strict => 5_000,
        }
    }

    pub fn socket_timeout_ms(&self) -> u64 {
        match self.requested_profile {
            VerificationProfile::Quick => 300,
            VerificationProfile::Standard => 750,
            VerificationProfile::Strict => 1_500,
        }
    }

    pub fn http_timeout_ms(&self) -> u64 {
        match self.requested_profile {
            VerificationProfile::Quick => 500,
            VerificationProfile::Standard => 1_000,
            VerificationProfile::Strict => 2_000,
        }
    }

    pub fn command_path_env(&self) -> Option<String> {
        env::join_paths(&self.search_paths)
            .ok()
            .map(|value| value.to_string_lossy().into_owned())
    }

    pub fn resolve_command(&self, command: &str) -> Option<PathBuf> {
        let command_path = Path::new(command);
        if command_path.components().count() > 1 {
            return is_executable_file(command_path).then(|| command_path.to_path_buf());
        }

        self.search_paths.iter().find_map(|directory| {
            let candidate = directory.join(command);
            is_executable_file(&candidate).then_some(candidate)
        })
    }

    pub fn observed_scope_for_path(&self, path: &Path) -> ObservedScope {
        let normalized = path;

        if self
            .user_home_dirs()
            .iter()
            .any(|home_dir| normalized.starts_with(home_dir))
        {
            return ObservedScope::User;
        }

        if normalized.is_absolute() {
            return ObservedScope::System;
        }

        ObservedScope::Unknown
    }

    pub fn default_username(&self) -> Option<&str> {
        self.platform
            .target_user
            .as_ref()
            .map(|user| user.username.as_str())
            .or_else(|| {
                (!self.platform.effective_user.is_root())
                    .then_some(self.platform.effective_user.username.as_str())
            })
    }

    pub fn default_user_gid(&self) -> Option<u32> {
        if let Some(gid) = self.platform.target_user.as_ref().and_then(|user| user.gid) {
            Some(gid)
        } else if self.platform.effective_user.is_root() {
            None
        } else {
            self.platform.effective_user.gid
        }
    }

    fn user_home_dirs(&self) -> Vec<&PathBuf> {
        let mut homes = Vec::new();

        if let Some(target_user) = self.platform.target_user.as_ref() {
            if !target_user.is_root() {
                homes.push(&target_user.home_dir);
            }
        }

        if self.platform.invocation == InvocationKind::User
            && !self.platform.effective_user.is_root()
        {
            homes.push(&self.platform.effective_user.home_dir);
        }

        homes
    }
}

fn collect_search_paths(platform: &PlatformContext) -> Vec<PathBuf> {
    let mut search_paths = Vec::new();
    let mut seen = BTreeSet::new();

    for home_dir in candidate_home_dirs(platform) {
        for suffix in DEFAULT_USER_BIN_SUFFIXES {
            let path = home_dir.join(suffix);
            if seen.insert(path.clone()) {
                search_paths.push(path);
            }
        }
    }

    if let Some(path) = env::var_os("PATH") {
        for entry in env::split_paths(&path) {
            if seen.insert(entry.clone()) {
                search_paths.push(entry);
            }
        }
    }

    search_paths
}

fn candidate_home_dirs(platform: &PlatformContext) -> Vec<&PathBuf> {
    let mut homes = Vec::new();

    if let Some(target_user) = platform.target_user.as_ref() {
        if !target_user.is_root() {
            homes.push(&target_user.home_dir);
        }
    }

    if platform.invocation == InvocationKind::User && !platform.effective_user.is_root() {
        homes.push(&platform.effective_user.home_dir);
    }

    homes
}

fn is_executable_file(path: &Path) -> bool {
    std::fs::metadata(path)
        .map(|metadata| {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;

                metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
            }

            #[cfg(not(unix))]
            {
                metadata.is_file()
            }
        })
        .unwrap_or(false)
}
