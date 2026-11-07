use super::*;
use crate::project::{Command, Contract, tests::Fixture};
use std::{
    collections::BTreeMap,
    sync::{Arc, atomic::AtomicBool},
};
fn task(f: &Fixture) -> String {
    f.app
        .create(
            &f.source,
            "Retain BACKUP_CANARY_GOAL",
            "melissa.code.reviewer",
            vec![],
        )
        .unwrap()["task_id"]
        .as_str()
        .unwrap()
        .into()
}
fn copied(source: &Path, out: &Path) {
    fs::create_dir(out).unwrap();
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        assert!(entry.file_type().unwrap().is_file());
        fs::copy(entry.path(), out.join(entry.file_name())).unwrap();
    }
}
#[test]
fn retention_preview_preserves_active_and_permanent_evidence_and_counts_unique_bytes() {
    let f = Fixture::new(false);
    let id = task(&f);
    let mut store = f.app.store(&id).unwrap();
    store
        .put_artifact(
            &melissa_local_store::ArtifactRequest {
                artifact_id: "optional".into(),
                task_id: id.clone(),
                content_type: "text/plain".into(),
                logical_type: "optional.debug".into(),
                schema_version: "v1".into(),
                retain_until_ms: 0,
                legal_hold: false,
            },
            b"optional",
        )
        .unwrap();
    let before = store.journal().events(&id).unwrap();
    drop(store);
    let preview = f.app.retention_preview(&id).unwrap();
    assert!(
        preview["retention"]["entries"]
            .as_array()
            .unwrap()
            .iter()
            .all(|e| e["eligible"] == false)
    );
    assert!(
        f.app.disk(&id).unwrap()["disk"]["task_directory_logical_bytes"]
            .as_u64()
            .unwrap()
            > 0
    );
    let store = f.app.store(&id).unwrap();
    assert_eq!(before, store.journal().events(&id).unwrap());
    drop(store);
    f.app.cancel(&id).unwrap();
    let mut store = f.app.store(&id).unwrap();
    store.release_artifact(&id, "optional").unwrap();
    let entries = store.retention_preview(&id, now().unwrap()).unwrap();
    assert!(
        entries
            .iter()
            .any(|e| e.artifact.artifact_id == "optional" && e.eligible)
    );
    assert!(entries.iter().any(|e| !e.eligible));
    assert_eq!(store.get_artifact(&id, "optional").unwrap(), b"optional");
}
#[test]
fn portable_backup_roundtrips_to_new_user_bound_home_and_never_copies_plain_metadata_or_keys() {
    let f = Fixture::new(true);
    let id = task(&f);
    let key = f.root.join("recovery.bin");
    portable::keygen(&key).unwrap();
    assert!(portable::keygen(&key).is_err());
    let out = f.root.join("archive");
    let store = f.app.store(&id).unwrap();
    let before = store.journal().events(&id).unwrap();
    let original = store.get_artifact(&id, crate::app::WORKSPACE).unwrap();
    drop(store);
    f.app.backup_task(&id, &out, &key).unwrap();
    assert!(f.app.backup_task(&id, &out, &key).is_err());
    for entry in fs::read_dir(&out).unwrap() {
        let raw = fs::read(entry.unwrap().path()).unwrap();
        assert!(
            !raw.windows(b"BACKUP_CANARY_GOAL".len())
                .any(|w| w == b"BACKUP_CANARY_GOAL")
        );
        assert!(!raw.windows(16).any(|w| w == b"SQLite format 3\0"));
    }
    let recovered = f.root.join("restored");
    let result = portable::restore(&out, &recovered, &key).unwrap();
    assert_eq!(result["evidence_only"], true);
    assert!(!recovered.join("settings.enc").exists());
    let app = App::open(&recovered).unwrap();
    let store = app.store(&id).unwrap();
    assert_eq!(store.journal().events(&id).unwrap(), before);
    assert_eq!(
        store.get_artifact(&id, crate::app::WORKSPACE).unwrap(),
        original
    );
    assert!(app.restored(&id).unwrap());
    drop(store);
    drop(app);
    assert!(portable::restore(&out, &recovered, &key).is_err());
    assert_eq!(
        f.app.store(&id).unwrap().journal().events(&id).unwrap(),
        before
    );
    fs::write(f.root.join("portable-proof.json"), bytes(&result).unwrap()).unwrap();
    println!("PHASE10_NATIVE_EVIDENCE={}", f.root.display());
}
#[test]
fn wrong_keys_corruption_incomplete_future_versions_and_path_aliases_preserve_existing_data() {
    use melissa_local_store::StoreKey;
    let f = Fixture::new(false);
    let id = task(&f);
    let key = f.root.join("recovery.bin");
    portable::keygen(&key).unwrap();
    let archive = f.root.join("archive");
    f.app.backup_task(&id, &archive, &key).unwrap();
    let wrong = f.root.join("wrong.bin");
    portable::keygen(&wrong).unwrap();
    let failed = f.root.join("must-not-exist");
    assert!(portable::restore(&archive, &failed, &wrong).is_err());
    assert!(!failed.exists());
    let bad = f.root.join("corrupt");
    copied(&archive, &bad);
    let part = bad.join("part-000000.enc");
    let mut raw = fs::read(&part).unwrap();
    raw[100] ^= 1;
    fs::write(&part, &raw).unwrap();
    assert!(portable::restore(&bad, &failed, &key).is_err());
    assert!(!failed.exists());
    let incomplete = f.root.join("incomplete");
    fs::create_dir(&incomplete).unwrap();
    fs::write(incomplete.join("part-000000.enc"), b"interrupted").unwrap();
    assert!(portable::restore(&incomplete, &failed, &key).is_err());
    assert!(!failed.exists());
    let future = f.root.join("future");
    copied(&archive, &future);
    let recovery = StoreKey::from_bytes(fs::read(&key).unwrap().try_into().unwrap());
    let raw = fs::read(future.join("archive.enc")).unwrap();
    let mut value: Value = serde_json::from_slice(
        &recovery
            .open(&raw, b"melissa.portable-task-manifest/v1")
            .unwrap(),
    )
    .unwrap();
    value["cli_version"] = json!("99.0.0");
    fs::write(
        future.join("archive.enc"),
        recovery
            .seal(
                &bytes(&value).unwrap(),
                b"melissa.portable-task-manifest/v1",
            )
            .unwrap(),
    )
    .unwrap();
    assert!(portable::restore(&future, &failed, &key).is_err());
    assert!(!failed.exists());
    let alias = f.root.join("alias.bin");
    fs::hard_link(&key, &alias).unwrap();
    assert!(
        f.app
            .backup_task(&id, &f.root.join("bad-link-archive"), &alias)
            .is_err()
    );
    fs::remove_file(&alias).unwrap();
    assert!(
        f.app
            .backup_task(&id, &f.source.join("bad-archive"), &key)
            .is_err()
    );
    assert!(portable::restore(&archive, &archive.join("bad-restore"), &key).is_err());
    assert!(!failed.exists());
    assert!(!fs::read(archive.join("archive.enc")).unwrap().is_empty());
}
#[tokio::test]
async fn restored_executable_task_is_quarantined_before_a_credential_or_model_can_be_used() {
    let f = Fixture::new(false);
    let path = f.root.join("draft.json");
    let prepared = f
        .app
        .prepare(&f.source, "Fix main.py", &path, None)
        .unwrap();
    let value = f
        .app
        .create_project(
            &f.source,
            "Fix main.py",
            crate::conversation::Mode::Develop,
            prepared["preparation"]["adaptive_plan"].clone(),
            Contract {
                environment: Default::default(),
                checks: BTreeMap::from([(
                    "check".into(),
                    Command {
                        executable: "python".into(),
                        argv: vec!["-c".into(), "print('offline')".into()],
                        cwd: ".".into(),
                    },
                )]),
                sync: vec![],
                services: None,
                verification: None,
                extensions: None,
            },
            1_000_000,
            600_000,
        )
        .unwrap();
    let id = value["task_id"].as_str().unwrap();
    f.app
        .approve(
            id,
            value["approval"]["request_id"].as_str().unwrap(),
            value["approval"]["digest"].as_str().unwrap(),
        )
        .unwrap();
    let key = f.root.join("key.bin");
    portable::keygen(&key).unwrap();
    let out = f.root.join("archive");
    f.app.backup_task(id, &out, &key).unwrap();
    let restored = f.root.join("restored");
    portable::restore(&out, &restored, &key).unwrap();
    let app = App::open(&restored).unwrap();
    app.configure(&f.root.join("settings.json")).unwrap();
    assert!(matches!(
        app.run_task(id, false).await,
        Err(Error::Denied(_))
    ));
    assert!(
        app.store(id)
            .unwrap()
            .journal()
            .model_requests(id)
            .unwrap()
            .is_empty()
    );
}
#[test]
fn active_task_owner_blocks_backup_and_stream_encryption_authenticates_every_chunk() {
    let f = Fixture::new(false);
    let id = task(&f);
    let key = f.root.join("key.bin");
    portable::keygen(&key).unwrap();
    let control = crate::control::Control::new(
        &f.app,
        &id,
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicBool::new(false)),
    )
    .unwrap();
    control
        .publish("preparing", json!({}), json!({}), json!({}), json!({}))
        .unwrap();
    assert!(
        f.app
            .backup_task(&id, &f.root.join("blocked"), &key)
            .is_err()
    );
    assert!(!f.root.join("blocked").exists());
    control
        .publish("paused", json!({}), json!({}), json!({}), json!({}))
        .unwrap();
    let mut store = f.app.store(&id).unwrap();
    let bytes = vec![93; 2 * 1024 * 1024 + 101];
    crate::app::put(&mut store, &id, "multi-chunk", "fixture.chunks", &bytes).unwrap();
    drop(store);
    let out = f.root.join("archive");
    f.app.backup_task(&id, &out, &key).unwrap();
    let restored = f.root.join("restored");
    portable::restore(&out, &restored, &key).unwrap();
    let app = App::open(&restored).unwrap();
    assert_eq!(
        app.store(&id)
            .unwrap()
            .get_artifact(&id, "multi-chunk")
            .unwrap(),
        bytes
    );
}
