//! `envira self-update`, and the notice that a newer envira is out. envira never updates itself
//! unasked: commands and the TUI only say so, and `self-update` (or `U` in the TUI) does it the
//! way this copy was installed (release binary, pipx, uv, pip or cargo).

use std::io::{IsTerminal, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Local};
use serde::{Deserialize, Serialize};

use crate::facts::{Facts, Privilege};
use crate::latest::{ERROR_TTL, Fetch, TTL};
use crate::state::{self, Paths};
use crate::version::is_newer;

pub const CURRENT: &str = env!("CARGO_PKG_VERSION");
const RELEASES: &str = "https://github.com/ControlNet/envira/releases";
/// Commands and the TUI wait this long for the release page; `self-update` waits longer.
pub const NOTICE_TIMEOUT: Duration = Duration::from_secs(3);
const UPDATE_TIMEOUT: Duration = Duration::from_secs(20);

/// The releases page; `ENVIRA_RELEASES_URL` points at a mirror (or a test server).
fn releases() -> String {
    std::env::var("ENVIRA_RELEASES_URL")
        .ok()
        .filter(|u| !u.is_empty())
        .unwrap_or_else(|| RELEASES.into())
        .trim_end_matches('/')
        .to_string()
}

/// How this envira was installed, which decides how it updates itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Channel {
    /// A release binary (the install script, or copied by hand): replaced in place.
    Binary,
    Pipx,
    Uv,
    /// pip into a virtualenv (its python) or with `--user` (`None`).
    Pip(Option<PathBuf>),
    Cargo,
    /// Built from a source checkout (`target/debug`, `target/release`).
    Source,
}

impl Channel {
    pub fn label(&self) -> &'static str {
        match self {
            Channel::Binary => "release binary",
            Channel::Pipx => "pipx",
            Channel::Uv => "uv tool",
            Channel::Pip(Some(_)) => "pip (virtualenv)",
            Channel::Pip(None) => "pip --user",
            Channel::Cargo => "cargo",
            Channel::Source => "source build",
        }
    }

    /// The package-manager command that installs `version` (latest when `None`); `None` for the
    /// release binary and source builds. `binstall`: cargo-binstall is available; it only
    /// downloads a prebuilt binary (a build from source is [`Channel::fallback`], shown first).
    pub fn command(&self, version: Option<&str>, binstall: bool) -> Option<Vec<String>> {
        let pinned =
            |sep: &str| version.map_or("envira".to_string(), |v| format!("envira{sep}{v}"));
        let words: Vec<String> = match self {
            Channel::Binary | Channel::Source => return None,
            Channel::Pipx if version.is_none() => {
                vec!["pipx".into(), "upgrade".into(), "envira".into()]
            }
            Channel::Pipx => vec![
                "pipx".into(),
                "install".into(),
                "--force".into(),
                pinned("=="),
            ],
            Channel::Uv if version.is_none() => {
                vec![
                    "uv".into(),
                    "tool".into(),
                    "upgrade".into(),
                    "envira".into(),
                ]
            }
            Channel::Uv => vec![
                "uv".into(),
                "tool".into(),
                "install".into(),
                "--force".into(),
                pinned("=="),
            ],
            Channel::Pip(python) => {
                let py = python
                    .as_ref()
                    .map_or("python3".to_string(), |p| p.display().to_string());
                let mut w = vec![py, "-m".into(), "pip".into(), "install".into()];
                if python.is_none() {
                    w.push("--user".into());
                }
                w.extend(["--upgrade".into(), pinned("==")]);
                w
            }
            Channel::Cargo if binstall => vec![
                "cargo".into(),
                "binstall".into(),
                "-y".into(),
                "--force".into(),
                "--disable-strategies".into(),
                "compile".into(),
                pinned("@"),
            ],
            Channel::Cargo => {
                let mut w = vec![
                    "cargo".into(),
                    "install".into(),
                    "--locked".into(),
                    "--force".into(),
                ];
                if let Some(v) = version {
                    w.extend(["--version".into(), v.to_string()]);
                }
                w.push("envira".into());
                w
            }
        };
        Some(words)
    }

    /// What runs when `command` fails: with cargo-binstall and no prebuilt binary for this
    /// platform, a build from source with `cargo install` (minutes, one job per GiB free).
    pub fn fallback(&self, version: Option<&str>, binstall: bool) -> Option<Vec<String>> {
        match self {
            Channel::Cargo if binstall => self.command(version, false),
            _ => None,
        }
    }
}

