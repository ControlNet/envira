//! `envira fetch-release`: download a GitHub release asset for this arch and install binaries
//! from it. Extraction is in-process, so bare machines need neither curl nor tar/unzip.

use std::fs::File;
use std::io::{BufReader, Read, Seek, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use sha2::Digest;

use crate::net;

pub struct FetchArgs {
    pub repo: String,
    /// Globs tried in order against the asset names.
    pub assets: Vec<String>,
    pub bins: Vec<String>,
    pub to: PathBuf,
    /// Release tag; latest when `None`.
    pub tag: Option<String>,
    /// Command that copies the binaries into `to` (`sudo -n`), so the download, checks and
    /// extraction run as the user, who keeps `GITHUB_TOKEN`. Empty: write `to` directly.
    pub install_with: Vec<String>,
}

/// `*` and `?` wildcard match.
pub fn glob_match(pat: &str, s: &str) -> bool {
    fn go(p: &[char], s: &[char]) -> bool {
        match (p.first(), s.first()) {
            (None, None) => true,
            (Some('*'), _) => go(&p[1..], s) || (!s.is_empty() && go(p, &s[1..])),
            (Some('?'), Some(_)) => go(&p[1..], &s[1..]),
            (Some(a), Some(b)) if a == b => go(&p[1..], &s[1..]),
            _ => false,
        }
    }
    let p: Vec<char> = pat.chars().collect();
    let s: Vec<char> = s.chars().collect();
    go(&p, &s)
}

pub fn pick_asset<'a>(globs: &[String], names: &'a [String]) -> Option<&'a String> {
    globs
        .iter()
        .find_map(|g| names.iter().find(|n| glob_match(g, n)))
}

pub fn client() -> Result<reqwest::blocking::Client> {
    Ok(reqwest::blocking::Client::builder()
        .user_agent(concat!("envira/", env!("CARGO_PKG_VERSION")))
        .timeout(std::time::Duration::from_secs(300))
        .build()?)
}

/// GET (with the token when it is the GitHub API, `net::github_token`), retrying transient
/// failures (`net::Retry::DOWNLOAD`). Returns the final status and body; non-transient errors
/// such as 403/404 come back on the first try.
pub fn get(
    c: &reqwest::blocking::Client,
    url: &str,
    accept: Option<&str>,
) -> Result<(u16, Vec<u8>)> {
    get_with(c, url, accept, |r| {
        r.bytes().map(Vec::from).map_err(std::io::Error::other)
    })
}

/// Streams the body of `url` into `file` (emptied before every attempt) and returns the final
/// status and the SHA-256 of what was written: a release asset never sits in memory.
fn download(c: &reqwest::blocking::Client, url: &str, file: &mut File) -> Result<(u16, String)> {
    get_with(c, url, None, |mut r| {
        file.set_len(0)?;
        file.rewind()?;
        let mut w = HashWriter {
            inner: &mut *file,
            hash: sha2::Sha256::new(),
        };
        std::io::copy(&mut r, &mut w)?;
        Ok(hex(&w.hash.finalize()))
    })
}

/// Writes through to `inner` and hashes what was written.
struct HashWriter<W> {
    inner: W,
    hash: sha2::Sha256,
}

impl<W: Write> Write for HashWriter<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.hash.update(&buf[..n]);
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// The retry loop of [`get`]; `read` consumes the body of each answer. A failure while reading
/// the body is retried when reqwest calls it transient (connection reset, timeout), not when
/// writing it failed (disk full).
fn get_with<T>(
    c: &reqwest::blocking::Client,
    url: &str,
    accept: Option<&str>,
    mut read: impl FnMut(reqwest::blocking::Response) -> std::io::Result<T>,
) -> Result<(u16, T)> {
    let policy = net::Retry::DOWNLOAD;
    let mut n = 1;
    loop {
        let mut req = c.get(url);
        if let Some(a) = accept {
            req = req.header("Accept", a);
        }
        if let Some(t) = net::github_token(url) {
            req = req.bearer_auth(t);
        }
        let sent = req.send().map(|r| {
            let status = r.status().as_u16();
            let after = net::retry_after(r.headers());
            (status, after, read(r))
        });
        let (why, after, last) = match sent {
            Ok((status, _, Ok(body))) if !net::transient_status(status) => {
                return Ok((status, body));
            }
            Ok((status, after, Ok(body))) => {
                (format!("HTTP {status}"), after, Some((status, body)))
            }
            Ok((_, _, Err(e))) if transient_io(&e) => (format!("{e}"), None, None),
            Ok((_, _, Err(e))) => return Err(e).with_context(|| format!("reading {url}")),
            Err(e) if net::transient_error(&e) => (format!("{e}"), None, None),
            Err(e) => return Err(e).with_context(|| format!("requesting {url}")),
        };
        match policy.delay(n, after) {
            Some(d) => {
                eprintln!(
                    "[envira] {why} from {url}; retrying in {}s (attempt {}/{})",
                    d.as_secs(),
                    n + 1,
                    policy.attempts
                );
                std::thread::sleep(d);
                n += 1;
            }
            None => match last {
                Some(r) => return Ok(r),
                None => bail!("{why} (requesting {url}, {n} attempts)"),
            },
        }
    }
}

