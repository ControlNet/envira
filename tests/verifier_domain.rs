use envira::verifier::{
    reduce_verifier_result, EvidenceRecord, EvidenceStatus, ObservedScope, ProbeKind,
    ProbeRequirement, ReductionError, VerificationHealth, VerificationProfile, VerificationStage,
    VerifierCheck, VerifierSpec,
};

#[test]
fn verifier_domain_result_shape_is_structured_and_machine_readable() {
    let spec = VerifierSpec {
        checks: vec![
            check(
                VerificationStage::Present,
                ProbeRequirement::Required,
                VerificationProfile::Quick,
                ProbeKind::Command,
                Some("git"),
                None,
                None,
                None,
            ),
            check(
                VerificationStage::Configured,
                ProbeRequirement::Optional,
                VerificationProfile::Standard,
                ProbeKind::File,
                None,
                None,
                Some("/home/alice/.gitconfig"),
                None,
            ),
        ],
        service: None,
    };
    let result = reduce_verifier_result(
        VerificationStage::Present,
        &spec,
        VerificationProfile::Standard,
        vec![
            evidence(EvidenceStatus::Satisfied, ObservedScope::User, "git found"),
            evidence(
                EvidenceStatus::Missing,
                ObservedScope::User,
                "config file missing",
            ),
        ],
    )
    .expect("result should reduce");

    assert_eq!(result.requested_profile, VerificationProfile::Standard);
    assert_eq!(result.required_stage, VerificationStage::Present);
    assert_eq!(result.achieved_stage, Some(VerificationStage::Present));
    assert!(result.threshold_met);
    assert_eq!(result.health, VerificationHealth::Missing);
    assert_eq!(result.observed_scope, ObservedScope::User);
    assert_eq!(result.summary.total_checks, 2);
    assert_eq!(result.summary.participating_checks, 2);
    assert_eq!(result.summary.skipped_checks, 0);
    assert_eq!(result.summary.satisfied_checks, 1);
    assert_eq!(result.summary.missing_checks, 1);
    assert_eq!(result.summary.required_failures, 0);
    assert_eq!(result.evidence.len(), 2);
    assert!(result.evidence.iter().all(|evidence| evidence.participates));

    let json = serde_json::to_value(&result).expect("result should serialize");
    assert_eq!(json["requested_profile"], "standard");
    assert_eq!(json["required_stage"], "present");
    assert_eq!(json["achieved_stage"], "present");
    assert_eq!(json["threshold_met"], true);
    assert_eq!(json["health"], "missing");
    assert_eq!(json["observed_scope"], "user");
    assert_eq!(json["evidence"].as_array().map(Vec::len), Some(2));
}

#[test]
fn verifier_domain_profiles_only_change_participating_checks() {
    let spec = VerifierSpec {
        checks: vec![
            check(
                VerificationStage::Present,
                ProbeRequirement::Required,
                VerificationProfile::Quick,
                ProbeKind::Command,
                Some("git"),
                None,
                None,
                None,
            ),
            check(
                VerificationStage::Configured,
                ProbeRequirement::Required,
                VerificationProfile::Standard,
                ProbeKind::File,
                None,
                None,
                Some("/home/alice/.gitconfig"),
                None,
            ),
            check(
                VerificationStage::Operational,
                ProbeRequirement::Optional,
                VerificationProfile::Strict,
                ProbeKind::Command,
                Some("git fetch"),
                None,
                None,
                None,
            ),
        ],
        service: None,
    };
    let evidence = vec![
        evidence(EvidenceStatus::Satisfied, ObservedScope::User, "git found"),
        evidence(
            EvidenceStatus::Satisfied,
            ObservedScope::User,
            "config present",
        ),
        evidence(
            EvidenceStatus::Broken,
            ObservedScope::User,
            "fetch command failed",
        ),
    ];

    let quick = reduce_verifier_result(
        VerificationStage::Present,
        &spec,
        VerificationProfile::Quick,
        evidence.clone(),
    )
    .expect("quick result should reduce");
    let standard = reduce_verifier_result(
        VerificationStage::Present,
        &spec,
        VerificationProfile::Standard,
        evidence.clone(),
    )
    .expect("standard result should reduce");
    let strict = reduce_verifier_result(
        VerificationStage::Present,
        &spec,
        VerificationProfile::Strict,
        evidence,
    )
    .expect("strict result should reduce");

    assert_eq!(quick.summary.participating_checks, 1);
    assert_eq!(quick.summary.skipped_checks, 2);
    assert_eq!(quick.achieved_stage, Some(VerificationStage::Present));
    assert_eq!(quick.health, VerificationHealth::Healthy);

    assert_eq!(standard.summary.participating_checks, 2);
    assert_eq!(standard.summary.skipped_checks, 1);
    assert_eq!(standard.achieved_stage, Some(VerificationStage::Configured));
    assert_eq!(standard.health, VerificationHealth::Healthy);

    assert_eq!(strict.summary.participating_checks, 3);
    assert_eq!(strict.summary.skipped_checks, 0);
    assert_eq!(strict.achieved_stage, Some(VerificationStage::Configured));
    assert_eq!(strict.health, VerificationHealth::Broken);
    assert_eq!(strict.summary.required_failures, 0);
}

