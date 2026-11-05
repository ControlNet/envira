//! Replacing files safely: every file envira writes (state, backups, configs, caches, installed
//! binaries) goes through `replace_with`.
//!
//! - The new content is written to a uniquely named temp file next to the destination, created
//!   exclusively (O_EXCL): an existing file or a symlink planted at a predictable name is never
//!   written through.
//! - The temp file is private (0600) while it fills, so a private config is never readable by
//!   others mid-write; it gets its final mode only when complete.
//! - As root, the new file keeps the owner of the file it replaces (a user's config edited by
//!   `sudo envira` stays theirs, a root-owned file stays root's).
//! - It is renamed over the destination: readers see the old file or the new one, and on any
//!   failure before the rename the destination is untouched.

use std::fs::File;
use std::io;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;

/// Replaces `path` with what `fill` writes. Mode: `mode`, else the replaced regular file's,
/// else what a new file gets (0666 less the umask). A symlink at `path` is replaced by the new
/// file, not followed.
pub fn replace_with(
    path: &Path,
    mode: Option<u32>,
    fill: impl FnOnce(&mut File) -> io::Result<()>,
) -> io::Result<()> {
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("f");
    let old = std::fs::symlink_metadata(path)
        .ok()
        .filter(|m| m.file_type().is_file());
    // Created empty with 0666 so the kernel applies the umask (the mode a new file would get),
    // then made private before anything is written.
    let tmp = tempfile::Builder::new()
        .prefix(&format!(".{name}."))
        .suffix(".envira-tmp")
        .permissions(std::fs::Permissions::from_mode(0o666))
        .tempfile_in(dir)?;
    let umasked = tmp.as_file().metadata()?.permissions().mode() & 0o777;
    tmp.as_file()
        .set_permissions(std::fs::Permissions::from_mode(0o600))?;
    let mut tmp = tmp;
    fill(tmp.as_file_mut())?;
    let f = tmp.as_file();
    // SAFETY: geteuid has no preconditions.
    if unsafe { libc::geteuid() } == 0
        && let Some(m) = &old
    {
        std::os::unix::fs::fchown(f, Some(m.uid()), Some(m.gid()))?;
    }
    // After chown, which clears set-id bits.
    let mode = mode
        .or(old.as_ref().map(|m| m.permissions().mode() & 0o7777))
        .unwrap_or(umasked);
    f.set_permissions(std::fs::Permissions::from_mode(mode))?;
    f.sync_all()?;
    tmp.persist(path).map_err(|e| e.error)?;
    Ok(())
}

/// `replace_with` for content in memory.
pub fn replace(path: &Path, data: &[u8], mode: Option<u32>) -> io::Result<()> {
    replace_with(path, mode, |f| io::Write::write_all(f, data))
}

/// `replace_with` copying the file `src`.
pub fn replace_copy(path: &Path, src: &Path, mode: Option<u32>) -> io::Result<()> {
    let mut from = File::open(src)?;
    replace_with(path, mode, |f| io::copy(&mut from, f).map(|_| ()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mode(p: &Path) -> u32 {
        std::fs::metadata(p).unwrap().permissions().mode() & 0o7777
    }

    #[test]
    fn replaces_through_a_fresh_private_file() {
        let d = tempfile::tempdir().unwrap();
        let victim = d.path().join("victim");
        std::fs::write(&victim, "keep").unwrap();
        // Names earlier versions used, planted as links.
        for n in [".rc.envira-tmp", "rc.tmp", ".rc.1.envira-tmp"] {
            std::os::unix::fs::symlink(&victim, d.path().join(n)).unwrap();
        }
        let rc = d.path().join("rc");
        replace(&rc, b"one", None).unwrap();
        assert_eq!(std::fs::read_to_string(&rc).unwrap(), "one");
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "keep");
        // A new file: the mode a plain write gives.
        let plain = d.path().join("plain");
        std::fs::write(&plain, "x").unwrap();
        assert_eq!(mode(&rc), mode(&plain));
        // An existing file keeps its mode; an explicit one wins.
        std::fs::set_permissions(&rc, std::fs::Permissions::from_mode(0o600)).unwrap();
        replace(&rc, b"two", None).unwrap();
        assert_eq!(mode(&rc), 0o600);
        replace(&rc, b"three", Some(0o640)).unwrap();
        assert_eq!(mode(&rc), 0o640);
        // While it fills, the temp file is private.
        let seen = std::cell::Cell::new(0);
        replace_with(&rc, None, |f| {
            seen.set(f.metadata()?.permissions().mode() & 0o777);
            io::Write::write_all(f, b"four")
        })
        .unwrap();
        assert_eq!(seen.get(), 0o600);
        // A link at the destination is replaced, not followed.
        let link = d.path().join("link");
        std::os::unix::fs::symlink(&victim, &link).unwrap();
        replace(&link, b"new", None).unwrap();
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "keep");
        assert!(std::fs::symlink_metadata(&link).unwrap().is_file());
        let left = std::fs::read_dir(d.path())
            .unwrap()
            .flatten()
            .filter(|e| {
                let n = e.file_name().to_string_lossy().into_owned();
                n.ends_with(".envira-tmp") && e.file_type().unwrap().is_file()
            })
            .count();
        assert_eq!(left, 0);
    }

    #[test]
    fn a_failed_fill_leaves_the_destination() {
        let d = tempfile::tempdir().unwrap();
        let rc = d.path().join("rc");
        std::fs::write(&rc, "old").unwrap();
        let e = replace_with(&rc, None, |_| Err(io::Error::other("boom"))).unwrap_err();
        assert_eq!(e.to_string(), "boom");
        assert_eq!(std::fs::read_to_string(&rc).unwrap(), "old");
        assert!(replace_copy(&rc, &d.path().join("missing"), None).is_err());
        assert_eq!(std::fs::read_to_string(&rc).unwrap(), "old");
        assert_eq!(std::fs::read_dir(d.path()).unwrap().count(), 1);
    }
}
