//! The published catalog: `catalog.toml` (the output of `envira catalog show`) on the project's
//! site, cached in `~/.cache/envira`. Catalog changes reach every machine without a new release;
//! the embedded snapshot is the base only when there is neither a usable download nor a cache.
//!
//! The catalog holds the commands envira runs, with sudo for system units: it is trusted like the
//! legacy `curl … | bash` scripts from the same site, over HTTPS. `--no-remote` turns it off.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use super::load::{Layer, Source, load};
use super::validate::validate;

pub const URL: &str = "https://boot.controlnet.space/envira/catalog.toml";
/// A cache younger than this is used without asking the server.
pub const TTL: Duration = Duration::from_secs(3600);
const TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug, Clone)]
pub struct Remote {
    pub url: String,
    /// The cached catalog; its ETag is kept next to it.
    pub cache: PathBuf,
    /// uid and gid to give the cache files when running as root for `SUDO_USER`.
    pub owner: Option<(u32, u32)>,
}

pub enum Fetched {
    NotModified,
    New { text: String, etag: Option<String> },
}

/// The remote layer, or `None` (use the embedded snapshot), plus a warning when something failed.
pub fn remote_layer(
    r: &Remote,
    now: SystemTime,
    fetch: impl FnOnce(&str, Option<&str>) -> Result<Fetched, String>,
) -> (Option<Layer>, Option<String>) {
    let etag_path = r.cache.with_extension("etag");
    let age = std::fs::metadata(&r.cache)
        .and_then(|m| m.modified())
        .ok()
        .map(|t| now.duration_since(t).unwrap_or_default());
    if age.is_some_and(|a| a < TTL) {
        return from_cache(r, &format!("cached {}", ago(age.unwrap())));
    }
    let etag = age
        .and_then(|_| std::fs::read_to_string(&etag_path).ok())
        .map(|e| e.trim().to_string());
    match fetch(&r.url, etag.as_deref()) {
        Ok(Fetched::NotModified) => {
            touch(&r.cache);
            from_cache(r, "checked now")
        }
        Ok(Fetched::New { text, etag }) => match usable(&text) {
            Ok(()) => {
                if let Err(e) = write_cache(&r.cache, &text, etag.as_deref(), r.owner) {
                    let layer = layer(r, &text, "downloaded now");
                    return (
                        Some(layer),
                        Some(format!(
                            "cannot cache the catalog in {}: {e}",
                            r.cache.display()
                        )),
                    );
                }
                (Some(layer(r, &text, "updated now")), None)
            }
            Err(e) => {
                // Do not ask again before the TTL; keep using what there is.
                touch(&r.cache);
                let (l, _) = from_cache(r, "cached");
                (l, Some(format!("the published catalog was not used: {e}")))
            }
        },
        Err(e) => {
            touch(&r.cache);
            let (l, w) = from_cache(r, "offline, cached");
            let fell_back = if l.is_some() {
                "the cache"
            } else {
                "the embedded catalog"
            };
            (
                l,
                Some(w.unwrap_or(format!("could not fetch {}: {e}; using {fell_back}", r.url))),
            )
        }
    }
}

fn layer(r: &Remote, text: &str, how: &str) -> Layer {
    Layer {
        label: format!("remote {} ({how})", r.url),
        sources: vec![Source {
            name: "catalog.toml".into(),
            text: text.to_string(),
        }],
    }
}

/// The cache as the base layer, if it exists and this envira can use it (an older envira may
/// find a newer catalog it cannot read).
fn from_cache(r: &Remote, how: &str) -> (Option<Layer>, Option<String>) {
    let Ok(text) = std::fs::read_to_string(&r.cache) else {
        return (None, None);
    };
    match usable(&text) {
        Ok(()) => (Some(layer(r, &text, how)), None),
        Err(e) => (
            None,
            Some(format!(
                "the cached catalog was not used: {e}; using the embedded catalog"
            )),
        ),
    }
}

/// Parses and validates a whole catalog on its own (`min_envira`, schema version, references).
fn usable(text: &str) -> Result<(), String> {
    let loaded = load(&[Layer {
        label: "remote".into(),
        sources: vec![Source {
            name: "catalog.toml".into(),
            text: text.to_string(),
        }],
    }])
    .map_err(|e| format!("{e:#}"))?;
    let report = validate(&loaded);
    match report.errors.first() {
        None => Ok(()),
        Some(first) => Err(format!("{first} ({} errors)", report.errors.len())),
    }
}

fn write_cache(
    path: &Path,
    text: &str,
    etag: Option<&str>,
    owner: Option<(u32, u32)>,
) -> std::io::Result<()> {
    let mut created = Vec::new();
    let mut cur = path.parent();
    while let Some(d) = cur.filter(|d| !d.exists()) {
        created.push(d.to_path_buf());
        cur = d.parent();
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)?;
    let etag_path = path.with_extension("etag");
    match etag {
        Some(e) => std::fs::write(&etag_path, e)?,
        None => std::fs::remove_file(&etag_path).or(Ok::<(), std::io::Error>(()))?,
    }
    if let Some((uid, gid)) = owner {
        for p in created
            .iter()
            .map(PathBuf::as_path)
            .chain([path, etag_path.as_path()])
        {
            let _ = std::os::unix::fs::lchown(p, Some(uid), Some(gid));
        }
    }
    Ok(())
}

fn touch(path: &Path) {
    if let Ok(f) = std::fs::File::options().append(true).open(path) {
        let _ = f.set_modified(SystemTime::now());
    }
}

