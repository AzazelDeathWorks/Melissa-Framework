#![cfg(windows)]
use melissa_repository::{
    Error, Limits, Repository, SourceVisibilityPolicy, discovery::DiscoveryOptions,
};
use std::{fs, sync::atomic::AtomicBool};

#[test]
fn sensitive_source_is_unavailable_to_reads_discovery_search_reopen_and_worker_capture() {
    let root = tempfile::tempdir().unwrap();
    for path in [
        ".env",
        "nested/.ENV.production",
        "keys/client.pem",
        ".secrets/token.txt",
    ] {
        let file = root.path().join(path);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, b"SENSITIVE_FIXTURE").unwrap();
    }
    fs::write(root.path().join(".env.example"), b"PUBLIC_EXAMPLE").unwrap();
    fs::write(root.path().join("main.rs"), b"fn main() {}\n").unwrap();
    let repo = Repository::open(root.path(), Limits::default(), vec![]).unwrap();
    let cancel = AtomicBool::new(false);
    for candidate in [repo.reopen().unwrap(), repo] {
        let snapshot = candidate.snapshot(&cancel).unwrap();
        assert_eq!(snapshot.files.len(), 2);
        for path in [
            ".env",
            "nested/.ENV.production",
            "keys/client.pem",
            ".secrets/token.txt",
        ] {
            assert!(matches!(candidate.read(path), Err(Error::Denied(_))));
            assert!(matches!(
                candidate.read_cancellable(path, &cancel),
                Err(Error::Denied(_))
            ));
            assert!(matches!(
                candidate.read_source_cancellable(path, &cancel),
                Err(Error::Denied(_))
            ));
        }
        let found = candidate
            .discover(
                &DiscoveryOptions {
                    respect_ignore: false,
                    ..Default::default()
                },
                &cancel,
            )
            .unwrap();
        assert_eq!(found.files.len(), 2);
        let encoded = serde_json::to_string(
            &candidate
                .search(&snapshot, b"SENSITIVE_FIXTURE", &cancel)
                .unwrap(),
        )
        .unwrap();
        assert!(!encoded.contains(".env\""));
        let source = candidate.worker_input(&snapshot, &cancel).unwrap();
        assert!(
            source
                .snapshot
                .files
                .values()
                .all(|bytes| bytes != b"SENSITIVE_FIXTURE")
        );
        assert!(candidate.read(".env.example").is_ok());
    }
}

#[test]
fn sensitive_exceptions_are_exact_host_policy_and_change_source_identity() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join(".env"), b"SENSITIVE_FIXTURE").unwrap();
    fs::write(root.path().join(".env.local"), b"SENSITIVE_FIXTURE").unwrap();
    let protected = Repository::open(root.path(), Limits::default(), vec![]).unwrap();
    let exception = Repository::open_with_source_policy(
        root.path(),
        Limits::default(),
        SourceVisibilityPolicy::source(vec![], vec![".env".into()]).unwrap(),
    )
    .unwrap();
    assert!(exception.read(".env").is_ok());
    assert!(exception.read(".env.local").is_err());
    assert_ne!(
        protected.source_visibility_digest().unwrap(),
        exception.source_visibility_digest().unwrap()
    );
    let excluded =
        SourceVisibilityPolicy::source(vec![".env".into()], vec![".env".into()]).unwrap();
    assert!(!excluded.allows(".env"));
    assert!(
        !SourceVisibilityPolicy::source(vec![], vec![".git/config".into()])
            .unwrap()
            .allows(".git/config")
    );
    assert!(
        Repository::open_for_package(root.path(), Limits::default())
            .unwrap()
            .snapshot(&AtomicBool::new(false))
            .is_err()
    );
}

#[test]
fn policy_upgrade_preserves_legacy_file_snapshot_hash_when_no_sources_are_removed() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("main.rs"), b"fn main() {}\n").unwrap();
    let exact = Repository::open_for_git(root.path(), Limits::default()).unwrap();
    let source = Repository::open_with_source_policy(
        root.path(),
        Limits::default(),
        SourceVisibilityPolicy::source(vec![".git".into()], vec![]).unwrap(),
    )
    .unwrap();
    let cancel = AtomicBool::new(false);
    assert_eq!(
        exact.snapshot(&cancel).unwrap(),
        source.snapshot(&cancel).unwrap()
    );
    assert_ne!(
        exact.source_visibility_digest().unwrap(),
        source.source_visibility_digest().unwrap()
    );
}

#[test]
fn exact_git_plan_refuses_sensitive_capture_before_worker_or_metadata_access() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join(".env"), b"SENSITIVE_FIXTURE").unwrap();
    let repo = Repository::open_for_git(root.path(), Limits::default()).unwrap();
    let cancel = AtomicBool::new(false);
    let source = repo.snapshot(&cancel).unwrap();
    let limits = melissa_container_executor::ContainerLimits {
        wall_time_ms: 60_000,
        cpu_time_ms: 30_000,
        cpu_rate_basis_points: 10_000,
        memory_bytes: 512 * 1024 * 1024,
        disk_bytes: 64 * 1024 * 1024,
        output_bytes: 1024 * 1024,
        max_processes: 64,
        export_bytes: 1024 * 1024,
    };
    let result = melissa_repository::git::GitPlan::prepare(
        &repo,
        &source,
        &melissa_repository::git::GitOperation::Status,
        melissa_repository::analysis::TOOLING_IMAGE,
        limits.clone(),
        &cancel,
    );
    assert!(matches!(
        result,
        Err(Error::Denied("exact Git capture contains sensitive paths"))
    ));
    assert_eq!(
        fs::read(root.path().join(".env")).unwrap(),
        b"SENSITIVE_FIXTURE"
    );
    let filtered = Repository::open_with_source_policy(
        root.path(),
        Limits::default(),
        SourceVisibilityPolicy::source(vec![".git".into()], vec![]).unwrap(),
    )
    .unwrap();
    let filtered_source = filtered.snapshot(&cancel).unwrap();
    assert!(filtered_source.files.is_empty());
    assert!(matches!(
        melissa_repository::git::GitPlan::prepare(
            &filtered,
            &filtered_source,
            &melissa_repository::git::GitOperation::Status,
            melissa_repository::analysis::TOOLING_IMAGE,
            limits,
            &cancel
        ),
        Err(Error::Denied(_))
    ));
}

#[test]
fn exact_sensitive_exception_traverses_only_approved_parents_and_never_siblings() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join(".secrets")).unwrap();
    fs::write(
        root.path().join(".secrets/approved.txt"),
        b"APPROVED_FIXTURE",
    )
    .unwrap();
    fs::write(
        root.path().join(".secrets/private.txt"),
        b"SENSITIVE_FIXTURE",
    )
    .unwrap();
    let repo = Repository::open_with_source_policy(
        root.path(),
        Limits::default(),
        SourceVisibilityPolicy::source(vec![], vec![".secrets/approved.txt".into()]).unwrap(),
    )
    .unwrap();
    let source = repo.snapshot(&AtomicBool::new(false)).unwrap();
    assert_eq!(source.files.len(), 1);
    assert_eq!(source.files[0].path, ".secrets/approved.txt");
    assert!(repo.read(".secrets/private.txt").is_err());
    assert!(!repo.source_visibility().allows(".secrets"));
}
