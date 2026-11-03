#![cfg(windows)]
use melissa_code_quality::{
    security::scan_secrets, source::FrozenSource, supply_chain::license_allowed, vault,
};
use melissa_core_contracts::catalog::Catalog;
use melissa_repository::{Limits, Repository};
use std::{fs, path::Path, sync::atomic::AtomicBool};
#[test]
fn secret_detection_finds_actual_patterns_without_returning_secret_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let token = format!("{}{}", "ghp_", "A".repeat(36));
    let key = format!("{}{}{}", "-----BEGIN ", "PRIVATE KEY", "-----");
    fs::write(
        dir.path().join("config.txt"),
        format!("ordinary text\n{token}\n{key}\n"),
    )
    .unwrap();
    let repo = Repository::open(dir.path(), Limits::default(), vec![]).unwrap();
    let source = FrozenSource::capture(&repo, &AtomicBool::new(false)).unwrap();
    let findings = scan_secrets(&source).unwrap();
    assert_eq!(findings.len(), 2);
    let report = serde_json::to_string(&findings).unwrap();
    assert!(!report.contains(&token));
    assert!(!report.contains(&key));
    assert!(findings.iter().all(|f| f.line >= 2));
}
#[test]
fn spdx_policy_distinguishes_or_and_unknown_and_exception_requirements() {
    assert!(license_allowed("MIT OR Apache-2.0"));
    assert!(license_allowed("MIT/Apache-2.0"));
    assert!(license_allowed("BSD-3-Clause AND MIT"));
    assert!(license_allowed("GPL-3.0-only OR MIT"));
    for license in [
        "",
        "invented-license",
        "MIT AND GPL-3.0-only",
        "Apache-2.0 WITH LLVM-exception",
        "MIT OR",
        "LicenseRef-private",
    ] {
        assert!(!license_allowed(license), "{license}");
    }
}
#[test]
fn dpapi_vault_reopens_key_preserves_existing_file_and_refuses_corruption() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("key.dpapi");
    let key = vault::create_key(&path).unwrap();
    let mut store = melissa_local_store::LocalStore::open(
        &dir.path().join("journal"),
        key,
        melissa_local_store::StoreConfig::default(),
    )
    .unwrap();
    let mut task: melissa_core_contracts::wire::TaskContractV1 = serde_json::from_slice(
        include_bytes!("../../../tests/fixtures/code01/wire/task-contract-v1.json"),
    )
    .unwrap();
    task.task_id = "task:vault".into();
    store.journal_mut().create_task(&task).unwrap();
    drop(store);
    let bytes = fs::read(&path).unwrap();
    assert_ne!(bytes.len(), 32);
    assert!(vault::create_key(&path).is_err());
    assert_eq!(bytes, fs::read(&path).unwrap());
    let opened = melissa_local_store::LocalStore::open(
        &dir.path().join("journal"),
        vault::open_key(&path).unwrap(),
        melissa_local_store::StoreConfig::default(),
    )
    .unwrap();
    assert_eq!(opened.journal().task(&task.task_id).unwrap().contract, task);
    drop(opened);
    fs::write(&path, b"not a DPAPI envelope").unwrap();
    assert!(vault::open_key(&path).is_err());
}
#[test]
fn definition_only_catalog_does_not_synthesize_evidence_or_weaken_strict_loading() {
    fn copy(from: &Path, to: &Path) {
        fs::create_dir(to).unwrap();
        for entry in fs::read_dir(from).unwrap() {
            let entry = entry.unwrap();
            let target = to.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                copy(&entry.path(), &target)
            } else {
                fs::copy(entry.path(), target).unwrap();
            }
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    copy(&root.join("catalog"), &dir.path().join("catalog"));
    let definitions = Catalog::load_definitions(dir.path()).unwrap();
    assert_eq!(definitions.profiles.len(), 62);
    assert!(!definitions.readiness().executable);
    assert!(Catalog::load(dir.path()).is_err());
    assert!(!dir.path().join("reports").exists());
}
