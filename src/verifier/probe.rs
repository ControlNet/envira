use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::verifier::{ProbeKind, VerifierCheck};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "family", rename_all = "snake_case")]
pub enum ProbeSpec {
    CommandExists(CommandExistsProbe),
    CommandExecution(CommandExecutionProbe),
    AnyCommand(AnyCommandProbe),
    File(FileProbe),
    Directory(DirectoryProbe),
    Contains(ContainsProbe),
    SymlinkTarget(SymlinkTargetProbe),
    GroupMembership(GroupMembershipProbe),
    UnixSocket(UnixSocketProbe),
    Tcp(TcpProbe),
    Http(HttpProbe),
    ServiceUnit(ServiceUnitProbe),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CommandExistsProbe {
    pub command: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CommandExecutionProbe {
    pub program: String,
    pub args: Vec<String>,
    pub timeout_ms: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AnyCommandProbe {
    pub commands: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FileProbe {
    pub path: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DirectoryProbe {
    pub path: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ContainsProbe {
    pub path: PathBuf,
    pub pattern: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SymlinkTargetProbe {
    pub path: PathBuf,
    pub target: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct GroupMembershipProbe {
    pub group: String,
    pub username: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct UnixSocketProbe {
    pub path: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TcpProbe {
    pub host: String,
    pub port: u16,
    pub timeout_ms: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct HttpProbe {
    pub url: String,
    pub expected_status: Option<u16>,
    pub timeout_ms: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ServiceUnitProbe {
    pub unit: String,
    #[serde(default)]
    pub scope: ServiceManagerScope,
    pub condition: ServiceUnitCondition,
    pub timeout_ms: Option<u64>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceManagerScope {
    #[default]
    System,
    User,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceUnitCondition {
    Exists,
    Active,
    Enabled,
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum ProbeAdapterError {
    #[error("verifier check `{kind}` is missing required field `{field}`")]
    MissingField {
        kind: &'static str,
        field: &'static str,
    },
    #[error("verifier check `{kind}` cannot use blank field `{field}`")]
    BlankField {
        kind: &'static str,
        field: &'static str,
    },
    #[error("verifier check `any_command` must contain at least one command")]
    EmptyAnyCommand,
}

impl TryFrom<&VerifierCheck> for ProbeSpec {
    type Error = ProbeAdapterError;

    fn try_from(check: &VerifierCheck) -> Result<Self, Self::Error> {
        match check.kind {
            ProbeKind::Command => Ok(Self::CommandExists(CommandExistsProbe {
                command: require_trimmed(check.kind, "command", check.command.as_deref())?,
            })),
            ProbeKind::AnyCommand => {
                let commands = check
                    .commands
                    .as_ref()
                    .ok_or(ProbeAdapterError::MissingField {
                        kind: check.kind.as_str(),
                        field: "commands",
                    })?
                    .iter()
                    .map(|command| {
                        let trimmed = command.trim();
                        if trimmed.is_empty() {
                            Err(ProbeAdapterError::BlankField {
                                kind: check.kind.as_str(),
                                field: "commands",
                            })
                        } else {
                            Ok(trimmed.to_string())
                        }
                    })
                    .collect::<Result<Vec<_>, _>>()?;

                if commands.is_empty() {
                    return Err(ProbeAdapterError::EmptyAnyCommand);
                }

                Ok(Self::AnyCommand(AnyCommandProbe { commands }))
            }
            ProbeKind::File => Ok(Self::File(FileProbe {
                path: PathBuf::from(require_trimmed(check.kind, "path", check.path.as_deref())?),
            })),
            ProbeKind::Directory => Ok(Self::Directory(DirectoryProbe {
                path: PathBuf::from(require_trimmed(check.kind, "path", check.path.as_deref())?),
            })),
            ProbeKind::Contains => Ok(Self::Contains(ContainsProbe {
                path: PathBuf::from(require_trimmed(check.kind, "path", check.path.as_deref())?),
                pattern: require_trimmed(check.kind, "pattern", check.pattern.as_deref())?,
            })),
        }
    }
}

fn require_trimmed(
    kind: ProbeKind,
    field: &'static str,
    value: Option<&str>,
) -> Result<String, ProbeAdapterError> {
    let value = value.ok_or(ProbeAdapterError::MissingField {
        kind: kind.as_str(),
        field,
    })?;
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(ProbeAdapterError::BlankField {
            kind: kind.as_str(),
            field,
        });
    }
    Ok(trimmed.to_string())
}