/// A body read that failed in reqwest (connection, timeout), not in the writer.
fn transient_io(e: &std::io::Error) -> bool {
    e.get_ref()
        .and_then(|r| r.downcast_ref::<reqwest::Error>())
        .is_some_and(net::transient_error)
}

pub fn run(args: &FetchArgs) -> Result<()> {
    let c = client()?;
    let url = match &args.tag {
        Some(t) => format!(
            "https://api.github.com/repos/{}/releases/tags/{t}",
            args.repo
        ),
        None => format!("https://api.github.com/repos/{}/releases/latest", args.repo),
    };
    eprintln!("[envira] GET {url}");
    let (status, body) = get(&c, &url, Some("application/vnd.github+json"))?;
    if !(200..300).contains(&status) {
        let body = String::from_utf8_lossy(&body).into_owned();
        let msg = serde_json::from_str::<serde_json::Value>(&body)
            .ok()
            .and_then(|v| v["message"].as_str().map(String::from))
            .unwrap_or(body);
        let hint = if status == 403 && net::github_token(&url).is_none() {
            " (set GITHUB_TOKEN to raise the rate limit)"
        } else {
            ""
        };
        bail!("HTTP {status} from GitHub: {msg}{hint}");
    }
    let release: serde_json::Value =
        serde_json::from_slice(&body).context("parsing the GitHub release")?;
    let tag = release["tag_name"].as_str().unwrap_or("?").to_string();
    let assets: Vec<(String, String)> = release["assets"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| {
                    Some((
                        x["name"].as_str()?.to_string(),
                        x["browser_download_url"].as_str()?.to_string(),
                    ))
                })
                .collect()
        })
        .unwrap_or_default();
    let names: Vec<String> = assets.iter().map(|a| a.0.clone()).collect();
    let Some(name) = pick_asset(&args.assets, &names) else {
        bail!(
            "release {tag} of {} has no asset matching {:?} (has: {})",
            args.repo,
            args.assets,
            names.join(", ")
        );
    };
    let dl = &assets
        .iter()
        .find(|a| &a.0 == name)
        .expect("picked from list")
        .1;
    eprintln!("[envira] downloading {name} ({tag})");
    // The asset and the binaries taken from it go to files, not memory (assets are tens of MB).
    let work = tempfile::tempdir().context("creating a download directory")?;
    let archive = work.path().join("asset");
    let mut file = File::options()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&archive)
        .with_context(|| format!("creating {}", archive.display()))?;
    let (status, got) = download(&c, dl, &mut file)?;
    drop(file);
    if !(200..300).contains(&status) {
        bail!("HTTP {status} downloading {dl}");
    }
    // GitHub records a digest for assets uploaded since 2025; older releases may publish
    // checksum files instead.
    let digest = release["assets"].as_array().and_then(|a| {
        a.iter()
            .find(|x| x["name"].as_str() == Some(name.as_str()))
            .and_then(|x| x["digest"].as_str())
            .and_then(|d| d.strip_prefix("sha256:"))
            .map(str::to_ascii_lowercase)
    });
    match digest {
        Some(want) => matches_sha256(name, "the GitHub asset digest", &want, &got)?,
        None => verify_checksum(&c, name, &got, &assets)?,
    }
    let files = extract(name, &archive, &args.bins, work.path())?;
    for dest in place(&files, &args.to, &args.install_with)? {
        println!("installed {tag} -> {}", dest.display());
    }
    Ok(())
}

