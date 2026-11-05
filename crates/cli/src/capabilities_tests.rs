//! No provider transport, credentials or paid inference in this suite.
use crate::{
    app::{App, WORKSPACE, Workspace, bytes, now, put},
    recipe::Recipe,
    resources,
};
use melissa_core_contracts::{TaskState, wire::TaskContractV1};
use melissa_local_store::LocalStore;
use melissa_model_gateway::{
    ToolCall,
    tools::{ToolContext, ToolDispatcher, ToolExecution},
};
use melissa_repository::{Limits, Repository};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::PathBuf,
    sync::{Arc, atomic::AtomicBool},
};

struct Fixture {
    _temp: tempfile::TempDir,
    app: App,
    source: PathBuf,
    store: LocalStore,
    task: TaskContractV1,
}

#[tokio::test]
async fn context_index_survives_a_new_cli_task_and_respects_its_exclusions() {
    let profile = "melissa.code.implementer";
    let mut fixture = Fixture::new(profile);
    let query = json!({"operation":"search","query":"double","limit":10,"symbols":[]});
    {
        let mut bridge = fixture.context();
        let result = fixture
            .call(&mut bridge, "context_search", query.clone(), "first-index")
            .await;
        assert!(result.success, "{}", result.content);
        assert_eq!(
            bridge.inner.host().lock().unwrap().telemetry()["index.persisted"],
            1
        );
    }
    let (store, task) = Fixture::task(&fixture.app, &fixture.source, profile, "b", vec![]);
    fixture.store = store;
    fixture.task = task;
    {
        let mut bridge = fixture.context();
        let result = fixture
            .call(&mut bridge, "context_search", query.clone(), "shared-index")
            .await;
        assert!(result.success, "{}", result.content);
        let v: Value = serde_json::from_str(&result.content).unwrap();
        assert_eq!(v["observation"]["data"][0]["source"]["id"], "main.py");
        let host = bridge.inner.host();
        let host = host.lock().unwrap();
        assert_eq!(host.telemetry()["index.restored"], 1);
        assert_eq!(host.telemetry()["index.documents_updated"], 0);
        assert_eq!(host.telemetry()["index.files_parsed"], 0);
    }
    let (store, task) = Fixture::task(
        &fixture.app,
        &fixture.source,
        profile,
        "c",
        vec!["main.py".into()],
    );
    fixture.store = store;
    fixture.task = task;
    let mut bridge = fixture.context();
    let result = fixture
        .call(&mut bridge, "context_search", query, "restricted-index")
        .await;
    assert!(result.success, "{}", result.content);
    let v: Value = serde_json::from_str(&result.content).unwrap();
    assert_eq!(v["observation"]["data"], json!([]));
    assert_eq!(
        bridge.inner.host().lock().unwrap().telemetry()["index.cold_start"],
        1
    );
}

