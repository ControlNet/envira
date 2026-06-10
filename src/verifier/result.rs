use serde::{Deserialize, Serialize};

use crate::verifier::{ServiceAssessment, ServiceProbeEvidence, VerifierCheck};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationHealth {
    Healthy,
    Unknown,
    Missing,
    Broken,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservedScope {
    Unknown,
    System,
    User,
    Both,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceStatus {
    Satisfied,
    Missing,
    Broken,
    Unknown,
    NotApplicable,
}

impl EvidenceStatus {
    pub fn blocks_stage(self) -> bool {
        matches!(self, Self::Missing | Self::Broken | Self::Unknown)
    }

    pub fn proves_stage(self) -> bool {
        matches!(self, Self::Satisfied)
    }

    pub fn failure_health(self) -> Option<VerificationHealth> {
        match self {
            Self::Satisfied | Self::NotApplicable => None,
            Self::Unknown => Some(VerificationHealth::Unknown),
            Self::Missing => Some(VerificationHealth::Missing),
            Self::Broken => Some(VerificationHealth::Broken),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct EvidenceRecord {
    pub status: EvidenceStatus,
    pub observed_scope: ObservedScope,
    pub summary: String,
    pub detail: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct VerifierEvidence {
    pub check: VerifierCheck,
    pub record: EvidenceRecord,
    pub participates: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct VerificationSummary {
    pub total_checks: usize,
    pub participating_checks: usize,
    pub skipped_checks: usize,
    pub satisfied_checks: usize,
    pub missing_checks: usize,
    pub broken_checks: usize,
    pub unknown_checks: usize,
    pub not_applicable_checks: usize,
    pub required_failures: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct VerifierResult {
    pub requested_profile: crate::verifier::VerificationProfile,
    pub required_stage: crate::verifier::VerificationStage,
    pub achieved_stage: Option<crate::verifier::VerificationStage>,
    pub threshold_met: bool,
    pub health: VerificationHealth,
    pub observed_scope: ObservedScope,
    pub summary: VerificationSummary,
    pub evidence: Vec<VerifierEvidence>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub service_evidence: Vec<ServiceProbeEvidence>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service: Option<ServiceAssessment>,
}
