#![cfg(windows)]
use melissa_core_contracts::wire::TaskContractV1;
use melissa_local_store::{ArtifactRequest, LocalStore, StoreConfig, StoreKey};
#[test]
fn immutable_inventory_reuses_verified_objects_blocks_tampering_and_releases_for_gc() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = LocalStore::open(
        dir.path(),
        StoreKey::from_bytes([219; 32]),
        StoreConfig::default(),
    )
    .unwrap()
    .with_pinned_cas_inventory()
    .unwrap();
    let task: TaskContractV1 = serde_json::from_slice(include_bytes!(
        "../../../tests/fixtures/code01/wire/task-contract-v1.json"
    ))
    .unwrap();
    store.journal_mut().create_task(&task).unwrap();
    let req = |id: &str| ArtifactRequest {
        artifact_id: id.into(),
        task_id: task.task_id.clone(),
        content_type: "text/plain".into(),
        logical_type: "fixture".into(),
        schema_version: "v1".into(),
        retain_until_ms: 0,
        legal_hold: false,
    };
    let first = store.put_artifact(&req("a"), b"first").unwrap();
    let mut other = store.connect().unwrap();
    other.put_artifact(&req("b"), b"second").unwrap();
    let before = store.inventory_metrics().unwrap().unwrap();
    assert_eq!(before.authenticated_blobs, 1);
    other.put_artifact(&req("c"), b"third").unwrap();
    let after = store.inventory_metrics().unwrap().unwrap();
    assert_eq!(after.authenticated_blobs, 2);
    assert!(after.reused_blobs > before.reused_blobs);
    let path = dir
        .path()
        .join("cas/blobs")
        .join(format!("{}.blob", &first.digest[7..]));
    assert!(std::fs::write(&path, b"damage").is_err());
    assert!(std::fs::remove_file(&path).is_err());
    assert_eq!(store.get_artifact(&task.task_id, "a").unwrap(), b"first");
    store.collect_garbage(1, 0).unwrap();
    // Collection releases the derived handles but retains referenced evidence.
    assert!(path.is_file());
    let mut bytes = std::fs::read(&path).unwrap();
    *bytes.last_mut().unwrap() ^= 1;
    std::fs::write(&path, bytes).unwrap();
    assert!(other.put_artifact(&req("d"), b"fourth").is_err());
    assert!(store.get_artifact(&task.task_id, "a").is_err());
}
