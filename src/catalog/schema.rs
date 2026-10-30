//! Serde types for catalog files. Every field is optional so that overlay files can override
//! single fields of an item; required-ness is checked by `validate` after layers are merged.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Template parameters: strings, numbers, booleans, arrays of those, or a table keyed by arch.
pub type Params = BTreeMap<String, Value>;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Catalog {
    /// Catalog format version understood by this envira.
    pub schema: Option<u32>,
    /// Oldest envira release that can read this catalog.
    pub min_envira: Option<String>,
    #[serde(default)]
    pub env: EnvSettings,
    #[serde(default)]
    pub methods: BTreeMap<String, Method>,
    #[serde(default)]
    pub actions: BTreeMap<String, Action>,
    #[serde(default)]
    pub tools: BTreeMap<String, Tool>,
    #[serde(default)]
    pub configs: BTreeMap<String, Config>,
    #[serde(default)]
    pub bundles: BTreeMap<String, Bundle>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EnvSettings {
    /// Extra PATH directories for detection and execution (`~/` is the target user's home).
    #[serde(default)]
    pub path: Vec<String>,
    /// PATH directories always rendered into the managed shell environment.
    #[serde(default)]
    pub shell_path: Vec<String>,
    #[serde(default)]
    pub bin_dir: BinDirs,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BinDirs {
    pub user: String,
    pub system: String,
}

impl Default for BinDirs {
    fn default() -> Self {
        Self {
            user: "~/.local/bin".into(),
            system: "/usr/local/bin".into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ScopeSpec {
    User,
    System,
    /// Follows the machine preference: system when sudo is available, else user.
    Any,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Method {
    pub desc: Option<String>,
    /// Facts the machine must satisfy, e.g. `["family:debian"]`.
    #[serde(default)]
    pub when: Vec<String>,
    pub scope: Option<ScopeSpec>,
    /// Items an entry using this method normally depends on (lint only, never injected).
    #[serde(default)]
    pub expects: Vec<String>,
    /// Default parameter values.
    #[serde(default)]
    pub defaults: Params,
    pub install: Option<String>,
    /// Upgrades an installed tool in place; defaults to `install`.
    pub update: Option<String>,
    /// Finds a tool installed by this method (adoption); the regex's first group is the version.
    pub installed: Option<Query>,
    /// Newest available version.
    pub latest: Option<Query>,
}

/// A command or HTTP GET whose output yields a version. With `http`, `json` (a dotted path,
/// numbers index arrays) picks a value first; `regex` (first capture group) is applied last.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Query {
    pub cmd: Option<String>,
    pub http: Option<String>,
    /// Dotted path into a JSON response, e.g. `crate.max_stable_version`.
    pub json: Option<String>,
    /// Regex whose first capture group is the version.
    pub regex: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Action {
    pub desc: Option<String>,
    #[serde(default)]
    pub defaults: Params,
    pub check: Option<String>,
    pub apply: Option<String>,
    pub revert: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Tool {
    pub desc: Option<String>,
    /// Defaults to the catalog file name.
    pub category: Option<String>,
    /// Primary executable; defaults to the tool id.
    pub bin: Option<String>,
    /// All executables the tool provides; defaults to `[bin]`.
    #[serde(default)]
    pub bins: Vec<String>,
    pub detect: Option<Detect>,
    /// Install methods in order of preference; the first feasible one is used.
    #[serde(default)]
    pub install: Vec<InstallEntry>,
    #[serde(default)]
    pub depends: Vec<String>,
    #[serde(default)]
    pub when: Vec<String>,
    pub shell: Option<ShellSpec>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Detect {
    /// Shell command that succeeds when the tool is present. Defaults to `{bin} --version`.
    pub cmd: Option<String>,
    /// Regex whose first capture group is the version.
    pub version: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct InstallEntry {
    /// Name of a `[methods.*]` template.
    pub method: Option<String>,
    /// Raw shell command (instead of `method`).
    pub run: Option<String>,
    /// Needed only when this entry is chosen.
    #[serde(default)]
    pub depends: Vec<String>,
    #[serde(default)]
    pub when: Vec<String>,
    pub scope: Option<ScopeSpec>,
    /// Self-contained binary that may be copied into the system bin dir.
    #[serde(default)]
    pub portable: bool,
    /// Shared resources the command uses without naming them (an installer that runs `npm -g` or
    /// apt itself): `packages`, `pipx`, `npm`, `build`. Units with a lane in common take turns.
    #[serde(default)]
    pub lanes: Vec<String>,
    /// Update command for this entry (raw, or overriding the method's `update`).
    pub update: Option<String>,
    /// Latest-version query for this entry (overrides the method's `latest`).
    pub latest: Option<Query>,
    /// Template parameters.
    #[serde(flatten)]
    pub params: Params,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ShellSpec {
    #[serde(default)]
    pub path: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub alias: BTreeMap<String, String>,
    /// Init lines per shell (`zsh`, `bash`).
    #[serde(default)]
    pub init: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub desc: Option<String>,
    pub category: Option<String>,
    #[serde(default)]
    pub depends: Vec<String>,
    #[serde(default)]
    pub when: Vec<String>,
    /// `system` configs need sudo. Defaults to `user`.
    pub scope: Option<ScopeSpec>,
    #[serde(default)]
    pub steps: Vec<Step>,
    /// Raw form: used instead of `steps`.
    pub check: Option<String>,
    pub apply: Option<String>,
    pub revert: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct Step {
    /// Name of an `[actions.*]` template.
    #[serde(rename = "use")]
    pub use_: Option<String>,
    /// Raw step: optional check, the command, optional revert.
    pub check: Option<String>,
    pub run: Option<String>,
    pub revert: Option<String>,
    #[serde(flatten)]
    pub params: Params,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Bundle {
    pub name: Option<String>,
    pub desc: Option<String>,
    #[serde(default)]
    pub items: Vec<String>,
    /// Other bundles whose items this bundle also includes.
    #[serde(default)]
    pub bundles: Vec<String>,
}

impl Tool {
    pub fn bin<'a>(&'a self, id: &'a str) -> &'a str {
        self.bin.as_deref().unwrap_or(id)
    }

    pub fn bins<'a>(&'a self, id: &'a str) -> Vec<&'a str> {
        if self.bins.is_empty() {
            vec![self.bin(id)]
        } else {
            self.bins.iter().map(String::as_str).collect()
        }
    }
}
