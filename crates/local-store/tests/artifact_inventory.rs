use melissa_core_contracts::wire::TaskContractV1;
use melissa_local_store::{ArtifactRequest, LocalStore, StoreConfig, StoreKey};

#[test]
fn inventory_is_task_scoped_sorted_and_metadata_authenticated() {
    let temp = tempfile::tempdir().unwrap();
    let mut store = LocalStore::open(
        temp.path(),
        StoreKey::from_bytes([73; 32]),
        StoreConfig::default(),
    )
    .unwrap();
    let mut contract: TaskContractV1 = serde_json::from_slice(include_bytes!(
        "../../../tests/fixtures/code01/wire/task-contract-v1.json"
    ))
    .unwrap();
    for task in ["task:a", "task:b"] {
        contract.task_id = task.into();
        store.journal_mut().create_task(&contract).unwrap();
    }
    for (task, id) in [
        ("task:a", "artifact:z"),
        ("task:b", "artifact:b"),
        ("task:a", "artifact:a"),
    ] {
        store
            .put_artifact(
                &ArtifactRequest {
                    artifact_id: id.into(),
                    task_id: task.into(),
                    content_type: "text/plain".into(),
                    logical_type: "inventory.fixture".into(),
                    schema_version: "v1".into(),
                    retain_until_ms: 0,
                    legal_hold: true,
                },
                b"evidence",
            )
            .unwrap();
    }
    let listed = store.list_artifacts("task:a").unwrap();
    assert_eq!(
        listed
            .iter()
            .map(|a| a.artifact_id.as_str())
            .collect::<Vec<_>>(),
        ["artifact:a", "artifact:z"]
    );
    assert!(listed.iter().all(|a| a.task_id == "task:a"));
    assert_eq!(store.list_artifacts("task:b").unwrap().len(), 1);
    assert!(store.list_artifacts("task:missing").is_err());
    assert!(store.get_artifact("task:b", "artifact:a").is_err());
    let conn = rusqlite::Connection::open(temp.path().join("store.sqlite3")).unwrap();
    conn.execute(
        "UPDATE artifacts SET content_type='altered' WHERE artifact_id='artifact:a'",
        [],
    )
    .unwrap();
    assert!(store.list_artifacts("task:a").is_err());
}
