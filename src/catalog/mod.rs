use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Deserializer, Serialize};
use thiserror::Error;

use crate::verifier::VerifierSpec;

pub use crate::verifier::VerificationStage as SuccessThreshold;

pub const SUPPORTED_SCHEMA_VERSION: u32 = 1;
pub const ALL_DEFAULT_BUNDLE_ID: &str = "all-default";

const EMBEDDED_MANIFEST: &str = include_str!("manifest.toml");

pub fn embedded_manifest() -> &'static str {
    EMBEDDED_MANIFEST
}

pub fn load_embedded_catalog() -> Result<Catalog, CatalogError> {
    Catalog::from_toml_str(EMBEDDED_MANIFEST)
}

#[derive(Debug, Error)]
pub enum CatalogError {
    #[error("catalog manifest parse error: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("catalog validation error: {0}")]
    Validation(String),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Catalog {
    pub schema_version: u32,
    pub items: Vec<CatalogItem>,
    pub bundles: Vec<CatalogBundle>,
    pub default_bundles: Vec<CanonicalId>,
}

impl Catalog {
    pub fn from_toml_str(raw: &str) -> Result<Self, CatalogError> {
        let catalog: Self = toml::from_str(raw)?;
        catalog.validate()?;
        Ok(catalog)
    }

    pub fn item(&self, id: &str) -> Option<&CatalogItem> {
        self.items.iter().find(|item| item.id.as_str() == id)
    }

    pub fn bundle(&self, id: &str) -> Option<&CatalogBundle> {
        self.bundles.iter().find(|bundle| bundle.id.as_str() == id)
    }

    pub fn expand_bundle(&self, bundle_id: &str) -> Result<Vec<&CatalogItem>, CatalogError> {
        if bundle_id == ALL_DEFAULT_BUNDLE_ID {
            return self.expand_default_bundles();
        }

        let bundle = self.bundle(bundle_id).ok_or_else(|| {
            CatalogError::Validation(format!(
                "bundle `{bundle_id}` is not defined in the catalog"
            ))
        })?;

        self.collect_items(&bundle.items)
    }

    pub fn expand_default_bundles(&self) -> Result<Vec<&CatalogItem>, CatalogError> {
        let mut ordered_item_ids = Vec::new();
        let mut seen = BTreeSet::new();

        for bundle_id in &self.default_bundles {
            let bundle = self.bundle(bundle_id.as_str()).ok_or_else(|| {
                CatalogError::Validation(format!(
                    "default bundle `{}` is not defined in the catalog",
                    bundle_id
                ))
            })?;

            for item_id in &bundle.items {
                if seen.insert(item_id.clone()) {
                    ordered_item_ids.push(item_id.clone());
                }
            }
        }

        self.collect_items(&ordered_item_ids)
    }

    pub fn default_install_ids(&self) -> Result<Vec<&str>, CatalogError> {
        Ok(self
            .expand_default_bundles()?
            .into_iter()
            .map(|item| item.id.as_str())
            .collect())
    }

