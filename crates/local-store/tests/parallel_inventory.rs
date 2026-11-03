use melissa_local_store::{
    ArtifactRequest, LocalStore, StoreConfig, StoreError, StoreKey, cas::Cas,
};
use std::fs;
fn key() -> StoreKey {
    StoreKey::from_bytes([184; 32])
}
#[test]
fn every_parallel_partition_is_reauthenticated_after_successful_inventory() {
    let d = tempfile::tempdir().unwrap();
    let cas = Cas::open(d.path(), key(), 65536).unwrap();
    for i in 1..=9u8 {
        cas.put(&vec![i; usize::from(i) * 4096]).unwrap();
    }
    let expected = cas.list().unwrap();
    assert_eq!(expected.len(), 9);
    assert_eq!(cas.list_with_parallelism(4).unwrap(), expected);
    assert!(cas.list_with_parallelism(0).is_err());
    assert!(cas.list_with_parallelism(5).is_err());
    for item in &expected {
        let file = d
            .path()
            .join("blobs")
            .join(format!("{}.blob", &item.digest[7..]));
        let original = fs::read(&file).unwrap();
        let mut bad = original.clone();
        *bad.last_mut().unwrap() ^= 1;
        fs::write(&file, bad).unwrap();
        assert!(
            cas.list_with_parallelism(4).is_err(),
            "corrupted {} must never yield partial success",
            item.digest
        );
        fs::write(file, original).unwrap();
    }
    assert_eq!(cas.list_with_parallelism(4).unwrap(), expected);
    assert_eq!(cas.list().unwrap(), expected);
}
#[test]
fn connected_parallel_publication_keeps_orphan_quota_and_authentication() {
    let d = tempfile::tempdir().unwrap();
    let mut store = LocalStore::open(
        &d.path().join("store"),
        key(),
        StoreConfig {
            max_blob_bytes: 2048,
            max_cas_bytes: 4096,
            ..StoreConfig::default()
        },
    )
    .unwrap()
    .with_parallel_cas_verification(4)
    .unwrap();
    let task: melissa_core_contracts::wire::TaskContractV1 = serde_json::from_slice(
        include_bytes!("../../../tests/fixtures/code01/wire/task-contract-v1.json"),
    )
    .unwrap();
    store.journal_mut().create_task(&task).unwrap();
    let cas = Cas::open(&store.root().join("cas"), key(), 2048).unwrap();
    let orphan = cas.put(&vec![1; 1500]).unwrap();
    cas.put(&vec![2; 1500]).unwrap();
    let mut connection = store.connect().unwrap();
    let request = ArtifactRequest {
        artifact_id: "artifact:parallel-quota".into(),
        task_id: task.task_id.clone(),
        content_type: "application/octet-stream".into(),
        logical_type: "test".into(),
        schema_version: "v1".into(),
        retain_until_ms: 0,
        legal_hold: false,
    };
    assert!(matches!(
        connection.put_artifact(&request, &vec![3; 1500]),
        Err(StoreError::Quota(_))
    ));
    assert!(
        connection
            .artifact_metadata(&task.task_id, &request.artifact_id)
            .is_err()
    );
    assert_eq!(cas.list().unwrap().len(), 2);
    let file = store
        .root()
        .join("cas/blobs")
        .join(format!("{}.blob", &orphan.digest[7..]));
    let mut bad = fs::read(&file).unwrap();
    *bad.last_mut().unwrap() ^= 1;
    fs::write(file, bad).unwrap();
    assert!(matches!(
        connection.put_artifact(&request, b"small"),
        Err(StoreError::Crypto | StoreError::Integrity(_))
    ));
    assert!(
        connection
            .artifact_metadata(&task.task_id, &request.artifact_id)
            .is_err()
    );
}
