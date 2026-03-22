use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::verifier::ServiceVerificationSpec;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationStage {
    #[default]
    Present,
    Configured,
    Operational,
}

impl VerificationStage {
    pub fn meets(self, required: Self) -> bool {
        self >= required
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationProfile {
    #[default]
    Quick,
    Standard,
    Strict,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProbeRequirement {
    Required,
    Optional,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerifierSpec {
    pub checks: Vec<VerifierCheck>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service: Option<ServiceVerificationSpec>,
}

impl VerifierSpec {
    pub fn validate(&self, item_id: &str) -> Result<(), String> {
        if self.checks.is_empty() {
            return Err(format!(
                "item `{item_id}` must define at least one verifier check"
            ));
        }

        for check in &self.checks {
            check.validate(item_id)?;
        }

        if let Some(service) = &self.service {
            service.validate(item_id)?;
        }

        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerifierCheck {
    #[serde(default)]
    pub stage: VerificationStage,
    #[serde(alias = "threshold")]
    pub requirement: ProbeRequirement,
    #[serde(default)]
    pub min_profile: VerificationProfile,
    pub kind: ProbeKind,
    pub command: Option<String>,
    pub commands: Option<Vec<String>>,
    pub path: Option<String>,
    pub pattern: Option<String>,
}

impl VerifierCheck {
    pub fn participates_in(&self, requested_profile: VerificationProfile) -> bool {
        self.min_profile <= requested_profile
    }

    pub fn validate(&self, item_id: &str) -> Result<(), String> {
        match self.kind {
            ProbeKind::Command => {
                require_value(item_id, self.kind, "command", self.command.as_deref())?;
                reject_present(item_id, self.kind, "commands", self.commands.is_some())?;
                reject_present(item_id, self.kind, "path", self.path.is_some())?;
                reject_present(item_id, self.kind, "pattern", self.pattern.is_some())
            }
            ProbeKind::AnyCommand => {
                let commands = self.commands.as_ref().ok_or_else(|| {
                    format!(
                        "item `{item_id}` verifier check `{}` must define `commands`",
                        self.kind.as_str()
                    )
                })?;

                if commands.is_empty() {
                    return Err(format!(
                        "item `{item_id}` verifier check `{}` must include at least one command",
                        self.kind.as_str()
                    ));
                }

                let mut seen = BTreeSet::new();
                for command in commands {
                    let trimmed = command.trim();
                    if trimmed.is_empty() {
                        return Err(format!(
                            "item `{item_id}` verifier check `{}` cannot contain an empty command",
                            self.kind.as_str()
                        ));
                    }
                    if !seen.insert(trimmed.to_string()) {
                        return Err(format!(
                            "item `{item_id}` verifier check `{}` contains duplicate command `{trimmed}`",
                            self.kind.as_str()
                        ));
                    }
                }

                reject_present(item_id, self.kind, "command", self.command.is_some())?;
                reject_present(item_id, self.kind, "path", self.path.is_some())?;
                reject_present(item_id, self.kind, "pattern", self.pattern.is_some())
            }
            ProbeKind::File | ProbeKind::Directory => {
                require_value(item_id, self.kind, "path", self.path.as_deref())?;
                reject_present(item_id, self.kind, "command", self.command.is_some())?;
                reject_present(item_id, self.kind, "commands", self.commands.is_some())?;
                reject_present(item_id, self.kind, "pattern", self.pattern.is_some())
            }
            ProbeKind::Contains => {
                require_value(item_id, self.kind, "path", self.path.as_deref())?;
                require_value(item_id, self.kind, "pattern", self.pattern.as_deref())?;
                reject_present(item_id, self.kind, "command", self.command.is_some())?;
                reject_present(item_id, self.kind, "commands", self.commands.is_some())
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProbeKind {
    Command,
    AnyCommand,
    File,
    Directory,
    Contains,
}

impl ProbeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Command => "command",
            Self::AnyCommand => "any_command",
            Self::File => "file",
            Self::Directory => "directory",
            Self::Contains => "contains",
        }
    }
}

fn require_value(
    item_id: &str,
    kind: ProbeKind,
    field: &str,
    value: Option<&str>,
) -> Result<(), String> {
    let value = value.ok_or_else(|| {
        format!(
            "item `{item_id}` verifier check `{}` must define `{field}`",
            kind.as_str()
        )
    })?;

    if value.trim().is_empty() {
        return Err(format!(
            "item `{item_id}` verifier check `{}` must define a non-empty `{field}`",
            kind.as_str()
        ));
    }

    Ok(())
}

fn reject_present(
    item_id: &str,
    kind: ProbeKind,
    field: &str,
    present: bool,
) -> Result<(), String> {
    if present {
        return Err(format!(
            "item `{item_id}` verifier check `{}` cannot define `{field}`",
            kind.as_str()
        ));
    }

    Ok(())
}