/// Installs the staged `files` (`(bin, path)`) into `to`: atomically when `via` is empty, else
/// with `<via> install -m 755` (after `<via> mkdir -p`; BSD `install` has no `-D`).
fn place(files: &[(String, PathBuf)], to: &Path, via: &[String]) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    let Some((prog, pre)) = via.split_first() else {
        std::fs::create_dir_all(to).with_context(|| format!("creating {}", to.display()))?;
        for (bin, src) in files {
            let dest = to.join(bin);
            install_file(&dest, src)?;
            out.push(dest);
        }
        return Ok(out);
    };
    let run = |args: &[&std::ffi::OsStr]| -> Result<()> {
        let st = std::process::Command::new(prog)
            .args(pre)
            .args(args)
            .status()
            .with_context(|| format!("running {prog}"))?;
        if !st.success() {
            let shown: Vec<_> = args.iter().map(|a| a.to_string_lossy()).collect();
            bail!("{} {} failed ({st})", via.join(" "), shown.join(" "));
        }
        Ok(())
    };
    run(&["mkdir".as_ref(), "-p".as_ref(), to.as_os_str()])?;
    for (bin, src) in files {
        let dest = to.join(bin);
        run(&[
            "install".as_ref(),
            "-m".as_ref(),
            "755".as_ref(),
            src.as_os_str(),
            dest.as_os_str(),
        ])?;
        out.push(dest);
    }
    Ok(out)
}

/// Checksum files a release may publish for `asset`, most specific first: `<asset>.sha256`
/// (bat, fd) or a shared list such as `checksums.txt` / `SHA256SUMS` (goreleaser projects).
pub fn checksum_assets<'a>(asset: &str, names: &'a [String]) -> Vec<&'a String> {
    let own = [format!("{asset}.sha256"), format!("{asset}.sha256sum")];
    let mut v: Vec<&String> = names.iter().filter(|n| own.contains(n)).collect();
    v.extend(names.iter().filter(|n| {
        let l = n.to_ascii_lowercase();
        !own.contains(n)
            && !l.ends_with(".sig")
            && !l.ends_with(".asc")
            && !l.ends_with(".pem")
            && (l.contains("checksums") || l.contains("sha256sums"))
    }));
    v
}

/// The SHA-256 a checksum file gives for `asset`: the line naming it (`<hex>  [*]<asset>`), or
/// the only hash of a single-asset `.sha256` file.
pub fn expected_sha256(asset: &str, text: &str) -> Option<String> {
    let is_hex = |s: &str| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit());
    let mut lone = None;
    let mut hashes = 0;
    for line in text.lines() {
        let mut parts = line.split_whitespace();
        let (Some(hash), file) = (parts.next(), parts.next()) else {
            continue;
        };
        if !is_hex(hash) {
            continue;
        }
        hashes += 1;
        match file.map(|f| f.trim_start_matches('*')) {
            Some(f) if f == asset || f.rsplit('/').next() == Some(asset) => {
                return Some(hash.to_ascii_lowercase());
            }
            None => lone = Some(hash.to_ascii_lowercase()),
            _ => {}
        }
    }
    lone.filter(|_| hashes == 1)
}