/// The channel of the binary at `exe` (canonical path). `cargo_bin` is `$CARGO_HOME/bin`;
/// `pip_record(site_packages)` says whether an envira dist-info there lists a `bin/envira`.
pub fn detect(exe: &Path, cargo_bin: &Path, pip_record: impl Fn(&Path) -> bool) -> Channel {
    let s = exe.to_string_lossy();
    let dir = exe.parent().unwrap_or(Path::new("/"));
    let in_target = dir
        .ancestors()
        .skip(1)
        .any(|a| a.file_name().is_some_and(|n| n == "target"));
    if in_target
        && dir
            .file_name()
            .is_some_and(|n| n == "debug" || n == "release")
    {
        return Channel::Source;
    }
    if s.contains("/pipx/venvs/envira/") {
        return Channel::Pipx;
    }
    if s.contains("/uv/tools/envira/") {
        return Channel::Uv;
    }
    if dir == cargo_bin {
        return Channel::Cargo;
    }
    // A virtualenv's bin, or the bin next to a user site (~/.local/bin, ~/Library/Python/3.x/bin).
    let root = dir.parent().unwrap_or(Path::new("/"));
    let owned = std::fs::read_dir(root.join("lib"))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("python"))
        .any(|e| pip_record(&e.path().join("site-packages")));
    if owned {
        let venv = root.join("pyvenv.cfg").exists();
        return Channel::Pip(venv.then(|| dir.join("python")));
    }
    Channel::Binary
}

/// Whether pip installed `bin/envira` from `site_packages` (an `envira-*.dist-info/RECORD`).
fn pip_owns(site_packages: &Path) -> bool {
    std::fs::read_dir(site_packages)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            n.starts_with("envira-") && n.ends_with(".dist-info")
        })
        .any(|e| {
            std::fs::read_to_string(e.path().join("RECORD"))
                .is_ok_and(|r| r.lines().any(|l| l.contains("/bin/envira,")))
        })
}

fn cargo_bin(facts: &Facts) -> PathBuf {
    std::env::var_os("CARGO_HOME")
        .filter(|_| facts.drop_to.is_none())
        .map(PathBuf::from)
        .unwrap_or_else(|| facts.home.join(".cargo"))
        .join("bin")
}

/// This envira's canonical path and how it was installed.
pub fn this_install(facts: &Facts) -> Result<(PathBuf, Channel)> {
    let exe = std::env::current_exe().context("cannot find the envira binary")?;
    let exe = live_path(&exe)
        .canonicalize()
        .with_context(|| format!("cannot find the envira binary ({})", exe.display()))?;
    let channel = detect(&exe, &cargo_bin(facts), pip_owns);
    Ok((exe, channel))
}

/// Once the binary was replaced (by an update), Linux names the running one `<path> (deleted)`;
/// the path itself still holds envira.
fn live_path(exe: &Path) -> PathBuf {
    let s = exe.to_string_lossy();
    PathBuf::from(s.strip_suffix(" (deleted)").unwrap_or(&s))
}

/// The version in a `…/releases/tag/v1.2.3` redirect.
pub fn tag_version(location: &str) -> Option<String> {
    let tag = location.trim_end_matches('/').rsplit_once("/tag/")?.1;
    let v = tag.strip_prefix('v').unwrap_or(tag);
    (!v.is_empty() && v.starts_with(|c: char| c.is_ascii_digit())).then(|| v.to_string())
}

/// The latest release, from where `releases/latest` redirects (no API call, so no rate limit).
pub fn latest_release(timeout: Duration) -> Result<String> {
    let url = format!("{}/latest", releases());
    let c = reqwest::blocking::Client::builder()
        .user_agent(concat!("envira/", env!("CARGO_PKG_VERSION")))
        .redirect(reqwest::redirect::Policy::none())
        .timeout(timeout)
        .build()?;
    let r = c
        .get(&url)
        .send()
        .with_context(|| format!("requesting {url}"))?;
    let loc = r
        .headers()
        .get(reqwest::header::LOCATION)
        .and_then(|l| l.to_str().ok())
        .with_context(|| format!("{url} answered HTTP {} without a redirect", r.status()))?;
    tag_version(loc).with_context(|| format!("{url} redirects to {loc}, not a release tag"))
}

/// `self-update.toml` in the state dir: the last answer about the latest release.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Cache {
    latest: Option<String>,
    error: Option<String>,
    checked_at: Option<String>,
}