fn ago(d: Duration) -> String {
    match d.as_secs() / 60 {
        0 => "just now".into(),
        m => format!("{m} min ago"),
    }
}

/// HTTP GET with `If-None-Match`, in its own thread: reqwest's blocking client must not run on
/// the async runtime's threads.
pub fn http_fetch(url: &str, etag: Option<&str>) -> Result<Fetched, String> {
    let (url, etag) = (url.to_string(), etag.map(str::to_string));
    std::thread::spawn(move || {
        let client = reqwest::blocking::Client::builder()
            .timeout(TIMEOUT)
            .user_agent(concat!("envira/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| e.to_string())?;
        let mut req = client.get(&url);
        if let Some(e) = &etag {
            req = req.header(reqwest::header::IF_NONE_MATCH, e);
        }
        let resp = req.send().map_err(|e| e.to_string())?;
        match resp.status().as_u16() {
            304 => Ok(Fetched::NotModified),
            200 => {
                let etag = resp
                    .headers()
                    .get(reqwest::header::ETAG)
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_string);
                let text = resp.text().map_err(|e| e.to_string())?;
                Ok(Fetched::New { text, etag })
            }
            code => Err(format!("HTTP {code}")),
        }
    })
    .join()
    .unwrap_or_else(|_| Err("the download thread panicked".into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::load::embedded_layer;

    fn bundle() -> String {
        let cat = load(&[embedded_layer()]).unwrap().catalog;
        toml::to_string_pretty(&cat).unwrap()
    }

    fn remote(dir: &Path) -> Remote {
        Remote {
            url: "https://example.org/catalog.toml".into(),
            cache: dir.join("cache/catalog.toml"),
            owner: None,
        }
    }

    fn label(l: &Option<Layer>) -> &str {
        l.as_ref().map_or("embedded", |l| l.label.as_str())
    }

    fn age(p: &Path, d: Duration) {
        let f = std::fs::File::options().append(true).open(p).unwrap();
        f.set_modified(SystemTime::now() - d).unwrap();
    }

    #[test]
    fn the_bundle_loads_like_the_embedded_files() {
        let embedded = load(&[embedded_layer()]).unwrap().catalog;
        let r = remote(Path::new("/nonexistent"));
        let bundled = load(&[layer(&r, &bundle(), "test")]).unwrap().catalog;
        assert_eq!(
            serde_json::to_value(&embedded).unwrap(),
            serde_json::to_value(&bundled).unwrap()
        );
    }

    #[test]
    fn downloads_caches_and_revalidates_with_the_etag() {
        let dir = tempfile::tempdir().unwrap();
        let r = remote(dir.path());
        let now = SystemTime::now();
        let (l, w) = remote_layer(&r, now, |_, etag| {
            assert_eq!(etag, None);
            Ok(Fetched::New {
                text: bundle(),
                etag: Some("\"v1\"".into()),
            })
        });
        assert!(label(&l).ends_with("(updated now)"), "{}", label(&l));
        assert_eq!(w, None);
        // Within the TTL the server is not asked.
        let (l, _) = remote_layer(&r, now, |_, _| panic!("fetched within the TTL"));
        assert!(label(&l).contains("(cached"), "{}", label(&l));
        // Past it, the ETag makes it a cheap check.
        age(&r.cache, TTL * 2);
        let (l, w) = remote_layer(&r, SystemTime::now(), |_, etag| {
            assert_eq!(etag, Some("\"v1\""));
            Ok(Fetched::NotModified)
        });
        assert!(label(&l).ends_with("(checked now)"), "{}", label(&l));
        assert_eq!(w, None);
    }

    #[test]
    fn falls_back_to_the_cache_then_the_embedded_catalog() {
        let dir = tempfile::tempdir().unwrap();
        let r = remote(dir.path());
        let (l, w) = remote_layer(&r, SystemTime::now(), |_, _| Err("timed out".into()));
        assert!(l.is_none());
        assert!(w.unwrap().contains("using the embedded catalog"));

        write_cache(&r.cache, &bundle(), None, None).unwrap();
        age(&r.cache, TTL * 2);
        let (l, w) = remote_layer(&r, SystemTime::now(), |_, _| Err("timed out".into()));
        assert!(label(&l).ends_with("(offline, cached)"), "{}", label(&l));
        assert!(w.unwrap().contains("using the cache"));
        // A failed check is not repeated before the TTL.
        let (l, _) = remote_layer(&r, SystemTime::now(), |_, _| panic!("asked again"));
        assert!(l.is_some());
    }

    #[test]
    fn a_catalog_this_envira_cannot_use_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let r = remote(dir.path());
        let b = bundle();
        assert!(b.contains("min_envira = \"0.1.0\"\n"));
        let newer = b.replace("min_envira = \"0.1.0\"\n", "min_envira = \"999.0.0\"\n");
        let (l, w) = remote_layer(&r, SystemTime::now(), |_, _| {
            Ok(Fetched::New {
                text: newer,
                etag: None,
            })
        });
        assert!(l.is_none());
        assert!(w.unwrap().contains("needs envira >= 999.0.0"));
        assert!(!r.cache.exists(), "a rejected catalog must not be cached");

        let unknown = format!("{}\n[tools.x]\ndesc = \"x\"\nnew_field = 1\n", bundle());
        let (l, w) = remote_layer(&r, SystemTime::now(), |_, _| {
            Ok(Fetched::New {
                text: unknown,
                etag: None,
            })
        });
        assert!(l.is_none());
        assert!(w.unwrap().contains("not used"));
    }
}
