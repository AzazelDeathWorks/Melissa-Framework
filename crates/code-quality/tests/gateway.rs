#![cfg(windows)]
use melissa_code_quality::{
    profiles::ReviewProfiles,
    review::{Redactor, ReviewSession},
    source::FrozenSource,
    tools::{ReviewConfig, ReviewDispatcher},
};
use melissa_core_contracts::{TaskState, wire::TaskContractV1};
use melissa_local_store::{LocalStore, StoreConfig, StoreKey};
use melissa_model_gateway::{
    ToolCall,
    tools::{ToolContext, ToolDispatcher},
};
use melissa_repository::{Limits, Repository};
use serde_json::{Value, json};
use std::{
    fs,
    path::Path,
    sync::{Arc, atomic::AtomicBool},
};

struct Fixture {
    dispatcher: ReviewDispatcher,
    observer: LocalStore,
    _dir: tempfile::TempDir,
}
fn no() -> AtomicBool {
    AtomicBool::new(false)
}
fn fixture(role: &str, evidence: bool, maximum: u64) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("source");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("app.rs"), "let answer = 1;\n").unwrap();
    let r = Repository::open(&root, Limits::default(), vec![]).unwrap();
    let base = FrozenSource::capture(&r, &no()).unwrap();
    fs::write(root.join("app.rs"), "let answer = 2;\n").unwrap();
    let session = ReviewSession::start(r, base, Redactor::new(vec![]).unwrap(), &no()).unwrap();
    let mut t: TaskContractV1 = serde_json::from_slice(include_bytes!(
        "../../../tests/fixtures/code01/wire/task-contract-v1.json"
    ))
    .unwrap();
    t.task_id = "task:quality".into();
    t.workspace_ref = "workspace:quality".into();
    t.selected_profile_ids = vec![role.into()];
    t.allowed_resource_ids = vec![t.workspace_ref.clone()];
    t.authorization_refs = vec!["auth:quality".into()];
    t.allowed_effects = vec!["workspace.read".into()];
    if evidence {
        t.allowed_effects.push("evidence.append".into());
    }
    t.input_snapshot_digest = session.source().snapshot().digest.clone();
    t.limits.max_output_bytes = maximum;
    let mut store = LocalStore::open(
        &dir.path().join("store"),
        StoreKey::from_bytes([73; 32]),
        StoreConfig::default(),
    )
    .unwrap();
    store.journal_mut().create_task(&t).unwrap();
    for state in [
        TaskState::Planned,
        TaskState::Queued,
        TaskState::Leased,
        TaskState::Running,
    ] {
        let v = store.journal().task(&t.task_id).unwrap().record.version;
        store.transition_task(&t.task_id, v, state, None).unwrap();
    }
    let observer = store.connect().unwrap();
    let profiles = profiles();
    let config = ReviewConfig {
        task_id: t.task_id,
        profile_id: role.into(),
        run_id: "run-quality".into(),
        resource_id: t.workspace_ref,
        authorization_ref: "auth:quality".into(),
        expires_at_ms: melissa_execution_broker::unix_ms().unwrap() + 60000,
    };
    let dispatcher = ReviewDispatcher::new(session, store, profiles, config).unwrap();
    Fixture {
        dispatcher,
        observer,
        _dir: dir,
    }
}
fn call(name: &str, v: Value) -> ToolCall {
    ToolCall {
        id: "call-quality".into(),
        name: name.into(),
        arguments: v.to_string(),
    }
}
fn profiles() -> ReviewProfiles {
    let repo = Repository::open(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap(),
        Limits::default(),
        vec![],
    )
    .unwrap();
    ReviewProfiles::load_from_repository(&repo).unwrap()
}
fn ctx(role: &str) -> ToolContext {
    ToolContext {
        task_id: "task:quality".into(),
        profile_id: role.into(),
        run_id: "run-quality".into(),
        request_id: "request-quality".into(),
        invocation_id: "inv-quality".into(),
        attempt_id: "attempt-quality".into(),
        generation: 1,
    }
}
async fn scope(f: &mut Fixture, role: &str) -> Value {
    let result = f
        .dispatcher
        .execute(call("review_scope", json!({})), ctx(role), Arc::new(no()))
        .await
        .unwrap();
    assert!(result.success && !result.requires_reconciliation);
    serde_json::from_str::<Value>(&result.content).unwrap()["result"].clone()
}
#[tokio::test]
async fn three_existing_roles_use_only_their_existing_canonical_permissions() {
    for role in [
        "melissa.code.reviewer",
        "melissa.code.security-reviewer",
        "melissa.core.independent-qa",
    ] {
        let mut f = fixture(role, true, 65536);
        let defs = f.dispatcher.definitions();
        assert_eq!(defs.len(), 4);
        let profiles = profiles();
        assert!(defs.iter().all(|d| {
            profiles
                .profile(role)
                .unwrap()
                .tool_ids
                .contains(&d.canonical_id)
        }));
        assert!(!defs.iter().any(|d| {
            ["fs.patch", "process.exec", "artifact.write"].contains(&d.canonical_id.as_str())
        }));
        assert!(
            scope(&mut f, role).await["source_digest"]
                .as_str()
                .unwrap()
                .starts_with("blake3:")
        );
    }
}
#[tokio::test]
async fn actual_gateway_interface_persists_a_located_candidate_without_acceptance() {
    let role = "melissa.code.reviewer";
    let mut f = fixture(role, true, 65536);
    let source = scope(&mut f, role).await;
    let result = f
        .dispatcher
        .execute(
            call(
                "review_anchor",
                json!({"side":"after","path":"app.rs","start":0,"end":3}),
            ),
            ctx(role),
            Arc::new(no()),
        )
        .await
        .unwrap();
    let anchor = serde_json::from_str::<Value>(&result.content).unwrap()["result"].clone();
    let input = json!({"source_digest":source["source_digest"],"baseline_digest":source["baseline_digest"],
        "findings":[{"id":"review-1","severity":"medium","title":"Candidate only","trigger":"Demonstrate separately","impact":"Needs independent behavioral confirmation","proposed_verification":"Authorized QA reproduction","anchor":anchor}],
        "limitations":["This fixture tests location binding, not the truth of this example finding"]});
    let result = f
        .dispatcher
        .execute(call("review_submit", input), ctx(role), Arc::new(no()))
        .await
        .unwrap();
    assert!(result.success);
    assert_eq!(result.artifact_refs.len(), 1);
    let persisted = f
        .observer
        .get_artifact("task:quality", &result.artifact_refs[0])
        .unwrap();
    assert_eq!(persisted, result.content.as_bytes());
    let value: Value = serde_json::from_slice(&persisted).unwrap();
    assert_eq!(value["task_accepted"], false);
    assert_eq!(value["result"]["protected_acceptance"], false);
    assert_eq!(
        f.observer
            .artifact_metadata("task:quality", &result.artifact_refs[0])
            .unwrap()
            .logical_type,
        "quality.located_candidate"
    );
}
#[tokio::test]
async fn wrong_task_role_run_or_generation_cannot_read_or_append() {
    let role = "melissa.code.reviewer";
    let mut f = fixture(role, true, 65536);
    for which in 0..4 {
        let mut context = ctx(role);
        match which {
            0 => context.task_id = "task:foreign".into(),
            1 => context.profile_id = "melissa.code.implementer".into(),
            2 => context.run_id = "run-foreign".into(),
            _ => context.generation = 0,
        }
        assert!(
            f.dispatcher
                .execute(call("review_scope", json!({})), context, Arc::new(no()))
                .await
                .is_err()
        );
    }
}
#[test]
fn write_process_and_unknown_argument_routes_are_refused_before_execution() {
    let f = fixture("melissa.code.reviewer", true, 65536);
    for name in [
        "fs_patch",
        "process_exec",
        "review_approve",
        "review_publish",
    ] {
        assert!(f.dispatcher.validate_call(&call(name, json!({}))).is_err());
    }
    assert!(
        f.dispatcher
            .validate_call(&call("review_scope", json!({"task_id":"task:foreign"})))
            .is_err()
    );
    assert!(
        f.dispatcher
            .validate_call(&ToolCall {
                id: "duplicate".into(),
                name: "review_scope".into(),
                arguments: "{\"x\":1,\"x\":2}".into()
            })
            .is_err()
    );
}
#[tokio::test]
async fn evidence_append_is_not_granted_by_read_authority() {
    let role = "melissa.code.reviewer";
    let mut f = fixture(role, false, 65536);
    let s = scope(&mut f, role).await;
    assert_eq!(f.dispatcher.definitions().len(), 3);
    let input = json!({"source_digest":s["source_digest"],"baseline_digest":s["baseline_digest"],"findings":[],"limitations":["No semantic proof"]});
    assert!(
        f.dispatcher
            .execute(call("review_submit", input), ctx(role), Arc::new(no()))
            .await
            .is_err()
    );
}
#[tokio::test]
async fn host_revocation_and_stopped_task_withdraw_all_routes() {
    let role = "melissa.code.reviewer";
    let mut f = fixture(role, true, 65536);
    f.dispatcher.revoke().unwrap();
    assert!(f.dispatcher.definitions().is_empty());
    assert!(
        f.dispatcher
            .execute(call("review_scope", json!({})), ctx(role), Arc::new(no()))
            .await
            .is_err()
    );
    let mut f = fixture(role, true, 65536);
    let v = f
        .observer
        .journal()
        .task("task:quality")
        .unwrap()
        .record
        .version;
    f.observer
        .transition_task("task:quality", v, TaskState::Failed, None)
        .unwrap();
    assert!(f.dispatcher.definitions().is_empty());
    assert!(
        f.dispatcher
            .execute(call("review_scope", json!({})), ctx(role), Arc::new(no()))
            .await
            .is_err()
    );
}
#[tokio::test]
async fn cancelled_operation_and_output_overflow_do_not_append_evidence() {
    let role = "melissa.code.reviewer";
    let mut f = fixture(role, true, 65536);
    assert!(
        f.dispatcher
            .execute(
                call("review_scope", json!({})),
                ctx(role),
                Arc::new(AtomicBool::new(true))
            )
            .await
            .is_err()
    );
    let mut f = fixture(role, true, 128);
    assert!(
        f.dispatcher
            .execute(call("review_scope", json!({})), ctx(role), Arc::new(no()))
            .await
            .is_err()
    );
    assert!(
        f.observer
            .artifact_metadata("task:quality", "quality:inv-quality:located-candidate")
            .is_err()
    );
}
#[tokio::test]
async fn source_change_after_task_preparation_is_refused() {
    let role = "melissa.code.reviewer";
    let mut f = fixture(role, true, 65536);
    fs::write(
        f._dir.path().join("source/app.rs"),
        "changed after preparation",
    )
    .unwrap();
    assert!(
        f.dispatcher
            .execute(call("review_scope", json!({})), ctx(role), Arc::new(no()))
            .await
            .is_err()
    );
}
