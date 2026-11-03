use super::*;
use crate::recipe::Recipe;

fn backend() -> DomainRecipe {
    serde_json::from_value(json!({"kind":"backend","app":{"language":"python","entry":"server.py"},"database":null,"seed_path":null,"migrations":{},"statements":{},"public_checks":[],"private_checks":[{"id":"private-a","steps":[{"operation":{"command":"api","request":{"session":"a","method":"get","path":"/a","headers":{},"body":null,"timeout_ms":5000}},"expect":[{"operator":"equals","pointer":"/body/status","value":200}]}]},{"id":"private-b","steps":[{"operation":{"command":"api","request":{"session":"a","method":"get","path":"/b","headers":{},"body":null,"timeout_ms":5000}},"expect":[{"operator":"contains","pointer":"/body/json/answer","value":"PRIVATE_SENTINEL_82791"}]}]}],"preview":null,"private_seed_sql":null})).unwrap()
}
#[test]
fn private_oracles_never_appear_in_the_public_packet_and_change_the_contract() {
    let mut d = backend();
    d.validate().unwrap();
    let original = d.acceptance_digest().unwrap();
    assert!(
        !d.public_plan()
            .to_string()
            .contains("PRIVATE_SENTINEL_82791")
    );
    d.private_checks[1].steps[0].expect = vec![Expectation::Equals {
        pointer: "/body/json/answer".into(),
        value: json!("different private answer"),
    }];
    assert_ne!(d.acceptance_digest().unwrap(), original);
    let steps = d.private_checks[0].steps.clone();
    d.private_checks[1].steps = steps;
    assert!(d.validate().is_err(), "same test cannot count twice");
}
#[test]
fn model_operations_cannot_choose_a_foreign_service_raw_sql_or_session_lifecycle() {
    let d = backend();
    for operation in [
        Command::Start {},
        Command::Close {},
        Command::Navigate {
            page: "p".into(),
            path: "/".into(),
        },
        Command::Execute {
            connection: "c".into(),
            statement: "INSERT INTO t VALUES (1)".into(),
            parameters: vec![],
        },
    ] {
        assert!(d.validate_operation(&operation).is_err());
    }
    assert!(
        serde_json::from_value::<DomainRecipe>(json!({"kind":"backend","shell":"powershell"}))
            .is_err()
    );
}
#[test]
fn v2_program_has_disjoint_public_development_and_private_report_acceptance() {
    let mut r:Recipe=serde_json::from_value(json!({"schema_version":"melissa.cli-recipe/v2","agents":[{"profile":"melissa.code.implementer","goal":"fix","write":true}],"writable_paths":["main.py"],"protected_paths":[],"language":"python","entry":"main.py","negative_workspace":"C:/negative","checks":[{"id":"secret-a","input":71,"expected":"PRIVATE_ANSWER_92041"},{"id":"secret-b","input":72,"expected":72}],"development_checks":[{"id":"public-a","input":1,"expected":1},{"id":"public-b","input":2,"expected":2}],"report_checks":[{"operator":"equals","pointer":"/finding","value":"PRIVATE_REPORT_802"}],"max_cost_micro_usd":5000000,"wall_time_ms":1800000})).unwrap();
    r.validate().unwrap();
    assert_ne!(
        r.development_suite().unwrap().digest(),
        r.suite().unwrap().digest()
    );
    assert!(!r.public_plan().to_string().contains("PRIVATE_"));
    let original = r.acceptance_digest().unwrap();
    r.report_checks.clear();
    assert_ne!(r.acceptance_digest().unwrap(), original);
    r.development_checks.clear();
    assert!(r.validate().is_err());
}
#[test]
#[ignore = "read-only projection of retained native control evidence; no execution or replay"]
fn native_control_diagnostics() {
    use melissa_local_store::{LocalStore, StoreConfig, StoreKey};
    let dir = std::path::PathBuf::from(
        std::env::var_os("MELISSA_NATIVE_DIAGNOSTIC_STORE").expect("retained native test store"),
    );
    let store = LocalStore::open(
        &dir,
        StoreKey::from_bytes([63; 32]),
        StoreConfig {
            max_blob_bytes: 512 * 1024 * 1024,
            max_cas_bytes: 8 * 1024 * 1024 * 1024,
            ..Default::default()
        },
    )
    .unwrap();
    let parent = "task:native-cli-postgres";
    for child in crate::engine::qa_children(&store, parent).unwrap() {
        for a in store.list_artifacts(&child).unwrap() {
            if a.logical_type == "web.reply" {
                let v: Value =
                    serde_json::from_slice(&store.get_artifact(&child, &a.artifact_id).unwrap())
                        .unwrap();
                println!(
                    "{}",
                    json!({"kind":a.logical_type,"child":child,"seq":v["seq"],"ok":v["ok"],"error":v["error"],"schema_digest":v["body"]["schema_digest"]})
                );
            } else if a.logical_type == "web.final" {
                #[derive(serde::Deserialize, serde::Serialize)]
                struct Projection {
                    success: bool,
                    requires_reconciliation: bool,
                    executor_error: Option<String>,
                    worker: Option<Worker>,
                }
                #[derive(serde::Deserialize, serde::Serialize)]
                struct Worker {
                    termination: melissa_container_executor::TerminationReason,
                    exit_code: Option<i32>,
                    export_truncated: bool,
                    cleanup_confirmed: bool,
                    tree_confirmed_stopped: bool,
                    elapsed_ms: u64,
                }
                let v: Projection =
                    serde_json::from_slice(&store.get_artifact(&child, &a.artifact_id).unwrap())
                        .unwrap();
                println!(
                    "{}",
                    json!({"kind":a.logical_type,"child":child,"outcome":v})
                );
            }
        }
    }
}

