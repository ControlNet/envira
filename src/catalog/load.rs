//! Catalog layers: a base (explicit `--catalog` paths, else the embedded snapshot) plus the
//! local overlay `~/.config/envira/catalog.d/*.toml`. Later files override earlier ones per field.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use include_dir::{Dir, include_dir};

use super::schema::Catalog;

static EMBEDDED: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/catalog");

/// One catalog file to load.
#[derive(Debug, Clone)]
pub struct Source {
    /// Display name, e.g. `terminal.toml` or a full path.
    pub name: String,
    pub text: String,
}

#[derive(Debug, Clone)]
pub struct Layer {
    /// e.g. `embedded`, `overlay ~/.config/envira/catalog.d`.
    pub label: String,
    pub sources: Vec<Source>,
}

#[derive(Debug, Clone, Default)]
pub struct Loaded {
    pub catalog: Catalog,
    /// `tools.bat` -> file that last defined it.
    pub origins: BTreeMap<String, String>,
    /// Human-readable description of each active layer.
    pub layers: Vec<String>,
    pub warnings: Vec<String>,
}

impl Loaded {
    pub fn origin(&self, section: &str, id: &str) -> &str {
        self.origins
            .get(&format!("{section}.{id}"))
            .map(String::as_str)
            .unwrap_or("?")
    }
}

pub fn embedded_layer() -> Layer {
    let mut sources: Vec<Source> = EMBEDDED
        .files()
        .filter(|f| f.path().extension().is_some_and(|e| e == "toml"))
        .map(|f| Source {
            name: f.path().display().to_string(),
            text: String::from_utf8_lossy(f.contents()).into_owned(),
        })
        .collect();
    sources.sort_by(|a, b| a.name.cmp(&b.name));
    Layer {
        label: format!("embedded ({} files)", sources.len()),
        sources,
    }
}

/// Reads a catalog file or every `*.toml` in a directory (sorted by name).
pub fn path_layer(label: &str, path: &Path) -> Result<Layer> {
    let mut files: Vec<PathBuf> = Vec::new();
    if path.is_dir() {
        for entry in
            std::fs::read_dir(path).with_context(|| format!("reading {}", path.display()))?
        {
            let p = entry?.path();
            if p.extension().is_some_and(|e| e == "toml") {
                files.push(p);
            }
        }
        files.sort();
    } else if path.is_file() {
        files.push(path.to_path_buf());
    } else {
        bail!("catalog path {} does not exist", path.display());
    }
    let sources = files
        .iter()
        .map(|p| {
            Ok(Source {
                name: p.display().to_string(),
                text: std::fs::read_to_string(p)
                    .with_context(|| format!("reading {}", p.display()))?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(Layer {
        label: format!("{label} {} ({} files)", path.display(), sources.len()),
        sources,
    })
}

/// Merges the layers in order and deserializes the result.
pub fn load(layers: &[Layer]) -> Result<Loaded> {
    let mut merged = toml::Table::new();
    let mut loaded = Loaded::default();
    for layer in layers {
        let mut seen_in_layer: BTreeMap<String, String> = BTreeMap::new();
        for src in &layer.sources {
            let mut table = parse_source(src)?;
            let stem = Path::new(&src.name)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string();
            inject_category(&mut table, &stem, &merged);
            for section in ["methods", "actions", "tools", "configs", "bundles"] {
                if let Some(toml::Value::Table(items)) = table.get(section) {
                    for id in items.keys() {
                        let key = format!("{section}.{id}");
                        if let Some(prev) = seen_in_layer.insert(key.clone(), src.name.clone()) {
                            loaded
                                .warnings
                                .push(format!("{key} is defined in both {prev} and {}", src.name));
                        }
                        loaded.origins.insert(key, src.name.clone());
                    }
                }
            }
            merge(&mut merged, table);
        }
        loaded.layers.push(layer.label.clone());
    }
    loaded.catalog = toml::Value::Table(merged)
        .try_into()
        .context("merging catalog layers")?;
    Ok(loaded)
}

fn parse_source(src: &Source) -> Result<toml::Table> {
    // Typed parse first: its errors carry line and column for unknown or mistyped fields.
    toml::from_str::<Catalog>(&src.text).with_context(|| format!("in {}", src.name))?;
    toml::from_str::<toml::Table>(&src.text).with_context(|| format!("in {}", src.name))
}

/// Items first defined by a file default to the file name as category; overrides keep theirs.
fn inject_category(table: &mut toml::Table, stem: &str, merged: &toml::Table) {
    if stem.is_empty() || stem == "templates" {
        return;
    }
    for section in ["tools", "configs"] {
        let existing = merged.get(section).and_then(toml::Value::as_table);
        if let Some(toml::Value::Table(items)) = table.get_mut(section) {
            for (id, item) in items.iter_mut() {
                if existing.is_some_and(|e| e.contains_key(id)) {
                    continue;
                }
                if let toml::Value::Table(t) = item {
                    t.entry("category")
                        .or_insert_with(|| toml::Value::String(stem.to_string()));
                }
            }
        }
    }
}

fn merge(base: &mut toml::Table, over: toml::Table) {
    for (k, v) in over {
        match (base.get_mut(&k), v) {
            (Some(toml::Value::Table(b)), toml::Value::Table(o)) => merge(b, o),
            (_, v) => {
                base.insert(k, v);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layer(files: &[(&str, &str)]) -> Layer {
        Layer {
            label: "test".into(),
            sources: files
                .iter()
                .map(|(n, t)| Source {
                    name: n.to_string(),
                    text: t.to_string(),
                })
                .collect(),
        }
    }

    #[test]
    fn overlay_overrides_single_field_and_keeps_the_rest() {
        let base = layer(&[(
            "terminal.toml",
            r#"[tools.bat]
desc = "cat clone"
install = [{ method = "apt", package = "bat" }]"#,
        )]);
        let overlay = layer(&[(
            "local.toml",
            r#"[tools.bat]
desc = "mine""#,
        )]);
        let loaded = load(&[base, overlay]).unwrap();
        let bat = &loaded.catalog.tools["bat"];
        assert_eq!(bat.desc.as_deref(), Some("mine"));
        assert_eq!(bat.install.len(), 1);
        assert_eq!(bat.category.as_deref(), Some("terminal"));
        assert_eq!(loaded.origin("tools", "bat"), "local.toml");
    }

    #[test]
    fn typo_in_field_reports_file() {
        let bad = layer(&[("x.toml", "[tools.bat]\ndesk = 1\n")]);
        let err = format!("{:#}", load(&[bad]).unwrap_err());
        assert!(err.contains("x.toml"), "{err}");
        assert!(err.contains("desk"), "{err}");
    }

    #[test]
    fn duplicate_id_in_one_layer_warns() {
        let l = layer(&[("a.toml", "[tools.x]\n"), ("b.toml", "[tools.x]\n")]);
        let loaded = load(&[l]).unwrap();
        assert_eq!(loaded.warnings.len(), 1);
    }

    #[test]
    fn embedded_catalog_loads() {
        let loaded = load(&[embedded_layer()]).unwrap();
        assert!(loaded.catalog.tools.contains_key("bat"));
        assert!(loaded.catalog.methods.contains_key("github-release"));
        assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);
    }
}