    fn collect_items<'a>(
        &'a self,
        item_ids: &[CanonicalId],
    ) -> Result<Vec<&'a CatalogItem>, CatalogError> {
        item_ids
            .iter()
            .map(|item_id| {
                self.item(item_id.as_str()).ok_or_else(|| {
                    CatalogError::Validation(format!(
                        "bundle references undefined item `{}`",
                        item_id
                    ))
                })
            })
            .collect()
    }

    fn validate(&self) -> Result<(), CatalogError> {
        if self.schema_version != SUPPORTED_SCHEMA_VERSION {
            return Err(CatalogError::Validation(format!(
                "schema_version `{}` is not supported; expected `{}`",
                self.schema_version, SUPPORTED_SCHEMA_VERSION
            )));
        }

        if self.items.is_empty() {
            return Err(CatalogError::Validation(
                "catalog must define at least one item".to_string(),
            ));
        }

        if self.bundles.is_empty() {
            return Err(CatalogError::Validation(
                "catalog must define at least one bundle".to_string(),
            ));
        }

        if self.default_bundles.is_empty() {
            return Err(CatalogError::Validation(
                "catalog must define at least one default bundle".to_string(),
            ));
        }

        let mut item_ids = BTreeSet::new();
        for item in &self.items {
            item.validate()?;
            if !item_ids.insert(item.id.clone()) {
                return Err(CatalogError::Validation(format!(
                    "item id `{}` is defined more than once",
                    item.id
                )));
            }
        }

        let mut bundle_ids = BTreeSet::new();
        let mut bundled_item_counts: BTreeMap<CanonicalId, usize> = self
            .items
            .iter()
            .map(|item| (item.id.clone(), 0usize))
            .collect();

        for bundle in &self.bundles {
            bundle.validate()?;

            if bundle.id.as_str() == ALL_DEFAULT_BUNDLE_ID {
                return Err(CatalogError::Validation(format!(
                    "bundle id `{ALL_DEFAULT_BUNDLE_ID}` is reserved for default bundle expansion"
                )));
            }

            if !bundle_ids.insert(bundle.id.clone()) {
                return Err(CatalogError::Validation(format!(
                    "bundle id `{}` is defined more than once",
                    bundle.id
                )));
            }

            let mut seen_bundle_items = BTreeSet::new();
            for item_id in &bundle.items {
                if !seen_bundle_items.insert(item_id.clone()) {
                    return Err(CatalogError::Validation(format!(
                        "bundle `{}` references item `{}` more than once",
                        bundle.id, item_id
                    )));
                }
                if !item_ids.contains(item_id) {
                    return Err(CatalogError::Validation(format!(
                        "bundle `{}` references undefined item `{}`",
                        bundle.id, item_id
                    )));
                }

                if let Some(count) = bundled_item_counts.get_mut(item_id) {
                    *count += 1;
                }
            }
        }

        let mut seen_default_bundles = BTreeSet::new();
        for bundle_id in &self.default_bundles {
            if !seen_default_bundles.insert(bundle_id.clone()) {
                return Err(CatalogError::Validation(format!(
                    "default bundle `{}` is listed more than once",
                    bundle_id
                )));
            }
            if !bundle_ids.contains(bundle_id) {
                return Err(CatalogError::Validation(format!(
                    "default bundle `{}` is not defined in bundles",
                    bundle_id
                )));
            }
        }

        for item in &self.items {
            for dependency in &item.depends_on {
                if dependency == &item.id {
                    return Err(CatalogError::Validation(format!(
                        "item `{}` cannot depend on itself",
                        item.id
                    )));
                }

                if !item_ids.contains(dependency) {
                    return Err(CatalogError::Validation(format!(
                        "item `{}` depends on undefined item `{}`",
                        item.id, dependency
                    )));
                }
            }

            let bundled = bundled_item_counts
                .get(&item.id)
                .copied()
                .unwrap_or_default()
                > 0;

            match (bundled, item.standalone) {
                (true, true) => {
                    return Err(CatalogError::Validation(format!(
                        "item `{}` cannot be bundled and standalone at the same time",
                        item.id
                    )));
                }
                (false, false) => {
                    return Err(CatalogError::Validation(format!(
                        "item `{}` must belong to at least one bundle or be marked standalone",
                        item.id
                    )));
                }
                _ => {}
            }
        }

        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogItem {
    pub id: CanonicalId,
    pub display_name: String,
    pub category: ItemCategory,
    pub scope: InstallScope,
    pub depends_on: Vec<CanonicalId>,
    pub targets: Vec<InstallTarget>,
    #[serde(default)]
    pub recipes: Vec<RecipeOverlay>,
    pub success_threshold: SuccessThreshold,
    pub verifier: VerifierSpec,
    pub standalone: bool,
}

impl CatalogItem {
    pub fn recipe_for_target(&self, target: &InstallTarget) -> Option<&RecipeOverlay> {
        self.recipes
            .iter()
            .find(|recipe| recipe.backend == target.backend && recipe.source == target.source)
    }

    fn validate(&self) -> Result<(), CatalogError> {
        if self.display_name.trim().is_empty() {
            return Err(CatalogError::Validation(format!(
                "item `{}` must define a non-empty display_name",
                self.id
            )));
        }

        if self.targets.is_empty() {
            return Err(CatalogError::Validation(format!(
                "item `{}` must define at least one target",
                self.id
            )));
        }

        let mut seen_targets = BTreeSet::new();
        for target in &self.targets {
            if !seen_targets.insert((target.backend, target.source)) {
                return Err(CatalogError::Validation(format!(
                    "item `{}` defines duplicate target `{:?}/{:?}`",
                    self.id, target.backend, target.source
                )));
            }
        }

        let declared_targets = self
            .targets
            .iter()
            .map(|target| (target.backend, target.source))
            .collect::<BTreeSet<_>>();
        let mut seen_recipes = BTreeSet::new();
        for recipe in &self.recipes {
            recipe
                .validate(self.id.as_str())
                .map_err(CatalogError::Validation)?;
            let key = (recipe.backend, recipe.source);
            if !seen_recipes.insert(key) {
                return Err(CatalogError::Validation(format!(
                    "item `{}` defines duplicate recipe overlay `{:?}/{:?}`",
                    self.id, recipe.backend, recipe.source
                )));
            }
            if !declared_targets.contains(&key) {
                return Err(CatalogError::Validation(format!(
                    "item `{}` defines recipe overlay `{:?}/{:?}` without a matching target",
                    self.id, recipe.backend, recipe.source
                )));
            }
        }

        let mut seen_dependencies = BTreeSet::new();
        for dependency in &self.depends_on {
            if !seen_dependencies.insert(dependency.clone()) {
                return Err(CatalogError::Validation(format!(
                    "item `{}` lists dependency `{}` more than once",
                    self.id, dependency
                )));
            }
        }

        self.verifier
            .validate(self.id.as_str())
            .map_err(CatalogError::Validation)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogBundle {
    pub id: CanonicalId,
    pub display_name: String,
    pub items: Vec<CanonicalId>,
}

impl CatalogBundle {
    fn validate(&self) -> Result<(), CatalogError> {
        if self.display_name.trim().is_empty() {
            return Err(CatalogError::Validation(format!(
                "bundle `{}` must define a non-empty display_name",
                self.id
            )));
        }

        if self.items.is_empty() {
            return Err(CatalogError::Validation(format!(
                "bundle `{}` must reference at least one item",
                self.id
            )));
        }

        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemCategory {
    Foundation,
    TerminalTool,
    ContainerTool,
    SystemMonitor,
    RemoteAccess,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallScope {
    System,
    User,
    Hybrid,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TargetBackend {
    Apt,
    Pacman,
    Dnf,
    Zypper,
    DirectBinary,
    Archive,
    SourceBuild,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TargetSource {
    DistributionPackage,
    GithubRelease,
    GitRepository,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstallTarget {
    pub backend: TargetBackend,
    pub source: TargetSource,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RecipeOverlay {
    pub backend: TargetBackend,
    pub source: TargetSource,
    #[serde(flatten)]
    pub recipe: RecipeSpec,
}

impl RecipeOverlay {
    fn validate(&self, item_id: &str) -> Result<(), String> {
        self.recipe.validate(item_id, self.backend, self.source)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "recipe", rename_all = "snake_case")]
pub enum RecipeSpec {
    NativePackage {
        packages: Vec<String>,
    },
    DirectBinary {
        url: String,
        binary_name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        checksum_sha256: Option<String>,
    },
    Archive {
        url: String,
        format: RecipeArchiveFormat,
        binary_name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        member_path: Option<PathBuf>,
        #[serde(default)]
        strip_components: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        checksum_sha256: Option<String>,
    },
    SourceBuild {
        source_url: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        revision: Option<String>,
        build_system: RecipeBuildSystem,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        working_subdir: Option<PathBuf>,
    },
}

impl RecipeSpec {
    fn validate(
        &self,
        item_id: &str,
        backend: TargetBackend,
        source: TargetSource,
    ) -> Result<(), String> {
        match self {
            Self::NativePackage { packages } => {
                if !matches!(
                    backend,
                    TargetBackend::Apt
                        | TargetBackend::Pacman
                        | TargetBackend::Dnf
                        | TargetBackend::Zypper
                ) {
                    return Err(format!(
                        "item `{item_id}` uses native_package recipe for non-native backend `{:?}`",
                        backend
                    ));
                }
                if source != TargetSource::DistributionPackage {
                    return Err(format!(
                        "item `{item_id}` uses native_package recipe for unsupported source `{:?}`",
                        source
                    ));
                }
                if packages.is_empty() {
                    return Err(format!(
                        "item `{item_id}` must declare at least one package for `{:?}/{:?}`",
                        backend, source
                    ));
                }
                for package in packages {
                    if package.trim().is_empty() {
                        return Err(format!(
                            "item `{item_id}` contains an empty package name for `{:?}/{:?}`",
                            backend, source
                        ));
                    }
                }
            }
            Self::DirectBinary {
                url, binary_name, ..
            } => {
                if backend != TargetBackend::DirectBinary {
                    return Err(format!(
                        "item `{item_id}` uses direct_binary recipe for backend `{:?}`",
                        backend
                    ));
                }
                if source != TargetSource::GithubRelease {
                    return Err(format!(
                        "item `{item_id}` uses direct_binary recipe for unsupported source `{:?}`",
                        source
                    ));
                }
                validate_non_empty_field(item_id, "url", url)?;
                validate_non_empty_field(item_id, "binary_name", binary_name)?;
            }
            Self::Archive {
                url, binary_name, ..
            } => {
                if backend != TargetBackend::Archive {
                    return Err(format!(
                        "item `{item_id}` uses archive recipe for backend `{:?}`",
                        backend
                    ));
                }
                if source != TargetSource::GithubRelease {
                    return Err(format!(
                        "item `{item_id}` uses archive recipe for unsupported source `{:?}`",
                        source
                    ));
                }
                validate_non_empty_field(item_id, "url", url)?;
                validate_non_empty_field(item_id, "binary_name", binary_name)?;
            }
            Self::SourceBuild {
                source_url,
                working_subdir,
                ..
            } => {
                if backend != TargetBackend::SourceBuild {
                    return Err(format!(
                        "item `{item_id}` uses source_build recipe for backend `{:?}`",
                        backend
                    ));
                }
                if source != TargetSource::GitRepository {
                    return Err(format!(
                        "item `{item_id}` uses source_build recipe for unsupported source `{:?}`",
                        source
                    ));
                }
                validate_non_empty_field(item_id, "source_url", source_url)?;
                if working_subdir
                    .as_ref()
                    .is_some_and(|path| path.as_os_str().is_empty())
                {
                    return Err(format!(
                        "item `{item_id}` must not declare an empty working_subdir for `{:?}/{:?}`",
                        backend, source
                    ));
                }
            }
        }

        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecipeArchiveFormat {
    TarGz,
    TarXz,
    TarBz2,
    Zip,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecipeBuildSystem {
    Autotools,
    Cmake,
    Cargo,
    Go,
    Python,
    Make,
}

fn validate_non_empty_field(item_id: &str, field_name: &str, value: &str) -> Result<(), String> {
    if value.trim().is_empty() {
        Err(format!(
            "item `{item_id}` must define a non-empty `{field_name}` in recipe overlays"
        ))
    } else {
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize)]
#[serde(transparent)]
pub struct CanonicalId(String);

impl CanonicalId {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn parse(value: String) -> Result<Self, String> {
        if value.is_empty() {
            return Err("canonical ids must not be empty".to_string());
        }

        if value.starts_with('-') || value.ends_with('-') {
            return Err(format!(
                "canonical id `{value}` must not start or end with a hyphen"
            ));
        }

        if value.contains("--") {
            return Err(format!(
                "canonical id `{value}` must not contain consecutive hyphens"
            ));
        }

        if !value.chars().all(|character| {
            character.is_ascii_lowercase() || character.is_ascii_digit() || character == '-'
        }) {
            return Err(format!(
                "canonical id `{value}` must contain only lowercase ASCII letters, digits, and hyphens"
            ));
        }

        Ok(Self(value))
    }
}

impl<'de> Deserialize<'de> for CanonicalId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(value).map_err(serde::de::Error::custom)
    }
}

impl fmt::Display for CanonicalId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}
