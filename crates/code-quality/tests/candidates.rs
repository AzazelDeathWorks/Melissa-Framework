use melissa_code_quality::{candidate::*, digest};
use std::collections::BTreeMap;
fn policy() -> CandidatePolicy {
    CandidatePolicy::new(
        digest(b"source"),
        digest(b"artifact"),
        digest(b"environment"),
        BTreeMap::from([
            (CheckKind::Build, digest(b"build-v1")),
            (CheckKind::Tests, digest(b"test-v1")),
            (CheckKind::PackageSmoke, digest(b"smoke-v1")),
        ]),
    )
    .unwrap()
}
fn claims() -> Vec<CheckClaim> {
    [
        (CheckKind::Build, b"build-v1".as_slice()),
        (CheckKind::Tests, b"test-v1".as_slice()),
        (CheckKind::PackageSmoke, b"smoke-v1".as_slice()),
    ]
    .into_iter()
    .map(|(kind, v)| CheckClaim {
        tests: (kind == CheckKind::Tests).then_some(TestCounts {
            discovered: 3,
            passed: 3,
            failed: 0,
            skipped: 0,
            filtered: 0,
        }),
        kind,
        status: CheckStatus::Passed,
        source_digest: digest(b"source"),
        verifier_digest: digest(v),
        environment_digest: digest(b"environment"),
        log_digest: digest(b"raw log"),
        artifact_digest: Some(digest(b"artifact")),
        exit_code: Some(0),
        truncated: false,
        requires_reconciliation: false,
    })
    .collect()
}
#[test]
fn complete_untrusted_claims_only_request_independent_verification() {
    let a = policy().assess(&claims()).unwrap();
    assert_eq!(a.status, "READY_FOR_INDEPENDENT_VERIFICATION");
    assert_eq!(a.claim_authenticity, "UNTRUSTED_DEVELOPER_CLAIMS");
    assert!(!a.task_accepted && !a.protected_acceptance);
}
#[test]
fn missing_check_is_not_run_not_a_pass() {
    let mut c = claims();
    c.pop();
    let r = policy().assess(&c).unwrap();
    assert_eq!(r.status, "BLOCKED");
    assert!(r.blockers.iter().any(|s| s.contains("NOT_RUN")));
}
#[test]
fn zero_skipped_filtered_failed_or_inconsistent_test_counts_block() {
    for counts in [
        TestCounts {
            discovered: 0,
            passed: 0,
            failed: 0,
            skipped: 0,
            filtered: 0,
        },
        TestCounts {
            discovered: 3,
            passed: 2,
            failed: 0,
            skipped: 1,
            filtered: 0,
        },
        TestCounts {
            discovered: 3,
            passed: 2,
            failed: 0,
            skipped: 0,
            filtered: 1,
        },
        TestCounts {
            discovered: 3,
            passed: 2,
            failed: 1,
            skipped: 0,
            filtered: 0,
        },
        TestCounts {
            discovered: 4,
            passed: 3,
            failed: 0,
            skipped: 0,
            filtered: 0,
        },
    ] {
        let mut c = claims();
        c[1].tests = Some(counts);
        assert_eq!(policy().assess(&c).unwrap().status, "BLOCKED");
    }
}
#[test]
fn failed_blocked_unknown_unsupported_and_notrun_never_count_as_pass() {
    for status in [
        CheckStatus::Failed,
        CheckStatus::NotRun,
        CheckStatus::Blocked,
        CheckStatus::Unknown,
        CheckStatus::Unsupported,
    ] {
        let mut c = claims();
        c[0].status = status;
        assert_eq!(policy().assess(&c).unwrap().status, "BLOCKED");
    }
}
#[test]
fn source_environment_verifier_log_and_package_mismatch_are_blocked() {
    for which in 0..5 {
        let mut c = claims();
        match which {
            0 => c[0].source_digest = digest(b"old"),
            1 => c[0].environment_digest = digest(b"other"),
            2 => c[0].verifier_digest = digest(b"modified assertions"),
            3 => c[0].log_digest = "".into(),
            _ => c[2].artifact_digest = Some(digest(b"other binary")),
        }
        assert_eq!(policy().assess(&c).unwrap().status, "BLOCKED");
    }
}
#[test]
fn zero_exit_does_not_hide_truncation_unknown_effect_or_missing_counts() {
    for which in 0..4 {
        let mut c = claims();
        match which {
            0 => c[0].truncated = true,
            1 => c[0].requires_reconciliation = true,
            2 => c[0].exit_code = None,
            _ => c[1].tests = None,
        }
        assert_eq!(policy().assess(&c).unwrap().status, "BLOCKED");
    }
}
#[test]
fn duplicates_unregistered_checks_and_counts_on_non_test_claims_are_invalid() {
    let mut c = claims();
    c.push(c[0].clone());
    assert!(policy().assess(&c).is_err());
    let mut c = claims();
    c[0].kind = CheckKind::Security;
    assert!(policy().assess(&c).is_err());
    let mut c = claims();
    c[0].tests = Some(TestCounts {
        discovered: 1,
        passed: 1,
        failed: 0,
        skipped: 0,
        filtered: 0,
    });
    assert!(policy().assess(&c).is_err());
}
#[test]
fn claimed_acceptance_field_is_rejected_by_wire_schema() {
    let mut v = serde_json::to_value(&claims()[0]).unwrap();
    v["protected_acceptance"] = true.into();
    assert!(serde_json::from_value::<CheckClaim>(v).is_err());
}
