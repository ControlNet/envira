use std::collections::BTreeMap;
use std::path::PathBuf;

use envira::catalog::TargetBackend;
use envira::platform::{
    ArchitectureIdentity, ArchitectureKind, DetectionSnapshot, DistroKind, InvocationKind,
    OsRelease, PlatformContext, RuntimeScope, UserAccount, UserDirectory,
};

#[derive(Default)]
struct FakeUserDirectory {
    by_uid: BTreeMap<u32, UserAccount>,
    by_name: BTreeMap<String, UserAccount>,
}

impl FakeUserDirectory {
    fn with_user(mut self, user: UserAccount) -> Self {
        if let Some(uid) = user.uid {
            self.by_uid.insert(uid, user.clone());
        }
        self.by_name.insert(user.username.clone(), user);
        self
    }
}

impl UserDirectory for FakeUserDirectory {
    fn by_uid(&self, uid: u32) -> Option<UserAccount> {
        self.by_uid.get(&uid).cloned()
    }

    fn by_name(&self, username: &str) -> Option<UserAccount> {
        self.by_name.get(username).cloned()
    }
}

#[test]
fn normal_user_context_resolves_user_scope_and_target() {
    let directory =
        FakeUserDirectory::default().with_user(user("alice", "/home/alice", 1000, 1000));
    let snapshot = snapshot(
        r#"
ID=ubuntu
NAME="Ubuntu"
PRETTY_NAME="Ubuntu 24.04 LTS"
VERSION_ID="24.04"
"#,
        "x86_64",
        1000,
        1000,
        Some("alice"),
        Some("/home/alice"),
        None,
    );

    let context = PlatformContext::from_snapshot(&snapshot, &directory);

    assert_eq!(context.distro.kind, DistroKind::Ubuntu);
    assert_eq!(context.distro.name, "Ubuntu");
    assert_eq!(context.native_backend, Some(TargetBackend::Apt));
    assert_eq!(context.arch.kind, ArchitectureKind::X86_64);
    assert_eq!(context.invocation, InvocationKind::User);
    assert_eq!(context.runtime_scope, RuntimeScope::User);
    assert_eq!(context.runtime_scope.as_str(), "user");
    assert_eq!(context.effective_user.username, "alice");
    assert_eq!(
        context.target_user.expect("user target exists").home_dir,
        PathBuf::from("/home/alice")
    );
}

#[test]
fn sudo_context_targets_original_user_home_without_root_leakage() {
    let directory = FakeUserDirectory::default()
        .with_user(user("root", "/root", 0, 0))
        .with_user(user("alice", "/home/alice", 1000, 1000));
    let snapshot = snapshot(
        r#"
ID=ubuntu
NAME="Ubuntu"
"#,
        "amd64",
        0,
        0,
        Some("root"),
        Some("/root"),
        Some("alice"),
    );

    let context = PlatformContext::from_snapshot(&snapshot, &directory);

    assert_eq!(context.invocation, InvocationKind::Sudo);
    assert_eq!(context.runtime_scope, RuntimeScope::Both);
    assert_eq!(context.runtime_scope.as_str(), "both");
    assert_eq!(context.effective_user.home_dir, PathBuf::from("/root"));

    let target_user = context.target_user.expect("sudo target user exists");
    assert_eq!(target_user.username, "alice");
    assert_eq!(target_user.home_dir, PathBuf::from("/home/alice"));
    assert_ne!(target_user.home_dir, PathBuf::from("/root"));
}

#[test]
fn distro_normalization_uses_id_like_and_preserves_exact_name() {
    let context = PlatformContext::from_snapshot(
        &snapshot(
            r#"
ID=linuxmint
ID_LIKE="ubuntu debian"
NAME="Linux Mint"
PRETTY_NAME="Linux Mint 22"
"#,
            "x86_64",
            1000,
            1000,
            Some("minty"),
            Some("/home/minty"),
            None,
        ),
        &FakeUserDirectory::default().with_user(user("minty", "/home/minty", 1000, 1000)),
    );

    assert_eq!(context.distro.kind, DistroKind::LinuxMint);
    assert_eq!(context.distro.name, "Linux Mint");
    assert_eq!(context.native_backend, Some(TargetBackend::Apt));
}

#[test]
fn architecture_normalization_maps_common_aliases_deterministically() {
    let amd64 = ArchitectureIdentity::from_machine("amd64");
    let arm64 = ArchitectureIdentity::from_machine("arm64");
    let x86 = ArchitectureIdentity::from_machine("i686");

    assert_eq!(amd64.kind, ArchitectureKind::X86_64);
    assert_eq!(amd64.raw, "amd64");
    assert_eq!(arm64.kind, ArchitectureKind::Aarch64);
    assert_eq!(x86.kind, ArchitectureKind::X86);
}

#[test]
fn unknown_platform_values_fall_back_without_guessing() {
    let snapshot = snapshot(
        r#"
ID=nixos
NAME="NixOS"
"#,
        "sparc64",
        1001,
        1001,
        Some("builder"),
        Some("/srv/builder"),
        None,
    );

    let context = PlatformContext::from_snapshot(&snapshot, &FakeUserDirectory::default());

    assert_eq!(context.distro.kind, DistroKind::Unknown);
    assert_eq!(context.distro.id, "nixos");
    assert_eq!(context.distro.name, "NixOS");
    assert_eq!(context.native_backend, None);
    assert_eq!(context.arch.kind, ArchitectureKind::Unknown);
    assert_eq!(context.arch.raw, "sparc64");
    assert_eq!(context.runtime_scope.as_str(), "user");
    assert_eq!(
        context.target_user.expect("target user exists").home_dir,
        PathBuf::from("/srv/builder")
    );
}

fn snapshot(
    os_release_raw: &str,
    architecture: &str,
    effective_uid: u32,
    effective_gid: u32,
    username_env: Option<&str>,
    home_dir_env: Option<&str>,
    sudo_user: Option<&str>,
) -> DetectionSnapshot {
    DetectionSnapshot {
        os_release: OsRelease::parse(os_release_raw),
        architecture: architecture.to_string(),
        effective_uid,
        effective_gid,
        username_env: username_env.map(ToOwned::to_owned),
        home_dir_env: home_dir_env.map(PathBuf::from),
        sudo_user: sudo_user.map(ToOwned::to_owned),
        user_env: username_env.map(ToOwned::to_owned),
    }
}

fn user(username: &str, home_dir: &str, uid: u32, gid: u32) -> UserAccount {
    UserAccount {
        username: username.to_string(),
        home_dir: PathBuf::from(home_dir),
        uid: Some(uid),
        gid: Some(gid),
    }
}