pub fn sha256_hex(data: &[u8]) -> String {
    hex(&sha2::Sha256::digest(data))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// `got` is the SHA-256 (hex) of the download.
fn matches_sha256(asset: &str, source: &str, want: &str, got: &str) -> Result<()> {
    if got != want {
        bail!("checksum mismatch for {asset}: {source} says {want}, the download is {got}");
    }
    eprintln!("[envira] sha256 verified against {source}");
    Ok(())
}

/// Verifies the download against a published checksum when the release has one; a mismatch is
/// an error, a release without checksums is only noted.
fn verify_checksum(
    c: &reqwest::blocking::Client,
    asset: &str,
    got: &str,
    assets: &[(String, String)],
) -> Result<()> {
    let names: Vec<String> = assets.iter().map(|a| a.0.clone()).collect();
    for sums in checksum_assets(asset, &names) {
        let url = &assets.iter().find(|a| &a.0 == sums).expect("from list").1;
        let (status, body) = get(c, url, None)?;
        if !(200..300).contains(&status) {
            eprintln!("[envira] could not read {sums} (HTTP {status}); checksum not verified");
            return Ok(());
        }
        let Some(want) = expected_sha256(asset, &String::from_utf8_lossy(&body)) else {
            continue;
        };
        return matches_sha256(asset, sums, &want, got);
    }
    eprintln!("[envira] the release publishes no checksum for {asset}; not verified");
    Ok(())
}

/// Copies the staged `src` to `dest` through a temp file next to it, so `dest` is replaced
/// atomically. The temp file has a unique name and is created exclusively: an existing file or
/// symlink of that name is never followed, and two installs never share one.
fn install_file(dest: &Path, src: &Path) -> Result<()> {
    let dir = dest.parent().context("destination without parent")?;
    let name = dest.file_name().and_then(|n| n.to_str()).unwrap_or("bin");
    let mut tmp = tempfile::Builder::new()
        .prefix(&format!(".{name}."))
        .suffix(".envira-tmp")
        .tempfile_in(dir)
        .with_context(|| format!("creating a temp file in {}", dir.display()))?;
    std::io::copy(&mut File::open(src)?, tmp.as_file_mut())
        .with_context(|| format!("writing {}", tmp.path().display()))?;
    tmp.as_file()
        .set_permissions(std::fs::Permissions::from_mode(0o755))?;
    tmp.persist(dest)
        .map_err(|e| e.error)
        .with_context(|| format!("installing {}", dest.display()))?;
    Ok(())
}

/// Extracts each requested binary from `archive` (the downloaded asset `name`) into the
/// directory `out` and returns `(bin, staged file)` in the order of `bins`. Entries are copied
/// to disk as they are read, never held in memory.
pub fn extract(
    name: &str,
    archive: &Path,
    bins: &[String],
    out: &Path,
) -> Result<Vec<(String, PathBuf)>> {
    let staged = |i: usize| out.join(format!("bin-{i}"));
    // Archive depth of the entry staged for each bin.
    let mut depth: Vec<Option<usize>> = vec![None; bins.len()];
    let mut consider = |path: &str, entry: &mut dyn Read| -> Result<()> {
        let base = path.rsplit('/').next().unwrap_or(path);
        let Some(i) = bins.iter().position(|b| b.as_str() == base) else {
            return Ok(());
        };
        // Prefer the shallowest match (e.g. `bat-v1/bat` over `bat-v1/autocomplete/bat`).
        let d = path.matches('/').count();
        if depth[i].is_some_and(|s| s <= d) {
            return Ok(());
        }
        let dest = staged(i);
        let mut f = File::create(&dest).with_context(|| format!("creating {}", dest.display()))?;
        std::io::copy(entry, &mut f).with_context(|| format!("extracting {path}"))?;
        depth[i] = Some(d);
        Ok(())
    };
    let open = || File::open(archive).with_context(|| format!("opening {}", archive.display()));
    if name.ends_with(".tar.gz") || name.ends_with(".tgz") {
        let gz = flate2::read::GzDecoder::new(BufReader::new(open()?));
        let mut ar = tar::Archive::new(gz);
        for entry in ar.entries()? {
            let mut e = entry?;
            if !e.header().entry_type().is_file() {
                continue;
            }
            let path = e.path()?.display().to_string();
            consider(&path, &mut e)?;
        }
    } else if name.ends_with(".zip") {
        let mut zip = zip::ZipArchive::new(BufReader::new(open()?))?;
        for i in 0..zip.len() {
            let mut f = zip.by_index(i)?;
            if !f.is_file() {
                continue;
            }
            let path = f.name()?.to_string();
            consider(&path, &mut f)?;
        }
    } else if [".tar.xz", ".tar.bz2", ".tar.zst", ".7z", ".deb", ".rpm"]
        .iter()
        .any(|ext| name.ends_with(ext))
    {
        bail!("unsupported archive format: {name}");
    } else {
        // A bare executable (or zipapp): installed under the single requested name.
        if bins.len() != 1 {
            bail!(
                "{name} is a single file but {} binaries were requested",
                bins.len()
            );
        }
        return Ok(vec![(bins[0].clone(), archive.to_path_buf())]);
    }
    let missing: Vec<&String> = bins
        .iter()
        .zip(&depth)
        .filter(|(_, d)| d.is_none())
        .map(|(b, _)| b)
        .collect();
    if !missing.is_empty() {
        bail!("{name} does not contain {missing:?}");
    }
    Ok(bins
        .iter()
        .enumerate()
        .map(|(i, b)| (b.clone(), staged(i)))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// `extract` on in-memory data: the archive and the extracted files go through a temp dir.
    fn extract_bytes(name: &str, data: &[u8], bins: &[String]) -> Result<Vec<(String, Vec<u8>)>> {
        let d = tempfile::tempdir().unwrap();
        let archive = d.path().join("asset");
        std::fs::write(&archive, data).unwrap();
        let out = extract(name, &archive, bins, d.path())?;
        Ok(out
            .into_iter()
            .map(|(b, p)| (b, std::fs::read(p).unwrap()))
            .collect())
    }

    #[test]
    fn place_directly_or_through_a_command() {
        let d = tempfile::tempdir().unwrap();
        let staged = d.path().join("staged-a");
        std::fs::write(&staged, b"#!/bin/sh\n").unwrap();
        let files = vec![("a".to_string(), staged)];
        let direct = place(&files, &d.path().join("direct"), &[]).unwrap();
        assert_eq!(direct, vec![d.path().join("direct/a")]);
        // `env` stands in for `sudo -n`: the copy runs through the given command.
        let to = d.path().join("via/bin");
        let via = place(&files, &to, &["env".to_string()]).unwrap();
        assert_eq!(via, vec![to.join("a")]);
        for f in [d.path().join("direct/a"), to.join("a")] {
            assert_eq!(std::fs::read(&f).unwrap(), b"#!/bin/sh\n");
            let mode = std::fs::metadata(&f).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o755, "{}", f.display());
        }
        let err = place(&files, &d.path().join("x"), &["false".to_string()]).unwrap_err();
        assert!(format!("{err:#}").contains("false mkdir -p"), "{err:#}");
    }

    #[test]
    fn globs() {
        assert!(glob_match(
            "bat-v*-x86_64-unknown-linux-musl.tar.gz",
            "bat-v0.26.1-x86_64-unknown-linux-musl.tar.gz"
        ));
        assert!(!glob_match(
            "bat-v*-x86_64-unknown-linux-musl.tar.gz",
            "bat-v0.26.1-x86_64-unknown-linux-musl.tar.gz.sha256"
        ));
        assert!(glob_match("a?c", "abc"));
        let names = vec!["x-gnu.tar.gz".to_string(), "x-musl.tar.gz".to_string()];
        assert_eq!(
            pick_asset(&["*-musl*".into(), "*-gnu*".into()], &names).unwrap(),
            "x-musl.tar.gz"
        );
    }

    fn targz(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut b = tar::Builder::new(flate2::write::GzEncoder::new(
            Vec::new(),
            flate2::Compression::fast(),
        ));
        for (p, d) in files {
            let mut h = tar::Header::new_gnu();
            h.set_size(d.len() as u64);
            h.set_mode(0o755);
            h.set_cksum();
            b.append_data(&mut h, p, *d).unwrap();
        }
        b.into_inner().unwrap().finish().unwrap()
    }

    #[test]
    fn finds_checksum_files_and_hashes() {
        let names: Vec<String> = [
            "lazygit_0.55.1_linux_x86_64.tar.gz",
            "checksums.txt",
            "checksums.txt.sig",
            "bat-v0.26.1-x86_64-unknown-linux-musl.tar.gz",
            "bat-v0.26.1-x86_64-unknown-linux-musl.tar.gz.sha256",
        ]
        .map(String::from)
        .to_vec();
        assert_eq!(
            checksum_assets("bat-v0.26.1-x86_64-unknown-linux-musl.tar.gz", &names),
            vec![&names[4], &names[1]]
        );
        let a = "0".repeat(64);
        let b = "Ab".repeat(32);
        let list = format!("{a}  other.tar.gz\n{b} *lazygit_0.55.1_linux_x86_64.tar.gz\n");
        assert_eq!(
            expected_sha256("lazygit_0.55.1_linux_x86_64.tar.gz", &list),
            Some(b.to_ascii_lowercase())
        );
        assert_eq!(expected_sha256("missing.tar.gz", &list), None);
        assert_eq!(
            expected_sha256("x.tar.gz", &format!("{a}\n")),
            Some(a.clone())
        );
        assert_eq!(expected_sha256("x.tar.gz", "not a hash  x.tar.gz"), None);
        let abc = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        assert_eq!(sha256_hex(b"abc"), abc);
        assert!(matches_sha256("x", "digest", abc, &sha256_hex(b"abc")).is_ok());
        let err = matches_sha256("x", "digest", abc, &sha256_hex(b"abd"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("checksum mismatch for x"), "{err}");
    }

    #[test]
    fn tar_gz_prefers_shallow_path() {
        let data = targz(&[
            ("bat-v1/autocomplete/bat", b"completion"),
            ("bat-v1/bat", b"binary"),
        ]);
        let out = extract_bytes("bat-v1.tar.gz", &data, &["bat".into()]).unwrap();
        assert_eq!(out, vec![("bat".to_string(), b"binary".to_vec())]);
        // The deeper match coming second must not replace the staged one either.
        let data = targz(&[
            ("bat-v1/bat", b"binary"),
            ("bat-v1/autocomplete/bat", b"completion"),
        ]);
        let out = extract_bytes("bat-v1.tar.gz", &data, &["bat".into()]).unwrap();
        assert_eq!(out, vec![("bat".to_string(), b"binary".to_vec())]);
        assert!(
            extract_bytes("bat-v1.tar.gz", &data, &["nope".into()])
                .unwrap_err()
                .to_string()
                .contains("nope")
        );
    }

    #[test]
    fn zip_with_two_bins() {
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut z = zip::ZipWriter::new(&mut buf);
            let o: zip::write::SimpleFileOptions = Default::default();
            z.start_file("yazi-x86_64/yazi", o).unwrap();
            z.write_all(b"Y").unwrap();
            z.start_file("yazi-x86_64/ya", o).unwrap();
            z.write_all(b"A").unwrap();
            z.finish().unwrap();
        }
        let out = extract_bytes("yazi.zip", buf.get_ref(), &["yazi".into(), "ya".into()]).unwrap();
        assert_eq!(out[0], ("yazi".to_string(), b"Y".to_vec()));
        assert_eq!(out[1], ("ya".to_string(), b"A".to_vec()));
    }

    #[test]
    fn bare_file_and_install() {
        let d = tempfile::tempdir().unwrap();
        let archive = d.path().join("asset");
        std::fs::write(&archive, b"PK..").unwrap();
        let out = extract("pipx.pyz", &archive, &["pipx".into()], d.path()).unwrap();
        assert_eq!(out, vec![("pipx".to_string(), archive.clone())]);
        install_file(&d.path().join("pipx"), &out[0].1).unwrap();
        assert_eq!(std::fs::read(d.path().join("pipx")).unwrap(), b"PK..");
        let m = std::fs::metadata(d.path().join("pipx")).unwrap();
        assert_eq!(m.permissions().mode() & 0o777, 0o755);
        assert!(extract_bytes("x.tar.xz", b"", &["x".into()]).is_err());
    }

    #[test]
    fn install_never_writes_through_a_planted_link() {
        let d = tempfile::tempdir().unwrap();
        let victim = d.path().join("victim");
        std::fs::write(&victim, b"keep").unwrap();
        // The temp name the old code used.
        std::os::unix::fs::symlink(&victim, d.path().join(".bat.envira-tmp")).unwrap();
        let src = d.path().join("src");
        std::fs::write(&src, b"BAT").unwrap();
        let dest = d.path().join("bat");
        std::fs::write(&dest, b"old").unwrap();
        install_file(&dest, &src).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"BAT");
        assert_eq!(std::fs::read(&victim).unwrap(), b"keep");
        let left: Vec<_> = std::fs::read_dir(d.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".envira-tmp") && n != ".bat.envira-tmp")
            .collect();
        assert!(left.is_empty(), "{left:?}");
    }

    #[test]
    fn hash_writer_hashes_what_it_writes() {
        let mut out = Vec::new();
        let mut w = HashWriter {
            inner: &mut out,
            hash: sha2::Sha256::new(),
        };
        std::io::copy(&mut &b"abc"[..], &mut w).unwrap();
        assert_eq!(hex(&w.hash.finalize()), sha256_hex(b"abc"));
        assert_eq!(out, b"abc");
    }
}