impl Cache {
    fn path(paths: &Paths) -> PathBuf {
        paths.state_dir.join("self-update.toml")
    }

    fn load(paths: &Paths) -> Cache {
        std::fs::read_to_string(Self::path(paths))
            .ok()
            .and_then(|t| toml::from_str(&t).ok())
            .unwrap_or_default()
    }

    fn fresh(&self, now: DateTime<Local>) -> bool {
        let ttl = if self.latest.is_some() {
            TTL
        } else {
            ERROR_TTL
        };
        self.checked_at
            .as_deref()
            .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
            .is_some_and(|t| now.signed_duration_since(t) < ttl)
    }

    fn save(&self, paths: &Paths, facts: &Facts) {
        if state::create_dir_all_owned(&paths.state_dir, facts).is_ok()
            && let Ok(t) = toml::to_string(self)
        {
            let _ = state::write_atomic(&Self::path(paths), t.as_bytes(), facts);
        }
    }
}

/// The latest release per `fetch` (`IfStale`: the cached answer while it is fresh). `None` when
/// unknown; errors are cached too, so an offline machine does not wait on every command.
pub fn latest(paths: &Paths, facts: &Facts, fetch: Fetch, timeout: Duration) -> Option<String> {
    let cache = Cache::load(paths);
    let now = Local::now();
    if fetch == Fetch::CacheOnly || (fetch == Fetch::IfStale && cache.fresh(now)) {
        return cache.latest;
    }
    let got = latest_release(timeout);
    let cache = Cache {
        latest: got.as_ref().ok().cloned(),
        error: got.as_ref().err().map(|e| format!("{e:#}")),
        checked_at: Some(now.to_rfc3339()),
    };
    cache.save(paths, facts);
    cache.latest
}

/// Whether commands should check for and mention a newer envira: on a terminal, outside CI,
/// unless `ENVIRA_NO_UPDATE_CHECK` is set.
pub fn check_enabled() -> bool {
    let set = |v: &str| std::env::var(v).is_ok_and(|s| !s.is_empty() && s != "0" && s != "false");
    !set("ENVIRA_NO_UPDATE_CHECK") && !set("CI")
}

/// The newer release, if there is one and this copy can update to it.
pub fn newer(paths: &Paths, facts: &Facts, fetch: Fetch) -> Option<String> {
    if !check_enabled() || matches!(this_install(facts), Ok((_, Channel::Source)) | Err(_)) {
        return None;
    }
    latest(paths, facts, fetch, NOTICE_TIMEOUT).filter(|v| is_newer(v, CURRENT))
}

/// The line commands print on stderr when a newer envira is out.
pub fn notice(paths: &Paths, facts: &Facts) {
    if !std::io::stderr().is_terminal() {
        return;
    }
    if let Some(v) = newer(paths, facts, Fetch::IfStale) {
        eprintln!("envira {v} is available (this is {CURRENT}): envira self-update");
    }
}

pub struct Opts {
    pub check: bool,
    pub version: Option<String>,
    pub yes: bool,
}

pub enum Outcome {
    /// Exit code; nothing was replaced.
    Unchanged(i32),
    /// The binary at this path is the new version.
    Updated(PathBuf),
}

fn tilde(facts: &Facts, p: &Path) -> String {
    match p.strip_prefix(&facts.home) {
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => p.display().to_string(),
    }
}

