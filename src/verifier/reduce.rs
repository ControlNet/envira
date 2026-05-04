use thiserror::Error;

use crate::verifier::{
    EvidenceRecord, EvidenceStatus, ObservedScope, ProbeRequirement, VerificationHealth,
    VerificationProfile, VerificationStage, VerificationSummary, VerifierEvidence, VerifierResult,
    VerifierSpec,
};

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum ReductionError {
    #[error("verifier evidence count mismatch: expected {expected}, got {actual}")]
    EvidenceCountMismatch { expected: usize, actual: usize },
}

pub fn reduce_verifier_result(
    required_stage: VerificationStage,
    spec: &VerifierSpec,
    requested_profile: VerificationProfile,
    evidence_records: Vec<EvidenceRecord>,
) -> Result<VerifierResult, ReductionError> {
    if spec.checks.len() != evidence_records.len() {
        return Err(ReductionError::EvidenceCountMismatch {
            expected: spec.checks.len(),
            actual: evidence_records.len(),
        });
    }

    let mut summary = VerificationSummary {
        total_checks: spec.checks.len(),
        ..VerificationSummary::default()
    };
    let mut evidence = Vec::with_capacity(spec.checks.len());
    let mut participating_required = Vec::new();
    let mut observed_scope = ScopeAccumulator::default();
    let mut saw_participating = false;
    let mut health = VerificationHealth::Healthy;

    for (check, record) in spec.checks.iter().cloned().zip(evidence_records) {
        let participates = check.participates_in(requested_profile);

        if participates {
            saw_participating = true;
            summary.participating_checks += 1;
            observed_scope.observe(record.observed_scope);

            match record.status {
                EvidenceStatus::Satisfied => summary.satisfied_checks += 1,
                EvidenceStatus::Missing => summary.missing_checks += 1,
                EvidenceStatus::Broken => summary.broken_checks += 1,
                EvidenceStatus::Unknown => summary.unknown_checks += 1,
                EvidenceStatus::NotApplicable => summary.not_applicable_checks += 1,
            }

            if let Some(candidate) = record.status.failure_health() {
                health = health.max(candidate);
                if check.requirement == ProbeRequirement::Required {
                    summary.required_failures += 1;
                }
            }

            if check.requirement == ProbeRequirement::Required {
                participating_required.push((check.stage, record.status));
            }
        } else {
            summary.skipped_checks += 1;
        }

        evidence.push(VerifierEvidence {
            check,
            record,
            participates,
        });
    }

    if !saw_participating {
        health = VerificationHealth::Unknown;
    }

    let achieved_stage = highest_achieved_stage(&participating_required);
    let threshold_met = achieved_stage.is_some_and(|stage| stage.meets(required_stage));

    Ok(VerifierResult {
        requested_profile,
        required_stage,
        achieved_stage,
        threshold_met,
        health,
        observed_scope: if saw_participating {
            observed_scope.finish()
        } else {
            ObservedScope::Unknown
        },
        summary,
        evidence,
        service_evidence: Vec::new(),
        service: None,
    })
}

fn highest_achieved_stage(
    participating_required: &[(VerificationStage, EvidenceStatus)],
) -> Option<VerificationStage> {
    for candidate in [
        VerificationStage::Operational,
        VerificationStage::Configured,
        VerificationStage::Present,
    ] {
        let has_proof = participating_required
            .iter()
            .any(|(stage, status)| *stage >= candidate && status.proves_stage());
        let lower_stages_clear = participating_required
            .iter()
            .filter(|(stage, _)| *stage <= candidate)
            .all(|(_, status)| !status.blocks_stage());

        if has_proof && lower_stages_clear {
            return Some(candidate);
        }
    }

    None
}

#[derive(Default)]
struct ScopeAccumulator {
    saw_system: bool,
    saw_user: bool,
}

impl ScopeAccumulator {
    fn observe(&mut self, scope: ObservedScope) {
        match scope {
            ObservedScope::Unknown => {}
            ObservedScope::System => self.saw_system = true,
            ObservedScope::User => self.saw_user = true,
            ObservedScope::Both => {
                self.saw_system = true;
                self.saw_user = true;
            }
        }
    }

    fn finish(self) -> ObservedScope {
        match (self.saw_system, self.saw_user) {
            (true, true) => ObservedScope::Both,
            (true, false) => ObservedScope::System,
            (false, true) => ObservedScope::User,
            (false, false) => ObservedScope::Unknown,
        }
    }
}