// This opt-in test uses the actual pinned local worker image. It makes NO model
// requests, leaves its exact native store in a new caller-selected directory,
// and must never be reported as AI quality acceptance.
#[test]
#[ignore = "requires the pinned Docker image; preserves a fresh explicit evidence directory"]
fn native_domain_controls() {
    use crate::{
        app::{bytes, now},
        recipe::Execution,
    };
    use melissa_code_quality::source::FrozenSource;
    use melissa_core_contracts::{TaskState, wire::TaskContractV1};
    use melissa_local_store::{LocalStore, StoreConfig, StoreKey};
    use std::{
        fs,
        path::PathBuf,
        sync::{Arc, atomic::AtomicBool},
    };
    let root = PathBuf::from(
        std::env::var_os("MELISSA_NATIVE_CONTROL_OUT").expect("fresh evidence directory required"),
    )
    .components()
    .collect::<PathBuf>();
    fs::create_dir(&root).expect("never overwrite/replay native evidence");
    fs::write(
        root.join("status.json"),
        b"{\"status\":\"IN_PROGRESS\",\"model_calls\":0}",
    )
    .unwrap();
    let mut rows = vec![];
    let labels = std::env::var("MELISSA_NATIVE_CONTROL_CASES")
        .unwrap_or_else(|_| "frontend,backend,sqlite,postgres".into());
    for label in labels.split(',') {
        assert!(["frontend", "backend", "sqlite", "postgres"].contains(&label));
        let dir = root.join(label);
        fs::create_dir(&dir).unwrap();
        let source = dir.join("source");
        let negative = dir.join("negative");
        fs::create_dir(&source).unwrap();
        fs::create_dir(&negative).unwrap();
        let mut d = backend();
        match label {
            "frontend" => {
                d=serde_json::from_value(json!({"kind":"frontend","app":{"language":"static","directory":"site"},"database":null,"seed_path":null,"migrations":{},"statements":{},"private_checks":[{"id":"title","steps":[{"operation":{"command":"navigate","page":"p","path":"/"}},{"operation":{"command":"inspect","page":"p"},"expect":[{"operator":"contains","pointer":"/body/dom","value":"control-ready"}]}]},{"id":"layout","steps":[{"operation":{"command":"viewport","page":"p","width":390,"height":900}},{"operation":{"command":"navigate","page":"p","path":"/"}},{"operation":{"command":"inspect","page":"p"},"expect":[{"operator":"equals","pointer":"/body/layout/width","value":390}]}]}],"preview":{"path":"/","widths":[390,1280]},"private_seed_sql":null})).unwrap();
                for (p, text) in [(&source, "control-ready"), (&negative, "control-broken")] {
                    fs::create_dir(p.join("site")).unwrap();
                    fs::write(p.join("site").join("index.html"),format!("<!doctype html><meta name=viewport content='width=device-width'><h1>{text}</h1>")).unwrap();
                }
            }
            "backend" => {
                for (p, n) in [(&source, 17), (&negative, 0)] {
                    fs::write(p.join("server.py"),format!("from http.server import BaseHTTPRequestHandler,HTTPServer\nclass H(BaseHTTPRequestHandler):\n def do_GET(self):\n  self.send_response(200); self.end_headers(); self.wfile.write(b'{{\"n\":{n}}}')\nHTTPServer(('127.0.0.1',37641),H).serve_forever()\n")).unwrap();
                }
                for flow in &mut d.private_checks {
                    flow.steps[0].expect = vec![Expectation::Equals {
                        pointer: "/body/json/n".into(),
                        value: json!(17),
                    }];
                }
            }
            _ => {
                d=serde_json::from_value(json!({"kind":"database","app":null,"database":label,"seed_path":"seed.sql","migrations":{"rollup":{"source_path":"rollup.sql","baseline":{"kind":"initial"},"transactional":true}},"statements":{},"private_checks":[{"id":"sum","steps":[{"operation":{"command":"migration","name":"rollup"}},{"operation":{"command":"query","connection":"r","sql":"SELECT total FROM totals","parameters":[]},"expect":[{"operator":"equals","pointer":"/body/rows","value":[[42]]}]}]},{"id":"shape","steps":[{"operation":{"command":"migration","name":"rollup"}},{"operation":{"command":"query","connection":"r","sql":"SELECT total*2 FROM totals","parameters":[]},"expect":[{"operator":"equals","pointer":"/body/rows","value":[[84]]}]}]}],"preview":null,"private_seed_sql":"CREATE TABLE item(n INTEGER NOT NULL); INSERT INTO item VALUES (19),(23);"})).unwrap();
                for (p, sql) in [
                    (
                        &source,
                        "CREATE VIEW totals AS SELECT SUM(n) AS total FROM item;",
                    ),
                    (
                        &negative,
                        "CREATE VIEW totals AS SELECT 0 AS total FROM item;",
                    ),
                ] {
                    fs::write(
                        p.join("seed.sql"),
                        b"CREATE TABLE item(n INTEGER NOT NULL); INSERT INTO item VALUES (7);",
                    )
                    .unwrap();
                    fs::write(p.join("rollup.sql"), sql).unwrap();
                }
            }
        }
        if label == "frontend" {
            d.private_checks[0].steps.push(Step {
                operation: Command::Elements {
                    page: "p".into(),
                    selector: "h1".into(),
                },
                expect_error: false,
                expect: vec![Expectation::Equals {
                    pointer: "/body/elements/0/normalized_text".into(),
                    value: json!("control-ready"),
                }],
            });
        }
        d.validate().unwrap();
        let recipe:Recipe=serde_json::from_value(json!({"schema_version":"melissa.cli-recipe/v2","agents":[{"profile":d.kind.profile(),"goal":"native control only","write":true}],"writable_paths":if label=="frontend"{vec!["site/index.html"]}else if label=="backend"{vec!["server.py"]}else{vec!["rollup.sql"]},"protected_paths":[],"negative_workspace":negative,"domain":d,"max_cost_micro_usd":5000000,"wall_time_ms":1800000})).unwrap();
        recipe.validate().unwrap();
        let repo = Repository::open(&source, Default::default(), vec![]).unwrap();
        let nr = Repository::open(&negative, Default::default(), vec![]).unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let frozen = FrozenSource::capture(&repo, &cancel).unwrap();
        let nf = FrozenSource::capture(&nr, &cancel).unwrap();
        let mut t: TaskContractV1 = serde_json::from_slice(include_bytes!(
            "../../../tests/fixtures/code01/wire/task-contract-v1.json"
        ))
        .unwrap();
        t.task_id = format!("task:native-cli-{label}");
        t.input_snapshot_digest = frozen.snapshot().digest.clone();
        t.acceptance_contract_digest = recipe.acceptance_digest().unwrap();
        t.selected_profile_ids = vec![
            d.kind.profile().into(),
            "melissa.core.independent-qa".into(),
        ];
        t.allowed_effects = vec![
            "workspace.read".into(),
            "process.execute_scoped".into(),
            "verification.run_protected".into(),
            "evidence.append".into(),
            d.kind.effect().into(),
        ];
        t.limits.max_wall_time_ms = 1800000;
        t.limits.max_disk_bytes = 512 * 1024 * 1024;
        t.limits.max_output_bytes = 128 * 1024 * 1024;
        let mut store = LocalStore::open(
            &dir.join("store"),
            StoreKey::from_bytes([63; 32]),
            StoreConfig {
                max_blob_bytes: 512 * 1024 * 1024,
                max_cas_bytes: 8 * 1024 * 1024 * 1024,
                ..Default::default()
            },
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
        let e = Execution {
            recipe,
            settings_digest: crate::digest(b"native-no-model"),
            expires_at_ms: now().unwrap() + 1800000,
            negative_files: BTreeMap::new(),
            runtime_digest: None,
        };
        if label == "frontend" {
            let flow = &d.private_checks[0];
            let pass = runtime::run_flow(
                &mut store,
                &t,
                &e,
                &repo,
                &frozen,
                flow,
                true,
                None,
                cancel.clone(),
                "native-positive",
            )
            .unwrap();
            assert!(pass.success);
            let fail = runtime::run_flow(
                &mut store,
                &t,
                &e,
                &nr,
                &nf,
                flow,
                true,
                None,
                cancel.clone(),
                "native-negative",
            )
            .unwrap();
            assert!(!fail.success);
        } else {
            let result =
                runtime::final_verify(&mut store, &t, &e, &repo, &frozen, &nr, &nf, cancel.clone())
                    .unwrap();
            assert_eq!(result.receipt["passed"], 2);
            assert_eq!(result.receipt["negative_control_rejected"], true);
        }
        assert!(crate::engine::native_stop_proven(&store, &t.task_id).unwrap());
        assert_eq!(
            store
                .journal()
                .model_budget(&t.task_id)
                .unwrap()
                .known_cost_micro_usd,
            0
        );
        let row = json!({"kind":label,"status":"PASS","source_digest":frozen.snapshot().digest,"model_calls":0,"native_workers_stopped":true});
        fs::write(dir.join("result.json"), bytes(&row).unwrap()).unwrap();
        rows.push(row);
    }
    fs::write(
        root.join("result.json"),
        bytes(
            &json!({"status":"PASS","model_calls":0,"controls":rows,"AI_quality_acceptance":false}),
        )
        .unwrap(),
    )
    .unwrap();
}