/// `envira self-update`: checks, confirms, then updates the way this copy was installed. Runs
/// without the catalog, so it works when the published catalog needs a newer envira.
pub fn run(o: &Opts) -> Result<Outcome> {
    let facts = Facts::probe();
    let paths = Paths::new(&facts);
    let (exe, channel) = this_install(&facts)?;
    let pinned = o
        .version
        .as_deref()
        .map(|v| v.trim_start_matches('v').to_string());
    let target = match &pinned {
        Some(v) => v.clone(),
        None => {
            let v = latest_release(UPDATE_TIMEOUT).context("cannot find the latest release")?;
            Cache {
                latest: Some(v.clone()),
                error: None,
                checked_at: Some(Local::now().to_rfc3339()),
            }
            .save(&paths, &facts);
            v
        }
    };
    let where_ = format!("{} · {}", channel.label(), tilde(&facts, &exe));
    if o.check {
        println!("envira {CURRENT} · latest {target} · installed via {where_}");
        return Ok(Outcome::Unchanged(if is_newer(&target, CURRENT) {
            println!("Update with: envira self-update");
            1
        } else {
            0
        }));
    }
    if pinned.is_none() && !is_newer(&target, CURRENT) {
        println!("envira {CURRENT} is the latest release.");
        return Ok(Outcome::Unchanged(0));
    }
    if target == CURRENT {
        println!("envira {CURRENT} is already installed.");
        return Ok(Outcome::Unchanged(0));
    }
    if channel == Channel::Source {
        bail!(
            "{} is a build from a source checkout; update it with git pull and cargo build",
            exe.display()
        );
    }
    // `pip install --user` would put a second copy in the user site, not replace this one.
    if channel == Channel::Pip(None) && !in_user_base(&facts, &exe) {
        bail!(
            "{} was installed by pip outside your user site; update it with the pip that \
             installed it (for example `sudo python3 -m pip install --upgrade envira`)",
            exe.display()
        );
    }
    // Only cargo installs use it; looked up, not run (a probe with no deadline could hold the
    // update before it even shows its plan).
    let binstall = channel == Channel::Cargo
        && (crate::facts::which("cargo-binstall")
            || cargo_bin(&facts).join("cargo-binstall").exists());
    let cmd = channel.command(pinned.as_deref(), binstall);
    let fallback = channel.fallback(pinned.as_deref(), binstall);
    println!("envira {CURRENT} → {target}");
    println!("  installed via  {where_}");
    if let Some(c) = &cmd {
        println!("  runs           {}", c.join(" "));
    }
    if let Some(c) = &fallback {
        println!("  if no prebuilt {} (builds from source)", c.join(" "));
    }
    println!("  notes          {}/tag/v{target}", releases());
    if !o.yes {
        if !std::io::stdin().is_terminal() {
            bail!("refusing to update without confirmation; pass -y");
        }
        print!("Update? [y/N] ");
        std::io::stdout().flush()?;
        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer)?;
        if !matches!(answer.trim(), "y" | "Y" | "yes") {
            println!("Aborted.");
            return Ok(Outcome::Unchanged(1));
        }
    }
    // Steps call `<envira> fs …`: never swap the binary under a running install.
    let _lock = state::RunLock::acquire_exclusive(&paths, &facts)?;
    match cmd {
        Some(words) => {
            if let Some(u) = &facts.drop_to {
                bail!(
                    "run `envira self-update` as {u}, without sudo: {} is theirs",
                    channel.label()
                );
            }
            let run = |words: &[String]| -> Result<()> {
                let st = Command::new(&words[0])
                    .args(&words[1..])
                    .envs(crate::runner::build_env())
                    .status()
                    .with_context(|| format!("cannot run {}", words[0]))?;
                if !st.success() {
                    bail!("`{}` failed ({st})", words.join(" "));
                }
                Ok(())
            };
            if let Err(e) = run(&words) {
                let Some(fb) = &fallback else {
                    return Err(e);
                };
                eprintln!("envira: {e:#}; building it from source instead");
                run(fb)?;
            }
        }
        None => replace_binary(&facts, &exe, &target)?,
    }
    let now = version_of(&exe);
    if now.as_deref() == Ok(target.as_str()) {
        println!("envira {target} is installed at {}.", tilde(&facts, &exe));
        return Ok(Outcome::Updated(exe));
    }
    // Not `Updated`: the TUI would offer to restart into the same old version.
    eprintln!(
        "warning: {} is not {target} afterwards ({}; the package index may not have it yet)",
        exe.display(),
        match now {
            Ok(v) => format!("it reports {v}"),
            Err(e) => e,
        }
    );
    Ok(Outcome::Unchanged(1))
}

/// Whether `exe` is the `bin/envira` of the user base `pip install --user` uses:
/// `$PYTHONUSERBASE`, `~/.local` (Linux), `~/Library/Python/<version>` (macOS).
fn in_user_base(facts: &Facts, exe: &Path) -> bool {
    let Some(root) = exe.parent().and_then(Path::parent) else {
        return false;
    };
    let canon = |p: PathBuf| p.canonicalize().unwrap_or(p);
    let root = canon(root.to_path_buf());
    let home = canon(facts.home.clone());
    let base = std::env::var_os("PYTHONUSERBASE").filter(|_| facts.drop_to.is_none());
    root == home.join(".local")
        || root.parent() == Some(home.join("Library/Python").as_path())
        || base.is_some_and(|b| canon(PathBuf::from(b)) == root)
}

