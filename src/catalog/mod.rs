pub mod load;
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
}

pub fn load_catalog(opts: &CatalogOptions) -> Result<Loaded> {
    let mut layers = Vec::new();
    if opts.explicit.is_empty() {
        layers.push(load::embedded_layer());
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
    load::load(&layers)
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

    pub fn bundle_name<'a>(&'a self, id: &'a str) -> &'a str {
        self.bundles
            .get(id)
            .and_then(|b| b.name.as_deref())
            .unwrap_or(id)
    }
}
