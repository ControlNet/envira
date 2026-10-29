//! `envira fetch-release`: download a GitHub release asset for this arch and install binaries
//! from it. Extraction is in-process, so bare machines need neither curl nor tar/unzip.

use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

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

pub fn run(args: &FetchArgs) -> Result<()> {
    let c = client()?;
    let url = match &args.tag {
        Some(t) => format!(
            "https://api.github.com/repos/{}/releases/tags/{t}",
            args.repo
        ),
        None => format!("https://api.github.com/repos/{}/releases/latest", args.repo),
    };
    let mut req = c.get(&url).header("Accept", "application/vnd.github+json");
    if let Some(t) = token() {
        req = req.bearer_auth(t);
    }
    eprintln!("[envira] GET {url}");
    let resp = req.send().with_context(|| format!("requesting {url}"))?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().unwrap_or_default();
        let msg = serde_json::from_str::<serde_json::Value>(&body)
            .ok()
            .and_then(|v| v["message"].as_str().map(String::from))
            .unwrap_or(body);
        let hint = if status.as_u16() == 403 && token().is_none() {
            " (set GITHUB_TOKEN to raise the rate limit)"
        } else {
            ""
        };
        bail!("HTTP {} from GitHub: {msg}{hint}", status.as_u16());
    }
    let release: serde_json::Value = resp.json()?;
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
    let mut req = c.get(dl);
    if let Some(t) = token() {
        req = req.bearer_auth(t);
    }
    let resp = req.send()?.error_for_status()?;
    let bytes = resp.bytes()?;
    let files = extract(name, &bytes, &args.bins)?;
    std::fs::create_dir_all(&args.to).with_context(|| format!("creating {}", args.to.display()))?;
    for (bin, data) in files {
        let dest = args.to.join(&bin);
        install_bytes(&dest, &data)?;
        println!("installed {bin} {tag} -> {}", dest.display());
    }
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
