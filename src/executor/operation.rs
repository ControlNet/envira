use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OperationSpec {
    Command(CommandOperation),
    Download(DownloadOperation),
    Assert(AssertOperation),
    Builtin(BuiltinOperation),
}

impl OperationSpec {
    pub fn command(program: impl Into<String>) -> Self {
        Self::Command(CommandOperation::new(program))
    }
}

impl From<CommandOperation> for OperationSpec {
    fn from(value: CommandOperation) -> Self {
        Self::Command(value)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionTarget {
    #[default]
    CurrentProcess,
    System,
    TargetUser,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CommandOperation {
    pub program: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub cwd: Option<PathBuf>,
    pub timeout_ms: Option<u64>,
    pub target: ExecutionTarget,
}

impl CommandOperation {
    pub fn new(program: impl Into<String>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            env: BTreeMap::new(),
            cwd: None,
            timeout_ms: None,
            target: ExecutionTarget::CurrentProcess,
        }
    }

    pub fn shell(shell: impl Into<String>, command: impl Into<String>) -> Self {
        Self::new(shell).with_args(vec!["-c".to_string(), command.into()])
    }

    pub fn with_args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.args = args.into_iter().map(Into::into).collect();
        self
    }

    pub fn with_env<K, V, I>(mut self, env: I) -> Self
    where
        K: Into<String>,
        V: Into<String>,
        I: IntoIterator<Item = (K, V)>,
    {
        self.env = env
            .into_iter()
            .map(|(key, value)| (key.into(), value.into()))
            .collect();
        self
    }

    pub fn with_cwd(mut self, cwd: impl Into<PathBuf>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    pub fn with_timeout_ms(mut self, timeout_ms: u64) -> Self {
        self.timeout_ms = Some(timeout_ms);
        self
    }

    pub fn with_target(mut self, target: ExecutionTarget) -> Self {
        self.target = target;
        self
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DownloadOperation {
    pub url: String,
    pub destination: PathBuf,
    pub checksum_sha256: Option<String>,
    pub executable: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AssertOperation {
    pub condition: AssertCondition,
    pub message: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AssertCondition {
    CommandAvailable { program: String },
    PathExists { path: PathBuf },
    EnvironmentValue { key: String, value: String },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "family", rename_all = "snake_case")]
pub enum BuiltinOperation {
    DirectBinaryInstall {
        url: String,
        destination: PathBuf,
        binary_name: String,
        checksum_sha256: Option<String>,
    },
    ArchiveInstall {
        url: String,
        destination_dir: PathBuf,
        format: ArchiveFormat,
        strip_components: u32,
        checksum_sha256: Option<String>,
    },
    SourceBuildInstall {
        source_url: String,
        revision: Option<String>,
        build_system: SourceBuildSystem,
        working_subdir: Option<PathBuf>,
        install_prefix: Option<PathBuf>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArchiveFormat {
    TarGz,
    TarXz,
    TarBz2,
    Zip,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceBuildSystem {
    Autotools,
    Cmake,
    Cargo,
    Go,
    Python,
    Make,
}
