#![cfg(windows)]
use melissa_code_quality::{digest, package::*, source::FrozenSource};
use melissa_repository::{Limits, Repository};
use std::{fs, sync::atomic::AtomicBool};
fn no() -> AtomicBool {
    AtomicBool::new(false)
}
#[test]
fn actual_package_bytes_and_lock_are_checked_without_claiming_a_build() {
    let d = tempfile::tempdir().unwrap();
    let s = d.path().join("source");
    let p = d.path().join("package");
    fs::create_dir(&s).unwrap();
    fs::create_dir(&p).unwrap();
    fs::write(s.join("Cargo.lock"), "lock bytes").unwrap();
    fs::write(p.join("app.exe"), b"test fixture bytes, not an executable").unwrap();
    let source_repo = Repository::open(&s, Limits::default(), vec![]).unwrap();
    let source = FrozenSource::capture(&source_repo, &no()).unwrap();
    let package = Repository::open_for_package(&p, Limits::default()).unwrap();
    let locks = vec!["Cargo.lock".into()];
    let env = digest(b"SDK fixture");
    let manifest = PackageManifest::capture(&source, &package, env.clone(), &locks, &no()).unwrap();
    let receipt = manifest
        .verify(&source, &source_repo, &package, &env, &locks, &no())
        .unwrap();
    assert_eq!(receipt.verified_files, 1);
    assert_eq!(
        receipt.build_provenance,
        "NOT_ATTESTED_REQUIRES_NATIVE_BUILD_AND_SMOKE"
    );
    assert!(!receipt.task_accepted && !receipt.protected_acceptance);
    fs::write(p.join("app.exe"), b"tampered bytes").unwrap();
    assert!(
        manifest
            .verify(&source, &source_repo, &package, &env, &locks, &no())
            .is_err()
    );
}
#[test]
fn runtime_directories_are_included_and_extra_files_cannot_hide_in_ignored_paths() {
    let d = tempfile::tempdir().unwrap();
    let s = d.path().join("source");
    let p = d.path().join("package");
    fs::create_dir(&s).unwrap();
    fs::create_dir_all(p.join("node_modules/runtime")).unwrap();
    fs::write(s.join("Cargo.lock"), "lock").unwrap();
    fs::write(p.join("app.js"), "main").unwrap();
    fs::write(p.join("node_modules/runtime/lib.js"), "runtime").unwrap();
    let r = Repository::open(&s, Limits::default(), vec![]).unwrap();
    let source = FrozenSource::capture(&r, &no()).unwrap();
    let env = digest(b"sdk");
    let locks = vec!["Cargo.lock".into()];
    let hidden = Repository::open(&p, Limits::default(), vec![]).unwrap();
    assert!(PackageManifest::capture(&source, &hidden, env.clone(), &locks, &no()).is_err());
    drop(hidden);
    let all = Repository::open_for_package(&p, Limits::default()).unwrap();
    let m = PackageManifest::capture(&source, &all, env.clone(), &locks, &no()).unwrap();
    assert_eq!(m.files.len(), 2);
    fs::create_dir(p.join("target")).unwrap();
    fs::write(p.join("target/hidden.bin"), b"extra").unwrap();
    assert!(m.verify(&source, &r, &all, &env, &locks, &no()).is_err());
}
#[test]
fn stale_source_wrong_environment_or_omitted_required_lock_is_rejected() {
    let d = tempfile::tempdir().unwrap();
    let s = d.path().join("source");
    let p = d.path().join("package");
    fs::create_dir(&s).unwrap();
    fs::create_dir(&p).unwrap();
    fs::write(s.join("Cargo.lock"), "lock").unwrap();
    fs::write(p.join("app"), "artifact").unwrap();
    let r = Repository::open(&s, Limits::default(), vec![]).unwrap();
    let source = FrozenSource::capture(&r, &no()).unwrap();
    let a = Repository::open_for_package(&p, Limits::default()).unwrap();
    let locks = vec!["Cargo.lock".into()];
    let env = digest(b"sdk");
    let mut m = PackageManifest::capture(&source, &a, env.clone(), &locks, &no()).unwrap();
    assert!(
        m.verify(&source, &r, &a, &digest(b"wrong sdk"), &locks, &no())
            .is_err()
    );
    m.lockfiles.clear();
    assert!(m.verify(&source, &r, &a, &env, &locks, &no()).is_err());
    m.lockfiles.insert("Cargo.lock".into(), digest(b"lock"));
    fs::write(s.join("Cargo.lock"), "new dependency resolution").unwrap();
    assert!(m.verify(&source, &r, &a, &env, &locks, &no()).is_err());
}
#[test]
fn duplicate_paths_git_metadata_and_missing_package_files_are_rejected() {
    let d = tempfile::tempdir().unwrap();
    let s = d.path().join("source");
    let p = d.path().join("package");
    fs::create_dir(&s).unwrap();
    fs::create_dir(&p).unwrap();
    fs::write(s.join("Cargo.lock"), "lock").unwrap();
    fs::write(p.join("app"), "artifact").unwrap();
    let r = Repository::open(&s, Limits::default(), vec![]).unwrap();
    let source = FrozenSource::capture(&r, &no()).unwrap();
    let a = Repository::open_for_package(&p, Limits::default()).unwrap();
    let locks = vec!["Cargo.lock".into()];
    let env = digest(b"sdk");
    let mut m = PackageManifest::capture(&source, &a, env.clone(), &locks, &no()).unwrap();
    m.files.push(m.files[0].clone());
    assert!(m.verify(&source, &r, &a, &env, &locks, &no()).is_err());
    m.files.pop();
    fs::remove_file(p.join("app")).unwrap();
    assert!(m.verify(&source, &r, &a, &env, &locks, &no()).is_err());
    fs::create_dir_all(p.join("nested/.git")).unwrap();
    fs::write(p.join("nested/.git/config"), "hidden").unwrap();
    assert!(a.snapshot(&no()).is_err());
}