/// Downloads the release binary for this machine, checks its SHA-256 and that it runs, then
/// puts it in place of `exe` (atomically when the directory is writable, else with sudo).
fn replace_binary(facts: &Facts, exe: &Path, version: &str) -> Result<()> {
    let arch = std::env::consts::ARCH;
    let asset = format!("envira-{arch}-{}", facts.os);
    if !matches!(
        (arch, facts.os.as_str()),
        ("x86_64" | "aarch64", "linux") | ("aarch64", "macos")
    ) {
        bail!("there is no release binary for {arch} {}", facts.os);
    }
    let url = format!("{}/download/v{version}/{asset}", releases());
    let c = crate::fetch::client()?;
    println!("Downloading {url}");
    let (status, bin) = crate::fetch::get(&c, &url, None)?;
    if status == 404 {
        bail!("envira {version} has no {asset} ({url}: HTTP 404)");
    }
    if !(200..300).contains(&status) {
        bail!("{url}: HTTP {status}");
    }
    let (status, sums) = crate::fetch::get(&c, &format!("{url}.sha256"), None)?;
    let want = (200..300)
        .contains(&status)
        .then(|| crate::fetch::expected_sha256(&asset, &String::from_utf8_lossy(&sums)))
        .flatten()
        .with_context(|| {
            format!("{url}.sha256 is missing or has no checksum; nothing was changed")
        })?;
    let got = crate::fetch::sha256_hex(&bin);
    if got != want {
        bail!("checksum mismatch for {asset} (expected {want}, got {got}); nothing was changed");
    }
    let dir = exe.parent().context("the envira binary has no directory")?;
    // The new file keeps the old one's owner: `sudo envira self-update` on a root-owned
    // /usr/local/bin/envira must not hand it to the user (only root can chown).
    let owner = std::fs::metadata(exe).ok().map(|m| (m.uid(), m.gid()));
    let as_root = facts.privilege == Privilege::Root;
    match install_binary(dir, exe, &bin, version, owner.filter(|_| as_root)) {
        Ok(()) => Ok(()),
        Err(e) if is_permission(&e) => {
            if facts.privilege == Privilege::None {
                bail!(
                    "{} is not writable and sudo is not available",
                    dir.display()
                );
            }
            // Check the download as the user, then let sudo put it next to the old one and
            // rename it over: until that rename, the old binary is untouched.
            let tmp = tempfile::tempdir()?;
            let staged = tmp.path().join("envira");
            install_binary(tmp.path(), &staged, &bin, version, None)?;
            println!("{} is not writable: using sudo", dir.display());
            sudo_replace(&staged, exe, owner.unwrap_or((0, 0)))
        }
        Err(e) => Err(e),
    }
}

/// `sudo install` of `staged` as a new file in `exe`'s directory (owned by `owner`, mode 755),
/// then `sudo mv` over `exe`: a rename in one directory, so `exe` is either the old binary or
/// the new one. The directory is not writable by the user, so the fixed temp name cannot be
/// planted by them.
fn sudo_replace(staged: &Path, exe: &Path, (uid, gid): (u32, u32)) -> Result<()> {
    let dir = exe.parent().context("the envira binary has no directory")?;
    let new = dir.join(format!(".envira.new.{}", std::process::id()));
    let sudo = |args: &[&std::ffi::OsStr]| {
        Command::new("sudo")
            .arg("--")
            .args(args)
            .status()
            .context("cannot run sudo")
    };
    let (uid, gid) = (uid.to_string(), gid.to_string());
    let st = sudo(&[
        "install".as_ref(),
        "-m".as_ref(),
        "755".as_ref(),
        "-o".as_ref(),
        uid.as_ref(),
        "-g".as_ref(),
        gid.as_ref(),
        staged.as_os_str(),
        new.as_os_str(),
    ])?;
    if !st.success() {
        let _ = sudo(&["rm".as_ref(), "-f".as_ref(), new.as_os_str()]);
        bail!("sudo install failed ({st}); nothing was changed");
    }
    let st = sudo(&[
        "mv".as_ref(),
        "-f".as_ref(),
        new.as_os_str(),
        exe.as_os_str(),
    ])?;
    if !st.success() {
        let _ = sudo(&["rm".as_ref(), "-f".as_ref(), new.as_os_str()]);
        bail!("sudo mv failed ({st}); {} was not replaced", exe.display());
    }
    Ok(())
}

