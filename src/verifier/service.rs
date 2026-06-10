use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::verifier::{
    CommandExecutionProbe, CommandExistsProbe, EvidenceRecord, EvidenceStatus,
    GroupMembershipProbe, HttpProbe, ObservedScope, ProbeSpec, ServiceManagerScope,
    ServiceUnitCondition, ServiceUnitProbe, TcpProbe, UnixSocketProbe, VerificationContext,
    VerificationHealth, VerificationStage, VerifierCheck,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceKind {
    Docker,
    Jupyter,
    Pm2,
    Syncthing,
    Vnc,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceUsabilityState {
    Operational,
    OnDemand,
    Blocked,
    NonUsable,
    Missing,
    Unknown,
}

impl ServiceUsabilityState {
    pub fn health(self) -> VerificationHealth {
        match self {
            Self::Operational => VerificationHealth::Healthy,
            Self::OnDemand | Self::Missing => VerificationHealth::Missing,
            Self::Blocked | Self::NonUsable => VerificationHealth::Broken,
            Self::Unknown => VerificationHealth::Unknown,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceVerificationSpec {
    pub kind: ServiceKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub commands: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_unit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_scope: Option<ServiceManagerScope>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub socket_paths: Vec<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access_group: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub http_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tcp_host: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tcp_port: Option<u16>,
}

impl ServiceVerificationSpec {
    pub fn validate(&self, item_id: &str) -> Result<(), String> {
        reject_blank(item_id, self.kind, "command", self.command.as_deref())?;
        reject_blank(
            item_id,
            self.kind,
            "service_unit",
            self.service_unit.as_deref(),
        )?;
        reject_blank(
            item_id,
            self.kind,
            "access_group",
            self.access_group.as_deref(),
        )?;
        reject_blank(item_id, self.kind, "http_url", self.http_url.as_deref())?;
        reject_blank(item_id, self.kind, "tcp_host", self.tcp_host.as_deref())?;

        for (index, command) in self.commands.iter().enumerate() {
            if command.trim().is_empty() {
                return Err(format!(
                    "item `{item_id}` service verifier `{}` contains a blank `commands[{index}]`",
                    self.kind.as_str()
                ));
            }
        }

        Ok(())
    }

    pub fn plan(&self, context: &VerificationContext) -> Vec<ServiceProbeDefinition> {
        match self.kind {
            ServiceKind::Docker => docker_plan(self, context),
            ServiceKind::Jupyter => jupyter_like_plan(
                self,
                context,
                "jupyter",
                "jupyter.service",
                "http://127.0.0.1:8888/",
            ),
            ServiceKind::Pm2 => pm2_plan(self, context),
            ServiceKind::Syncthing => jupyter_like_plan(
                self,
                context,
                "syncthing",
                "syncthing.service",
                "http://127.0.0.1:18384/",
            ),
            ServiceKind::Vnc => vnc_plan(self),
        }
    }

    pub fn assess(
        &self,
        evidence: &[ServiceProbeEvidence],
        context: &VerificationContext,
    ) -> ServiceAssessment {
        match self.kind {
            ServiceKind::Docker => assess_docker(self, evidence, context),
            ServiceKind::Jupyter => assess_http_service(self.kind, evidence),
            ServiceKind::Pm2 => assess_pm2(evidence),
            ServiceKind::Syncthing => assess_http_service(self.kind, evidence),
            ServiceKind::Vnc => assess_vnc(self, evidence),
        }
    }
}

pub fn infer_service_verification_spec(
    checks: &[VerifierCheck],
) -> Option<ServiceVerificationSpec> {
    checks.iter().find_map(|check| {
        let command = check.command.as_deref()?.trim();
        infer_service_from_contract(command)
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServiceProbeDefinition {
    pub id: String,
    pub stage: VerificationStage,
    pub probe: ProbeSpec,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ServiceProbeEvidence {
    pub id: String,
    pub stage: VerificationStage,
    pub probe: ProbeSpec,
    pub record: EvidenceRecord,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ServiceAssessment {
    pub kind: ServiceKind,
    pub state: ServiceUsabilityState,
    pub achieved_stage: Option<VerificationStage>,
    pub observed_scope: ObservedScope,
    pub summary: String,
    pub detail: Option<String>,
}

impl ServiceAssessment {
    pub fn health(&self) -> VerificationHealth {
        self.state.health()
    }
}

pub fn combine_achieved_stage(
    base_stage: Option<VerificationStage>,
    service_stage: Option<VerificationStage>,
) -> Option<VerificationStage> {
    let _ = base_stage;
    service_stage.or(base_stage)
}

pub fn combine_observed_scope(left: ObservedScope, right: ObservedScope) -> ObservedScope {
    match (left, right) {
        (ObservedScope::Both, _) | (_, ObservedScope::Both) => ObservedScope::Both,
        (ObservedScope::Unknown, scope) | (scope, ObservedScope::Unknown) => scope,
        (ObservedScope::System, ObservedScope::User)
        | (ObservedScope::User, ObservedScope::System) => ObservedScope::Both,
        (scope, _) => scope,
    }
}

fn docker_plan(
    spec: &ServiceVerificationSpec,
    context: &VerificationContext,
) -> Vec<ServiceProbeDefinition> {
    let mut plan = vec![ServiceProbeDefinition {
        id: "command".to_string(),
        stage: VerificationStage::Present,
        probe: ProbeSpec::CommandExists(CommandExistsProbe {
            command: spec
                .command
                .clone()
                .unwrap_or_else(|| "command -v docker".to_string()),
        }),
    }];

    if let Some(unit) = spec
        .service_unit
        .clone()
        .or_else(|| Some("docker.service".to_string()))
    {
        plan.push(ServiceProbeDefinition {
            id: "unit".to_string(),
            stage: VerificationStage::Configured,
            probe: ProbeSpec::ServiceUnit(ServiceUnitProbe {
                unit,
                scope: spec.service_scope.unwrap_or(ServiceManagerScope::System),
                condition: ServiceUnitCondition::Exists,
                timeout_ms: None,
            }),
        });
    }

    for (id, path) in docker_socket_paths(spec, context) {
        plan.push(ServiceProbeDefinition {
            id,
            stage: VerificationStage::Configured,
            probe: ProbeSpec::UnixSocket(UnixSocketProbe { path }),
        });
    }

    if should_require_group_access(spec, context) {
        plan.push(ServiceProbeDefinition {
            id: "access_group".to_string(),
            stage: VerificationStage::Configured,
            probe: ProbeSpec::GroupMembership(GroupMembershipProbe {
                group: spec
                    .access_group
                    .clone()
                    .unwrap_or_else(|| "docker".to_string()),
                username: None,
            }),
        });
    }

    plan.push(ServiceProbeDefinition {
        id: "info".to_string(),
        stage: VerificationStage::Operational,
        probe: ProbeSpec::CommandExecution(CommandExecutionProbe {
            program: service_program(spec.command.as_deref(), "docker"),
            args: vec![
                "info".to_string(),
                "--format".to_string(),
                "{{.ServerVersion}}".to_string(),
            ],
            timeout_ms: None,
        }),
    });

    plan
}

fn jupyter_like_plan(
    spec: &ServiceVerificationSpec,
    context: &VerificationContext,
    default_command: &str,
    default_unit: &str,
    default_http_url: &str,
) -> Vec<ServiceProbeDefinition> {
    let mut plan = vec![ServiceProbeDefinition {
        id: "command".to_string(),
        stage: VerificationStage::Present,
        probe: ProbeSpec::CommandExists(CommandExistsProbe {
            command: spec
                .command
                .clone()
                .unwrap_or_else(|| format!("command -v {default_command}")),
        }),
    }];

    let _ = context;

    if let Some(unit) = spec
        .service_unit
        .clone()
        .or_else(|| Some(default_unit.to_string()))
    {
        plan.push(ServiceProbeDefinition {
            id: "unit".to_string(),
            stage: VerificationStage::Configured,
            probe: ProbeSpec::ServiceUnit(ServiceUnitProbe {
                unit,
                scope: spec.service_scope.unwrap_or(ServiceManagerScope::User),
                condition: ServiceUnitCondition::Exists,
                timeout_ms: None,
            }),
        });
    }

    plan.push(ServiceProbeDefinition {
        id: "http".to_string(),
        stage: VerificationStage::Operational,
        probe: ProbeSpec::Http(HttpProbe {
            url: spec
                .http_url
                .clone()
                .unwrap_or_else(|| default_http_url.to_string()),
            expected_status: None,
            timeout_ms: None,
        }),
    });

    plan
}

fn pm2_plan(
    spec: &ServiceVerificationSpec,
    context: &VerificationContext,
) -> Vec<ServiceProbeDefinition> {
    let mut plan = vec![ServiceProbeDefinition {
        id: "command".to_string(),
        stage: VerificationStage::Present,
        probe: ProbeSpec::CommandExists(CommandExistsProbe {
            command: spec
                .command
                .clone()
                .unwrap_or_else(|| "command -v pm2".to_string()),
        }),
    }];

    let socket_paths = if spec.socket_paths.is_empty() {
        pm2_socket_paths(context)
    } else {
        spec.socket_paths.clone()
    };

    let socket_ids = ["socket_rpc", "socket_pub"];
    for (socket_id, socket_path) in socket_ids.into_iter().zip(socket_paths) {
        plan.push(ServiceProbeDefinition {
            id: socket_id.to_string(),
            stage: VerificationStage::Configured,
            probe: ProbeSpec::UnixSocket(UnixSocketProbe { path: socket_path }),
        });
    }

    plan.push(ServiceProbeDefinition {
        id: "ping".to_string(),
        stage: VerificationStage::Operational,
        probe: ProbeSpec::CommandExecution(CommandExecutionProbe {
            program: service_program(spec.command.as_deref(), "pm2"),
            args: vec!["ping".to_string()],
            timeout_ms: None,
        }),
    });

    plan
}

fn vnc_plan(spec: &ServiceVerificationSpec) -> Vec<ServiceProbeDefinition> {
    let mut plan = vec![ServiceProbeDefinition {
        id: "command".to_string(),
        stage: VerificationStage::Present,
        probe: if let Some(command) = spec.command.clone() {
            ProbeSpec::CommandExists(CommandExistsProbe { command })
        } else if spec.commands.is_empty() {
            ProbeSpec::AnyCommand(crate::verifier::AnyCommandProbe {
                commands: vec![
                    "tigervncserver".to_string(),
                    "vncserver".to_string(),
                    "Xvnc".to_string(),
                ],
            })
        } else {
            ProbeSpec::AnyCommand(crate::verifier::AnyCommandProbe {
                commands: spec.commands.clone(),
            })
        },
    }];

    if let Some(unit) = spec.service_unit.clone() {
        plan.push(ServiceProbeDefinition {
            id: "unit".to_string(),
            stage: VerificationStage::Configured,
            probe: ProbeSpec::ServiceUnit(ServiceUnitProbe {
                unit,
                scope: spec.service_scope.unwrap_or(ServiceManagerScope::System),
                condition: ServiceUnitCondition::Exists,
                timeout_ms: None,
            }),
        });
    }

    plan.push(ServiceProbeDefinition {
        id: "tcp".to_string(),
        stage: VerificationStage::Operational,
        probe: ProbeSpec::Tcp(TcpProbe {
            host: spec
                .tcp_host
                .clone()
                .unwrap_or_else(|| "127.0.0.1".to_string()),
            port: spec.tcp_port.unwrap_or(5901),
            timeout_ms: None,
        }),
    });

    plan
}

fn assess_docker(
    spec: &ServiceVerificationSpec,
    evidence: &[ServiceProbeEvidence],
    context: &VerificationContext,
) -> ServiceAssessment {
    let command = find_record(evidence, "command");
    let info = find_record(evidence, "info");
    let unit = find_record(evidence, "unit");
    let system_socket = find_record(evidence, "socket_system");
    let rootless_socket = find_record(evidence, "socket_rootless");
    let access_group = find_record(evidence, "access_group");
    let unit_state = unit.and_then(|record| parse_service_unit_state(record.detail.as_deref()));
    let command_ok = is_satisfied(command);
    let unit_ok = is_satisfied(unit);
    let system_socket_ok = is_satisfied(system_socket);
    let rootless_socket_ok = is_satisfied(rootless_socket);
    let configured_ok = system_socket_ok || rootless_socket_ok || unit_ok;
    let access_blocked = system_socket_ok
        && should_require_group_access(spec, context)
        && !is_satisfied(access_group);

    if is_satisfied(info) {
        return assessment(
            ServiceKind::Docker,
            ServiceUsabilityState::Operational,
            Some(VerificationStage::Operational),
            evidence,
            "docker CLI can talk to a usable daemon",
            join_details([info, system_socket, rootless_socket, access_group, unit]),
        );
    }

    if access_blocked
        || detail_contains_any(
            info,
            &["permission denied", "got permission denied", "eacces"],
        )
    {
        return assessment(
            ServiceKind::Docker,
            ServiceUsabilityState::Blocked,
            Some(VerificationStage::Present),
            evidence,
            "docker is present but current user access is blocked",
            join_details([info, system_socket, access_group, unit]),
        );
    }

    if unit_state.clone().is_some_and(is_on_demand_state)
        || detail_contains_any(
            info,
            &[
                "cannot connect to the docker daemon",
                "is the docker daemon running",
                "error during connect",
            ],
        ) && unit_state.is_some_and(is_on_demand_state)
    {
        return assessment(
            ServiceKind::Docker,
            ServiceUsabilityState::OnDemand,
            proven_service_stage(command_ok, configured_ok),
            evidence,
            "docker is installed but the daemon is not currently available",
            join_details([info, system_socket, rootless_socket, unit]),
        );
    }

    if is_unknown(info)
        || is_unknown(unit)
        || is_unknown(system_socket)
        || is_unknown(rootless_socket)
    {
        return assessment(
            ServiceKind::Docker,
            ServiceUsabilityState::Unknown,
            proven_service_stage(command_ok, configured_ok),
            evidence,
            "docker usability could not be determined",
            join_details([info, system_socket, rootless_socket, access_group, unit]),
        );
    }

    if command_ok {
        return assessment(
            ServiceKind::Docker,
            ServiceUsabilityState::NonUsable,
            proven_service_stage(command_ok, configured_ok),
            evidence,
            "docker is present but not currently usable",
            join_details([info, system_socket, rootless_socket, access_group, unit]),
        );
    }

    assessment(
        ServiceKind::Docker,
        ServiceUsabilityState::Missing,
        None,
        evidence,
        "docker CLI is missing",
        join_details([command, system_socket, rootless_socket, access_group, unit]),
    )
}

fn proven_service_stage(command_ok: bool, configured_ok: bool) -> Option<VerificationStage> {
    if configured_ok {
        Some(VerificationStage::Configured)
    } else if command_ok {
        Some(VerificationStage::Present)
    } else {
        None
    }
}

fn assess_http_service(kind: ServiceKind, evidence: &[ServiceProbeEvidence]) -> ServiceAssessment {
    let command = find_record(evidence, "command");
    let unit = find_record(evidence, "unit");
    let http = find_record(evidence, "http");
    let unit_state = unit.and_then(|record| parse_service_unit_state(record.detail.as_deref()));
    let command_ok = is_satisfied(command);
    let unit_ok = is_satisfied(unit);

    if is_satisfied(http) {
        return assessment(
            kind,
            ServiceUsabilityState::Operational,
            Some(VerificationStage::Operational),
            evidence,
            "service endpoint responded successfully",
            join_details([http, unit, command]),
        );
    }

    if is_unknown(http) || is_unknown(unit) {
        return assessment(
            kind,
            ServiceUsabilityState::Unknown,
            proven_service_stage(command_ok, unit_ok),
            evidence,
            "service usability could not be determined",
            join_details([http, unit, command]),
        );
    }

    if unit_ok && unit_state.is_some_and(is_on_demand_state) {
        return assessment(
            kind,
            ServiceUsabilityState::OnDemand,
            Some(VerificationStage::Configured),
            evidence,
            "service is configured but not currently running",
            join_details([http, unit, command]),
        );
    }

    if command_ok && unit_ok {
        return assessment(
            kind,
            ServiceUsabilityState::NonUsable,
            proven_service_stage(command_ok, unit_ok),
            evidence,
            "service exists but its endpoint is not currently usable",
            join_details([http, unit, command]),
        );
    }

    assessment(
        kind,
        ServiceUsabilityState::Missing,
        proven_service_stage(command_ok, unit_ok),
        evidence,
        "service prerequisites are missing",
        join_details([http, unit, command]),
    )
}

fn assess_pm2(evidence: &[ServiceProbeEvidence]) -> ServiceAssessment {
    let command = find_record(evidence, "command");
    let socket_rpc = find_record(evidence, "socket_rpc");
    let socket_pub = find_record(evidence, "socket_pub");
    let ping = find_record(evidence, "ping");
    let command_ok = is_satisfied(command);
    let sockets_ready = is_satisfied(socket_rpc) && is_satisfied(socket_pub);

    if is_satisfied(ping) {
        return assessment(
            ServiceKind::Pm2,
            ServiceUsabilityState::Operational,
            Some(VerificationStage::Operational),
            evidence,
            "pm2 daemon answered a ping",
            join_details([ping, socket_rpc, socket_pub, command]),
        );
    }

    if detail_contains_any(ping, &["permission denied", "eacces"]) {
        return assessment(
            ServiceKind::Pm2,
            ServiceUsabilityState::Blocked,
            Some(VerificationStage::Present),
            evidence,
            "pm2 is present but access to the daemon is blocked",
            join_details([ping, socket_rpc, socket_pub, command]),
        );
    }

    if is_unknown(ping) || is_unknown(socket_rpc) || is_unknown(socket_pub) {
        return assessment(
            ServiceKind::Pm2,
            ServiceUsabilityState::Unknown,
            proven_service_stage(command_ok, sockets_ready),
            evidence,
            "pm2 usability could not be determined",
            join_details([ping, socket_rpc, socket_pub, command]),
        );
    }

    if sockets_ready {
        return assessment(
            ServiceKind::Pm2,
            ServiceUsabilityState::NonUsable,
            Some(VerificationStage::Configured),
            evidence,
            "pm2 daemon artifacts exist but the daemon is not usable",
            join_details([ping, socket_rpc, socket_pub, command]),
        );
    }

    if command_ok {
        return assessment(
            ServiceKind::Pm2,
            ServiceUsabilityState::OnDemand,
            Some(VerificationStage::Present),
            evidence,
            "pm2 CLI is installed but no live daemon is available yet",
            join_details([ping, socket_rpc, socket_pub, command]),
        );
    }

    assessment(
        ServiceKind::Pm2,
        ServiceUsabilityState::Missing,
        None,
        evidence,
        "pm2 CLI is missing",
        join_details([ping, socket_rpc, socket_pub, command]),
    )
}

fn assess_vnc(
    spec: &ServiceVerificationSpec,
    evidence: &[ServiceProbeEvidence],
) -> ServiceAssessment {
    let command = find_record(evidence, "command");
    let unit = find_record(evidence, "unit");
    let tcp = find_record(evidence, "tcp");
    let unit_state = unit.and_then(|record| parse_service_unit_state(record.detail.as_deref()));
    let command_ok = is_satisfied(command);
    let unit_ok = is_satisfied(unit);
    let configured_ok = unit_ok;
    let unit_planned = unit.is_some() || spec.service_unit.is_some();

    if is_satisfied(tcp) {
        return assessment(
            ServiceKind::Vnc,
            ServiceUsabilityState::Operational,
            Some(VerificationStage::Operational),
            evidence,
            "vnc endpoint accepted a connection",
            join_details([tcp, unit, command]),
        );
    }

    if is_unknown(tcp) || is_unknown(unit) {
        return assessment(
            ServiceKind::Vnc,
            ServiceUsabilityState::Unknown,
            proven_service_stage(command_ok, configured_ok),
            evidence,
            "vnc usability could not be determined",
            join_details([tcp, unit, command]),
        );
    }

    if unit_ok && unit_state.is_some_and(is_on_demand_state) {
        return assessment(
            ServiceKind::Vnc,
            ServiceUsabilityState::OnDemand,
            Some(VerificationStage::Configured),
            evidence,
            "vnc is configured but not currently listening",
            join_details([tcp, unit, command]),
        );
    }

    if command_ok && (!unit_planned || unit_ok) {
        return assessment(
            ServiceKind::Vnc,
            ServiceUsabilityState::NonUsable,
            proven_service_stage(command_ok, configured_ok),
            evidence,
            "vnc tooling is present but no usable endpoint is available",
            join_details([tcp, unit, command]),
        );
    }

    assessment(
        ServiceKind::Vnc,
        ServiceUsabilityState::Missing,
        proven_service_stage(command_ok, configured_ok),
        evidence,
        "vnc endpoint prerequisites are missing",
        join_details([tcp, unit, command]),
    )
}

fn assessment(
    kind: ServiceKind,
    state: ServiceUsabilityState,
    achieved_stage: Option<VerificationStage>,
    evidence: &[ServiceProbeEvidence],
    summary: &str,
    detail: Option<String>,
) -> ServiceAssessment {
    ServiceAssessment {
        kind,
        state,
        achieved_stage,
        observed_scope: evidence
            .iter()
            .fold(ObservedScope::Unknown, |scope, entry| {
                combine_observed_scope(scope, entry.record.observed_scope)
            }),
        summary: summary.to_string(),
        detail,
    }
}

fn reject_blank(
    item_id: &str,
    kind: ServiceKind,
    field: &str,
    value: Option<&str>,
) -> Result<(), String> {
    if value.is_some_and(|entry| entry.trim().is_empty()) {
        return Err(format!(
            "item `{item_id}` service verifier `{}` cannot define a blank `{field}`",
            kind.as_str()
        ));
    }

    Ok(())
}

fn docker_socket_paths(
    spec: &ServiceVerificationSpec,
    context: &VerificationContext,
) -> Vec<(String, PathBuf)> {
    if !spec.socket_paths.is_empty() {
        return spec
            .socket_paths
            .iter()
            .enumerate()
            .map(|(index, path)| {
                let id = match index {
                    0 => "socket_system".to_string(),
                    1 => "socket_rootless".to_string(),
                    _ => format!("socket_{index}"),
                };
                (id, path.clone())
            })
            .collect();
    }

    let mut sockets = vec![(
        "socket_system".to_string(),
        PathBuf::from("/var/run/docker.sock"),
    )];

    let fallback_uid = if context.platform.effective_user.is_root() {
        None
    } else {
        context.platform.effective_user.uid
    };

    if let Some(uid) = context
        .platform
        .target_user
        .as_ref()
        .and_then(|user| user.uid)
        .or(fallback_uid)
    {
        sockets.push((
            "socket_rootless".to_string(),
            PathBuf::from(format!("/run/user/{uid}/docker.sock")),
        ));
    }

    sockets
}

fn pm2_socket_paths(context: &VerificationContext) -> Vec<PathBuf> {
    let home = service_home_dir(context)
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));

    vec![home.join(".pm2/rpc.sock"), home.join(".pm2/pub.sock")]
}

fn service_home_dir(context: &VerificationContext) -> Option<&Path> {
    context
        .platform
        .target_user
        .as_ref()
        .filter(|user| !user.is_root())
        .map(|user| user.home_dir.as_path())
        .or_else(|| {
            (!context.platform.effective_user.is_root())
                .then_some(context.platform.effective_user.home_dir.as_path())
        })
}

fn should_require_group_access(
    spec: &ServiceVerificationSpec,
    context: &VerificationContext,
) -> bool {
    let _ = spec;
    !context.platform.effective_user.is_root()
}

fn find_record<'a>(evidence: &'a [ServiceProbeEvidence], id: &str) -> Option<&'a EvidenceRecord> {
    evidence
        .iter()
        .find(|entry| entry.id == id)
        .map(|entry| &entry.record)
}

fn is_satisfied(record: Option<&EvidenceRecord>) -> bool {
    record.is_some_and(|record| record.status == EvidenceStatus::Satisfied)
}

fn is_unknown(record: Option<&EvidenceRecord>) -> bool {
    record.is_some_and(|record| record.status == EvidenceStatus::Unknown)
}

fn detail_contains_any(record: Option<&EvidenceRecord>, needles: &[&str]) -> bool {
    let Some(detail) = record
        .and_then(|record| record.detail.as_deref())
        .map(str::to_ascii_lowercase)
    else {
        return false;
    };

    needles
        .iter()
        .map(|needle| needle.to_ascii_lowercase())
        .any(|needle| detail.contains(&needle))
}

fn join_details<const N: usize>(records: [Option<&EvidenceRecord>; N]) -> Option<String> {
    let joined = records
        .into_iter()
        .flatten()
        .filter_map(|record| record.detail.as_deref())
        .filter(|detail| !detail.trim().is_empty())
        .collect::<Vec<_>>()
        .join(" | ");

    (!joined.is_empty()).then_some(joined)
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct ParsedServiceUnitState {
    load_state: String,
    active_state: String,
    unit_file_state: String,
    sub_state: String,
}

fn parse_service_unit_state(detail: Option<&str>) -> Option<ParsedServiceUnitState> {
    let detail = detail?;
    let mut parsed = ParsedServiceUnitState::default();

    for field in detail.split(';') {
        let (key, value) = field.trim().split_once('=')?;
        match key.trim() {
            "load_state" => parsed.load_state = value.trim().to_string(),
            "active_state" => parsed.active_state = value.trim().to_string(),
            "unit_file_state" => parsed.unit_file_state = value.trim().to_string(),
            "sub_state" => parsed.sub_state = value.trim().to_string(),
            _ => {}
        }
    }

    Some(parsed)
}

fn is_on_demand_state(state: ParsedServiceUnitState) -> bool {
    matches!(
        state.active_state.as_str(),
        "inactive" | "activating" | "deactivating"
    ) || matches!(state.unit_file_state.as_str(), "enabled" | "linked")
        && matches!(state.sub_state.as_str(), "dead" | "waiting")
}

impl ServiceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Docker => "docker",
            Self::Jupyter => "jupyter",
            Self::Pm2 => "pm2",
            Self::Syncthing => "syncthing",
            Self::Vnc => "vnc",
        }
    }
}

fn infer_service_from_contract(command: &str) -> Option<ServiceVerificationSpec> {
    let kind = match command {
        "command -v docker" => ServiceKind::Docker,
        "command -v jupyter" => ServiceKind::Jupyter,
        "command -v pm2" => ServiceKind::Pm2,
        "command -v tigervncserver" | "command -v vncserver" | "command -v Xvnc" => {
            ServiceKind::Vnc
        }
        _ => return None,
    };

    Some(ServiceVerificationSpec {
        kind,
        command: Some(command.to_string()),
        commands: Vec::new(),
        service_unit: None,
        service_scope: None,
        socket_paths: Vec::new(),
        access_group: None,
        http_url: None,
        tcp_host: None,
        tcp_port: None,
    })
}

fn service_program(command: Option<&str>, default_program: &str) -> String {
    command
        .map(str::trim)
        .and_then(command_contract_program)
        .unwrap_or_else(|| default_program.to_string())
}

fn command_contract_program(command: &str) -> Option<String> {
    if command.is_empty() {
        return None;
    }

    if command.split_whitespace().count() == 1 {
        return Some(command.to_string());
    }

    let suffix = command.strip_prefix("command -v ")?.trim();
    (!suffix.is_empty() && suffix.split_whitespace().count() == 1).then(|| suffix.to_string())
}
