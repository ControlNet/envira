//! `envira fetch-release`: download a GitHub release asset for this arch and install binaries
//! from it. Extraction is in-process, so bare machines need neither curl nor tar/unzip.

use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::net;

pub struct FetchArgs {
    pub repo: String,
    /// Globs tried in order against the asset names.
    pub assets: Vec<String>,
    pub bins: Vec<String>,
    pub to: PathBuf,
    /// Release tag; latest when `None`.
    pub tag: Option<String>,
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

fn client() -> Result<reqwest::blocking::Client> {
    Ok(reqwest::blocking::Client::builder()
        .user_agent(concat!("envira/", env!("CARGO_PKG_VERSION")))
        .timeout(std::time::Duration::from_secs(300))
        .build()?)
}

fn token() -> Option<String> {
    std::env::var("GITHUB_TOKEN")
        .ok()
        .or_else(|| std::env::var("GH_TOKEN").ok())
        .filter(|t| !t.is_empty())
}

/// GET with the token, retrying transient failures (`net::Retry::DOWNLOAD`). Returns the final
/// status and body; non-transient errors such as 403/404 come back on the first try.
fn get(c: &reqwest::blocking::Client, url: &str, accept: Option<&str>) -> Result<(u16, Vec<u8>)> {
    let policy = net::Retry::DOWNLOAD;
    let mut n = 1;
    loop {
        let mut req = c.get(url);
        if let Some(a) = accept {
            req = req.header("Accept", a);
        }
        if let Some(t) = token() {
            req = req.bearer_auth(t);
        }
        let sent = req.send().and_then(|r| {
            let status = r.status().as_u16();
            let after = net::retry_after(r.headers());
            r.bytes().map(|b| (status, after, b.to_vec()))
        });
        let (why, after, last) = match sent {
            Ok((status, _, body)) if !net::transient_status(status) => return Ok((status, body)),
            Ok((status, after, body)) => (format!("HTTP {status}"), after, Some((status, body))),
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
        let hint = if status == 403 && token().is_none() {
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
    let (status, bytes) = get(&c, dl, None)?;
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
        Some(want) => matches_sha256(name, "the GitHub asset digest", &want, &bytes)?,
        None => verify_checksum(&c, name, &bytes, &assets)?,
    }
    let files = extract(name, &bytes, &args.bins)?;
    std::fs::create_dir_all(&args.to).with_context(|| format!("creating {}", args.to.display()))?;
    for (bin, data) in files {
        let dest = args.to.join(&bin);
        install_bytes(&dest, &data)?;
        println!("installed {bin} {tag} -> {}", dest.display());
    }
    Ok(())
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
    use sha2::Digest;
    sha2::Sha256::digest(data)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn matches_sha256(asset: &str, source: &str, want: &str, data: &[u8]) -> Result<()> {
    let got = sha256_hex(data);
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
    data: &[u8],
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
        return matches_sha256(asset, sums, &want, data);
    }
    eprintln!("[envira] the release publishes no checksum for {asset}; not verified");
    Ok(())
}

fn install_bytes(dest: &Path, data: &[u8]) -> Result<()> {
    let dir = dest.parent().context("destination without parent")?;
    let tmp = dir.join(format!(
        ".{}.envira-tmp",
        dest.file_name().and_then(|n| n.to_str()).unwrap_or("bin")
    ));
    std::fs::write(&tmp, data).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))?;
    std::fs::rename(&tmp, dest).with_context(|| format!("installing {}", dest.display()))?;
    Ok(())
}

/// Returns `(bin, content)` for each requested binary found in the asset.
pub fn extract(name: &str, bytes: &[u8], bins: &[String]) -> Result<Vec<(String, Vec<u8>)>> {
    let mut found: Vec<(String, String, Vec<u8>)> = Vec::new(); // bin, archive path, data
    let mut consider = |path: &str, read: &mut dyn FnMut() -> Result<Vec<u8>>| -> Result<()> {
        let base = path.rsplit('/').next().unwrap_or(path);
        if let Some(b) = bins.iter().find(|b| b.as_str() == base) {
            // Prefer the shallowest match (e.g. `bat-v1/bat` over `bat-v1/autocomplete/bat`).
            let depth = path.matches('/').count();
            let better = found.iter().position(|f| &f.0 == b);
            match better {
                Some(i) if found[i].1.matches('/').count() <= depth => {}
                Some(i) => found[i] = (b.clone(), path.to_string(), read()?),
                None => found.push((b.clone(), path.to_string(), read()?)),
            }
        }
        Ok(())
    };
    if name.ends_with(".tar.gz") || name.ends_with(".tgz") {
        let mut ar = tar::Archive::new(flate2::read::GzDecoder::new(bytes));
        for entry in ar.entries()? {
            let mut e = entry?;
            if !e.header().entry_type().is_file() {
                continue;
            }
            let path = e.path()?.display().to_string();
            consider(&path, &mut || {
                let mut v = Vec::new();
                e.read_to_end(&mut v)?;
                Ok(v)
            })?;
        }
    } else if name.ends_with(".zip") {
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes))?;
        for i in 0..zip.len() {
            let mut f = zip.by_index(i)?;
            if !f.is_file() {
                continue;
            }
            let path = f.name()?.to_string();
            consider(&path, &mut || {
                let mut v = Vec::new();
                f.read_to_end(&mut v)?;
                Ok(v)
            })?;
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
        return Ok(vec![(bins[0].clone(), bytes.to_vec())]);
    }
    let missing: Vec<&String> = bins
        .iter()
        .filter(|b| !found.iter().any(|f| &f.0 == *b))
        .collect();
    if !missing.is_empty() {
        bail!("{name} does not contain {missing:?}");
    }
    Ok(bins
        .iter()
        .map(|b| {
            found
                .iter()
                .find(|f| &f.0 == b)
                .map(|f| (f.0.clone(), f.2.clone()))
                .unwrap()
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

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
        assert!(matches_sha256("x", "digest", abc, b"abc").is_ok());
        let err = matches_sha256("x", "digest", abc, b"abd")
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
        let out = extract("bat-v1.tar.gz", &data, &["bat".into()]).unwrap();
        assert_eq!(out, vec![("bat".to_string(), b"binary".to_vec())]);
        assert!(
            extract("bat-v1.tar.gz", &data, &["nope".into()])
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
        let out = extract("yazi.zip", buf.get_ref(), &["yazi".into(), "ya".into()]).unwrap();
        assert_eq!(out[0], ("yazi".to_string(), b"Y".to_vec()));
        assert_eq!(out[1], ("ya".to_string(), b"A".to_vec()));
    }

    #[test]
    fn bare_file_and_install() {
        let out = extract("pipx.pyz", b"PK..", &["pipx".into()]).unwrap();
        assert_eq!(out[0].0, "pipx");
        let d = tempfile::tempdir().unwrap();
        install_bytes(&d.path().join("pipx"), &out[0].1).unwrap();
        let m = std::fs::metadata(d.path().join("pipx")).unwrap();
        assert_eq!(m.permissions().mode() & 0o777, 0o755);
        assert!(extract("x.tar.xz", b"", &["x".into()]).is_err());
    }
}