#[test]
fn verifier_domain_stage_thresholds_remain_data_driven() {
    let spec = VerifierSpec {
        checks: vec![
            check(
                VerificationStage::Present,
                ProbeRequirement::Required,
                VerificationProfile::Quick,
                ProbeKind::Command,
                Some("git"),
                None,
                None,
                None,
            ),
            check(
                VerificationStage::Configured,
                ProbeRequirement::Required,
                VerificationProfile::Quick,
                ProbeKind::File,
                None,
                None,
                Some("/home/alice/.gitconfig"),
                None,
            ),
        ],
        service: None,
    };
    let evidence = vec![
        evidence(EvidenceStatus::Satisfied, ObservedScope::User, "git found"),
        evidence(
            EvidenceStatus::Satisfied,
            ObservedScope::User,
            "config present",
        ),
    ];

    let configured = reduce_verifier_result(
        VerificationStage::Configured,
        &spec,
        VerificationProfile::Quick,
        evidence.clone(),
    )
    .expect("configured threshold should reduce");
    let operational = reduce_verifier_result(
        VerificationStage::Operational,
        &spec,
        VerificationProfile::Quick,
        evidence,
    )
    .expect("operational threshold should reduce");

    assert_eq!(
        configured.achieved_stage,
        Some(VerificationStage::Configured)
    );
    assert!(configured.threshold_met);
    assert_eq!(
        operational.achieved_stage,
        Some(VerificationStage::Configured)
    );
    assert!(!operational.threshold_met);
}

#[test]
fn verifier_domain_health_distinguishes_missing_broken_and_unknown() {
    let spec = VerifierSpec {
        checks: vec![check(
            VerificationStage::Present,
            ProbeRequirement::Required,
            VerificationProfile::Quick,
            ProbeKind::Command,
            Some("git"),
            None,
            None,
            None,
        )],
        service: None,
    };

    for (status, expected_health) in [
        (EvidenceStatus::Missing, VerificationHealth::Missing),
        (EvidenceStatus::Broken, VerificationHealth::Broken),
        (EvidenceStatus::Unknown, VerificationHealth::Unknown),
    ] {
        let result = reduce_verifier_result(
            VerificationStage::Present,
            &spec,
            VerificationProfile::Quick,
            vec![evidence(status, ObservedScope::User, "probe outcome")],
        )
        .expect("result should reduce");

        assert_eq!(result.achieved_stage, None);
        assert!(!result.threshold_met);
        assert_eq!(result.health, expected_health);
        assert_eq!(result.summary.required_failures, 1);
    }
}

#[test]
fn verifier_domain_rejects_evidence_count_mismatches() {
    let spec = VerifierSpec {
        checks: vec![check(
            VerificationStage::Present,
            ProbeRequirement::Required,
            VerificationProfile::Quick,
            ProbeKind::Command,
            Some("git"),
            None,
            None,
            None,
        )],
        service: None,
    };

    let error = reduce_verifier_result(
        VerificationStage::Present,
        &spec,
        VerificationProfile::Quick,
        Vec::new(),
    )
    .expect_err("mismatched evidence counts should fail");

    assert_eq!(
        error,
        ReductionError::EvidenceCountMismatch {
            expected: 1,
            actual: 0,
        }
    );
}

fn check(
    stage: VerificationStage,
    requirement: ProbeRequirement,
    min_profile: VerificationProfile,
    kind: ProbeKind,
    command: Option<&str>,
    commands: Option<Vec<&str>>,
    path: Option<&str>,
    pattern: Option<&str>,
) -> VerifierCheck {
    VerifierCheck {
        stage,
        requirement,
        min_profile,
        kind,
        command: command.map(str::to_string),
        commands: commands.map(|values| values.into_iter().map(str::to_string).collect()),
        path: path.map(str::to_string),
        pattern: pattern.map(str::to_string),
    }
}

fn evidence(
    status: EvidenceStatus,
    observed_scope: ObservedScope,
    summary: &str,
) -> EvidenceRecord {
    EvidenceRecord {
        status,
        observed_scope,
        summary: summary.to_string(),
        detail: None,
    }
}
