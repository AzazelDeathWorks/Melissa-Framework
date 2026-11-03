#![cfg(windows)]
use melissa_code_quality::{
    Error, digest,
    review::*,
    source::{ChangeKind, FrozenSource},
};
use melissa_repository::{Limits, Repository};
use std::{fs, path::Path, sync::atomic::AtomicBool};
fn no() -> AtomicBool {
    AtomicBool::new(false)
}
fn repo(p: &Path) -> Repository {
    Repository::open(p, Limits::default(), vec![]).unwrap()
}
fn session(p: &Path, secrets: Vec<String>) -> ReviewSession {
    fs::write(
        p.join("app.rs"),
        b"fn first(v: &[u8]) -> Option<&u8> { v.first() }\r\n",
    )
    .unwrap();
    let r = repo(p);
    let before = FrozenSource::capture(&r, &no()).unwrap();
    fs::write(
        p.join("app.rs"),
        b"fn first(v: &[u8]) -> &u8 { &v[v.len()] }\r\n",
    )
    .unwrap();
    ReviewSession::start(r, before, Redactor::new(secrets).unwrap(), &no()).unwrap()
}
fn finding(s: &ReviewSession) -> Finding {
    Finding {
        id: "bounds-1".into(),
        severity: Severity::High,
        title: "Index equals length".into(),
        trigger: "Call with any input".into(),
        impact: "The index is outside the slice".into(),
        proposed_verification: "Delegate a regression to an authorized test engineer".into(),
        anchor: s
            .anchor(
                &AnchorRequest {
                    side: Side::After,
                    path: "app.rs".into(),
                    start: 27,
                    end: 38,
                },
                &no(),
            )
            .unwrap(),
    }
}
fn submission(s: &ReviewSession) -> Submission {
    Submission {
        source_digest: s.source().snapshot().digest.clone(),
        baseline_digest: s.baseline().snapshot().digest.clone(),
        findings: vec![finding(s)],
        limitations: vec!["Location checked; behavior still needs independent reproduction".into()],
    }
}
#[test]
fn exact_before_after_and_located_candidate_are_distinct_from_acceptance() {
    let d = tempfile::tempdir().unwrap();
    let s = session(d.path(), vec![]);
    let before = fs::read(d.path().join("app.rs")).unwrap();
    let report = s.submit(submission(&s), &no()).unwrap();
    assert_eq!(report.changes.len(), 1);
    assert_eq!(report.changes[0].kind, ChangeKind::Modified);
    assert!(!report.task_accepted && !report.protected_acceptance);
    assert_eq!(
        report.defect_confirmation,
        "NOT_CONFIRMED_REQUIRES_INDEPENDENT_QA"
    );
    assert_eq!(fs::read(d.path().join("app.rs")).unwrap(), before);
}
#[test]
fn wrong_line_number_or_span_digest_is_rejected() {
    let d = tempfile::tempdir().unwrap();
    let s = session(d.path(), vec![]);
    let mut input = submission(&s);
    input.findings[0].anchor.start_line = 9;
    assert!(matches!(s.submit(input, &no()), Err(Error::Stale(_))));
    let mut input = submission(&s);
    input.findings[0].anchor.span_digest = digest(b"different");
    assert!(matches!(s.submit(input, &no()), Err(Error::Stale(_))));
}
#[test]
fn source_mutation_invalidates_read_anchor_and_submission() {
    let d = tempfile::tempdir().unwrap();
    let s = session(d.path(), vec![]);
    let input = submission(&s);
    fs::write(d.path().join("app.rs"), "new version").unwrap();
    assert!(s.read_page(Side::After, "app.rs", 0, 20, &no()).is_err());
    assert!(
        s.anchor(
            &AnchorRequest {
                side: Side::After,
                path: "app.rs".into(),
                start: 0,
                end: 2
            },
            &no()
        )
        .is_err()
    );
    assert!(s.submit(input, &no()).is_err());
}
#[test]
fn missing_limitations_duplicate_findings_and_wrong_baseline_are_rejected() {
    let d = tempfile::tempdir().unwrap();
    let s = session(d.path(), vec![]);
    let mut a = submission(&s);
    a.limitations.clear();
    assert!(s.submit(a, &no()).is_err());
    let mut a = submission(&s);
    a.findings.push(a.findings[0].clone());
    assert!(s.submit(a, &no()).is_err());
    let mut a = submission(&s);
    a.baseline_digest = digest(b"other");
    assert!(s.submit(a, &no()).is_err());
}
#[test]
fn clean_review_does_not_approve_the_task() {
    let d = tempfile::tempdir().unwrap();
    let s = session(d.path(), vec![]);
    let mut a = submission(&s);
    a.findings.clear();
    let report = s.submit(a, &no()).unwrap();
    assert!(report.findings.is_empty());
    assert!(!report.task_accepted);
}
#[test]
fn traversal_and_unobserved_files_are_not_readable() {
    let d = tempfile::tempdir().unwrap();
    let s = session(d.path(), vec![]);
    for p in [
        "../outside.txt",
        "C:/Windows/win.ini",
        "app.rs:stream",
        "app.rs/..",
        "APP.rs",
        "missing.rs",
    ] {
        assert!(s.read_page(Side::After, p, 0, 65536, &no()).is_err(), "{p}");
    }
}
#[test]
fn utf8_boundaries_crlf_and_actual_lines_are_preserved() {
    let d = tempfile::tempdir().unwrap();
    fs::write(d.path().join("unicode.rs"), "ş\r\nç🙂\n").unwrap();
    let r = repo(d.path());
    let b = FrozenSource::capture(&r, &no()).unwrap();
    let s = ReviewSession::start(r, b, Redactor::new(vec![]).unwrap(), &no()).unwrap();
    let a = s
        .anchor(
            &AnchorRequest {
                side: Side::After,
                path: "unicode.rs".into(),
                start: 4,
                end: 10,
            },
            &no(),
        )
        .unwrap();
    assert_eq!((a.start_line, a.end_line), (2, 2));
    assert_eq!(a.span_digest, digest("ç🙂".as_bytes()));
    assert!(
        s.anchor(
            &AnchorRequest {
                side: Side::After,
                path: "unicode.rs".into(),
                start: 5,
                end: 10
            },
            &no()
        )
        .is_err()
    );
    assert!(
        s.read_page(Side::After, "unicode.rs", 1, 20, &no())
            .is_err()
    );
    assert!(s.read_page(Side::After, "unicode.rs", 0, 1, &no()).is_err());
}
#[test]
fn secret_crossing_page_edges_is_redacted_and_anchor_has_no_quote() {
    let d = tempfile::tempdir().unwrap();
    let secret = "demo-only-secret-12345678";
    fs::write(d.path().join("config.rs"), format!("AAA{secret}ZZZ")).unwrap();
    let r = repo(d.path());
    let b = FrozenSource::capture(&r, &no()).unwrap();
    let s = ReviewSession::start(r, b, Redactor::new(vec![secret.into()]).unwrap(), &no()).unwrap();
    for (start, max) in [(0, 8), (8, 12), (20, 12), (0, 65536)] {
        let page = s
            .read_page(Side::After, "config.rs", start, max, &no())
            .unwrap();
        let text = page["text"].as_str().unwrap();
        assert!(text.contains("[REDACTED]"));
        assert!(!text.contains("secret-123"));
    }
    let a = s
        .anchor(
            &AnchorRequest {
                side: Side::After,
                path: "config.rs".into(),
                start: 3,
                end: 3 + secret.len(),
            },
            &no(),
        )
        .unwrap();
    assert!(!serde_json::to_string(&a).unwrap().contains(secret));
}
#[test]
fn finding_prose_is_redacted_before_return_and_debug_does_not_echo_secret() {
    let d = tempfile::tempdir().unwrap();
    let secret = "demo-only-secret-12345678";
    let s = session(d.path(), vec![secret.into()]);
    let mut input = submission(&s);
    input.findings[0].title = format!("Accidental {secret}");
    input.limitations.push(secret.into());
    let report = serde_json::to_string(&s.submit(input, &no()).unwrap()).unwrap();
    assert!(!report.contains(secret));
    assert!(report.contains("[REDACTED]"));
    let e = serde_json::from_str::<Submission>(&format!("{{\"{secret}\":1}}")).unwrap_err();
    assert!(!format!("{:?}", Error::Json(e)).contains(secret));
}
#[test]
fn unrelated_workspace_cannot_supply_the_baseline() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    fs::write(a.path().join("a.rs"), "same").unwrap();
    fs::write(b.path().join("a.rs"), "same").unwrap();
    let base = FrozenSource::capture(&repo(a.path()), &no()).unwrap();
    assert!(
        ReviewSession::start(repo(b.path()), base, Redactor::new(vec![]).unwrap(), &no()).is_err()
    );
}
#[test]
fn added_and_deleted_files_and_before_anchors_are_supported() {
    let d = tempfile::tempdir().unwrap();
    fs::write(d.path().join("removed.rs"), "old").unwrap();
    let r = repo(d.path());
    let base = FrozenSource::capture(&r, &no()).unwrap();
    fs::remove_file(d.path().join("removed.rs")).unwrap();
    fs::write(d.path().join("added.rs"), "new").unwrap();
    let s = ReviewSession::start(r, base, Redactor::new(vec![]).unwrap(), &no()).unwrap();
    let v = s.scope(&no()).unwrap();
    assert_eq!(v["changes"].as_array().unwrap().len(), 2);
    assert!(
        s.anchor(
            &AnchorRequest {
                side: Side::Before,
                path: "removed.rs".into(),
                start: 0,
                end: 3
            },
            &no()
        )
        .is_ok()
    );
    assert!(
        s.anchor(
            &AnchorRequest {
                side: Side::After,
                path: "removed.rs".into(),
                start: 0,
                end: 3
            },
            &no()
        )
        .is_err()
    );
}
#[test]
fn cancellation_prevents_capture_and_reads() {
    let d = tempfile::tempdir().unwrap();
    let s = session(d.path(), vec![]);
    let stop = AtomicBool::new(true);
    assert!(s.scope(&stop).is_err());
    assert!(FrozenSource::capture(&repo(d.path()), &stop).is_err());
}