fn is_permission(e: &anyhow::Error) -> bool {
    e.downcast_ref::<std::io::Error>()
        .is_some_and(|e| e.kind() == std::io::ErrorKind::PermissionDenied)
}

/// Writes `bin` to a new file in `dir`, checks that it runs and reports `version`, then
/// renames it over `exe`: a running envira keeps its old file. The temp file has a unique name
/// and is created exclusively, so an existing file or symlink is never written through.
/// `owner` (uid, gid) is given to the new file before the rename (root only).
pub fn install_binary(
    dir: &Path,
    exe: &Path,
    bin: &[u8],
    version: &str,
    owner: Option<(u32, u32)>,
) -> Result<()> {
    // Keep io::Errors as the source, so a permission problem can fall back to sudo.
    let mut f = tempfile::Builder::new()
        .prefix(".envira.new.")
        .tempfile_in(dir)
        .map_err(anyhow::Error::new)?;
    let write = |f: &mut tempfile::NamedTempFile| -> std::io::Result<()> {
        f.write_all(bin)?;
        f.as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o755))?;
        if let Some((uid, gid)) = owner {
            std::os::unix::fs::fchown(f.as_file(), Some(uid), Some(gid))?;
        }
        f.as_file().sync_all()
    };
    write(&mut f).map_err(anyhow::Error::new)?;
    // Closed before it runs (an open writer makes exec fail with ETXTBSY); removed on drop.
    let new = f.into_temp_path();
    match version_of(&new) {
        Ok(v) if v == version => {}
        got => bail!(
            "the downloaded envira does not run here or is not {version} ({}); nothing was changed",
            match got {
                Ok(v) => format!("it says {v}"),
                Err(e) => e,
            }
        ),
    }
    new.persist(exe).map_err(|e| anyhow::Error::new(e.error))
}

