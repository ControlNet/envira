use std::fs;
use std::io::{Read, Write};
use std::net::{Shutdown, TcpStream, ToSocketAddrs};
use std::os::unix::fs::FileTypeExt;
use std::path::Path;
use std::time::Duration;

use regex::Regex;
use thiserror::Error;

use crate::executor::{CommandOperation, CommandRunner, ExecutorError};
use crate::verifier::{
    combine_achieved_stage, combine_observed_scope, ServiceAssessment, ServiceVerificationSpec,
};
use crate::verifier::{
    reduce_verifier_result, CommandExecutionProbe, ContainsProbe, DirectoryProbe, EvidenceRecord,
    EvidenceStatus, FileProbe, GroupMembershipProbe, HttpProbe, ObservedScope, ProbeAdapterError,
    ProbeSpec, ReductionError, ServiceManagerScope, ServiceProbeEvidence, ServiceUnitCondition,
    ServiceUnitProbe, SymlinkTargetProbe, TcpProbe, UnixSocketProbe, VerificationContext,
    VerificationStage, VerifierCheck, VerifierResult, VerifierSpec,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CollectedProbeEvidence {
    pub check_index: usize,
    pub check: VerifierCheck,
    pub probe: ProbeSpec,
    pub record: EvidenceRecord,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct EvidenceAggregation {
    pub collected: Vec<CollectedProbeEvidence>,
}

impl EvidenceAggregation {
    pub fn records(&self) -> Vec<EvidenceRecord> {
        self.collected
            .iter()
            .map(|entry| entry.record.clone())
            .collect()
    }

    pub fn into_records(self) -> Vec<EvidenceRecord> {
        self.collected
            .into_iter()
            .map(|entry| entry.record)
            .collect()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerificationRun {
    pub aggregation: EvidenceAggregation,
    pub result: VerifierResult,
}

#[derive(Debug, Error)]
pub enum VerificationError {
    #[error(transparent)]
    ProbeAdapter(#[from] ProbeAdapterError),
    #[error(transparent)]
    Reduction(#[from] ReductionError),
}

#[derive(Clone, Debug, Default)]
pub struct VerifierProbeRunner {
    command_runner: CommandRunner,
}

impl VerifierProbeRunner {
    pub fn new(command_runner: CommandRunner) -> Self {
        Self { command_runner }
    }

    pub fn aggregate(
        &self,
        spec: &VerifierSpec,
        context: &VerificationContext,
    ) -> Result<EvidenceAggregation, VerificationError> {
        let mut collected = Vec::with_capacity(spec.checks.len());

        for (check_index, check) in spec.checks.iter().cloned().enumerate() {
            let probe = ProbeSpec::try_from(&check)?;
            let record = self.run_probe(&probe, context);
            collected.push(CollectedProbeEvidence {
                check_index,
                check,
                probe,
                record,
            });
        }

        Ok(EvidenceAggregation { collected })
    }

    pub fn verify(
        &self,
        required_stage: VerificationStage,
        spec: &VerifierSpec,
        context: &VerificationContext,
    ) -> Result<VerificationRun, VerificationError> {
        let aggregation = self.aggregate(spec, context)?;
        let mut result = reduce_verifier_result(
            required_stage,
            spec,
            context.requested_profile,
            aggregation.records(),
        )?;

        if let Some(service_spec) = &spec.service {
            let (service_evidence, service) = self.verify_service(service_spec, context);
            result.achieved_stage =
                combine_achieved_stage(result.achieved_stage, service.achieved_stage);
            result.threshold_met = result
                .achieved_stage
                .is_some_and(|stage| stage.meets(required_stage));
            result.health = result.health.max(service.health());
            result.observed_scope =
                combine_observed_scope(result.observed_scope, service.observed_scope);
            result.service_evidence = service_evidence;
            result.service = Some(service);
        }

        Ok(VerificationRun {
            aggregation,
            result,
        })
    }

    fn verify_service(
        &self,
        service_spec: &ServiceVerificationSpec,
        context: &VerificationContext,
    ) -> (Vec<ServiceProbeEvidence>, ServiceAssessment) {
        let service_evidence = service_spec
            .plan(context)
            .into_iter()
            .map(|definition| ServiceProbeEvidence {
                id: definition.id,
                stage: definition.stage,
                record: self.run_probe(&definition.probe, context),
                probe: definition.probe,
            })
            .collect::<Vec<_>>();
        let service = service_spec.assess(&service_evidence, context);

        (service_evidence, service)
    }

    pub fn run_probe(&self, probe: &ProbeSpec, context: &VerificationContext) -> EvidenceRecord {
        match probe {
            ProbeSpec::CommandExists(probe) => {
                self.run_command_exists_probe(&probe.command, context)
            }
            ProbeSpec::CommandExecution(probe) => self.run_command_execution_probe(probe, context),
            ProbeSpec::AnyCommand(probe) => self.run_any_command_probe(&probe.commands, context),
            ProbeSpec::File(probe) => run_file_probe(probe, context),
            ProbeSpec::Directory(probe) => run_directory_probe(probe, context),
            ProbeSpec::Contains(probe) => run_contains_probe(probe, context),
            ProbeSpec::SymlinkTarget(probe) => run_symlink_target_probe(probe, context),
            ProbeSpec::GroupMembership(probe) => run_group_membership_probe(probe, context),
            ProbeSpec::UnixSocket(probe) => run_unix_socket_probe(probe, context),
            ProbeSpec::Tcp(probe) => run_tcp_probe(probe, context),
            ProbeSpec::Http(probe) => run_http_probe(probe, context),
            ProbeSpec::ServiceUnit(probe) => self.run_service_unit_probe(probe, context),
        }
    }

    fn run_command_exists_probe(
        &self,
        command: &str,
        context: &VerificationContext,
    ) -> EvidenceRecord {
        match context.resolve_command(command) {
            Some(path) => EvidenceRecord {
                status: EvidenceStatus::Satisfied,
                observed_scope: context.observed_scope_for_path(&path),
                summary: format!("command `{command}` found"),
                detail: Some(path.display().to_string()),
            },
            None => EvidenceRecord {
                status: EvidenceStatus::Missing,
                observed_scope: ObservedScope::Unknown,
                summary: format!("command `{command}` not found"),
                detail: context.command_path_env(),
            },
        }
    }

    fn run_command_execution_probe(
        &self,
        probe: &CommandExecutionProbe,
        context: &VerificationContext,
    ) -> EvidenceRecord {
        let Some(program_path) = context.resolve_command(&probe.program) else {
            return EvidenceRecord {
                status: EvidenceStatus::Missing,
                observed_scope: ObservedScope::Unknown,
                summary: format!("command `{}` not found", probe.program),
                detail: context.command_path_env(),
            };
        };

        let mut operation = CommandOperation::new(program_path.display().to_string())
            .with_args(probe.args.clone())
            .with_timeout_ms(
                probe
                    .timeout_ms
                    .unwrap_or_else(|| context.command_timeout_ms()),
            );

        if let Some(path_env) = context.command_path_env() {
            operation = operation.with_env([(String::from("PATH"), path_env)]);
        }

        match self.command_runner.execute(&operation) {
            Ok(execution) if execution.succeeded() => EvidenceRecord {
                status: EvidenceStatus::Satisfied,
                observed_scope: context.observed_scope_for_path(&program_path),
                summary: format!("command `{}` exited successfully", probe.program),
                detail: Some(join_command_detail(
                    &execution.stdout.evidence,
                    &execution.stderr.evidence,
                )),
            },
            Ok(execution) => EvidenceRecord {
                status: EvidenceStatus::Broken,
                observed_scope: context.observed_scope_for_path(&program_path),
                summary: execution.summary.message,
                detail: Some(join_command_detail(
                    &execution.stdout.evidence,
                    &execution.stderr.evidence,
                )),
            },
            Err(error) => map_executor_error(&probe.program, &program_path, context, error),
        }
    }

    fn run_any_command_probe(
        &self,
        commands: &[String],
        context: &VerificationContext,
    ) -> EvidenceRecord {
        for command in commands {
            if let Some(path) = context.resolve_command(command) {
                return EvidenceRecord {
                    status: EvidenceStatus::Satisfied,
                    observed_scope: context.observed_scope_for_path(&path),
                    summary: format!("one of `{}` found", commands.join(" | ")),
                    detail: Some(format!("{} => {}", command, path.display())),
                };
            }
        }

        EvidenceRecord {
            status: EvidenceStatus::Missing,
            observed_scope: ObservedScope::Unknown,
            summary: format!("none of `{}` found", commands.join(" | ")),
            detail: context.command_path_env(),
        }
    }

    fn run_service_unit_probe(
        &self,
        probe: &ServiceUnitProbe,
        context: &VerificationContext,
    ) -> EvidenceRecord {
        let Some(systemctl_path) = context.resolve_command("systemctl") else {
            return EvidenceRecord {
                status: EvidenceStatus::Unknown,
                observed_scope: scope_to_observed_scope(probe.scope),
                summary: format!("service unit `{}` could not be inspected", probe.unit),
                detail: Some("systemctl not found".to_string()),
            };
        };

        let mut operation = CommandOperation::new(systemctl_path.display().to_string())
            .with_args(service_unit_args(probe))
            .with_timeout_ms(
                probe
                    .timeout_ms
                    .unwrap_or_else(|| context.command_timeout_ms()),
            );

        if let Some(path_env) = context.command_path_env() {
            operation = operation.with_env([(String::from("PATH"), path_env)]);
        }

        match self.command_runner.execute(&operation) {
            Ok(execution) if execution.succeeded() => {
                let fields = execution.stdout.evidence.lines().collect::<Vec<_>>();
                let load_state = fields.first().copied().unwrap_or_default();
                let active_state = fields.get(1).copied().unwrap_or_default();
                let unit_file_state = fields.get(2).copied().unwrap_or_default();
                let sub_state = fields.get(3).copied().unwrap_or_default();
                let detail = format!(
                    "load_state={load_state}; active_state={active_state}; unit_file_state={unit_file_state}; sub_state={sub_state}"
                );

                if load_state == "not-found" {
                    return EvidenceRecord {
                        status: EvidenceStatus::Missing,
                        observed_scope: scope_to_observed_scope(probe.scope),
                        summary: format!("service unit `{}` not found", probe.unit),
                        detail: Some(detail),
                    };
                }

                let (status, summary) = match probe.condition {
                    ServiceUnitCondition::Exists => (
                        EvidenceStatus::Satisfied,
                        format!("service unit `{}` exists", probe.unit),
                    ),
                    ServiceUnitCondition::Active if active_state == "active" => (
                        EvidenceStatus::Satisfied,
                        format!("service unit `{}` is active", probe.unit),
                    ),
                    ServiceUnitCondition::Enabled if unit_file_state == "enabled" => (
                        EvidenceStatus::Satisfied,
                        format!("service unit `{}` is enabled", probe.unit),
                    ),
                    ServiceUnitCondition::Active => (
                        EvidenceStatus::Broken,
                        format!("service unit `{}` is not active", probe.unit),
                    ),
                    ServiceUnitCondition::Enabled => (
                        EvidenceStatus::Broken,
                        format!("service unit `{}` is not enabled", probe.unit),
                    ),
                };

                EvidenceRecord {
                    status,
                    observed_scope: scope_to_observed_scope(probe.scope),
                    summary,
                    detail: Some(detail),
                }
            }
            Ok(execution) => EvidenceRecord {
                status: EvidenceStatus::Unknown,
                observed_scope: scope_to_observed_scope(probe.scope),
                summary: format!("service unit `{}` could not be inspected", probe.unit),
                detail: Some(join_command_detail(
                    &execution.stdout.evidence,
                    &execution.stderr.evidence,
                )),
            },
            Err(error) => EvidenceRecord {
                status: EvidenceStatus::Unknown,
                observed_scope: scope_to_observed_scope(probe.scope),
                summary: format!("service unit `{}` could not be inspected", probe.unit),
                detail: Some(error.to_string()),
            },
        }
    }
}

pub fn aggregate_verifier_evidence(
    spec: &VerifierSpec,
    context: &VerificationContext,
) -> Result<EvidenceAggregation, VerificationError> {
    VerifierProbeRunner::default().aggregate(spec, context)
}

pub fn verify_with_context(
    required_stage: VerificationStage,
    spec: &VerifierSpec,
    context: &VerificationContext,
) -> Result<VerificationRun, VerificationError> {
    VerifierProbeRunner::default().verify(required_stage, spec, context)
}

fn run_file_probe(probe: &FileProbe, context: &VerificationContext) -> EvidenceRecord {
    match fs::metadata(&probe.path) {
        Ok(metadata) if metadata.is_file() => EvidenceRecord {
            status: EvidenceStatus::Satisfied,
            observed_scope: context.observed_scope_for_path(&probe.path),
            summary: format!("file `{}` exists", probe.path.display()),
            detail: None,
        },
        Ok(_) => EvidenceRecord {
            status: EvidenceStatus::Broken,
            observed_scope: context.observed_scope_for_path(&probe.path),
            summary: format!("path `{}` is not a file", probe.path.display()),
            detail: None,
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => EvidenceRecord {
            status: EvidenceStatus::Missing,
            observed_scope: context.observed_scope_for_path(&probe.path),
            summary: format!("file `{}` is missing", probe.path.display()),
            detail: None,
        },
        Err(error) => EvidenceRecord {
            status: EvidenceStatus::Unknown,
            observed_scope: context.observed_scope_for_path(&probe.path),
            summary: format!("file `{}` could not be inspected", probe.path.display()),
            detail: Some(error.to_string()),
        },
    }
}

fn run_directory_probe(probe: &DirectoryProbe, context: &VerificationContext) -> EvidenceRecord {
    match fs::metadata(&probe.path) {
        Ok(metadata) if metadata.is_dir() => EvidenceRecord {
            status: EvidenceStatus::Satisfied,
            observed_scope: context.observed_scope_for_path(&probe.path),
            summary: format!("directory `{}` exists", probe.path.display()),
            detail: None,
        },
        Ok(_) => EvidenceRecord {
            status: EvidenceStatus::Broken,
            observed_scope: context.observed_scope_for_path(&probe.path),
            summary: format!("path `{}` is not a directory", probe.path.display()),
            detail: None,
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => EvidenceRecord {
            status: EvidenceStatus::Missing,
            observed_scope: context.observed_scope_for_path(&probe.path),
            summary: format!("directory `{}` is missing", probe.path.display()),
            detail: None,
        },
        Err(error) => EvidenceRecord {
            status: EvidenceStatus::Unknown,
            observed_scope: context.observed_scope_for_path(&probe.path),
            summary: format!(
                "directory `{}` could not be inspected",
                probe.path.display()
            ),
            detail: Some(error.to_string()),
        },
    }
}

fn run_contains_probe(probe: &ContainsProbe, context: &VerificationContext) -> EvidenceRecord {
    match fs::read_to_string(&probe.path) {
        Ok(contents) => match Regex::new(&probe.pattern) {
            Ok(pattern) if pattern.is_match(&contents) => EvidenceRecord {
                status: EvidenceStatus::Satisfied,
                observed_scope: context.observed_scope_for_path(&probe.path),
                summary: format!(
                    "file `{}` contains pattern `{}`",
                    probe.path.display(),
                    probe.pattern
                ),
                detail: None,
            },
            Ok(_) => EvidenceRecord {
                status: EvidenceStatus::Broken,
                observed_scope: context.observed_scope_for_path(&probe.path),
                summary: format!(
                    "file `{}` does not contain pattern `{}`",
                    probe.path.display(),
                    probe.pattern
                ),
                detail: None,
            },
            Err(error) => EvidenceRecord {
                status: EvidenceStatus::Broken,
                observed_scope: context.observed_scope_for_path(&probe.path),
                summary: format!(
                    "pattern `{}` is invalid for `{}`",
                    probe.pattern,
                    probe.path.display()
                ),
                detail: Some(error.to_string()),
            },
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => EvidenceRecord {
            status: EvidenceStatus::Missing,
            observed_scope: context.observed_scope_for_path(&probe.path),
            summary: format!("file `{}` is missing", probe.path.display()),
            detail: None,
        },
        Err(error) => EvidenceRecord {
            status: EvidenceStatus::Unknown,
            observed_scope: context.observed_scope_for_path(&probe.path),
            summary: format!("file `{}` could not be read", probe.path.display()),
            detail: Some(error.to_string()),
        },
    }
}

fn run_symlink_target_probe(
    probe: &SymlinkTargetProbe,
    context: &VerificationContext,
) -> EvidenceRecord {
    match fs::symlink_metadata(&probe.path) {
        Ok(metadata) if metadata.file_type().is_symlink() => match fs::read_link(&probe.path) {
            Ok(target) if target == probe.target => EvidenceRecord {
                status: EvidenceStatus::Satisfied,
                observed_scope: context.observed_scope_for_path(&probe.path),
                summary: format!(
                    "symlink `{}` points to `{}`",
                    probe.path.display(),
                    probe.target.display()
                ),
                detail: None,
            },
            Ok(target) => EvidenceRecord {
                status: EvidenceStatus::Broken,
                observed_scope: context.observed_scope_for_path(&probe.path),
                summary: format!(
                    "symlink `{}` points to `{}`",
                    probe.path.display(),
                    target.display()
                ),
                detail: Some(format!("expected {}", probe.target.display())),
            },
            Err(error) => EvidenceRecord {
                status: EvidenceStatus::Unknown,
                observed_scope: context.observed_scope_for_path(&probe.path),
                summary: format!("symlink `{}` could not be read", probe.path.display()),
                detail: Some(error.to_string()),
            },
        },
        Ok(_) => EvidenceRecord {
            status: EvidenceStatus::Broken,
            observed_scope: context.observed_scope_for_path(&probe.path),
            summary: format!("path `{}` is not a symlink", probe.path.display()),
            detail: None,
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => EvidenceRecord {
            status: EvidenceStatus::Missing,
            observed_scope: context.observed_scope_for_path(&probe.path),
            summary: format!("symlink `{}` is missing", probe.path.display()),
            detail: None,
        },
        Err(error) => EvidenceRecord {
            status: EvidenceStatus::Unknown,
            observed_scope: context.observed_scope_for_path(&probe.path),
            summary: format!("symlink `{}` could not be inspected", probe.path.display()),
            detail: Some(error.to_string()),
        },
    }
}

fn run_group_membership_probe(
    probe: &GroupMembershipProbe,
    context: &VerificationContext,
) -> EvidenceRecord {
    let username = probe
        .username
        .as_deref()
        .or_else(|| context.default_username());
    let Some(username) = username else {
        return EvidenceRecord {
            status: EvidenceStatus::Unknown,
            observed_scope: ObservedScope::User,
            summary: format!("group `{}` membership could not be resolved", probe.group),
            detail: Some("no user context available".to_string()),
        };
    };

    match fs::read_to_string("/etc/group") {
        Ok(group_file) => {
            for line in group_file.lines() {
                if line.trim().is_empty() || line.starts_with('#') {
                    continue;
                }

                let fields = line.split(':').collect::<Vec<_>>();
                if fields.len() != 4 || fields[0] != probe.group {
                    continue;
                }

                let gid_matches = fields[2]
                    .parse::<u32>()
                    .ok()
                    .zip(context.default_user_gid())
                    .is_some_and(|(group_gid, user_gid)| group_gid == user_gid);
                let listed_member = fields[3]
                    .split(',')
                    .map(str::trim)
                    .any(|member| !member.is_empty() && member == username);

                return if gid_matches || listed_member {
                    EvidenceRecord {
                        status: EvidenceStatus::Satisfied,
                        observed_scope: ObservedScope::User,
                        summary: format!("user `{username}` belongs to group `{}`", probe.group),
                        detail: None,
                    }
                } else {
                    EvidenceRecord {
                        status: EvidenceStatus::Missing,
                        observed_scope: ObservedScope::User,
                        summary: format!(
                            "user `{username}` does not belong to group `{}`",
                            probe.group
                        ),
                        detail: None,
                    }
                };
            }

            EvidenceRecord {
                status: EvidenceStatus::Missing,
                observed_scope: ObservedScope::System,
                summary: format!("group `{}` is missing", probe.group),
                detail: None,
            }
        }
        Err(error) => EvidenceRecord {
            status: EvidenceStatus::Unknown,
            observed_scope: ObservedScope::System,
            summary: format!("group `{}` membership could not be resolved", probe.group),
            detail: Some(error.to_string()),
        },
    }
}

fn run_unix_socket_probe(probe: &UnixSocketProbe, context: &VerificationContext) -> EvidenceRecord {
    match fs::symlink_metadata(&probe.path) {
        Ok(metadata) if metadata.file_type().is_socket() => EvidenceRecord {
            status: EvidenceStatus::Satisfied,
            observed_scope: context.observed_scope_for_path(&probe.path),
            summary: format!("unix socket `{}` exists", probe.path.display()),
            detail: None,
        },
        Ok(_) => EvidenceRecord {
            status: EvidenceStatus::Broken,
            observed_scope: context.observed_scope_for_path(&probe.path),
            summary: format!("path `{}` is not a unix socket", probe.path.display()),
            detail: None,
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => EvidenceRecord {
            status: EvidenceStatus::Missing,
            observed_scope: context.observed_scope_for_path(&probe.path),
            summary: format!("unix socket `{}` is missing", probe.path.display()),
            detail: None,
        },
        Err(error) => EvidenceRecord {
            status: EvidenceStatus::Unknown,
            observed_scope: context.observed_scope_for_path(&probe.path),
            summary: format!(
                "unix socket `{}` could not be inspected",
                probe.path.display()
            ),
            detail: Some(error.to_string()),
        },
    }
}

fn run_tcp_probe(probe: &TcpProbe, context: &VerificationContext) -> EvidenceRecord {
    let timeout = Duration::from_millis(
        probe
            .timeout_ms
            .unwrap_or_else(|| context.socket_timeout_ms()),
    );
    let endpoint = format!("{}:{}", probe.host, probe.port);
    let addresses = match endpoint.to_socket_addrs() {
        Ok(addresses) => addresses.collect::<Vec<_>>(),
        Err(error) => {
            return EvidenceRecord {
                status: EvidenceStatus::Unknown,
                observed_scope: ObservedScope::Unknown,
                summary: format!("tcp endpoint `{endpoint}` could not be resolved"),
                detail: Some(error.to_string()),
            };
        }
    };

    for address in &addresses {
        if TcpStream::connect_timeout(address, timeout).is_ok() {
            return EvidenceRecord {
                status: EvidenceStatus::Satisfied,
                observed_scope: ObservedScope::Unknown,
                summary: format!("tcp endpoint `{endpoint}` accepted a connection"),
                detail: Some(address.to_string()),
            };
        }
    }

    EvidenceRecord {
        status: EvidenceStatus::Missing,
        observed_scope: ObservedScope::Unknown,
        summary: format!("tcp endpoint `{endpoint}` did not accept a connection"),
        detail: Some(
            addresses
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", "),
        ),
    }
}

fn run_http_probe(probe: &HttpProbe, context: &VerificationContext) -> EvidenceRecord {
    let timeout = Duration::from_millis(
        probe
            .timeout_ms
            .unwrap_or_else(|| context.http_timeout_ms()),
    );
    let parsed = match parse_http_url(&probe.url) {
        Ok(parsed) => parsed,
        Err(message) => {
            return EvidenceRecord {
                status: EvidenceStatus::Unknown,
                observed_scope: ObservedScope::Unknown,
                summary: format!("http endpoint `{}` could not be inspected", probe.url),
                detail: Some(message),
            };
        }
    };

    let endpoint = format!("{}:{}", parsed.host, parsed.port);
    let addresses = match endpoint.to_socket_addrs() {
        Ok(addresses) => addresses.collect::<Vec<_>>(),
        Err(error) => {
            return EvidenceRecord {
                status: EvidenceStatus::Unknown,
                observed_scope: ObservedScope::Unknown,
                summary: format!("http endpoint `{}` could not be resolved", probe.url),
                detail: Some(error.to_string()),
            };
        }
    };

    for address in addresses {
        if let Ok(mut stream) = TcpStream::connect_timeout(&address, timeout) {
            let _ = stream.set_read_timeout(Some(timeout));
            let _ = stream.set_write_timeout(Some(timeout));

            let request = format!(
                "GET {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
                parsed.path, parsed.host
            );

            if let Err(error) = stream.write_all(request.as_bytes()) {
                return EvidenceRecord {
                    status: EvidenceStatus::Unknown,
                    observed_scope: ObservedScope::Unknown,
                    summary: format!("http endpoint `{}` could not be inspected", probe.url),
                    detail: Some(error.to_string()),
                };
            }

            let mut response = String::new();
            if let Err(error) = stream.read_to_string(&mut response) {
                return EvidenceRecord {
                    status: EvidenceStatus::Unknown,
                    observed_scope: ObservedScope::Unknown,
                    summary: format!("http endpoint `{}` could not be inspected", probe.url),
                    detail: Some(error.to_string()),
                };
            }
            let _ = stream.shutdown(Shutdown::Both);

            let status_line = response.lines().next().unwrap_or_default().to_string();
            let status_code = status_line
                .split_whitespace()
                .nth(1)
                .and_then(|value| value.parse::<u16>().ok());

            let expected = probe.expected_status;
            let satisfied = match (expected, status_code) {
                (Some(expected), Some(actual)) => expected == actual,
                (None, Some(actual)) => (200..400).contains(&actual),
                _ => false,
            };

            return EvidenceRecord {
                status: if satisfied {
                    EvidenceStatus::Satisfied
                } else {
                    EvidenceStatus::Broken
                },
                observed_scope: ObservedScope::Unknown,
                summary: if satisfied {
                    format!("http endpoint `{}` returned a matching response", probe.url)
                } else {
                    format!(
                        "http endpoint `{}` returned an unexpected response",
                        probe.url
                    )
                },
                detail: Some(status_line),
            };
        }
    }

    EvidenceRecord {
        status: EvidenceStatus::Missing,
        observed_scope: ObservedScope::Unknown,
        summary: format!("http endpoint `{}` did not accept a connection", probe.url),
        detail: None,
    }
}

fn map_executor_error(
    program: &str,
    program_path: &Path,
    context: &VerificationContext,
    error: ExecutorError,
) -> EvidenceRecord {
    let status = match error {
        ExecutorError::Spawn { .. } | ExecutorError::Kill { .. } | ExecutorError::Wait { .. } => {
            EvidenceStatus::Broken
        }
        ExecutorError::MissingPipe { .. }
        | ExecutorError::StreamRead { .. }
        | ExecutorError::ReaderJoin { .. }
        | ExecutorError::MissingTargetUser { .. } => EvidenceStatus::Unknown,
    };

    EvidenceRecord {
        status,
        observed_scope: context.observed_scope_for_path(program_path),
        summary: format!("command `{program}` could not be executed"),
        detail: Some(error.to_string()),
    }
}

fn service_unit_args(probe: &ServiceUnitProbe) -> Vec<String> {
    let mut args = Vec::new();
    if probe.scope == ServiceManagerScope::User {
        args.push("--user".to_string());
    }
    args.push("show".to_string());
    args.push("--property=LoadState,ActiveState,UnitFileState,SubState".to_string());
    args.push("--value".to_string());
    args.push(probe.unit.clone());
    args
}

fn scope_to_observed_scope(scope: ServiceManagerScope) -> ObservedScope {
    match scope {
        ServiceManagerScope::System => ObservedScope::System,
        ServiceManagerScope::User => ObservedScope::User,
    }
}

fn join_command_detail(stdout: &str, stderr: &str) -> String {
    match (stdout.is_empty(), stderr.is_empty()) {
        (true, true) => String::new(),
        (false, true) => format!("stdout={stdout}"),
        (true, false) => format!("stderr={stderr}"),
        (false, false) => format!("stdout={stdout}\nstderr={stderr}"),
    }
}

struct ParsedHttpUrl {
    host: String,
    port: u16,
    path: String,
}

fn parse_http_url(url: &str) -> Result<ParsedHttpUrl, String> {
    let Some(rest) = url.strip_prefix("http://") else {
        return Err("only plain http URLs are supported".to_string());
    };

    let (authority, path) = match rest.split_once('/') {
        Some((authority, path)) => (authority, format!("/{path}")),
        None => (rest, "/".to_string()),
    };

    if authority.trim().is_empty() {
        return Err("http URL is missing a host".to_string());
    }

    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) if !host.contains(']') => {
            let port = port
                .parse::<u16>()
                .map_err(|_| format!("invalid http port `{port}`"))?;
            (host.to_string(), port)
        }
        _ => (authority.to_string(), 80),
    };

    Ok(ParsedHttpUrl { host, port, path })
}