#[tokio::test]
async fn context_graph_and_budgeted_search_return_current_related_code() {
    let profile = "melissa.code.implementer";
    let mut f = Fixture::new(profile);
    let mut calc = "# boşluk ve kaynak konumu\n".repeat(400);
    calc.push_str("def choose(n):\n    return n + 1\n");
    fs::write(f.source.join("calc.py"), &calc).unwrap();
    fs::create_dir(f.source.join("tests")).unwrap();
    fs::write(
        f.source.join("tests/test_calc.py"),
        "from calc import choose\ndef test_choice():\n    assert choose(2) == 3\n",
    )
    .unwrap();
    fs::create_dir(f.source.join("private")).unwrap();
    fs::write(
        f.source.join("private/secret.py"),
        "def choose(n): return 'hidden_fixture'",
    )
    .unwrap();
    fs::write(f.source.join("noise.txt"), "choose ".repeat(500)).unwrap();
    let (store, task) = Fixture::task(&f.app, &f.source, profile, "b", vec!["private".into()]);
    f.store = store;
    f.task = task;
    let mut context = f.context();
    let graph = f.call(&mut context, "context_script", json!({
        "operation":"common_script","id":"dependency_graph","input":{"path":"calc.py","symbol":"choose","limit":4}
    }), "phase-c-graph").await;
    assert!(graph.success, "{}", graph.content);
    let output: Value = serde_json::from_str(&graph.content).unwrap();
    let data = &output["observation"]["data"];
    assert_eq!(data["nodes"][0]["definitions"][0]["name"], "choose");
    assert!(
        data["edges"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["kind"] == "call_candidate"
                && e["related_test"]["name"] == "test_choice"
                && e["target"]["id"]["path"] == "calc.py"
                && e["compiler_resolved"] == false)
    );
    assert!(!graph.content.contains("private/secret.py"));

    let file = f.repo().read("calc.py").unwrap().0;
    let pos = calc.rfind('1').unwrap();
    let patch = json!({"schema_version":"melissa.repository-patch/v1","root_identity":f.repo().root_identity(),
        "files":[{"before":file,"edits":[{"start":pos,"end":pos+1,"expected":[49],"replacement":[50]}]}]});
    let affected = f.call(&mut context, "context_script", json!({
            "operation":"common_script","id":"dependency_graph","input":{"patch":patch,"limit":2}
    }), "phase-c-patch").await;
    assert!(affected.success, "{}", affected.content);
    let output: Value = serde_json::from_str(&affected.content).unwrap();
    assert_eq!(
        output["observation"]["data"]["edges"][0]["kind"],
        "affected_symbol"
    );
    assert_eq!(
        output["observation"]["data"]["edges"][0]["source"]["name"],
        "choose"
    );
    assert_eq!(
        output["observation"]["data"]["edges"][1]["kind"],
        "affected_symbol_preview"
    );
    assert_eq!(
        output["observation"]["data"]["edges"][1]["definition"]["name"],
        "choose"
    );
    assert_eq!(fs::read_to_string(f.source.join("calc.py")).unwrap(), calc);

    let query = json!({"operation":"search","query":"choose","symbols":["choose"],"limit":3,"max_bytes":3400,"max_tokens":3000});
    let search = f
        .call(
            &mut context,
            "context_search",
            query.clone(),
            "phase-c-search",
        )
        .await;
    assert!(search.success, "{}", search.content);
    let output: Value = serde_json::from_str(&search.content).unwrap();
    let result = &output["observation"];
    let hits = result["data"].as_array().unwrap();
    assert_eq!(hits[0]["source"]["id"], "calc.py");
    assert!(hits[0]["excerpt"].as_str().unwrap().contains("def choose"));
    assert!(hits[0]["start_line"].as_u64().unwrap() > 350);
    assert!(hits.iter().any(|hit| {
        hit["source"]["id"] == "tests/test_calc.py"
            && hit["signals"]
                .as_array()
                .unwrap()
                .contains(&json!("related_test"))
    }));
    let len = serde_json::to_vec(hits).unwrap().len();
    assert!(len <= 3000);
    assert_eq!(result["budget"]["data_bytes"], len);
    assert!(!search.content.contains("hidden_fixture"));
    let cached = f
        .call(&mut context, "context_search", query, "phase-c-cached")
        .await;
    assert!(cached.success, "{}", cached.content);
    let output: Value = serde_json::from_str(&cached.content).unwrap();
    assert_eq!(output["observation"]["cached"], true);
    assert_eq!(output["observation"]["data"], result["data"]);
}
impl Fixture {
    fn new(profile: &str) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let source = temp.path().join("source");
        fs::create_dir(&source).unwrap();
        fs::write(
            source.join("main.py"),
            b"def double(x):\n    return x * 2\n",
        )
        .unwrap();
        fs::write(source.join("protected.txt"), b"user-owned\r\n").unwrap();
        App::init(&home).unwrap();
        let app = App::open(&home).unwrap();
        let (store, task) = Self::task(&app, &source, profile, "a", vec![]);
        Self {
            _temp: temp,
            app,
            source,
            store,
            task,
        }
    }
    fn task(
        app: &App,
        source: &std::path::Path,
        profile: &str,
        id: &str,
        excludes: Vec<String>,
    ) -> (LocalStore, TaskContractV1) {
        let repo = Repository::open(source, Limits::default(), excludes.clone()).unwrap();
        let snapshot = repo.snapshot(&AtomicBool::new(false)).unwrap();
        let mut task: TaskContractV1 = serde_json::from_slice(include_bytes!(
            "../../../tests/fixtures/code01/wire/task-contract-v1.json"
        ))
        .unwrap();
        task.task_id = format!("task_{}", id.repeat(32));
        task.selected_profile_ids = vec![profile.into()];
        task.input_snapshot_digest = snapshot.digest.clone();
        task.allowed_effects = vec![
            "workspace.read",
            "workspace.propose_patch",
            "workspace.apply_patch_scoped",
            "workspace.create_worktree_scoped",
            "git.read",
            "process.execute_scoped",
            "memory.propose",
            "evidence.append",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        task.limits.max_wall_time_ms = 86400000;
        task.limits.max_disk_bytes = 1024 * 1024 * 1024;
        task.limits.max_output_bytes = 64 * 1024 * 1024;
        let dir = app.home.join("tasks").join(&task.task_id);
        fs::create_dir(&dir).unwrap();
        let mut store =
            LocalStore::open(&dir.join("store"), app.key.clone(), Default::default()).unwrap();
        store.journal_mut().create_task(&task).unwrap();
        for state in [
            TaskState::Planned,
            TaskState::Queued,
            TaskState::Leased,
            TaskState::Running,
        ] {
            let v = store.journal().task(&task.task_id).unwrap().record.version;
            store
                .transition_task(&task.task_id, v, state, None)
                .unwrap();
        }
        let workspace = Workspace {
            schema_version: "melissa.cli-workspace/v1".into(),
            root: source.to_str().unwrap().into(),
            limits: Limits::default(),
            explicit_excludes: excludes,
            snapshot,
            files: BTreeMap::new(),
        };
        put(
            &mut store,
            &task.task_id,
            WORKSPACE,
            "workspace",
            &bytes(&workspace).unwrap(),
        )
        .unwrap();
        (store, task)
    }
    fn tools(&self) -> BTreeSet<String> {
        resources::catalog().unwrap().profiles[&self.task.selected_profile_ids[0]]
            .tool_ids
            .iter()
            .cloned()
            .collect()
    }
    fn context(&self) -> crate::context_bridge::ContextBridge {
        crate::context_bridge::runtime(
            &self.app,
            &self.store,
            &self.task,
            &self.task.selected_profile_ids[0],
            now().unwrap() + 600000,
            &self.tools(),
        )
        .unwrap()
    }
    fn development(&self) -> crate::development::DevelopmentTools {
        crate::development::DevelopmentTools::new(
            &self.app,
            &self.store,
            &self.task,
            &self.task.selected_profile_ids[0],
            now().unwrap() + 600000,
            &self.tools(),
            "main.py",
            self.tools().contains("fs.patch"),
        )
        .unwrap()
    }
    fn repo(&self) -> Repository {
        Repository::open(&self.source, Limits::default(), vec![]).unwrap()
    }
    async fn call(
        &self,
        d: &mut dyn ToolDispatcher,
        name: &str,
        args: Value,
        label: &str,
    ) -> ToolExecution {
        use melissa_local_store::journal::{
            ModelRequestState, ModelReservation, ModelSettlement, ModelUsage,
        };
        // Identity-only ledger fixture: explicitly NEVER_DISPATCHED, no transport.
        let mut s = self.store.connect().unwrap();
        let time = now().unwrap();
        let request = format!("request-{label}");
        let version = s.journal().task(&self.task.task_id).unwrap().record.version;
        s.journal_mut()
            .reserve_model_request(
                &ModelReservation {
                    schema_version: "melissa.model-reservation/v1".into(),
                    request_id: request.clone(),
                    task_id: self.task.task_id.clone(),
                    profile_id: self.task.selected_profile_ids[0].clone(),
                    provider_id: "offline-fixture".into(),
                    model_id: "not-run".into(),
                    request_digest: crate::digest(b"offline fixture"),
                    pricing_digest: Some(crate::digest(b"offline zero cost fixture")),
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
                &request,
                0,
                &ModelSettlement {
                    state: ModelRequestState::NeverDispatched,
                    usage: ModelUsage {
                        input_tokens: Some(0),
                        output_tokens: Some(0),
                        cached_input_tokens: Some(0),
                        cost_micro_usd: Some(0),
                    },
                    receipt_digest: Some(crate::digest(b"offline fixture without any transport")),
                    reason: Some(
                        "Offline tool dispatch test; no inference or provider transport".into(),
                    ),
                    completed_at_unix_ms: time,
                },
            )
            .unwrap();
        let c = ToolCall {
            id: format!("call-{label}"),
            name: name.into(),
            arguments: args.to_string(),
        };
        d.validate_call(&c).unwrap();
        d.execute(
            c,
            ToolContext {
                task_id: self.task.task_id.clone(),
                profile_id: self.task.selected_profile_ids[0].clone(),
                run_id: "capability-test".into(),
                request_id: format!("request-{label}"),
                invocation_id: format!("inv-{label}"),
                attempt_id: format!("attempt-{label}"),
                generation: 1,
            },
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .unwrap_or_else(|e| panic!("{label}: {e:?}"))
    }
}
#[test]
fn all_code_roles_construct_context_and_native_development_without_extra_authority() {
    for profile in resources::catalog()
        .unwrap()
        .profiles
        .values()
        .filter(|p| p.id.starts_with("melissa.code."))
    {
        let f = Fixture::new(&profile.id);
        let context = f.context();
        let defs = context.definitions();
        assert!(
            defs.iter().any(|d| d.definition.name == "memory_search"),
            "{}",
            profile.id
        );
        assert!(defs.iter().any(|d| d.definition.name == "skill_helper"));
        let development = f.development();
        assert!(
            development
                .definitions()
                .iter()
                .any(|d| d.definition.name == "code_semantic")
        );
        if !profile
            .authority
            .capability_ceiling
            .iter()
            .any(|c| c == "process.execute_scoped")
        {
            assert!(
                !development
                    .definitions()
                    .iter()
                    .any(|d| d.definition.name == "code_project_tests")
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn html_only_edits_do_not_invent_a_python_check_and_strict_git_rejects_extra_fields() {
    let f = Fixture::new("melissa.code.implementer");
    fs::remove_file(f.source.join("main.py")).unwrap();
    fs::write(f.source.join("index.html"), b"<p>old</p>\n").unwrap();
    let development = f.development();
    assert!(
        !development
            .definitions()
            .iter()
            .any(|d| d.definition.name == "code_project_check")
    );
    assert!(
        development
            .validate_call(&ToolCall {
                id: "injected".into(),
                name: "code_git".into(),
                arguments: json!({"operation":"status","executable":"anything"}).to_string()
            })
            .is_err()
    );
    let policy = melissa_repository::PatchPolicy {
        task_id: f.task.task_id.clone(),
        principal_id: "cli:operator".into(),
        profile_id: f.task.selected_profile_ids[0].clone(),
        resource_id: f.task.workspace_ref.clone(),
        authorization_ref: f.task.authorization_refs[0].clone(),
        expires_at_ms: now().unwrap() + 600000,
        epoch: 1,
        writable_paths: BTreeSet::from(["index.html".into()]),
        protected_paths: BTreeSet::from(["protected.txt".into()]),
    };
    let repo_tool = melissa_repository::tools::RepositoryDispatcher::new(
        f.repo(),
        f.store.connect().unwrap(),
        melissa_repository::tools::RepositoryToolPolicy {
            files: policy,
            read_repository: true,
            enabled_tools: BTreeSet::from(["fs_patch".into(), "code_unified_propose".into()]),
        },
    )
    .unwrap()
    .with_portable_patch_wire()
    .unwrap();
    let mut d = crate::development::CheckedEdits {
        context: None,
        inner: Box::new(repo_tool),
        repo: Arc::new(f.repo()),
        store: f.store.connect().unwrap(),
        task: f.task.task_id.clone(),
        checker: None,
        language: melissa_build_test::full::Language::Python,
    };
    let preview=f.call(&mut d,"code_unified_propose",json!({"patch":"--- a/index.html\n+++ b/index.html\n@@ -1 +1 @@\n-<p>old</p>\n+<p>new</p>\n"}),"html-preview").await;
    let proposal: Value = serde_json::from_str(&preview.content).unwrap();
    let applied = f
        .call(
            &mut d,
            "fs_patch",
            json!({"proposal_json":proposal["result"]["changes"][0]["proposal"].to_string()}),
            "html-patch",
        )
        .await;
    assert!(applied.success, "{}", applied.content);
    let value: Value = serde_json::from_str(&applied.content).unwrap();
    assert_eq!(value["mutation_applied"], true);
    assert_eq!(value["after_edit"]["status"], "NOT_RUN");
    assert!(value["after_edit"]["passed"].is_null());
}
#[tokio::test(flavor = "multi_thread")]
async fn post_edit_check_or_receipt_failure_preserves_the_applied_mutation() {
    for fail_receipt in [false, true] {
        let f = Fixture::new("melissa.code.implementer");
        let repo_tool = melissa_repository::tools::RepositoryDispatcher::new(
            f.repo(),
            f.store.connect().unwrap(),
            melissa_repository::tools::RepositoryToolPolicy {
                files: melissa_repository::PatchPolicy {
                    task_id: f.task.task_id.clone(),
                    principal_id: "cli:operator".into(),
                    profile_id: f.task.selected_profile_ids[0].clone(),
                    resource_id: f.task.workspace_ref.clone(),
                    authorization_ref: f.task.authorization_refs[0].clone(),
                    expires_at_ms: now().unwrap() + 600000,
                    epoch: 1,
                    writable_paths: BTreeSet::from(["main.py".into()]),
                    protected_paths: BTreeSet::from(["protected.txt".into()]),
                },
                read_repository: true,
                enabled_tools: BTreeSet::from(["fs_patch".into(), "code_unified_propose".into()]),
            },
        )
        .unwrap()
        .with_portable_patch_wire()
        .unwrap();
        // Deliberately missing check authority; no SDK process can start.
        let semantic_only = crate::development::DevelopmentTools::new(
            &f.app,
            &f.store,
            &f.task,
            &f.task.selected_profile_ids[0],
            now().unwrap() + 600000,
            &BTreeSet::from(["code.references".into()]),
            "main.py",
            false,
        )
        .unwrap()
        .code;
        let mut d = crate::development::CheckedEdits {
            context: None,
            inner: Box::new(repo_tool),
            repo: Arc::new(f.repo()),
            store: f.store.connect().unwrap(),
            // An unknown receipt task simulates persistence rejection after a real edit.
            task: if fail_receipt {
                format!("task_{}", "b".repeat(32))
            } else {
                f.task.task_id.clone()
            },
            checker: if fail_receipt { None } else { semantic_only },
            language: melissa_build_test::full::Language::Python,
        };
        let preview = f.call(&mut d, "code_unified_propose", json!({"patch":"--- a/main.py\n+++ b/main.py\n@@ -1,2 +1,2 @@\n def double(x):\n-    return x * 2\n+    return x + x\n"}), "check-failure-preview").await;
        assert!(preview.success);
        let proposal: Value = serde_json::from_str(&preview.content).unwrap();
        let result = f
            .call(
                &mut d,
                "fs_patch",
                json!({"proposal_json":proposal["result"]["changes"][0]["proposal"].to_string()}),
                "check-failure-patch",
            )
            .await;
        let value: Value = serde_json::from_str(&result.content).unwrap();
        assert!(!result.success);
        assert_eq!(result.requires_reconciliation, fail_receipt);
        assert_eq!(value["mutation_applied"], true);
        assert_eq!(value["after_edit"]["status"], "BLOCKED");
        assert_eq!(
            fs::read(f.source.join("main.py")).unwrap(),
            b"def double(x):\n    return x + x\n"
        );
        assert_eq!(
            fs::read(f.source.join("protected.txt")).unwrap(),
            b"user-owned\r\n"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn memory_survives_new_context_requires_review_and_rejects_changed_sources() {
    let f = Fixture::new("melissa.code.implementer");
    let mut context = f.context();
    let proposed=f.call(&mut context,"memory_propose",json!({"operation":"memory_propose","key":"double","claim":"double multiplies the input by two","source_ids":["main.py"]}),"memory-propose").await;
    assert!(proposed.success, "{}", proposed.content);
    drop(context);
    let listing = f.app.memory(&f.source, "list", None, None).unwrap();
    let id = listing["records"][0]["id"].as_str().unwrap();
    assert_eq!(listing["records"][0]["status"], "proposed");
    assert!(
        f.app
            .memory(&f.source, "accept", Some(id), Some("wrong-digest"))
            .is_err()
    );
    let approved = f
        .app
        .memory(
            &f.source,
            "accept",
            Some(id),
            listing["review_digest"].as_str(),
        )
        .unwrap();
    assert_eq!(approved["records"][0]["status"], "accepted");
    let restricted =
        Repository::open(&f.source, Limits::default(), vec!["main.py".into()]).unwrap();
    {
        let mut memory =
            crate::project_memory::ProjectMemory::open(&f.app.home, f.app.key.clone(), &restricted)
                .unwrap();
        let ns = crate::project_memory::namespace(&restricted);
        let current = crate::project_memory::sources(&restricted).unwrap();
        let scope = melissa_skill_context::retrieval::SearchScope {
            namespace: ns.clone(),
            domain: melissa_skill_context::retrieval::Domain::Code,
            now_ms: now().unwrap(),
            current: current
                .iter()
                .map(|s| (s.id.clone(), (s.version.clone(), s.digest.clone())))
                .collect(),
        };
        assert!(memory.book.search(&scope, "double").is_empty());
        memory.save(&ns).unwrap();
    }
    assert_eq!(
        f.app.memory(&f.source, "list", None, None).unwrap()["records"][0]["status"],
        "accepted"
    );
    let mut context = f.context();
    let found = f
        .call(
            &mut context,
            "memory_search",
            json!({"operation":"memory_search","query":"double"}),
            "memory-found",
        )
        .await;
    assert!(
        found.success && found.content.contains("multiplies the input"),
        "{}",
        found.content
    );
    drop(context);
    let other = f._temp.path().join("other");
    fs::create_dir(&other).unwrap();
    fs::write(other.join("main.py"), b"def double(x):\n    return x * 2\n").unwrap();
    assert_eq!(
        f.app.memory(&other, "list", None, None).unwrap()["records"],
        json!([])
    );
    fs::write(
        f.source.join("main.py"),
        b"def double(x):\n    return x * 3\n",
    )
    .unwrap();
    let stale = f.app.memory(&f.source, "list", None, None).unwrap();
    assert_eq!(stale["records"][0]["status"], "stale");
    let forgotten = f
        .app
        .memory(
            &f.source,
            "forget",
            Some(id),
            stale["review_digest"].as_str(),
        )
        .unwrap();
    assert_eq!(forgotten["records"][0]["status"], "deleted");
    assert_eq!(forgotten["records"][0]["claim"], "");
}
#[tokio::test(flavor = "multi_thread")]
async fn context_native_discovery_symbols_and_preview_use_current_repository() {
    let f = Fixture::new("melissa.code.implementer");
    let mut context = f.context();
    for (id, input) in [
        ("inspect_environment", json!({})),
        ("build_symbol_index", json!({})),
        (
            "generate_patch_preview",
            json!({"patch":"--- a/main.py\n+++ b/main.py\n@@ -1,2 +1,2 @@\n def double(x):\n-    return x * 2\n+    return x * 4\n"}),
        ),
    ] {
        let r = f
            .call(
                &mut context,
                "context_script",
                json!({"operation":"common_script","id":id,"input":input}),
                id,
            )
            .await;
        assert!(r.success, "{id}: {}", r.content);
    }
    assert!(
        fs::read_to_string(f.source.join("main.py"))
            .unwrap()
            .contains("x * 2")
    );
}
#[test]
fn prepare_is_source_bound_and_never_invents_acceptance_or_overwrites() {
    let f = Fixture::new("melissa.code.implementer");
    let out = f._temp.path().join("plan.json");
    let v = f
        .app
        .prepare(&f.source, "main.py içindeki hatayı düzelt", &out, None)
        .unwrap();
    assert_eq!(v["preparation"]["model_calls"], 0);
    assert_eq!(v["preparation"]["ready_to_create"], false);
    assert_eq!(
        v["preparation"]["recipe"]["writable_paths"],
        json!(["main.py"])
    );
    assert_eq!(
        v["preparation"]["recipe"]["agents"][1]["profile"],
        "melissa.code.debugger"
    );
    assert!(f.app.prepare(&f.source, "other task", &out, None).is_err());
    assert!(
        f.app
            .prepare(&f.source, "fix", &f.source.join("draft.json"), None)
            .is_err()
    );
}
#[test]
fn long_recipe_accepts_more_than_sixteen_steps_and_half_an_hour_but_rejects_overflow() {
    let mut r:Recipe=serde_json::from_value(json!({"schema_version":"melissa.cli-recipe/v1","agents":[{"profile":"melissa.code.implementer","goal":"Fix the program","write":true}],"writable_paths":["main.py"],"protected_paths":[],"language":"python","entry":"main.py","negative_workspace":"C:/negative","checks":[{"id":"one","input":1,"expected":1},{"id":"two","input":2,"expected":2}],"max_cost_micro_usd":5000000,"wall_time_ms":86400000})).unwrap();
    r.agents = vec![r.agents[0].clone(); 48];
    assert!(r.validate().is_ok());
    r.wall_time_ms = u64::MAX;
    assert!(r.validate().is_err());
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "local isolated Docker integration; no AI or provider"]
async fn native_cli_semantics_tests_helper_and_result_paging() {
    let f = Fixture::new("melissa.code.implementer");
    fs::write(
        f.source.join("consumer.py"),
        b"from main import double\nvalue = double(4)\n",
    )
    .unwrap();
    fs::write(
        f.source.join("unrelated.py"),
        b"def double(x):\n    return x + 100\n",
    )
    .unwrap();
    fs::write(
        f.source.join("test_main.py"),
        b"from main import double\ndef test_double():\n    assert double(4) == 8\n",
    )
    .unwrap();
    let before = f.repo().snapshot(&AtomicBool::new(false)).unwrap().digest;
    let mut d = f.development();
    let result = f
        .call(
            &mut d,
            "code_semantic",
            json!({"method":"definition","path":"consumer.py","line":1,"character":9}),
            "lsp-definition",
        )
        .await;
    assert!(result.success, "{}", result.content);
    let value: Value = serde_json::from_str(&result.content).unwrap();
    assert_eq!(value["locations_authenticated"], true);
    assert!(value["result"]["result"].to_string().contains("main.py"));
    assert!(
        !value["result"]["result"]
            .to_string()
            .contains("unrelated.py")
    );
    drop(d);
    let mut d = f.development(); // A fresh dispatcher recovers authenticated pages.
    let page=f.call(&mut d,"code_result_page",json!({"artifact_id":"inv-lsp-definition:semantic-service-result","offset":0,"max_bytes":65536}),"semantic-page").await;
    assert!(page.success, "{}", page.content);
    let sdk_tests = f
        .call(
            &mut d,
            "code_project_tests",
            serde_json::to_value(melissa_build_test::tools::CodeOperation::Procedure {
                language: melissa_build_test::full::Language::Python,
                operation: melissa_build_test::full::Operation::Test {
                    suite: melissa_build_test::full::TestSuite::UnitIntegration,
                    repetitions: 1,
                },
                harness: None,
            })
            .unwrap(),
            "normalized-sdk-tests",
        )
        .await;
    assert!(sdk_tests.success, "{}", sdk_tests.content);
    let normalized =
        melissa_model_gateway::observations::normalize("code_project_tests", &sdk_tests.content)
            .unwrap();
    assert_eq!(normalized["status"], "passed");
    assert!(normalized["passed_count"].as_u64().unwrap() > 0);
    assert_eq!(normalized["failed_count"], 0);
    assert_eq!(normalized["protected_acceptance"], false);
    assert_eq!(normalized["host_source_current"], true);
    let mut context = f.context();
    let tested = f
        .call(
            &mut context,
            "context_script",
            json!({"operation":"common_script","id":"run_targeted_tests","input":{}}),
            "native-tests",
        )
        .await;
    assert!(tested.success, "{}", tested.content);
    assert!(tested.content.contains("test_double"));
    let (pins, _, _) = resources::skills(
        "melissa.code.implementer",
        f.tools(),
        f.task.allowed_effects.iter().cloned().collect(),
    )
    .unwrap();
    let helper=f.call(&mut context,"skill_helper",json!({"operation":"helper","pin":pins[0],"path":"scripts/assess.py","input":{"expected":"original","actual":"changed","paths":["forbidden"],"allowed":["main.py"]}}),"native-helper").await;
    assert!(helper.success, "{}", helper.content);
    assert!(
        helper.content.contains("stale_preimage")
            && helper.content.contains("out_of_scope:forbidden")
    );
    assert_eq!(
        before,
        f.repo().snapshot(&AtomicBool::new(false)).unwrap().digest
    );
    assert_eq!(
        f.store
            .journal()
            .model_budget(&f.task.task_id)
            .unwrap()
            .known_cost_micro_usd,
        0
    );
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "local isolated Docker integration; no AI or provider"]
async fn native_cli_after_edit_reports_invalid_regex_without_replaying_or_undoing_write() {
    let f = Fixture::new("melissa.code.implementer");
    fs::write(f.source.join("main.js"), b"const pattern = /ok/;\n").unwrap();
    let policy = melissa_repository::PatchPolicy {
        task_id: f.task.task_id.clone(),
        principal_id: "cli:operator".into(),
        profile_id: f.task.selected_profile_ids[0].clone(),
        resource_id: f.task.workspace_ref.clone(),
        authorization_ref: f.task.authorization_refs[0].clone(),
        expires_at_ms: now().unwrap() + 600000,
        epoch: 1,
        writable_paths: BTreeSet::from(["main.js".into()]),
        protected_paths: BTreeSet::from(["protected.txt".into()]),
    };
    let repo_tool = melissa_repository::tools::RepositoryDispatcher::new(
        f.repo(),
        f.store.connect().unwrap(),
        melissa_repository::tools::RepositoryToolPolicy {
            files: policy,
            read_repository: true,
            enabled_tools: BTreeSet::from(["fs_patch".into(), "code_unified_propose".into()]),
        },
    )
    .unwrap()
    .with_portable_patch_wire()
    .unwrap();
    let mut d = crate::development::CheckedEdits {
        context: None,
        inner: Box::new(repo_tool),
        repo: Arc::new(f.repo()),
        store: f.store.connect().unwrap(),
        task: f.task.task_id.clone(),
        checker: f.development().code,
        language: melissa_build_test::full::Language::JavaScript,
    };
    let preview=f.call(&mut d,"code_unified_propose",json!({"patch":"--- a/main.js\n+++ b/main.js\n@@ -1 +1 @@\n-const pattern = /ok/;\n+const pattern = /(/;\n"}),"regex-preview").await;
    assert!(preview.success);
    let proposal: Value = serde_json::from_str(&preview.content).unwrap();
    let result = f
        .call(
            &mut d,
            "fs_patch",
            json!({"proposal_json":proposal["result"]["changes"][0]["proposal"].to_string()}),
            "regex-patch",
        )
        .await;
    assert!(
        !result.success && !result.requires_reconciliation,
        "{}",
        result.content
    );
    let v: Value = serde_json::from_str(&result.content).unwrap();
    assert_eq!(v["mutation_applied"], true);
    assert_eq!(v["after_edit"]["passed"], false);
    assert!(v["after_edit"]["sdk"].is_object());
    assert_eq!(
        fs::read(f.source.join("main.js")).unwrap(),
        b"const pattern = /(/;\n"
    );
    assert_eq!(
        fs::read(f.source.join("protected.txt")).unwrap(),
        b"user-owned\r\n"
    );
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "local isolated Docker integration; no AI or provider"]
async fn native_cli_git_is_fresh_read_only_and_worktree_is_private() {
    let f = Fixture::new("melissa.code.implementer");
    for argv in [
        vec!["init", "--quiet"],
        vec!["config", "user.name", "Offline Fixture"],
        vec!["config", "user.email", "fixture@example.invalid"],
        vec!["add", "main.py", "protected.txt"],
        vec![
            "-c",
            "core.hooksPath=NUL",
            "commit",
            "--quiet",
            "-m",
            "fixture",
        ],
    ] {
        let result = std::process::Command::new("git")
            .args(argv)
            .current_dir(&f.source)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
    let commit = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(&f.source)
        .output()
        .unwrap();
    let commit = String::from_utf8(commit.stdout).unwrap().trim().to_string();
    fs::write(
        f.source.join("main.py"),
        b"def double(x):\n    return x * 3\n",
    )
    .unwrap();
    let before = f.repo().snapshot(&AtomicBool::new(false)).unwrap().digest;
    let index = fs::read(f.source.join(".git/index")).unwrap();
    let context = f.context();
    let host = context.inner.host();
    let router = melissa_repository::tools::ToolRouter::new(vec![
        Box::new(context),
        Box::new(f.development()),
    ])
    .unwrap();
    let mut d = crate::development::CheckedEdits {
        inner: Box::new(router),
        repo: Arc::new(f.repo()),
        store: f.store.connect().unwrap(),
        task: f.task.task_id.clone(),
        checker: None,
        language: melissa_build_test::full::Language::Python,
        context: Some(host),
    };
    let status = f
        .call(
            &mut d,
            "code_git",
            json!({"operation":"status"}),
            "git-status",
        )
        .await;
    assert!(status.success, "{}", status.content);
    assert!(status.content.contains("main.py"));
    let diff = f
        .call(
            &mut d,
            "code_git",
            json!({"operation":"diff","staged":false}),
            "git-diff",
        )
        .await;
    assert!(diff.success, "{}", diff.content);
    assert!(diff.content.contains("x * 3"));
    let log = f
        .call(
            &mut d,
            "code_git",
            json!({"operation":"log","max_entries":5}),
            "git-log",
        )
        .await;
    assert!(log.success && log.content.contains(&commit));
    let history = f
        .call(
            &mut d,
            "code_git",
            json!({"operation":"log_paths","max_entries":5}),
            "git-log-paths",
        )
        .await;
    assert!(history.success, "{}", history.content);
    let query = json!({"operation":"search","query":"main.py","symbols":[],"limit":1});
    let ranking = f
        .call(&mut d, "context_search", query.clone(), "git-ranking")
        .await;
    assert!(ranking.success, "{}", ranking.content);
    let value: Value = serde_json::from_str(&ranking.content).unwrap();
    let signals = value["observation"]["data"][0]["signals"]
        .as_array()
        .unwrap();
    assert!(
        signals.contains(&json!("git_diff")) && signals.contains(&json!("git_history")),
        "{value}"
    );

    let tree = f
        .call(
            &mut d,
            "code_git",
            json!({"operation":"worktree_create","name":"branch-fixture","commit":commit}),
            "git-worktree",
        )
        .await;
    assert!(tree.success, "{}", tree.content);
    assert!(!f.source.join(".code05-worktrees").exists());
    assert_eq!(fs::read(f.source.join(".git/index")).unwrap(), index);
    assert_eq!(
        before,
        f.repo().snapshot(&AtomicBool::new(false)).unwrap().digest
    );

    fs::write(
        f.source.join("main.py"),
        b"def double(x):\n    return (x * 3\n",
    )
    .unwrap();
    let operation = melissa_build_test::tools::CodeOperation::Procedure {
        language: melissa_build_test::full::Language::Python,
        operation: melissa_build_test::full::Operation::Check {},
        harness: None,
    };
    let checked = f
        .call(
            &mut d,
            "code_project_check",
            serde_json::to_value(operation).unwrap(),
            "rank-diagnostics",
        )
        .await;
    assert!(
        !checked.success && !checked.requires_reconciliation && !checked.cancelled,
        "{}",
        checked.content
    );
    let ranking = f
        .call(&mut d, "context_search", query, "diagnostic-ranking")
        .await;
    assert!(ranking.success, "{}", ranking.content);
    let value: Value = serde_json::from_str(&ranking.content).unwrap();
    let signals = value["observation"]["data"][0]["signals"]
        .as_array()
        .unwrap();
    assert!(signals.contains(&json!("diagnostics")), "{value}");
    assert!(
        !signals.contains(&json!("git_diff")) && !signals.contains(&json!("git_history")),
        "{value}"
    );
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "local isolated Docker integration; no AI or provider"]
async fn native_cli_debug_owns_inspects_and_closes_its_session() {
    let f = Fixture::new("melissa.code.debugger");
    fs::write(
        f.source.join("main.py"),
        b"value = 2\nanswer = value * 3\nprint(answer)\n",
    )
    .unwrap();
    let mut d = f.development();
    let started = f
        .call(
            &mut d,
            "code_debug_start",
            json!({"breakpoints":[{"path":"main.py","line":2}]}),
            "debug-start",
        )
        .await;
    assert!(started.success, "{}", started.content);
    let v: Value = serde_json::from_str(&started.content).unwrap();
    let session = v["session_id"].as_str().unwrap();
    let stack = f
        .call(
            &mut d,
            "code_debug_command",
            json!({"session_id":session,"operation":{"command":"stack"}}),
            "debug-stack",
        )
        .await;
    // Always close the owned session even if a diagnostic assertion fails.
    let closed = f
        .call(
            &mut d,
            "code_debug_command",
            json!({"session_id":session,"operation":{"command":"close"}}),
            "debug-close",
        )
        .await;
    assert!(stack.success, "{}", stack.content);
    assert!(
        closed.success && !closed.requires_reconciliation,
        "{}",
        closed.content
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn replan_preserves_active_step_then_changes_pending_steps_without_budget_or_deadline_reset()
{
    use melissa_workflow::scheduler::{Freshness, Phase, Refresh, Scheduler};
    let f = Fixture::new("melissa.code.implementer");
    // Deterministic ledger fixture, no real/simulated network provider.
    let mut context = f.context();
    let r = f
        .call(
            &mut context,
            "context_script",
            json!({"operation":"common_script","id":"inspect_environment","input":{}}),
            "replan-ledger-fixture",
        )
        .await;
    assert!(r.success);
    drop(context);
    let Fixture {
        _temp,
        app,
        source,
        mut store,
        task,
    } = f;
    let mut recipe:Recipe=serde_json::from_value(json!({"schema_version":"melissa.cli-recipe/v1","agents":[{"profile":"melissa.code.implementer","goal":"First step","write":true},{"profile":"melissa.code.implementer","goal":"Second step","write":true}],"writable_paths":["main.py"],"protected_paths":["protected.txt"],"language":"python","entry":"main.py","negative_workspace":_temp.path().join("negative"),"checks":[{"id":"one","input":1,"expected":1},{"id":"two","input":2,"expected":2}],"max_cost_micro_usd":5000000,"wall_time_ms":86400000})).unwrap();
    let execution = crate::recipe::Execution {
        recipe: recipe.clone(),
        settings_digest: crate::digest(b"offline policy"),
        expires_at_ms: now().unwrap() + 86400000,
        negative_files: BTreeMap::new(),
        runtime_digest: None,
    };
    put(
        &mut store,
        &task.task_id,
        crate::recipe::EXECUTION,
        "execution",
        &bytes(&execution).unwrap(),
    )
    .unwrap();
    let plan = crate::engine::plan(&task, &execution).unwrap();
    let source_digest = task.input_snapshot_digest.clone();
    let policy = execution.settings_digest.clone();
    let refresh: Refresh = Arc::new(move || {
        Ok(Freshness {
            workspace_digest: source_digest.clone(),
            policy_digest: policy.clone(),
        })
    });
    let mut scheduler = Scheduler::create(
        store.connect().unwrap(),
        plan,
        &resources::catalog().unwrap(),
        refresh.clone(),
        Arc::new(AtomicBool::new(false)),
    )
    .unwrap();
    let packet = scheduler.acquire("step-0", "test:offline", 600000).unwrap();
    let before = scheduler.state().unwrap();
    put(
        &mut store,
        &task.task_id,
        &format!("workflow-handoff:{}", packet.lease.run),
        "workflow.handoff",
        &bytes(&packet).unwrap(),
    )
    .unwrap();
    let control = crate::control::Control::new(
        &app,
        &task.task_id,
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicBool::new(true)),
    )
    .unwrap();
    control
        .publish("paused", json!({}), json!({}), json!({}), json!({}))
        .unwrap();
    let repo = Repository::open(&source, Limits::default(), vec![]).unwrap();
    put(
        &mut store,
        &task.task_id,
        &format!("cli:paused-source:{}", control.session),
        "paused-source",
        &bytes(&repo.snapshot(&AtomicBool::new(false)).unwrap()).unwrap(),
    )
    .unwrap();
    drop(scheduler);
    drop(store);
    let mut invalid = recipe.clone();
    invalid.agents[0].goal = "replace active work".into();
    assert!(app.replan(&task.task_id, invalid).is_err());
    let mut invalid = recipe.clone();
    invalid.checks[0].expected = json!(999);
    assert!(app.replan(&task.task_id, invalid).is_err());
    recipe.agents[1].goal = "Revised remaining work".into();
    recipe.agents.push(recipe.agents[1].clone());
    let queued = app.replan(&task.task_id, recipe).unwrap();
    assert_eq!(queued["status"], "queued_until_current_step_completes");
    let mut store = app.store(&task.task_id).unwrap();
    let mut scheduler = Scheduler::open(
        store.connect().unwrap(),
        task.task_id.clone(),
        "cli-workflow-v1".into(),
        refresh,
        Arc::new(AtomicBool::new(false)),
    )
    .unwrap();
    assert!(
        !crate::preparation::activate_pending(&mut store, &task.task_id, &mut scheduler).unwrap()
    );
    assert_eq!(scheduler.state().unwrap().plan.revision, 1);
    assert_eq!(
        scheduler.resume_packet("step-0").unwrap().lease,
        packet.lease
    );
    // Synthetic terminal node fixture exercises scheduler mechanics only.
    let result=melissa_model_gateway::agent_loop::RunResult{schema_version:"melissa.agent-run-result/v1".into(),run_id:packet.lease.run.clone(),task_id:task.task_id.clone(),status:melissa_model_gateway::agent_loop::RunStatus::FinalCandidate,answer:Some(json!({"summary":"offline deterministic fixture","source_digest":task.input_snapshot_digest}).to_string()),request_ids:vec!["request-replan-ledger-fixture".into()],tool_calls:0,artifact_refs:vec![],requires_reconciliation:false,task_accepted:false};
    put(
        &mut store,
        &task.task_id,
        &format!("model-run:{}:result", packet.lease.run),
        "offline.fixture-result",
        &bytes(&result).unwrap(),
    )
    .unwrap();
    scheduler.complete_native(&packet.lease).unwrap();
    let budget = store.journal().model_budget(&task.task_id).unwrap();
    assert!(
        crate::preparation::activate_pending(&mut store, &task.task_id, &mut scheduler).unwrap()
    );
    let after = scheduler.state().unwrap();
    assert_eq!(after.plan.revision, 2);
    assert_eq!(after.plan.nodes.len(), 3);
    assert_eq!(after.slots["step-0"].phase, Phase::Candidate);
    assert_eq!(after.slots["step-1"].phase, Phase::Pending);
    assert_eq!(after.deadline_ms, before.deadline_ms);
    assert_eq!(store.journal().model_budget(&task.task_id).unwrap(), budget);
    assert!(
        !crate::preparation::activate_pending(&mut store, &task.task_id, &mut scheduler).unwrap()
    );
}