/// The version `<exe> --version` reports (`envira X`), if it exits successfully within 10 s.
/// Retried briefly while another thread's fork still holds the file open for writing (ETXTBSY).
fn version_of(exe: &Path) -> Result<String, String> {
    use std::io::Read;
    let mut n = 0;
    let mut child = loop {
        match Command::new(exe)
            .arg("--version")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            Err(e) if e.raw_os_error() == Some(libc::ETXTBSY) && n < 5 => {
                n += 1;
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => return Err(e.to_string()),
            Ok(c) => break c,
        }
    };
    let mut out = child.stdout.take().expect("piped");
    // What it printed so far (at most 4 KiB), and a signal once its stdout closed.
    let got = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let (closed_tx, closed) = std::sync::mpsc::channel::<()>();
    {
        let got = got.clone();
        std::thread::spawn(move || {
            let mut chunk = [0u8; 512];
            while let Ok(n @ 1..) = out.read(&mut chunk) {
                let mut g = got.lock().unwrap();
                let room = 4096usize.saturating_sub(g.len());
                g.extend_from_slice(&chunk[..n.min(room)]);
            }
            let _ = closed_tx.send(());
        });
    }
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break st,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("it did not answer `--version` within 10 s".into());
            }
            Err(e) => return Err(e.to_string()),
        }
    };
    // It has exited, so what it printed is in the pipe already. A process it left behind may
    // keep the pipe open: give the reader a moment, never wait for that process.
    let _ = closed.recv_timeout(Duration::from_secs(1));
    let text = String::from_utf8_lossy(&got.lock().unwrap())
        .trim()
        .to_string();
    if !status.success() {
        return Err(format!("`--version` failed ({status}): {text:?}"));
    }
    text.split_whitespace()
        .nth(1)
        .map(String::from)
        .ok_or_else(|| format!("it says {text:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A new binary that answers and exits but leaves a process holding its stdout (test
    /// fixture: a shell script standing in for the binary).
    #[test]
    fn the_version_probe_does_not_wait_for_what_the_binary_left_behind() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let exe = d.path().join("envira");
        std::fs::write(&exe, "#!/bin/sh\necho envira 9.9.9\nsleep 30 &\nexit 0\n").unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        let start = std::time::Instant::now();
        assert_eq!(version_of(&exe).unwrap(), "9.9.9");
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "{:?}",
            start.elapsed()
        );
    }

    fn none(_: &Path) -> bool {
        false
    }

    #[test]
    fn channel_follows_the_install_path() {
        let cargo = Path::new("/home/user/.cargo/bin");
        let d = |p: &str| detect(Path::new(p), cargo, none);
        assert_eq!(d("/home/user/.local/bin/envira"), Channel::Binary);
        assert_eq!(d("/usr/local/bin/envira"), Channel::Binary);
        assert_eq!(
            d("/home/user/.local/share/pipx/venvs/envira/bin/envira"),
            Channel::Pipx
        );
        assert_eq!(
            d("/home/user/.local/share/uv/tools/envira/bin/envira"),
            Channel::Uv
        );
        assert_eq!(d("/home/user/.cargo/bin/envira"), Channel::Cargo);
        assert_eq!(
            d("/home/user/src/envira/target/debug/envira"),
            Channel::Source
        );
        assert_eq!(
            d("/home/user/src/envira/target/x86_64-unknown-linux-musl/release/envira"),
            Channel::Source
        );
        // A `release` directory elsewhere is not a build.
        assert_eq!(d("/opt/release/envira"), Channel::Binary);
    }

    #[test]
    fn pip_installs_are_found_by_their_record() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let site = root.join("lib/python3.12/site-packages");
        let info = site.join("envira-0.1.2.dist-info");
        std::fs::create_dir_all(&info).unwrap();
        std::fs::create_dir_all(root.join("bin")).unwrap();
        let exe = root.join("bin/envira");
        let cargo = Path::new("/nowhere/.cargo/bin");
        assert_eq!(detect(&exe, cargo, pip_owns), Channel::Binary);
        std::fs::write(
            info.join("RECORD"),
            "envira-0.1.2.dist-info/METADATA,sha256=x,1\n../../../bin/envira,sha256=y,2\n",
        )
        .unwrap();
        assert_eq!(detect(&exe, cargo, pip_owns), Channel::Pip(None));
        std::fs::write(root.join("pyvenv.cfg"), "home = /usr/bin\n").unwrap();
        assert_eq!(
            detect(&exe, cargo, pip_owns),
            Channel::Pip(Some(root.join("bin/python")))
        );
    }

    #[test]
    fn package_manager_commands_pin_versions() {
        let j = |c: &Channel, v: Option<&str>, b: bool| c.command(v, b).map(|w| w.join(" "));
        assert_eq!(j(&Channel::Binary, None, false), None);
        assert_eq!(
            j(&Channel::Pipx, None, false).unwrap(),
            "pipx upgrade envira"
        );
        assert_eq!(
            j(&Channel::Pipx, Some("0.1.1"), false).unwrap(),
            "pipx install --force envira==0.1.1"
        );
        assert_eq!(
            j(&Channel::Uv, None, false).unwrap(),
            "uv tool upgrade envira"
        );
        assert_eq!(
            j(&Channel::Uv, Some("0.1.1"), false).unwrap(),
            "uv tool install --force envira==0.1.1"
        );
        assert_eq!(
            j(&Channel::Pip(None), None, false).unwrap(),
            "python3 -m pip install --user --upgrade envira"
        );
        assert_eq!(
            j(
                &Channel::Pip(Some("/v/bin/python".into())),
                Some("0.1.1"),
                false
            )
            .unwrap(),
            "/v/bin/python -m pip install --upgrade envira==0.1.1"
        );
        assert_eq!(
            j(&Channel::Cargo, Some("0.1.1"), true).unwrap(),
            "cargo binstall -y --force --disable-strategies compile envira@0.1.1"
        );
        // A build from source is never implicit: it is the fallback, shown before asking.
        assert_eq!(
            Channel::Cargo
                .fallback(Some("0.1.1"), true)
                .unwrap()
                .join(" "),
            "cargo install --locked --force --version 0.1.1 envira"
        );
        assert_eq!(Channel::Cargo.fallback(None, false), None);
        assert_eq!(Channel::Pipx.fallback(None, true), None);
        assert_eq!(
            j(&Channel::Cargo, None, false).unwrap(),
            "cargo install --locked --force envira"
        );
        assert_eq!(
            j(&Channel::Cargo, Some("0.1.1"), false).unwrap(),
            "cargo install --locked --force --version 0.1.1 envira"
        );
    }

    #[test]
    fn a_replaced_binary_is_found_at_its_path() {
        assert_eq!(
            live_path(Path::new("/home/user/.local/bin/envira (deleted)")),
            Path::new("/home/user/.local/bin/envira")
        );
        assert_eq!(live_path(Path::new("/a/envira")), Path::new("/a/envira"));
    }

    #[test]
    fn release_tags_come_from_the_redirect() {
        assert_eq!(
            tag_version("https://github.com/ControlNet/envira/releases/tag/v0.1.2").as_deref(),
            Some("0.1.2")
        );
        assert_eq!(
            tag_version("/releases/tag/1.0.0/").as_deref(),
            Some("1.0.0")
        );
        assert_eq!(
            tag_version("https://github.com/ControlNet/envira/releases"),
            None
        );
        assert_eq!(tag_version("https://x/releases/tag/nightly"), None);
    }

    fn fake(version: &str) -> Vec<u8> {
        format!("#!/bin/sh\necho envira {version}\n").into_bytes()
    }

    /// Temp files `install_binary` left in `dir`.
    fn leftovers(dir: &Path) -> Vec<String> {
        std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with(".envira.new.") && n != ".envira.new.planted")
            .collect()
    }

    #[test]
    fn the_binary_is_replaced_only_when_the_download_runs() {
        let tmp = tempfile::tempdir().unwrap();
        let exe = tmp.path().join("envira");
        std::fs::write(&exe, fake("0.1.0")).unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();

        let e = install_binary(tmp.path(), &exe, &fake("0.0.9"), "0.2.0", None).unwrap_err();
        assert!(format!("{e:#}").contains("is not 0.2.0"), "{e:#}");
        assert_eq!(std::fs::read(&exe).unwrap(), fake("0.1.0"));
        assert!(leftovers(tmp.path()).is_empty());
        // The right version but a failing exit: not a working envira.
        let failing = b"#!/bin/sh\necho envira 0.2.0\nexit 42\n";
        let e = install_binary(tmp.path(), &exe, failing, "0.2.0", None).unwrap_err();
        assert!(format!("{e:#}").contains("failed"), "{e:#}");
        assert_eq!(std::fs::read(&exe).unwrap(), fake("0.1.0"));

        let m = std::fs::metadata(&exe).unwrap();
        install_binary(
            tmp.path(),
            &exe,
            &fake("0.2.0"),
            "0.2.0",
            Some((m.uid(), m.gid())),
        )
        .unwrap();
        assert_eq!(std::fs::read(&exe).unwrap(), fake("0.2.0"));
        let mode = std::fs::metadata(&exe).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o755);
        assert!(leftovers(tmp.path()).is_empty());
    }

    #[test]
    fn pip_user_installs_are_told_from_other_prefixes() {
        let facts = Facts::sample(Path::new("/home/user"));
        let user = |p: &str| in_user_base(&facts, Path::new(p));
        assert!(user("/home/user/.local/bin/envira"));
        assert!(user("/home/user/Library/Python/3.12/bin/envira"));
        assert!(!user("/home/user/custom-prefix/bin/envira"));
        assert!(!user("/usr/local/bin/envira"));
    }

    #[test]
    fn a_planted_link_is_never_written_through() {
        let tmp = tempfile::tempdir().unwrap();
        let victim = tmp.path().join("victim");
        std::fs::write(&victim, b"keep").unwrap();
        // The fixed name the first version used, and a pattern-like one.
        for name in [".envira.new", ".envira.new.planted"] {
            std::os::unix::fs::symlink(&victim, tmp.path().join(name)).unwrap();
        }
        let exe = tmp.path().join("envira");
        std::fs::write(&exe, fake("0.1.0")).unwrap();
        install_binary(tmp.path(), &exe, &fake("0.2.0"), "0.2.0", None).unwrap();
        assert_eq!(std::fs::read(&exe).unwrap(), fake("0.2.0"));
        assert_eq!(std::fs::read(&victim).unwrap(), b"keep");
        let m = std::fs::metadata(&victim).unwrap().permissions().mode();
        assert_ne!(m & 0o111, 0o111, "the victim was made executable");
    }

    #[test]
    fn answers_are_reused_until_they_go_stale() {
        let now = Local::now();
        let at = |h: i64| Some((now - chrono::Duration::hours(h)).to_rfc3339());
        let ok = |h| Cache {
            latest: Some("1.0.0".into()),
            error: None,
            checked_at: at(h),
        };
        let err = |h| Cache {
            latest: None,
            error: Some("offline".into()),
            checked_at: at(h),
        };
        assert!(ok(5).fresh(now));
        assert!(!ok(7).fresh(now));
        assert!(err(0).fresh(now));
        assert!(!err(1).fresh(now));
        assert!(!Cache::default().fresh(now));
    }
}
