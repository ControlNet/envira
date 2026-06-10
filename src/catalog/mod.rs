use std::collections::BTreeSet;
use std::fmt;

use serde::{Deserialize, Deserializer, Serialize};
use thiserror::Error;

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
pub struct Catalog {
    pub required_version: String,
    pub distros: Vec<String>,
    pub shell: String,
    pub default_bundles: Vec<CanonicalId>,
    pub bundles: Vec<CatalogBundle>,
    pub items: Vec<CatalogItem>,
}

impl Catalog {
    pub fn from_toml_str(raw: &str) -> Result<Self, CatalogError> {
        let root: toml::Value = toml::from_str(raw)?;
        reject_legacy_catalog_shape(root.as_table())?;

        let document: RawCatalogDocument = root.try_into()?;
        let bundles = parse_bundles(document.bundles)?;
        let items = parse_items(document.items)?;

        let catalog = Self {
            required_version: document.required_version,
            distros: document.distros,
            shell: document.shell,
            default_bundles: document.default_bundles,
            bundles,
            items,
        };

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
                    "default_bundles references undefined bundle `{}`",
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
        if self.required_version.trim().is_empty() {
            return Err(CatalogError::Validation(
                "required_version must not be empty".to_string(),
            ));
        }

        if self.distros.is_empty() {
            return Err(CatalogError::Validation(
                "distros must define at least one supported distro".to_string(),
            ));
        }

        for distro in &self.distros {
            if distro.trim().is_empty() {
                return Err(CatalogError::Validation(
                    "distros must not contain empty distro ids".to_string(),
                ));
            }
        }

        if self.shell.trim().is_empty() {
            return Err(CatalogError::Validation(
                "shell must not be empty".to_string(),
            ));
        }

        if self.default_bundles.is_empty() {
            return Err(CatalogError::Validation(
                "default_bundles must reference at least one bundle".to_string(),
            ));
        }

        if self.bundles.is_empty() {
            return Err(CatalogError::Validation(
                "catalog must define at least one bundle".to_string(),
            ));
        }

        if self.items.is_empty() {
            return Err(CatalogError::Validation(
                "catalog must define at least one item".to_string(),
            ));
        }

        let supported_distro_ids = self.distros.iter().cloned().collect::<BTreeSet<_>>();
        let item_ids = self
            .items
            .iter()
            .map(|item| item.id.clone())
            .collect::<BTreeSet<_>>();
        let bundle_ids = self
            .bundles
            .iter()
            .map(|bundle| bundle.id.clone())
            .collect::<BTreeSet<_>>();

        for bundle_id in &self.default_bundles {
            if !bundle_ids.contains(bundle_id) {
                return Err(CatalogError::Validation(format!(
                    "default_bundles references undefined bundle `{bundle_id}`"
                )));
            }
        }

        for bundle in &self.bundles {
            bundle.validate(&item_ids)?;
        }

        for item in &self.items {
            item.validate(&item_ids, &supported_distro_ids)?;
        }

        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CatalogBundle {
    pub id: CanonicalId,
    pub name: String,
    pub desc: String,
    pub items: Vec<CanonicalId>,
}

impl CatalogBundle {
    fn validate(&self, item_ids: &BTreeSet<CanonicalId>) -> Result<(), CatalogError> {
        if self.name.trim().is_empty() {
            return Err(CatalogError::Validation(format!(
                "bundle `{}` must define a non-empty name",
                self.id
            )));
        }

        if self.desc.trim().is_empty() {
            return Err(CatalogError::Validation(format!(
                "bundle `{}` must define a non-empty desc",
                self.id
            )));
        }

        if self.items.is_empty() {
            return Err(CatalogError::Validation(format!(
                "bundle `{}` must reference at least one item",
                self.id
            )));
        }

        for item_id in &self.items {
            if !item_ids.contains(item_id) {
                return Err(CatalogError::Validation(format!(
                    "bundle `{}` references undefined item `{}`",
                    self.id, item_id
                )));
            }
        }

        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CatalogItem {
    pub id: CanonicalId,
    pub name: String,
    pub desc: String,
    pub depends_on: Vec<CanonicalId>,
    pub recipes: Vec<CatalogCommand>,
    pub verifiers: Vec<CatalogCommand>,
}

impl CatalogItem {
    fn validate(
        &self,
        item_ids: &BTreeSet<CanonicalId>,
        supported_distro_ids: &BTreeSet<String>,
    ) -> Result<(), CatalogError> {
        if self.name.trim().is_empty() {
            return Err(CatalogError::Validation(format!(
                "item `{}` must define a non-empty name",
                self.id
            )));
        }

        if self.desc.trim().is_empty() {
            return Err(CatalogError::Validation(format!(
                "item `{}` must define a non-empty desc",
                self.id
            )));
        }

        if self.recipes.is_empty() {
            return Err(CatalogError::Validation(format!(
                "item `{}` must define at least one recipe",
                self.id
            )));
        }

        if self.verifiers.is_empty() {
            return Err(CatalogError::Validation(format!(
                "item `{}` must define at least one verifier",
                self.id
            )));
        }

        for dependency in &self.depends_on {
            if !item_ids.contains(dependency) {
                return Err(CatalogError::Validation(format!(
                    "item `{}` depends_on undefined item `{}`",
                    self.id, dependency
                )));
            }
        }

        validate_command_contracts(
            self.id.as_str(),
            "recipes",
            &self.recipes,
            supported_distro_ids,
        )?;
        validate_command_contracts(
            self.id.as_str(),
            "verifiers",
            &self.verifiers,
            supported_distro_ids,
        )?;

        Ok(())
    }

    pub fn install_scope(&self) -> InstallScope {
        let has_user = self
            .recipes
            .iter()
            .any(|recipe| recipe.mode == CommandMode::User);
        let has_sudo = self
            .recipes
            .iter()
            .any(|recipe| recipe.mode == CommandMode::Sudo);

        match (has_user, has_sudo) {
            (true, true) => InstallScope::Hybrid,
            (true, false) => InstallScope::User,
            _ => InstallScope::System,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CatalogCommand {
    pub mode: CommandMode,
    pub distros: Vec<String>,
    pub cmd: String,
}

impl CatalogCommand {
    fn from_raw(item_id: &str, contract_kind: &str, raw: RawCommand) -> Result<Self, CatalogError> {
        let mode = CommandMode::parse(item_id, contract_kind, raw.mode.as_str())?;

        if raw.cmd.trim().is_empty() {
            return Err(CatalogError::Validation(format!(
                "item `{item_id}` {contract_kind} entry for mode `{}` must define a non-empty cmd",
                mode.as_str()
            )));
        }

        if raw.distros.is_empty() {
            return Err(CatalogError::Validation(format!(
                "item `{item_id}` {contract_kind} entry for mode `{}` must define at least one distro",
                mode.as_str()
            )));
        }

        Ok(Self {
            mode,
            distros: raw.distros,
            cmd: raw.cmd,
        })
    }

    fn expand_distros(
        &self,
        item_id: &str,
        contract_kind: &str,
        supported_distro_ids: &BTreeSet<String>,
    ) -> Result<BTreeSet<String>, CatalogError> {
        if self.distros.iter().any(|distro| distro == "*") {
            if self.distros.len() != 1 {
                return Err(CatalogError::Validation(format!(
                    "item `{item_id}` {contract_kind} entry for mode `{}` must use `distros = [\"*\"]` only",
                    self.mode.as_str(),
                )));
            }

            return Ok(supported_distro_ids.clone());
        }

        let mut expanded = BTreeSet::new();
        for distro in &self.distros {
            if !supported_distro_ids.contains(distro) {
                return Err(CatalogError::Validation(format!(
                    "item `{item_id}` {contract_kind} entry for mode `{}` references unsupported distro `{distro}`",
                    self.mode.as_str(),
                )));
            }

            expanded.insert(distro.clone());
        }

        Ok(expanded)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandMode {
    Sudo,
    User,
}

impl CommandMode {
    fn parse(item_id: &str, contract_kind: &str, raw_mode: &str) -> Result<Self, CatalogError> {
        match raw_mode {
            "sudo" => Ok(Self::Sudo),
            "user" => Ok(Self::User),
            other => Err(CatalogError::Validation(format!(
                "item `{item_id}` {contract_kind} entry uses unsupported mode `{other}`"
            ))),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Sudo => "sudo",
            Self::User => "user",
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCatalogDocument {
    #[serde(default)]
    required_version: String,
    #[serde(default)]
    distros: Vec<String>,
    #[serde(default)]
    shell: String,
    #[serde(default)]
    default_bundles: Vec<CanonicalId>,
    #[serde(default)]
    bundles: toml::Table,
    #[serde(default)]
    items: toml::Table,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawBundle {
    #[serde(default)]
    name: String,
    #[serde(default)]
    desc: String,
    #[serde(default)]
    items: Vec<CanonicalId>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawItem {
    #[serde(default)]
    name: String,
    #[serde(default)]
    desc: String,
    #[serde(default)]
    depends_on: Vec<CanonicalId>,
    #[serde(default)]
    recipes: Vec<RawCommand>,
    #[serde(default)]
    verifiers: Vec<RawCommand>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCommand {
    #[serde(default)]
    mode: String,
    #[serde(default)]
    distros: Vec<String>,
    #[serde(default)]
    cmd: String,
}

fn reject_legacy_catalog_shape(root: Option<&toml::Table>) -> Result<(), CatalogError> {
    let Some(root) = root else {
        return Ok(());
    };

    let has_legacy_root_field = root.contains_key("schema_version");
    let has_legacy_item_array = root.get("items").is_some_and(toml::Value::is_array);
    let has_legacy_bundle_array = root.get("bundles").is_some_and(toml::Value::is_array);

    if has_legacy_root_field || has_legacy_item_array || has_legacy_bundle_array {
        return Err(CatalogError::Validation(
            "legacy catalog shape is no longer supported; use `required_version`, `distros`, `shell`, `default_bundles`, and keyed `[bundles.<id>]` / `[items.<id>]` tables"
                .to_string(),
        ));
    }

    Ok(())
}

fn parse_bundles(raw_bundles: toml::Table) -> Result<Vec<CatalogBundle>, CatalogError> {
    raw_bundles
        .into_iter()
        .map(|(bundle_id, value)| {
            let id = CanonicalId::parse(bundle_id).map_err(CatalogError::Validation)?;
            let bundle: RawBundle = value.try_into()?;

            Ok(CatalogBundle {
                id,
                name: bundle.name,
                desc: bundle.desc,
                items: bundle.items,
            })
        })
        .collect()
}

fn parse_items(raw_items: toml::Table) -> Result<Vec<CatalogItem>, CatalogError> {
    raw_items
        .into_iter()
        .map(|(item_id, value)| {
            let id = CanonicalId::parse(item_id).map_err(CatalogError::Validation)?;
            let item: RawItem = value.try_into()?;
            let item_id = id.as_str().to_string();

            Ok(CatalogItem {
                id,
                name: item.name,
                desc: item.desc,
                depends_on: item.depends_on,
                recipes: item
                    .recipes
                    .into_iter()
                    .map(|recipe| CatalogCommand::from_raw(item_id.as_str(), "recipes", recipe))
                    .collect::<Result<Vec<_>, _>>()?,
                verifiers: item
                    .verifiers
                    .into_iter()
                    .map(|verifier| {
                        CatalogCommand::from_raw(item_id.as_str(), "verifiers", verifier)
                    })
                    .collect::<Result<Vec<_>, _>>()?,
            })
        })
        .collect()
}

fn validate_command_contracts(
    item_id: &str,
    contract_kind: &str,
    commands: &[CatalogCommand],
    supported_distro_ids: &BTreeSet<String>,
) -> Result<(), CatalogError> {
    let mut seen = BTreeSet::new();

    for command in commands {
        let expanded_distros =
            command.expand_distros(item_id, contract_kind, supported_distro_ids)?;
        for distro in expanded_distros {
            if !seen.insert((command.mode, distro.clone())) {
                return Err(CatalogError::Validation(format!(
                    "item `{item_id}` {contract_kind} overlap for mode `{}` on distro `{distro}`",
                    command.mode.as_str()
                )));
            }
        }
    }

    Ok(())
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

impl InstallTarget {
    pub fn native_package(backend: TargetBackend) -> Self {
        Self {
            backend,
            source: TargetSource::DistributionPackage,
        }
    }

    pub fn generic_user() -> Self {
        Self {
            backend: TargetBackend::DirectBinary,
            source: TargetSource::GithubRelease,
        }
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
