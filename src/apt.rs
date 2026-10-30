//! `envira apt-refresh`: refreshes apt's package lists before an install when they are older than
//! a day, so a machine that has not run `apt update` for weeks does not install old versions (or
//! report its apt tools as current forever), without running `apt-get update` for every package.
//!
//! The age of the lists is not the mtime of their files: apt keeps the server's Last-Modified time
//! and leaves files untouched on a 304. It is the newest of apt's own success stamp (written by
//! apt.systemd.daily and, on Ubuntu desktops, after every `apt update`) and envira's stamp, which
//! records envira's last attempt.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime};

use anyhow::Result;

pub const MAX_AGE: Duration = Duration::from_secs(24 * 3600);
/// Exit code when another process holds the package manager lock (the step is retried).
const BUSY: i32 = 100;

pub struct Paths {
    pub lists: PathBuf,
    pub apt_stamp: PathBuf,
    pub stamp: PathBuf,
    pub sources: PathBuf,
    pub sources_d: PathBuf,
}

impl Default for Paths {
    fn default() -> Self {
        Paths {
            lists: "/var/lib/apt/lists".into(),
            apt_stamp: "/var/lib/apt/periodic/update-success-stamp".into(),
            stamp: "/var/lib/envira/apt-refreshed".into(),
            sources: "/etc/apt/sources.list".into(),
            sources_d: "/etc/apt/sources.list.d".into(),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Decision {
    Fresh(String),
    Refresh(String),
}

fn mtime(p: &Path) -> Option<SystemTime> {
    std::fs::metadata(p).ok()?.modified().ok()
}

fn ago(d: Duration) -> String {
    let h = d.as_secs() / 3600;
    if h >= 48 {
        format!("{} days ago", h / 24)
    } else if h >= 1 {
        format!("{h} h ago")
    } else {
        format!("{} min ago", d.as_secs() / 60)
    }
}

pub fn decide(p: &Paths, now: SystemTime, max_age: Duration) -> Decision {
    let has_lists = std::fs::read_dir(&p.lists).is_ok_and(|rd| {
        rd.flatten()
            .any(|e| e.file_name().to_string_lossy().contains("_Packages"))
    });
    if !has_lists {
        return Decision::Refresh("there are no package lists".into());
    }
    // A stamp in the future (clock moved back) proves nothing.
    let Some(stamp) = [&p.apt_stamp, &p.stamp]
        .into_iter()
        .filter_map(|s| mtime(s))
        .filter(|t| *t <= now + Duration::from_secs(60))
        .max()
    else {
        return Decision::Refresh("no refresh is on record".into());
    };
    let sources = std::fs::read_dir(&p.sources_d)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .chain([p.sources.clone(), p.sources_d.clone()])
        .filter_map(|s| mtime(&s))
        .max();
    let age = now.duration_since(stamp).unwrap_or_default();
    if sources.is_some_and(|s| s > stamp) {
        Decision::Refresh("the apt sources changed since the last refresh".into())
    } else if age > max_age {
        Decision::Refresh(format!("the lists were refreshed {}", ago(age)))
    } else {
        Decision::Fresh(format!("the lists were refreshed {}", ago(age)))
    }
}

/// Runs `apt-get update` when [`decide`] says so. A failure other than a busy lock (a broken
/// third-party source, no network) only warns and is recorded, so the install goes ahead with
/// the lists there are and the next package does not try again; a busy lock fails the step
/// without recording, so it is retried.
pub fn run(p: &Paths, max_age: Duration) -> Result<i32> {
    let why = match decide(p, SystemTime::now(), max_age) {
        Decision::Fresh(why) => {
            println!("[envira] {why}; not refreshing them");
            return Ok(0);
        }
        Decision::Refresh(why) => why,
    };
    println!("[envira] {why}: apt-get update");
    let mut child = Command::new("sh")
        .args(["-c", "exec apt-get update 2>&1"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()?;
    let mut lines = Vec::new();
    if let Some(out) = child.stdout.take() {
        for l in BufReader::new(out).lines() {
            let l = l?;
            println!("{l}");
            lines.push(l);
        }
    }
    let status = child.wait()?;
    if !status.success() {
        if crate::diagnose::diagnose(&lines).is_some_and(|d| d.kind == "package manager busy") {
            println!("[envira] apt-get update: another process holds the package manager lock");
            return Ok(BUSY);
        }
        println!(
            "[envira] warning: apt-get update failed ({status}); installing from the current lists"
        );
    }
    if let Err(e) = p
        .stamp
        .parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|_| std::fs::write(&p.stamp, b""))
    {
        println!(
            "[envira] warning: cannot record the refresh in {}: {e}",
            p.stamp.display()
        );
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: Duration = Duration::from_secs(24 * 3600);

    fn touch(p: &Path, t: SystemTime) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        let f = std::fs::File::create(p).unwrap();
        f.set_modified(t).unwrap();
    }

    fn setup(dir: &Path, now: SystemTime) -> Paths {
        let p = Paths {
            lists: dir.join("lists"),
            apt_stamp: dir.join("periodic/update-success-stamp"),
            stamp: dir.join("envira/apt-refreshed"),
            sources: dir.join("sources.list"),
            sources_d: dir.join("sources.list.d"),
        };
        let old = now - 30 * DAY;
        touch(
            &p.lists
                .join("archive_ubuntu_dists_noble_main_binary-amd64_Packages.lz4"),
            old,
        );
        touch(&p.sources, old);
        touch(&p.sources_d.join("ubuntu.sources"), old);
        std::fs::File::options()
            .read(true)
            .open(&p.sources_d)
            .unwrap()
            .set_modified(old)
            .unwrap();
        p
    }

    fn refresh(d: Decision) -> bool {
        matches!(d, Decision::Refresh(_))
    }

    #[test]
    fn refreshes_without_lists_or_a_stamp() {
        let dir = tempfile::tempdir().unwrap();
        let now = SystemTime::now();
        let p = setup(dir.path(), now);
        assert_eq!(
            decide(&p, now, MAX_AGE),
            Decision::Refresh("no refresh is on record".into())
        );
        touch(&p.stamp, now);
        std::fs::remove_dir_all(&p.lists).unwrap();
        assert_eq!(
            decide(&p, now, MAX_AGE),
            Decision::Refresh("there are no package lists".into())
        );
    }

    #[test]
    fn a_stamp_younger_than_a_day_is_fresh() {
        let dir = tempfile::tempdir().unwrap();
        let now = SystemTime::now();
        let p = setup(dir.path(), now);
        touch(&p.stamp, now - Duration::from_secs(3 * 3600));
        assert_eq!(
            decide(&p, now, MAX_AGE),
            Decision::Fresh("the lists were refreshed 3 h ago".into())
        );
        touch(&p.stamp, now - 2 * DAY);
        assert_eq!(
            decide(&p, now, MAX_AGE),
            Decision::Refresh("the lists were refreshed 2 days ago".into())
        );
        // apt's own stamp counts too (apt.systemd.daily refreshed them this morning).
        touch(&p.apt_stamp, now - Duration::from_secs(600));
        assert_eq!(
            decide(&p, now, MAX_AGE),
            Decision::Fresh("the lists were refreshed 10 min ago".into())
        );
    }

    #[test]
    fn changed_sources_and_future_stamps_refresh() {
        let dir = tempfile::tempdir().unwrap();
        let now = SystemTime::now();
        let p = setup(dir.path(), now);
        touch(&p.stamp, now - Duration::from_secs(3600));
        touch(&p.sources_d.join("fastfetch-ppa.sources"), now);
        assert_eq!(
            decide(&p, now, MAX_AGE),
            Decision::Refresh("the apt sources changed since the last refresh".into())
        );
        let dir = tempfile::tempdir().unwrap();
        let p = setup(dir.path(), now);
        touch(&p.stamp, now + DAY);
        assert!(refresh(decide(&p, now, MAX_AGE)));
    }
}
