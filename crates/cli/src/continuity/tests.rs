use super::*;
use crate::{
    continuity::undo::{Pick, Request},
    project::{Command, Contract, tests::Fixture},
};
use melissa_repository::{
    ByteEdit, FileMutation, FilePatch, MutationProposal, PatchBroker, PatchPolicy, PatchProposal,
    Repository,
};
use std::fs;
fn contract() -> Contract {
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
    }
}
fn create(f: &Fixture, paths: &[&str]) -> (String, LocalStore) {
    let goal = format!("{} dosyalarını düzelt", paths.join(" "));
    let draft = f.root.join(format!("draft-{}.json", nonce().unwrap()));
    let prepared = f.app.prepare(&f.source, &goal, &draft, None).unwrap();
    let value = f
        .app
        .create_project(
            &f.source,
            &goal,
            crate::conversation::Mode::Develop,
            prepared["preparation"]["adaptive_plan"].clone(),
            contract(),
            5_000_000,
            3_600_000,
        )
        .unwrap();
    let (value, store) = f.running_value(value);
    (value["task_id"].as_str().unwrap().into(), store)
}
fn repo(f: &Fixture) -> Repository {
    Repository::open(&f.source, Default::default(), vec![]).unwrap()
}
fn policy(store: &LocalStore, task: &str, paths: &[&str]) -> PatchPolicy {
    let t = store.journal().task(task).unwrap();
    PatchPolicy {
        task_id: task.into(),
        principal_id: "cli:operator".into(),
        profile_id: "melissa.code.implementer".into(),
        resource_id: t.contract.workspace_ref,
        authorization_ref: t.contract.authorization_refs[0].clone(),
        expires_at_ms: now().unwrap() + 600000,
        epoch: 1,
        writable_paths: paths.iter().map(|s| s.to_string()).collect(),
        protected_paths: BTreeSet::new(),
    }
}
fn forward_patch(
    f: &Fixture,
    store: &mut LocalStore,
    task: &str,
    edits: Vec<ByteEdit>,
    turn: &str,
) -> String {
    let r = repo(f);
    let id = format!("forward:{}", nonce().unwrap());
    let proposal = PatchProposal {
        schema_version: "melissa.repository-patch/v1".into(),
        root_identity: r.root_identity().into(),
        files: vec![FilePatch {
            before: r.read("main.py").unwrap().0,
            edits,
        }],
    };
    let p = policy(store, task, &["main.py"]);
    let mut broker = PatchBroker::new(&r, store, p).unwrap();
    let permit = broker
        .authorize(&id, &format!("attempt:{id}"), proposal)
        .unwrap();
    broker.execute(permit, &AtomicBool::new(false)).unwrap();
    store
        .put_artifact(
            &melissa_local_store::ArtifactRequest {
                artifact_id: format!("{id}:tool-binding"),
                task_id: task.into(),
                content_type: "application/json".into(),
                logical_type: "repository_tool_binding".into(),
                schema_version: "melissa.repository-tool-evidence/v1".into(),
                retain_until_ms: 0,
                legal_hold: true,
            },
            &bytes(&json!({"task":task,"invocation":id,"request":turn,"run":"owned-fixture"}))
                .unwrap(),
        )
        .unwrap();
    format!("{id}:receipt")
}
fn pause(f: &Fixture, store: &LocalStore, task: &str) {
    paused_snapshot(
        &f.app,
        store,
        task,
        &repo(f).snapshot(&AtomicBool::new(false)).unwrap(),
    )
    .unwrap();
}
fn selection(receipt: &str, indices: &[usize]) -> Request {
    Request {
        turn_request: None,
        receipts: vec![Pick {
            receipt_id: receipt.into(),
            hunks: BTreeMap::from([("main.py".into(), indices.iter().copied().collect())]),
        }],
    }
}
#[test]
fn selected_native_hunks_undo_in_stages_keep_other_hunks_and_preserve_parent_accounting() {
    let f = Fixture::new(false);
    fs::write(f.source.join("main.py"), b"alpha\nmiddle\nomega\n").unwrap();
    let (task, mut store) = create(&f, &["main.py"]);
    let before_budget = store.journal().model_budget(&task).unwrap();
    let receipt = forward_patch(
        &f,
        &mut store,
        &task,
        vec![
            ByteEdit {
                start: 0,
                end: 5,
                expected: b"alpha".to_vec(),
                replacement: b"first-longer".to_vec(),
            },
            ByteEdit {
                start: 13,
                end: 18,
                expected: b"omega".to_vec(),
                replacement: b"OMEGA".to_vec(),
            },
        ],
        "turn-1",
    );
    pause(&f, &store, &task);
    drop(store);
    let request = selection(&receipt, &[0]);
    let preview = f.app.undo_preview(&task, request.clone()).unwrap();
    assert!(f.app.undo(&task, request.clone(), "wrong").is_err());
    let result = f
        .app
        .undo(
            &task,
            request.clone(),
            preview["review_digest"].as_str().unwrap(),
        )
        .unwrap();
    assert_eq!(result["outcome"], "PASS");
    assert_eq!(
        fs::read(f.source.join("main.py")).unwrap(),
        b"alpha\nmiddle\nOMEGA\n"
    );
    let reused = f
        .app
        .undo(&task, request, preview["review_digest"].as_str().unwrap())
        .unwrap();
    assert_eq!(reused["reused"], true);
    let request = selection(&receipt, &[1]);
    let preview = f.app.undo_preview(&task, request.clone()).unwrap();
    let result = f
        .app
        .undo(&task, request, preview["review_digest"].as_str().unwrap())
        .unwrap();
    assert_eq!(result["outcome"], "PASS");
    assert_eq!(
        fs::read(f.source.join("main.py")).unwrap(),
        b"alpha\nmiddle\nomega\n"
    );
    let store = f.app.store(&task).unwrap();
    assert_eq!(store.journal().model_budget(&task).unwrap(), before_budget);
    assert_eq!(
        store.journal().task(&task).unwrap().record.state,
        melissa_core_contracts::TaskState::Running
    );
    assert!(store.artifact_metadata(&task, &receipt).is_ok());
    assert!(store.journal().model_requests(&task).unwrap().is_empty());
}
#[test]
fn user_edits_stale_reviews_and_foreign_or_unknown_receipts_never_overwrite_files() {
    let f = Fixture::new(false);
    fs::write(f.source.join("main.py"), "old\n").unwrap();
    let (task, mut store) = create(&f, &["main.py"]);
    let receipt = forward_patch(
        &f,
        &mut store,
        &task,
        vec![ByteEdit {
            start: 0,
            end: 3,
            expected: b"old".to_vec(),
            replacement: b"new".to_vec(),
        }],
        "turn",
    );
    pause(&f, &store, &task);
    drop(store);
    let request = selection(&receipt, &[0]);
    let preview = f.app.undo_preview(&task, request.clone()).unwrap();
    fs::write(f.source.join("main.py"), "new\nuser-owned line\n").unwrap();
    assert!(
        f.app
            .undo(
                &task,
                request.clone(),
                preview["review_digest"].as_str().unwrap()
            )
            .is_err()
    );
    assert_eq!(
        fs::read_to_string(f.source.join("main.py")).unwrap(),
        "new\nuser-owned line\n"
    );
    assert!(
        f.app
            .undo_preview(&task, selection("foreign:receipt", &[0]))
            .is_err()
    );
    let mut store = f.app.store(&task).unwrap();
    put(
        &mut store,
        &task,
        "cli:undo:unfinished:intent",
        "continuity.undo-intent",
        b"{}",
    )
    .unwrap();
    drop(store);
    assert!(f.app.checkpoint(&task, "must block").is_err());
}
#[test]
fn whole_turn_reverses_multiple_native_patches_in_reverse_order() {
    let f = Fixture::new(false);
    fs::write(f.source.join("main.py"), "one\n").unwrap();
    let (task, mut store) = create(&f, &["main.py"]);
    forward_patch(
        &f,
        &mut store,
        &task,
        vec![ByteEdit {
            start: 0,
            end: 3,
            expected: b"one".to_vec(),
            replacement: b"two".to_vec(),
        }],
        "turn-batch",
    );
    forward_patch(
        &f,
        &mut store,
        &task,
        vec![ByteEdit {
            start: 0,
            end: 3,
            expected: b"two".to_vec(),
            replacement: b"three".to_vec(),
        }],
        "turn-batch",
    );
    pause(&f, &store, &task);
    drop(store);
    let request = Request {
        turn_request: Some("turn-batch".into()),
        receipts: vec![],
    };
    let p = f.app.undo_preview(&task, request.clone()).unwrap();
    let result = f
        .app
        .undo(&task, request, p["review_digest"].as_str().unwrap())
        .unwrap();
    assert_eq!(result["outcome"], "PASS");
    assert_eq!(
        fs::read_to_string(f.source.join("main.py")).unwrap(),
        "one\n"
    );
    assert_eq!(
        undo::changes(&f.app, &task).unwrap()["changes"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}
#[test]
fn file_namespace_undo_uses_native_ownership_and_does_not_touch_reused_paths() {
    let f = Fixture::new(false);
    fs::write(f.source.join("a.py"), "original\n").unwrap();
    fs::write(f.source.join("b.py"), "placeholder\n").unwrap();
    let (task, mut store) = create(&f, &["main.py", "a.py", "b.py"]);
    fs::remove_file(f.source.join("b.py")).unwrap();
    let r = repo(&f);
    let id = format!("move:{}", nonce().unwrap());
    let p = policy(&store, &task, &["a.py", "b.py"]);
    let proposal = MutationProposal {
        schema_version: "melissa.repository-mutation/v1".into(),
        root_identity: r.root_identity().into(),
        mutation: FileMutation::Move {
            before: r.read("a.py").unwrap().0,
            destination: r.absent_path("b.py").unwrap(),
        },
    };
    let mut broker = PatchBroker::new(&r, &mut store, p).unwrap();
    let permit = broker
        .authorize_mutation(&id, &format!("attempt:{id}"), proposal)
        .unwrap();
    broker
        .execute_mutation(permit, &AtomicBool::new(false))
        .unwrap();
    pause(&f, &store, &task);
    drop(store);
    let request = Request {
        turn_request: None,
        receipts: vec![Pick {
            receipt_id: format!("{id}:receipt"),
            hunks: BTreeMap::new(),
        }],
    };
    fs::write(f.source.join("a.py"), "user reused old path\n").unwrap();
    assert!(f.app.undo_preview(&task, request.clone()).is_err());
    assert_eq!(
        fs::read_to_string(f.source.join("a.py")).unwrap(),
        "user reused old path\n"
    );
    fs::remove_file(f.source.join("a.py")).unwrap();
    let p = f.app.undo_preview(&task, request.clone()).unwrap();
    let result = f
        .app
        .undo(&task, request, p["review_digest"].as_str().unwrap())
        .unwrap();
    assert_eq!(result["outcome"], "PASS");
    assert_eq!(
        fs::read_to_string(f.source.join("a.py")).unwrap(),
        "original\n"
    );
    assert!(!f.source.join("b.py").exists());
}
#[test]
fn checkpoint_reopens_and_explicit_source_review_fences_old_leases_without_resetting_budget() {
    let f = Fixture::new(false);
    let (task, store) = create(&f, &["main.py"]);
    let t = store.journal().task(&task).unwrap();
    let e = f.app.execution(&store, &task).unwrap().unwrap();
    let plan = crate::engine::plan(&t.contract, &e).unwrap();
    let source = t.contract.input_snapshot_digest.clone();
    let settings = e.settings_digest.clone();
    let mut scheduler = melissa_workflow::scheduler::Scheduler::create_until(
        store.connect().unwrap(),
        plan,
        &crate::resources::catalog().unwrap(),
        Arc::new(move || {
            Ok(melissa_workflow::scheduler::Freshness {
                workspace_digest: source.clone(),
                policy_digest: settings.clone(),
            })
        }),
        Arc::new(AtomicBool::new(false)),
        e.expires_at_ms,
    )
    .unwrap();
    let node = scheduler.state().unwrap().plan.nodes[0].id.clone();
    let packet = scheduler.acquire(&node, "owned-fixture", 600000).unwrap();
    pause(&f, &store, &task);
    let budget = store.journal().model_budget(&task).unwrap();
    drop(scheduler);
    drop(store);
    let cp = f.app.checkpoint(&task, "before operator edit").unwrap();
    assert!(
        f.app.continuity(&task).unwrap()["checkpoints"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["id"] == cp["checkpoint"])
    );
    fs::write(
        f.source.join("main.py"),
        "print('operator edit retained')\n",
    )
    .unwrap();
    let live_before = f.app.live(&task).unwrap();
    f.app
        .checkpoint(&task, "inspect changed source without granting resume")
        .unwrap();
    let live_after = f.app.live(&task).unwrap();
    assert_eq!(live_before.session, live_after.session);
    let store = f.app.store(&task).unwrap();
    let paused: RepositorySnapshot = decode(
        &store
            .get_artifact(&task, &format!("cli:paused-source:{}", live_after.session))
            .unwrap(),
    )
    .unwrap();
    assert_ne!(
        paused.digest,
        repo(&f).snapshot(&AtomicBool::new(false)).unwrap().digest
    );
    drop(store);
    let p = f.app.adopt_preview(&task).unwrap();
    assert!(f.app.adopt_source(&task, "wrong").is_err());
    let adopted = f
        .app
        .adopt_source(&task, p["review_digest"].as_str().unwrap())
        .unwrap();
    assert_eq!(adopted["fresh_specialist_runs_required"], true);
    let store = f.app.store(&task).unwrap();
    let mut s = super::scheduler(&f.app, &store, &task).unwrap();
    assert!(s.check(&packet.lease).is_err());
    let state = s.state().unwrap();
    assert!(
        state
            .slots
            .values()
            .all(|v| v.phase == melissa_workflow::scheduler::Phase::Pending)
    );
    assert_eq!(store.journal().model_budget(&task).unwrap(), budget);
    let next = s.acquire(&node, "fresh-fixture", 600000).unwrap();
    assert_ne!(next.lease.run, packet.lease.run);
    assert!(next.lease.generation > packet.lease.generation);
    assert_eq!(
        fs::read_to_string(f.source.join("main.py")).unwrap(),
        "print('operator edit retained')\n"
    );
    assert!(store.journal().model_requests(&task).unwrap().is_empty());
}

#[test]
fn typed_memory_tool_captures_host_provenance_and_rechecks_branch_without_auto_acceptance() {
    use melissa_model_gateway::{
        ToolCall,
        tools::{ToolContext, ToolDispatcher},
    };
    let f = Fixture::new(false);
    fs::create_dir_all(f.source.join(".git/refs/heads")).unwrap();
    fs::write(f.source.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
    fs::write(
        f.source.join(".git/refs/heads/main"),
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n",
    )
    .unwrap();
    let (task, store) = create(&f, &["main.py"]);
    let parent = store.journal().task(&task).unwrap().contract;
    let mut context = crate::context_bridge::runtime(
        &f.app,
        &store,
        &parent,
        "melissa.code.implementer",
        now().unwrap() + 600000,
        &BTreeSet::from(["memory.propose".into(), "memory.search".into()]),
        false,
    )
    .unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    for (kind, key) in [
        ("project_decision", "decision"),
        ("user_preference", "preference"),
        ("failure_experience", "experience"),
        ("transient_state", "transient"),
    ] {
        // Identity-only fixture; explicitly NEVER_DISPATCHED, no provider or API.
        {
            use melissa_local_store::journal::{
                ModelRequestState, ModelReservation, ModelSettlement, ModelUsage,
            };
            let mut s = store.connect().unwrap();
            let time = now().unwrap();
            let version = s.journal().task(&task).unwrap().record.version;
            s.journal_mut()
                .reserve_model_request(
                    &ModelReservation {
                        schema_version: "melissa.model-reservation/v1".into(),
                        request_id: format!("request-{key}"),
                        task_id: task.clone(),
                        profile_id: "melissa.code.implementer".into(),
                        provider_id: "offline-fixture".into(),
                        model_id: "NOT_RUN".into(),
                        request_digest: digest(b"offline identity fixture"),
                        pricing_digest: Some(digest(b"no transport or cost")),
                        reserved_input_tokens: 1,
                        reserved_output_tokens: 1,
                        reserved_cost_micro_usd: 0,
                        created_at_unix_ms: time,
                    },
                    version,
                )
                .unwrap();
            s.journal_mut()
                .finish_model_request(
                    &format!("request-{key}"),
                    0,
                    &ModelSettlement {
                        state: ModelRequestState::NeverDispatched,
                        usage: ModelUsage {
                            input_tokens: Some(0),
                            output_tokens: Some(0),
                            cached_input_tokens: Some(0),
                            cost_micro_usd: Some(0),
                        },
                        receipt_digest: Some(digest(b"never dispatched")),
                        reason: Some("Offline tool binding only; no transport or inference".into()),
                        completed_at_unix_ms: time,
                    },
                )
                .unwrap();
        }
        let invocation = format!("memory-{key}");
        let call=ToolCall{id:invocation.clone(),name:"memory_propose".into(),arguments:json!({"operation":"memory_propose","key":key,"claim":format!("Synthetic {key} statement pending operator review"),"kind":kind,"expires_after_ms":600000,"source_ids":["main.py"]}).to_string()};
        let result = runtime
            .block_on(context.execute(
                call,
                ToolContext {
                    task_id: task.clone(),
                    profile_id: "melissa.code.implementer".into(),
                    run_id: "memory-run".into(),
                    request_id: format!("request-{key}"),
                    invocation_id: invocation.clone(),
                    attempt_id: format!("attempt-{key}"),
                    generation: 1,
                },
                Arc::new(AtomicBool::new(false)),
            ))
            .unwrap();
        assert!(result.success, "{}", result.content);
        let value: Value = serde_json::from_str(&result.content).unwrap();
        assert_eq!(value["observation"]["data"]["metadata"]["kind"], kind);
        assert_eq!(
            value["observation"]["data"]["metadata"]["git"]["branch"],
            "refs/heads/main"
        );
    }
    assert!(context.validate_call(&ToolCall{id:"invalid".into(),name:"memory_propose".into(),arguments:json!({"operation":"memory_propose","key":"temporary","claim":"no expiry","kind":"transient_state","source_ids":["main.py"]}).to_string()}).is_err());
    drop(context);
    drop(store);
    let list = f.app.memory(&f.source, "list", None, None).unwrap();
    assert_eq!(list["records"].as_array().unwrap().len(), 4);
    assert!(
        list["records"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["status"] == "proposed")
    );
    for key in ["decision", "preference", "experience", "transient"] {
        let list = f.app.memory(&f.source, "list", None, None).unwrap();
        f.app
            .memory(
                &f.source,
                "accept",
                Some(&format!("memory:memory-{key}")),
                list["review_digest"].as_str(),
            )
            .unwrap();
    }
    let repo = repo(&f);
    let book =
        crate::project_memory::ProjectMemory::open(&f.app.home, f.app.key.clone(), &repo).unwrap();
    let sources = crate::project_memory::sources(&repo).unwrap();
    let scope = melissa_skill_context::retrieval::SearchScope {
        namespace: crate::project_memory::namespace(&repo),
        domain: melissa_skill_context::retrieval::Domain::Code,
        now_ms: now().unwrap(),
        current: sources
            .iter()
            .map(|s| (s.id.clone(), (s.version.clone(), s.digest.clone())))
            .collect(),
    };
    let git = melissa_repository::git_context::observe(&repo);
    assert_eq!(
        book.book
            .search_context(
                &scope,
                "Synthetic",
                &task,
                &git,
                now().unwrap(),
                repo.source_visibility()
            )
            .len(),
        4
    );
    assert_eq!(
        book.book
            .search_context(
                &scope,
                "Synthetic",
                "next-task",
                &git,
                now().unwrap(),
                repo.source_visibility()
            )
            .len(),
        3
    );
    fs::write(f.source.join(".git/HEAD"), "ref: refs/heads/other\n").unwrap();
    let git = melissa_repository::git_context::observe(&repo);
    let matches = book.book.search_context(
        &scope,
        "Synthetic",
        &task,
        &git,
        now().unwrap(),
        repo.source_visibility(),
    );
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].key, "preference");
    let expired = now().unwrap() + 700000;
    assert!(
        book.book
            .search_context(
                &scope,
                "Synthetic",
                &task,
                &git,
                expired,
                repo.source_visibility()
            )
            .is_empty()
    );
    drop(book);
    fs::write(f.source.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
    fs::write(
        f.source.join("main.py"),
        "print('operator changed source')\n",
    )
    .unwrap();
    let book =
        crate::project_memory::ProjectMemory::open(&f.app.home, f.app.key.clone(), &repo).unwrap();
    assert_eq!(
        book.book.records()["memory:memory-decision"].status,
        melissa_skill_context::state::MemoryStatus::Stale
    );
    let git = melissa_repository::git_context::observe(&repo);
    let visible = book.book.search_context(
        &scope,
        "Synthetic",
        &task,
        &git,
        now().unwrap(),
        repo.source_visibility(),
    );
    assert_eq!(visible.len(), 1);
    assert_eq!(visible[0].key, "preference");
    let excluded =
        melissa_repository::SourceVisibilityPolicy::source(vec!["main.py".into()], vec![]).unwrap();
    assert!(
        book.book
            .search_context(&scope, "Synthetic", &task, &git, now().unwrap(), &excluded)
            .is_empty()
    );
}

#[test]
fn provenance_does_not_follow_external_git_files_and_legacy_fields_stay_unknown() {
    let f = Fixture::new(false);
    assert_eq!(
        melissa_repository::git_context::observe(&repo(&f)).status,
        "not_git"
    );
    fs::write(f.source.join(".git"), "gitdir: C:/unrelated-private-data\n").unwrap();
    let git = melissa_repository::git_context::observe(&repo(&f));
    assert_eq!(git.status, "unavailable");
    assert!(git.scope_key().is_none());
    let mut legacy = melissa_skill_context::state::MemoryBook::default();
    let current = crate::project_memory::sources(&repo(&f)).unwrap();
    legacy
        .propose(
            "legacy",
            crate::project_memory::namespace(&repo(&f)),
            "legacy",
            "Old record with unknown provenance",
            current,
        )
        .unwrap();
    let raw = bytes(&legacy).unwrap();
    let wire: Value = decode(&raw).unwrap();
    assert!(wire["records"]["legacy"].get("metadata").is_none());
    let restored: melissa_skill_context::state::MemoryBook = decode(&raw).unwrap();
    assert!(restored.records()["legacy"].metadata.is_none());
    let memory = melissa_skill_context::state::MemoryMetadata {
        kind: melissa_skill_context::state::MemoryKind::TransientState,
        created_at_ms: 100,
        task_id: "original".into(),
        git: melissa_repository::git_context::GitContext {
            status: "not_git".into(),
            branch: None,
            head: None,
        },
        expires_at_ms: Some(200),
    };
    assert_eq!(memory.withheld("original", &memory.git, 199), None);
    assert_eq!(
        memory.withheld("original", &memory.git, 200),
        Some("expired")
    );
    assert_eq!(
        memory.withheld("other", &memory.git, 199),
        Some("different_task")
    );
}

#[test]
fn exact_approach_tracking_separates_changed_commands_and_preserves_evidence() {
    let f = Fixture::new(false);
    let (task, store) = create(&f, &["main.py"]);
    let t = store.journal().task(&task).unwrap();
    let e = f.app.execution(&store, &task).unwrap().unwrap();
    let plan = crate::engine::plan(&t.contract, &e).unwrap();
    let source = t.contract.input_snapshot_digest.clone();
    let policy = e.settings_digest.clone();
    let mut s = melissa_workflow::scheduler::Scheduler::create_until(
        store.connect().unwrap(),
        plan,
        &crate::resources::catalog().unwrap(),
        Arc::new(move || {
            Ok(melissa_workflow::scheduler::Freshness {
                workspace_digest: source.clone(),
                policy_digest: policy.clone(),
            })
        }),
        Arc::new(AtomicBool::new(false)),
        e.expires_at_ms,
    )
    .unwrap();
    let node = s.state().unwrap().plan.nodes[0].id.clone();
    let packet = s.acquire(&node, "native-fixture", 600000).unwrap();
    let input_a = digest(b"command a");
    let input_b = digest(b"command b");
    let source = repo(&f).snapshot(&AtomicBool::new(false)).unwrap().digest;
    for input in [&input_a, &input_b, &input_a] {
        s.observe_check_input(
            &packet.lease,
            "code_tests",
            &source,
            false,
            "synthetic assertion failed",
            Some(input),
        )
        .unwrap();
    }
    let state = s.state().unwrap();
    let ledger = state.adaptive.as_ref().unwrap();
    assert_eq!(ledger.failed_approaches.len(), 2);
    assert!(
        ledger
            .failed_approaches
            .values()
            .any(|a| a.input_digest == input_a && a.count == 2)
    );
    assert!(!melissa_workflow::adaptive::repeated_approach(ledger, 3));
    s.observe_check_input(
        &packet.lease,
        "code_tests",
        &source,
        false,
        "synthetic assertion failed",
        Some(&input_a),
    )
    .unwrap();
    let state = s.state().unwrap();
    assert!(melissa_workflow::adaptive::repeated_approach(
        state.adaptive.as_ref().unwrap(),
        3
    ));
    s.observe_check_input(
        &packet.lease,
        "code_tests",
        &source,
        true,
        "actual local check passed",
        Some(&input_a),
    )
    .unwrap();
    let state = s.state().unwrap();
    assert!(!melissa_workflow::adaptive::repeated_approach(
        state.adaptive.as_ref().unwrap(),
        3
    ));
    let h = state
        .adaptive
        .as_ref()
        .unwrap()
        .handoff(&state, &packet.node)
        .unwrap();
    assert_eq!(h.failed_approaches.len(), 2);
    for a in h.failed_approaches.values() {
        assert!(store.artifact_metadata(&task, &a.last_evidence_ref).is_ok());
    }
}

#[test]
fn native_empty_directory_inverse_preserves_user_files_and_reopens_evidence() {
    let f = Fixture::new(true);
    let draft = f.root.join("directory-draft.json");
    let prepared = f
        .app
        .prepare(&f.source, "main.py düzelt", &draft, None)
        .unwrap();
    // Expand the fixture recipe before task creation and operator approval.
    let base = f
        .app
        .create_project(
            &f.source,
            "main.py düzelt",
            crate::conversation::Mode::Develop,
            prepared["preparation"]["adaptive_plan"].clone(),
            contract(),
            5_000_000,
            3_600_000,
        )
        .unwrap();
    let base_id = base["task_id"].as_str().unwrap();
    let base_store = f.app.store(base_id).unwrap();
    let mut recipe = f
        .app
        .execution(&base_store, base_id)
        .unwrap()
        .unwrap()
        .recipe;
    recipe.writable_paths.insert("newdir".into());
    drop(base_store);
    let value = f
        .app
        .create_execution(&f.source, "Create an empty directory", vec![], recipe)
        .unwrap();
    let (value, mut store) = f.running_value(value);
    let task = value["task_id"].as_str().unwrap();
    let r = repo(&f);
    let id = format!("directory:{}", nonce().unwrap());
    let proposal = MutationProposal {
        schema_version: "melissa.repository-mutation/v2".into(),
        root_identity: r.root_identity().into(),
        mutation: FileMutation::CreateDirectory {
            destination: r.absent_path("newdir").unwrap(),
        },
    };
    let p = policy(&store, task, &["newdir"]);
    let mut broker = PatchBroker::new(&r, &mut store, p).unwrap();
    let permit = broker
        .authorize_mutation(&id, &format!("attempt:{id}"), proposal)
        .unwrap();
    broker
        .execute_mutation(permit, &AtomicBool::new(false))
        .unwrap();
    pause(&f, &store, task);
    drop(store);
    let selection = Request {
        turn_request: None,
        receipts: vec![Pick {
            receipt_id: format!("{id}:receipt"),
            hunks: BTreeMap::new(),
        }],
    };
    fs::write(f.source.join("newdir/user.txt"), "operator file").unwrap();
    assert!(f.app.undo_preview(task, selection.clone()).is_err());
    assert_eq!(
        fs::read_to_string(f.source.join("newdir/user.txt")).unwrap(),
        "operator file"
    );
    // Remove only the known fixture file after proving it blocked undo.
    fs::remove_file(f.source.join("newdir/user.txt")).unwrap();
    let p = f.app.undo_preview(task, selection.clone()).unwrap();
    let result = f
        .app
        .undo(task, selection, p["review_digest"].as_str().unwrap())
        .unwrap();
    assert_eq!(result["outcome"], "PASS");
    assert!(!f.source.join("newdir").exists());
    let reopened = crate::app::App::open(&f.app.home).unwrap();
    assert!(reopened.continuity(task).is_ok());
    assert!(
        reopened
            .store(task)
            .unwrap()
            .journal()
            .model_requests(task)
            .unwrap()
            .is_empty()
    );
    fs::write(
        f.root.join("continuity-proof.json"),
        bytes(&result).unwrap(),
    )
    .unwrap();
    println!("PHASE09_NATIVE_EVIDENCE={}", f.root.display());
}
