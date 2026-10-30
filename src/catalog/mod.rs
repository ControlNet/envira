pub mod load;
pub mod remote;
pub mod schema;
pub mod validate;

pub use load::Loaded;
pub use schema::*;

use std::path::PathBuf;

use anyhow::Result;

/// Where to read catalog layers from.
#[derive(Debug, Clone, Default)]
pub struct CatalogOptions {
    /// Replaces the embedded snapshot as the base layer (`--catalog`, `ENVIRA_CATALOG`).
    pub explicit: Vec<PathBuf>,
    /// `~/.config/envira/catalog.d`; skipped when it does not exist.
    pub overlay: Option<PathBuf>,
    /// The published catalog, used instead of the embedded snapshot when it can be.
    pub remote: Option<remote::Remote>,
}

pub fn load_catalog(opts: &CatalogOptions) -> Result<Loaded> {
    let mut layers = Vec::new();
    let mut warning = None;
    if opts.explicit.is_empty() {
        let base = opts.remote.as_ref().and_then(|r| {
            let (layer, w) =
                remote::remote_layer(r, std::time::SystemTime::now(), remote::http_fetch);
            warning = w;
            layer
        });
        layers.push(base.unwrap_or_else(load::embedded_layer));
    } else {
        for p in &opts.explicit {
            layers.push(load::path_layer("explicit", p)?);
        }
    }
    if let Some(dir) = &opts.overlay
        && dir.is_dir()
    {
        layers.push(load::path_layer("overlay", dir)?);
    }
    let mut loaded = load::load(&layers)?;
    loaded.warnings.extend(warning);
    Ok(loaded)
}

impl Catalog {
    /// Ids of tools and configs in display order: category, then id.
    pub fn item_ids(&self) -> Vec<String> {
        let mut v: Vec<(String, String)> = self
            .tools
            .iter()
            .map(|(id, t)| (t.category.clone().unwrap_or_default(), id.clone()))
            .chain(
                self.configs
                    .iter()
                    .map(|(id, c)| (c.category.clone().unwrap_or_default(), id.clone())),
            )
            .collect();
        v.sort();
        v.into_iter().map(|(_, id)| id).collect()
    }

    pub fn category(&self, id: &str) -> &str {
        self.tools
            .get(id)
            .and_then(|t| t.category.as_deref())
            .or_else(|| self.configs.get(id).and_then(|c| c.category.as_deref()))
            .unwrap_or("other")
    }

    pub fn desc(&self, id: &str) -> &str {
        self.tools
            .get(id)
            .and_then(|t| t.desc.as_deref())
            .or_else(|| self.configs.get(id).and_then(|c| c.desc.as_deref()))
            .unwrap_or("")
    }

    /// Item-level dependencies of a tool or config.
    pub fn depends(&self, id: &str) -> &[String] {
        self.tools
            .get(id)
            .map(|t| t.depends.as_slice())
            .or_else(|| self.configs.get(id).map(|c| c.depends.as_slice()))
            .unwrap_or(&[])
    }

    /// Items of a bundle including those of nested bundles, in order and without duplicates.
    pub fn bundle_items(&self, id: &str) -> Vec<String> {
        fn walk(cat: &Catalog, id: &str, seen: &mut Vec<String>, out: &mut Vec<String>) {
            if seen.iter().any(|s| s == id) {
                return;
            }
            seen.push(id.to_string());
            let Some(b) = cat.bundles.get(id) else { return };
            for i in &b.items {
                if !out.contains(i) {
                    out.push(i.clone());
                }
            }
            for nested in &b.bundles {
                walk(cat, nested, seen, out);
            }
        }
        let mut out = Vec::new();
        walk(self, id, &mut Vec::new(), &mut out);
        out
    }

    pub fn bundle_name<'a>(&'a self, id: &'a str) -> &'a str {
        self.bundles
            .get(id)
            .and_then(|b| b.name.as_deref())
            .unwrap_or(id)
    }
}
