use super::host::{Host, Operation};
use super::*;
use crate::{
    app::{bytes, decode, nonce, now},
    project::{Command, Contract, Environment, tests::Fixture},
};
use melissa_container_executor::CreationBackend;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    sync::{Arc, atomic::AtomicBool},
};
fn command(args: &[&str]) -> Command {
    Command {
        executable: "python".into(),
        argv: args.iter().map(|s| s.to_string()).collect(),
        cwd: ".".into(),
    }
}
fn fixtures(f: &Fixture) {
    fs::write(f.source.join("api.py"), include_str!("fixture_api.py")).unwrap();
    for (name, sql) in [
        (
            "v1.sql",
            "CREATE TABLE items(id INTEGER PRIMARY KEY,name TEXT NOT NULL);",
        ),
        (
            "seed.sql",
            "INSERT INTO items(id,name) VALUES(7,'synthetic-record');",
        ),
        (
            "up.sql",
            "ALTER TABLE items ADD COLUMN score INTEGER NOT NULL DEFAULT 0;",
        ),
        ("down.sql", "ALTER TABLE items DROP COLUMN score;"),
        (
            "bad.sql",
            "ALTER TABLE items ADD COLUMN broken INTEGER; INSERT INTO missing_table VALUES(1);",
        ),
        ("insert.sql", "INSERT INTO items(id,name) VALUES(?,?);"),
    ] {
        fs::write(f.source.join(name), sql).unwrap();
    }
    let item = json!({"type":"object","additionalProperties":false,"required":["id","name"],"properties":{"id":{"type":"integer","minimum":1,"maximum":1000},"name":{"type":"string","minLength":1,"maxLength":20,"pattern":"^[a-zA-Z0-9_-]+$"}}});
    let error = json!({"description":"invalid or missing","content":{"application/json":{"schema":{"type":"object","required":["error"],"properties":{"error":{"type":"string"}}}}}});
    let api = json!({"openapi":"3.0.3","info":{"title":"Synthetic native API","version":"1.0.0"},"paths":{
     "/health":{"get":{"responses":{"200":{"description":"ready","content":{"application/json":{"schema":{"type":"object","required":["ready"],"properties":{"ready":{"type":"boolean"}}}}}}}}},
     "/items":{"get":{"responses":{"200":{"description":"list","content":{"application/json":{"schema":{"type":"array","items":item}}}}}},"post":{"operationId":"createItem","requestBody":{"required":true,"content":{"application/json":{"schema":item,"example":{"id":3,"name":"synthetic"}}}},"responses":{"201":{"description":"created","content":{"application/json":{"schema":item}},"links":{"lookup":{"operationId":"getItem","parameters":{"id":"$response.body#/id"}}}},"409":error,"415":error,"422":error}}},
     "/items/{id}":{"get":{"operationId":"getItem","parameters":[{"in":"path","name":"id","required":true,"schema":{"type":"integer","minimum":1,"maximum":1000}}],"responses":{"200":{"description":"found","content":{"application/json":{"schema":item}}},"404":error,"422":error}}}
    }});
    fs::write(f.source.join("openapi.json"), bytes(&api).unwrap()).unwrap();
    fs::write(f.source.join("test_business.py"),r#"import os,json,unittest,urllib.request,urllib.error
class Business(unittest.TestCase):
 def test_persistence_and_invalid_input(self):
  origin=json.loads(os.environ['MELISSA_SERVICE_ORIGINS'])['api']
  req=urllib.request.Request(origin+'/items',data=b'{"id":99,"name":"synthetic-business"}',headers={'content-type':'application/json'},method='POST')
  with urllib.request.urlopen(req) as response:self.assertEqual(response.status,201)
  with urllib.request.urlopen(origin+'/items/99') as response:self.assertEqual(json.load(response),{'id':99,'name':'synthetic-business'})
  req=urllib.request.Request(origin+'/items',data=b'{"id":"invalid","name":false}',headers={'content-type':'application/json'},method='POST')
  with self.assertRaises(urllib.error.HTTPError) as error:urllib.request.urlopen(req)
  self.assertEqual(error.exception.code,422)
"#).unwrap();
}
fn spec(engine: melissa_web_development::plan::Database) -> Spec {
    Spec {
        backend: CreationBackend::Compose,
        services: BTreeMap::from([
            (
                "api".into(),
                Service {
                    command: command(&["-I", "api.py"]),
                    port: 38171,
                    ready: Ready {
                        path: "/health".into(),
                        status: 200,
                        timeout_ms: 20000,
                    },
                    depends_on: vec!["database".into()],
                },
            ),
            (
                "sidecar".into(),
                Service {
                    command: command(&["-I", "api.py"]),
                    port: 38172,
                    ready: Ready {
                        path: "/health".into(),
                        status: 200,
                        timeout_ms: 20000,
                    },
                    depends_on: vec!["api".into()],
                },
            ),
        ]),
        database: Some(Data {
            engine,
            schema: Some("v1.sql".into()),
            sample: Some("seed.sql".into()),
            provenance: "synthetic".into(),
        }),
        statements: BTreeMap::from([("insert".into(), "insert.sql".into())]),
        api_checks: BTreeMap::from([(
            "contract".into(),
            ApiCheck {
                service: "api".into(),
                schema: "openapi.json".into(),
                seed: 42,
                examples: 8,
                phases: all_phases(),
            },
        )]),
        business_checks: vec!["business".into()],
        ..Default::default()
    }
}
fn contract(spec: Spec) -> Contract {
    let mut environment = Environment {
        image_id: std::env::var("MELISSA_PHASE07_IMAGE")
            .unwrap_or_else(|_| melissa_web_development::plan::WEB_IMAGE.into()),
        ..Default::default()
    };
    environment.limits.memory_bytes = 1024 * 1024 * 1024;
    environment.limits.disk_bytes = 512 * 1024 * 1024;
    environment.limits.output_bytes = 16 * 1024 * 1024;
    Contract {
        services: Some(spec),
        verification: Some(crate::project_verification::Spec {
            checks: BTreeMap::from([(
                "business".into(),
                crate::project_verification::Check {
                    kind: crate::project_verification::Kind::Test,
                    repetitions: 1,
                    report: None,
                },
            )]),
            ..Default::default()
        }),
        extensions: None,
        environment,
        checks: BTreeMap::from([(
            "business".into(),
            command(&["-I", "-m", "unittest", "discover", "-p", "test_business.py"]),
        )]),
        sync: vec![],
    }
}
fn call(host: &mut Host, operation: Operation) -> Value {
    let sid = host.session_id().map(str::to_string);
    match host.run(
        operation,
        sid.as_deref(),
        &format!("services:{}", nonce().unwrap()),
        Arc::new(AtomicBool::new(false)),
    ) {
        Ok(value) => value,
        Err(error) => {
            if let Some(id) = host.session_id() {
                let app = crate::app::App::open(&host.home).unwrap();
                let page = app.project_poll(&host.task, id, 0, 256 * 1024).unwrap();
                fs::write(
                    host.home.parent().unwrap().join("service-failure.json"),
                    bytes(&page).unwrap(),
                )
                .unwrap();
                println!(
                    "NATIVE_SERVICE_FAILURE {} {}",
                    host.home.parent().unwrap().display(),
                    page["state"]["result"]
                );
            }
            panic!("{error}")
        }
    }
}
fn detail(host: &Host, receipt: &Value) -> Value {
    decode(
        &host
            .store
            .get_artifact(&host.task, receipt["detail_artifact"].as_str().unwrap())
            .unwrap(),
    )
    .unwrap()
}
#[test]
fn invalid_service_scope_ports_cycles_production_data_and_unknown_sql_are_denied() {
    let mut s = spec(melissa_web_development::plan::Database::Sqlite);
    let c = contract(s.clone());
    s.validate(&c).unwrap();
    s.services.get_mut("api").unwrap().port = 38172;
    assert!(s.validate(&c).is_err());
    let mut s = spec(melissa_web_development::plan::Database::Sqlite);
    s.database.as_mut().unwrap().provenance = "production".into();
    assert!(s.validate(&c).is_err());
    let mut s = spec(melissa_web_development::plan::Database::Sqlite);
    s.services.get_mut("api").unwrap().depends_on = vec!["sidecar".into()];
    assert!(s.validate(&c).is_err());
    assert!(
        Operation::Database {
            command: melissa_web_development::protocol::Command::Migration {
                name: "arbitrary".into()
            }
        }
        .validate(&c.services.unwrap())
        .is_err()
    );
}
#[test]
#[ignore = "real Compose/native broker and API plus DB readiness, query and business tests; no model"]
fn native_compose_bundle_runs_two_services_and_sqlite_with_owned_logs_and_cleanup() {
    let f = Fixture::new(true);
    fixtures(&f);
    let c = contract(spec(melissa_web_development::plan::Database::Sqlite));
    let v = f.contract_task(c.clone());
    let (v, store) = f.running_value(v);
    let id = v["task_id"].as_str().unwrap();
    let mut host = Host::new(
        &f.app,
        &store,
        id,
        "melissa.code.implementer",
        c,
        now().unwrap() + 300000,
    )
    .unwrap();
    let start = call(&mut host, Operation::Start);
    fs::write(f.root.join("service-start.json"), bytes(&start).unwrap()).unwrap();
    println!("NATIVE_SERVICE_START {start}");
    assert_eq!(start["outcome"], "PASS");
    assert_eq!(start["body"]["services"]["api"]["ready"], true);
    assert_eq!(start["body"]["services"]["sidecar"]["ready"], true);
    let query = call(
        &mut host,
        Operation::Database {
            command: melissa_web_development::protocol::Command::Query {
                connection: "read".into(),
                sql: "SELECT name FROM items WHERE id=7".into(),
                parameters: vec![],
            },
        },
    );
    assert_eq!(query["body"]["rows"], json!([["synthetic-record"]]));
    let business = call(
        &mut host,
        Operation::Business {
            name: "business".into(),
        },
    );
    println!("NATIVE_BUSINESS {business}");
    assert_eq!(business["outcome"], "PASS");
    let logs = call(
        &mut host,
        Operation::Log {
            service: "api".into(),
            stream: "stderr".into(),
            offset: 0,
            max_bytes: 32768,
        },
    );
    assert!(
        !detail(&host, &logs)["body"]["data_hex"]
            .as_str()
            .unwrap()
            .is_empty()
    );
    let closed = call(&mut host, Operation::Close);
    assert_eq!(closed["native_result"]["tree_confirmed_stopped"], true);
    assert!(crate::engine::native_stop_proven(&store, id).unwrap());
    assert!(store.journal().model_requests(id).unwrap().is_empty());
    println!("NATIVE_SERVICE_EVIDENCE {}", f.root.display());
}

#[test]
#[ignore = "real Schemathesis positive/negative/stateful requests, failing then fixed captured API; no model"]
fn native_api_contract_finds_invalid_request_acceptance_and_preserves_failure_after_fix() {
    let f = Fixture::new(true);
    fixtures(&f);
    let good = fs::read_to_string(f.source.join("api.py")).unwrap();
    fs::write(
        f.source.join("api.py"),
        good.replace("BUG = False", "BUG = True"),
    )
    .unwrap();
    let c = contract(spec(melissa_web_development::plan::Database::Sqlite));
    let v = f.contract_task(c.clone());
    let (v, store) = f.running_value(v);
    let id = v["task_id"].as_str().unwrap();
    let mut host = Host::new(
        &f.app,
        &store,
        id,
        "melissa.code.implementer",
        c,
        now().unwrap() + 600000,
    )
    .unwrap();
    let start = call(&mut host, Operation::Start);
    assert_eq!(start["outcome"], "PASS");
    let failed = call(
        &mut host,
        Operation::Contract {
            name: "contract".into(),
        },
    );
    fs::write(
        f.root.join("contract-failed.json"),
        bytes(&detail(&host, &failed)).unwrap(),
    )
    .unwrap();
    println!("NATIVE_CONTRACT_FAILED {failed}");
    assert_eq!(failed["outcome"], "FAIL");
    assert!(
        failed["body"]["operation_counts"]["failed"]
            .as_u64()
            .unwrap()
            > 0
    );
    call(&mut host, Operation::Close);
    fs::write(f.source.join("api.py"), good).unwrap();
    call(&mut host, Operation::Start);
    let fixed = call(
        &mut host,
        Operation::Contract {
            name: "contract".into(),
        },
    );
    fs::write(
        f.root.join("contract-fixed.json"),
        bytes(&detail(&host, &fixed)).unwrap(),
    )
    .unwrap();
    println!("NATIVE_CONTRACT_FIXED {fixed}");
    assert_eq!(fixed["outcome"], "PASS");
    assert_ne!(failed["source_digest"], fixed["source_digest"]);
    assert_eq!(failed["body"]["seed"], fixed["body"]["seed"]);
    call(&mut host, Operation::Close);
    assert!(store.journal().model_requests(id).unwrap().is_empty());
    println!("NATIVE_CONTRACT_EVIDENCE {}", f.root.display());
}
fn rehearsal(engine: melissa_web_development::plan::Database) -> Rehearsal {
    Rehearsal {
        engine,
        fresh: vec!["v1.sql".into(), "up.sql".into()],
        fresh_sample: None,
        baseline: "v1.sql".into(),
        sample: "seed.sql".into(),
        upgrade: vec!["up.sql".into()],
        downgrade: Some(vec!["down.sql".into()]),
        transactional: true,
        provenance: "synthetic".into(),
        before: vec![Assertion {
            sql: "SELECT id,name FROM items ORDER BY id".into(),
            rows: vec![vec![json!(7), json!("synthetic-record")]],
        }],
        after: vec![Assertion {
            sql: "SELECT id,name,score FROM items ORDER BY id".into(),
            rows: vec![vec![json!(7), json!("synthetic-record"), json!(0)]],
        }],
    }
}
#[test]
#[ignore = "real SQLite and PostgreSQL fresh/upgrade/down/data checks, failed transactional and partial migrations; no model"]
fn native_migration_rehearsals_separate_fresh_upgrade_failure_and_rollback_capability() {
    use melissa_web_development::plan::Database;
    for engine in [Database::Sqlite, Database::Postgres] {
        let f = Fixture::new(true);
        fixtures(&f);
        let mut s = spec(engine);
        let valid = rehearsal(engine);
        let mut failing = valid.clone();
        failing.upgrade = vec!["bad.sql".into()];
        let mut partial = failing.clone();
        partial.transactional = false;
        let mut unsupported = valid.clone();
        unsupported.downgrade = None;
        fs::write(
            f.source.join("target-seed.sql"),
            "INSERT INTO items(id,name) VALUES(7,'SYNTHETIC-RECORD');",
        )
        .unwrap();
        fs::write(f.source.join("transform-up.sql"),"ALTER TABLE items ADD COLUMN score INTEGER NOT NULL DEFAULT 0; UPDATE items SET name=upper(name);").unwrap();
        fs::write(
            f.source.join("transform-down.sql"),
            "UPDATE items SET name=lower(name); ALTER TABLE items DROP COLUMN score;",
        )
        .unwrap();
        let mut transformed = valid.clone();
        transformed.fresh_sample = Some("target-seed.sql".into());
        transformed.upgrade = vec!["transform-up.sql".into()];
        transformed.downgrade = Some(vec!["transform-down.sql".into()]);
        transformed.after[0].rows[0][1] = json!("SYNTHETIC-RECORD");
        s.rehearsals = BTreeMap::from([
            ("valid".into(), valid),
            ("transformed".into(), transformed),
            ("failing".into(), failing),
            ("partial".into(), partial),
            ("unsupported".into(), unsupported),
        ]);
        let c = contract(s);
        let v = f.contract_task(c.clone());
        let (v, store) = f.running_value(v);
        let id = v["task_id"].as_str().unwrap();
        let mut host = Host::new(
            &f.app,
            &store,
            id,
            "melissa.code.implementer",
            c,
            now().unwrap() + 600000,
        )
        .unwrap();
        assert_eq!(call(&mut host, Operation::Start)["outcome"], "PASS");
        if matches!(engine, Database::Postgres) {
            let logs = call(
                &mut host,
                Operation::Log {
                    service: "database".into(),
                    stream: "stderr".into(),
                    offset: 0,
                    max_bytes: 32768,
                },
            );
            assert!(
                !detail(&host, &logs)["body"]["data_hex"]
                    .as_str()
                    .unwrap()
                    .is_empty()
            );
        }
        let mut results = vec![];
        for name in ["valid", "transformed", "failing", "partial", "unsupported"] {
            let result = call(&mut host, Operation::Migration { name: name.into() });
            println!("NATIVE_MIGRATION {engine:?} {name} {result}");
            match name {
                "valid" | "transformed" => {
                    assert_eq!(result["outcome"], "PASS");
                    assert_eq!(result["body"]["rollback"]["status"], "PASS");
                }
                "failing" => {
                    assert_eq!(result["outcome"], "FAIL");
                    assert_eq!(result["body"]["upgrade_failure"]["rolled_back"], true);
                    assert_eq!(result["body"]["failure_preserved_original"], true);
                }
                "partial" => {
                    assert_eq!(result["outcome"], "FAIL");
                    assert_eq!(result["body"]["upgrade_failure"]["rolled_back"], false);
                    assert_eq!(result["body"]["failure_preserved_original"], false);
                }
                _ => {
                    assert_eq!(result["outcome"], "PASS");
                    assert_eq!(result["body"]["rollback"]["status"], "NOT_SUPPORTED");
                }
            }
            results.push(result);
        }
        fs::write(
            f.root.join("migration-results.json"),
            bytes(&results).unwrap(),
        )
        .unwrap();
        let closed = call(&mut host, Operation::Close);
        assert_eq!(closed["native_result"]["tree_confirmed_stopped"], true);
        assert!(store.journal().model_requests(id).unwrap().is_empty());
        println!("NATIVE_MIGRATION_EVIDENCE {}", f.root.display());
    }
}

#[test]
#[ignore = "real task cancellation removes owned Compose bundle while independent PostgreSQL service keeps working; no model"]
fn native_cancellation_cannot_stop_another_task_service_or_database() {
    use melissa_web_development::plan::Database;
    let sentinel = Fixture::new(true);
    fixtures(&sentinel);
    let mut sentinel_spec = spec(Database::Postgres);
    sentinel_spec.backend = CreationBackend::Docker;
    sentinel_spec.services.remove("sidecar");
    let c = contract(sentinel_spec);
    let v = sentinel.contract_task(c.clone());
    let (v, sentinel_store) = sentinel.running_value(v);
    let sid = v["task_id"].as_str().unwrap();
    let mut other = Host::new(
        &sentinel.app,
        &sentinel_store,
        sid,
        "melissa.code.implementer",
        c,
        now().unwrap() + 300000,
    )
    .unwrap();
    assert_eq!(call(&mut other, Operation::Start)["outcome"], "PASS");
    let target = Fixture::new(true);
    fixtures(&target);
    let c = contract(spec(Database::Postgres));
    let v = target.contract_task(c.clone());
    let (v, store) = target.running_value(v);
    let id = v["task_id"].as_str().unwrap();
    let mut owned = Host::new(
        &target.app,
        &store,
        id,
        "melissa.code.implementer",
        c,
        now().unwrap() + 300000,
    )
    .unwrap();
    assert_eq!(call(&mut owned, Operation::Start)["outcome"], "PASS");
    let native_id = owned.session_id().unwrap().to_string();
    target
        .app
        .project_input(id, &native_id, "operator-cancel-owned-bundle", "cancel", "")
        .unwrap();
    owned.close().unwrap();
    assert!(crate::engine::native_stop_proven(&store, id).unwrap());
    let current = call(&mut other, Operation::Status);
    assert_eq!(current["outcome"], "PASS");
    assert_eq!(current["body"]["database"]["ready"], true);
    let rows = call(
        &mut other,
        Operation::Database {
            command: melissa_web_development::protocol::Command::Query {
                connection: "after-cancel".into(),
                sql: "SELECT id,name FROM items ORDER BY id".into(),
                parameters: vec![],
            },
        },
    );
    assert_eq!(rows["body"]["rows"], json!([[7, "synthetic-record"]]));
    assert_eq!(
        call(&mut other, Operation::Close)["native_result"]["tree_confirmed_stopped"],
        true
    );
    assert!(store.journal().model_requests(id).unwrap().is_empty());
    println!(
        "NATIVE_OWNERSHIP_PROVEN {} {}",
        target.root.display(),
        sentinel.root.display()
    );
}
#[test]
#[ignore = "real independent final gate starts API/DB, runs the service-dependent business check, closes all workers; no model"]
fn native_final_project_gate_binds_service_dependent_checks_and_rejects_failures() {
    let f = Fixture::new(true);
    fixtures(&f);
    let c = contract(spec(melissa_web_development::plan::Database::Sqlite));
    let v = f.contract_task(c.clone());
    let (v, mut store) = f.running_value(v);
    let id = v["task_id"].as_str().unwrap();
    let task = store.journal().task(id).unwrap().contract;
    let repo = melissa_repository::Repository::open(&f.source, Default::default(), vec![]).unwrap();
    let frozen =
        melissa_code_quality::source::FrozenSource::capture(&repo, &AtomicBool::new(false))
            .unwrap();
    let result = crate::project::task::verify(
        &f.app,
        &mut store,
        &task,
        &frozen,
        &repo,
        &c,
        now().unwrap() + 300000,
        Arc::new(AtomicBool::new(false)),
    )
    .unwrap();
    assert_eq!(result["all_required_checks_selected"], true);
    assert_eq!(
        result["service_checks"][1]["body"]["test_counts"]["passed"],
        1
    );
    assert_eq!(result["private_oracle"], false);
    let source = fs::read_to_string(f.source.join("api.py")).unwrap();
    fs::write(
        f.source.join("api.py"),
        source.replace("BUG = False", "BUG = True"),
    )
    .unwrap();
    let failed_source =
        melissa_code_quality::source::FrozenSource::capture(&repo, &AtomicBool::new(false))
            .unwrap();
    assert!(
        crate::project::task::verify(
            &f.app,
            &mut store,
            &task,
            &failed_source,
            &repo,
            &c,
            now().unwrap() + 300000,
            Arc::new(AtomicBool::new(false))
        )
        .is_err()
    );
    assert!(store.journal().model_requests(id).unwrap().is_empty());
    assert!(crate::engine::native_stop_proven(&store, id).unwrap());
    fs::write(
        f.root.join("service-final-verification.json"),
        bytes(&result).unwrap(),
    )
    .unwrap();
    println!("NATIVE_SERVICE_FINAL {}", f.root.display());
}
#[test]
fn service_schema_and_intents_do_not_grant_foreign_sessions_or_private_artifacts() {
    use melissa_model_gateway::{ToolCall, tools::ToolDispatcher};
    use std::sync::Mutex;
    let f = Fixture::new(false);
    fixtures(&f);
    let c = contract(spec(melissa_web_development::plan::Database::Sqlite));
    let v = f.contract_task(c.clone());
    let (v, mut store) = f.running_value(v);
    let id = v["task_id"].as_str().unwrap();
    let mut host = Host::new(
        &f.app,
        &store,
        id,
        "melissa.code.implementer",
        c,
        now().unwrap() + 120000,
    )
    .unwrap();
    let intent = format!("service:{}:intent", blake3::hash(b"unfinished"));
    crate::app::put(&mut store, id, &intent, "services.intent", b"{}").unwrap();
    assert!(matches!(
        host.run(
            Operation::Start,
            None,
            "unfinished",
            Arc::new(AtomicBool::new(false))
        ),
        Err(crate::Error::Reconciliation(_))
    ));
    assert!(matches!(
        host.run(
            Operation::Status,
            None,
            "not-started",
            Arc::new(AtomicBool::new(false))
        ),
        Err(crate::Error::Denied(_))
    ));
    assert!(
        store
            .artifact_metadata(
                id,
                &format!("service:{}:intent", blake3::hash(b"not-started"))
            )
            .is_err()
    );
    let tools = super::tools::Tools(Arc::new(Mutex::new(host)));
    for definition in tools.definitions() {
        jsonschema::validator_for(&definition.definition.parameters).unwrap();
    }
    assert!(
        tools
            .validate_call(&ToolCall {
                id: "foreign".into(),
                name: "project_services".into(),
                arguments: json!({"session_id":"foreign","command":{"operation":"status"}})
                    .to_string()
            })
            .is_err()
    );
    assert!(
        tools
            .validate_call(&ToolCall {
                id: "intent".into(),
                name: "service_result_page".into(),
                arguments: json!({"artifact_id":intent,"offset":0,"max_bytes":100}).to_string()
            })
            .is_err()
    );
    let schema = super::tools::operation_schema();
    assert!(jsonschema::is_valid(
        &schema,
        &json!({"operation":"database","command":{"command":"query","connection":"read","sql":"SELECT 1","parameters":[]}})
    ));
    assert!(!jsonschema::is_valid(
        &schema,
        &json!({"operation":"api","service":"api","request":{"url":"https://outside.invalid"}})
    ));
    assert!(store.journal().model_requests(id).unwrap().is_empty());
}

#[test]
#[ignore = "actual installed executable, PowerShell helpers and foreground session on one paused task; no model"]
fn native_operator_scripts_and_foreground_session_keep_one_task_and_accounting() {
    use std::io::Write;
    use std::process::{Command as Process, Stdio};
    let cli = std::env::var("MELISSA_PHASE07_CLI").expect("build current CLI first");
    let f = Fixture::new(true);
    fixtures(&f);
    let mut s = spec(melissa_web_development::plan::Database::Sqlite);
    s.rehearsals.insert(
        "upgrade".into(),
        rehearsal(melissa_web_development::plan::Database::Sqlite),
    );
    let c = contract(s.clone());
    let config = f.root.join("services.json");
    fs::write(&config, bytes(&s).unwrap()).unwrap();
    let configured = Process::new(&cli)
        .args([
            "--home",
            f.app.home.to_str().unwrap(),
            "--json",
            "services",
            "configure",
            "--workspace",
            f.source.to_str().unwrap(),
            "--path",
            config.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        configured.status.success(),
        "{}",
        String::from_utf8_lossy(&configured.stderr)
    );
    let v = f.contract_task(c);
    let id = v["task_id"].as_str().unwrap();
    f.app
        .approve(
            id,
            v["approval"]["request_id"].as_str().unwrap(),
            v["approval"]["digest"].as_str().unwrap(),
        )
        .unwrap();
    let before = f.app.show(id).unwrap();
    for (script, name) in [
        ("api-contract-check.ps1", "contract"),
        ("migration-rehearsal.ps1", "upgrade"),
    ] {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../scripts")
            .join(script);
        let out = Process::new("pwsh")
            .args(["-NoProfile", "-NonInteractive", "-File"])
            .arg(path)
            .args([
                "-TaskId",
                id,
                "-Name",
                name,
                "-Melissa",
                &cli,
                "-MelissaHome",
                f.app.home.to_str().unwrap(),
            ])
            .output()
            .unwrap();
        fs::write(f.root.join(format!("{name}.stdout")), &out.stdout).unwrap();
        fs::write(f.root.join(format!("{name}.stderr")), &out.stderr).unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let value: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(value["outcome"], "PASS");
        assert_eq!(value["state"], "paused");
        assert_eq!(value["task_id"], id);
    }
    let script =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/service-session.ps1");
    let mut child = Process::new("pwsh")
        .args(["-NoProfile", "-NonInteractive", "-File"])
        .arg(script)
        .args([
            "-TaskId",
            id,
            "-Melissa",
            &cli,
            "-MelissaHome",
            f.app.home.to_str().unwrap(),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"{\"operation\":\"status\"}\n/close\n")
        .unwrap();
    let out = child.wait_with_output().unwrap();
    fs::write(f.root.join("session.stdout"), &out.stdout).unwrap();
    fs::write(f.root.join("session.stderr"), &out.stderr).unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let values = serde_json::Deserializer::from_slice(&out.stdout)
        .into_iter::<Value>()
        .collect::<std::result::Result<Vec<_>, _>>()
        .unwrap();
    assert!(values.len() >= 3);
    assert_eq!(values.last().unwrap()["state"], "paused");
    let after = f.app.show(id).unwrap();
    for field in ["model_budget", "expires_at_ms", "runtime_digest"] {
        assert_eq!(before[field], after[field]);
    }
    let store = f.app.store(id).unwrap();
    assert!(store.journal().model_requests(id).unwrap().is_empty());
    assert!(crate::engine::native_stop_proven(&store, id).unwrap());
    println!("NATIVE_OPERATOR_SERVICE_HELPERS {}", f.root.display());
}

#[test]
#[ignore = "real startup failure logs and stale source denial, with proven cleanup; no model"]
fn native_failed_readiness_retains_diagnostics_and_old_services_cannot_validate_new_source() {
    let f = Fixture::new(true);
    fixtures(&f);
    let mut s = spec(melissa_web_development::plan::Database::Sqlite);
    s.services.remove("sidecar");
    s.services.get_mut("api").unwrap().command =
        command(&["-I", "-c", "raise RuntimeError('owned startup failure')"]);
    let c = contract(s);
    let v = f.contract_task(c.clone());
    let (v, store) = f.running_value(v);
    let id = v["task_id"].as_str().unwrap();
    let mut host = Host::new(
        &f.app,
        &store,
        id,
        "melissa.code.implementer",
        c,
        now().unwrap() + 120000,
    )
    .unwrap();
    let failed = call(&mut host, Operation::Start);
    assert_eq!(failed["outcome"], "FAIL");
    let raw = detail(&host, &failed);
    let hex = raw["body"]["service_logs"]["api"]["stderr_hex"]
        .as_str()
        .unwrap();
    let bytes = hex
        .as_bytes()
        .chunks(2)
        .map(|p| u8::from_str_radix(std::str::from_utf8(p).unwrap(), 16).unwrap())
        .collect::<Vec<_>>();
    assert!(String::from_utf8_lossy(&bytes).contains("owned startup failure"));
    host.close().unwrap();
    assert!(crate::engine::native_stop_proven(&store, id).unwrap());
    let g = Fixture::new(true);
    fixtures(&g);
    let c = contract(spec(melissa_web_development::plan::Database::Sqlite));
    let v = g.contract_task(c.clone());
    let (v, store) = g.running_value(v);
    let id = v["task_id"].as_str().unwrap();
    let mut host = Host::new(
        &g.app,
        &store,
        id,
        "melissa.code.implementer",
        c,
        now().unwrap() + 120000,
    )
    .unwrap();
    call(&mut host, Operation::Start);
    fs::write(g.source.join("main.py"), "print('operator change')\n").unwrap();
    assert_eq!(call(&mut host, Operation::Status)["source_current"], false);
    let sid = host.session_id().map(str::to_string);
    assert!(matches!(
        host.run(
            Operation::Business {
                name: "business".into()
            },
            sid.as_deref(),
            "stale-check",
            Arc::new(AtomicBool::new(false))
        ),
        Err(crate::Error::Conflict(_))
    ));
    assert_eq!(
        call(&mut host, Operation::Close)["native_result"]["tree_confirmed_stopped"],
        true
    );
    println!(
        "NATIVE_READINESS_STALE {} {}",
        f.root.display(),
        g.root.display()
    );
}

#[test]
#[ignore = "actual foreground service task cancellation via authenticated control mailbox; no model"]
fn native_foreground_task_cancel_stops_services_and_marks_task_cancelled() {
    use std::io::{BufRead, Read};
    use std::process::{Command as Process, Stdio};
    use std::time::{Duration, Instant};
    let cli = std::env::var("MELISSA_PHASE07_CLI").expect("build CLI");
    let f = Fixture::new(true);
    fixtures(&f);
    let c = contract(spec(melissa_web_development::plan::Database::Sqlite));
    let v = f.contract_task(c);
    let id = v["task_id"].as_str().unwrap();
    f.app
        .approve(
            id,
            v["approval"]["request_id"].as_str().unwrap(),
            v["approval"]["digest"].as_str().unwrap(),
        )
        .unwrap();
    let mut child = Process::new(cli)
        .args([
            "--home",
            f.app.home.to_str().unwrap(),
            "--json",
            "services",
            "session",
            id,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut stream = std::io::BufReader::new(stdout);
        let mut first = String::new();
        stream.read_line(&mut first).unwrap();
        let _ = tx.send(first.clone());
        let mut rest = String::new();
        stream.read_to_string(&mut rest).unwrap();
        first + &rest
    });
    let first = rx.recv_timeout(Duration::from_secs(30)).unwrap();
    let start: Value = serde_json::from_str(&first).unwrap();
    assert_eq!(start["outcome"], "PASS");
    f.app.request_control(id, "cancel").unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    while child.try_wait().unwrap().is_none() {
        assert!(Instant::now() < deadline, "cancel did not settle");
        std::thread::sleep(Duration::from_millis(30));
    }
    let status = child.wait().unwrap();
    let output = reader.join().unwrap();
    fs::write(f.root.join("cancel.stdout"), output).unwrap();
    assert!(status.success());
    let shown = f.app.show(id).unwrap();
    assert_eq!(shown["state"], "cancelled");
    let store = f.app.store(id).unwrap();
    assert!(crate::engine::native_stop_proven(&store, id).unwrap());
    assert!(store.journal().model_requests(id).unwrap().is_empty());
    println!("NATIVE_TASK_CANCEL {}", f.root.display());
}

#[test]
#[ignore = "actual missing service SDK preflight, no service command launched and no fake model failure"]
fn native_missing_contract_sdk_is_blocked_before_services_start() {
    let f = Fixture::new(true);
    fixtures(&f);
    let mut c = contract(spec(melissa_web_development::plan::Database::Sqlite));
    c.environment.image_id = melissa_web_development::plan::WEB_IMAGE.into();
    let v = f.contract_task(c.clone());
    let (v, store) = f.running_value(v);
    let id = v["task_id"].as_str().unwrap();
    let mut host = Host::new(
        &f.app,
        &store,
        id,
        "melissa.code.implementer",
        c,
        now().unwrap() + 120000,
    )
    .unwrap();
    let blocked = call(&mut host, Operation::Start);
    assert_eq!(blocked["outcome"], "BLOCKED");
    assert_eq!(blocked["requires_reconciliation"], false);
    assert_eq!(blocked["body"]["command_started"], false);
    assert!(host.session_id().is_none());
    assert!(crate::engine::native_stop_proven(&store, id).unwrap());
    assert!(store.journal().model_requests(id).unwrap().is_empty());
    println!("NATIVE_SERVICE_NOT_RUN {blocked}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "real agent tool wire for service start/API/SQL/detail paging/profile denial/close; no model"]
async fn native_agent_tool_wire_reads_api_and_sql_details_without_expanding_profile_authority() {
    use melissa_model_gateway::{
        ToolCall,
        tools::{ToolContext, ToolDispatcher},
    };
    use std::sync::Mutex;
    let f = Fixture::new(true);
    fixtures(&f);
    let c = contract(spec(melissa_web_development::plan::Database::Sqlite));
    let v = f.contract_task(c.clone());
    let (v, store) = f.running_value(v);
    let id = v["task_id"].as_str().unwrap();
    let host = Host::new(
        &f.app,
        &store,
        id,
        "melissa.code.implementer",
        c,
        now().unwrap() + 120000,
    )
    .unwrap();
    let mut tools = super::tools::Tools(Arc::new(Mutex::new(host)));
    let context = |name: &str| ToolContext {
        task_id: id.into(),
        profile_id: "melissa.code.implementer".into(),
        run_id: "service-wire-run".into(),
        request_id: "service-wire-request".into(),
        invocation_id: format!("service-wire:{name}"),
        attempt_id: format!("service-wire-attempt:{name}"),
        generation: 1,
    };
    let call = |name: &str, args: Value| ToolCall {
        id: name.into(),
        name: "project_services".into(),
        arguments: args.to_string(),
    };
    let start = tools
        .execute(
            call(
                "start",
                json!({"session_id":null,"command":{"operation":"start"}}),
            ),
            context("start"),
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .unwrap();
    assert!(start.success);
    let start: Value = serde_json::from_str(&start.content).unwrap();
    let sid = start["session_id"].as_str().unwrap();
    let api=tools.execute(call("api",json!({"session_id":sid,"command":{"operation":"api","service":"api","request":{"session":"client","method":"get","path":"/items/7","headers":{},"body":null,"timeout_ms":3000}}})),context("api"),Arc::new(AtomicBool::new(false))).await.unwrap();
    assert!(api.success);
    let api: Value = serde_json::from_str(&api.content).unwrap();
    assert_eq!(api["body"]["status"], 200);
    assert!(api["body"].get("body_hex").is_none());
    let page = tools
        .execute(
            ToolCall {
                id: "page".into(),
                name: "service_result_page".into(),
                arguments:
                    json!({"artifact_id":api["detail_artifact"],"offset":0,"max_bytes":32768})
                        .to_string(),
            },
            context("page"),
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .unwrap();
    assert!(page.success);
    let page: Value = serde_json::from_str(&page.content).unwrap();
    assert_eq!(page["complete"], true);
    let raw = page["bytes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b.as_u64().unwrap() as u8)
        .collect::<Vec<_>>();
    let detail: Value = serde_json::from_slice(&raw).unwrap();
    assert!(
        detail["body"]["body_hex"]
            .as_str()
            .unwrap()
            .contains("73796e746865746963")
    );
    let sql=tools.execute(call("sql",json!({"session_id":sid,"command":{"operation":"database","command":{"command":"query","connection":"read","sql":"SELECT count(*) FROM items","parameters":[]}}})),context("sql"),Arc::new(AtomicBool::new(false))).await.unwrap();
    assert!(sql.success);
    let sql: Value = serde_json::from_str(&sql.content).unwrap();
    assert_eq!(sql["body"]["rows"], json!([[1]]));
    let mut foreign = context("foreign");
    foreign.profile_id = "melissa.code.reviewer".into();
    assert!(
        tools
            .execute(
                call(
                    "foreign",
                    json!({"session_id":sid,"command":{"operation":"status"}})
                ),
                foreign,
                Arc::new(AtomicBool::new(false))
            )
            .await
            .is_err()
    );
    let closed = tools
        .execute(
            call(
                "close",
                json!({"session_id":sid,"command":{"operation":"close"}}),
            ),
            context("close"),
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .unwrap();
    assert!(closed.success);
    assert!(crate::engine::native_stop_proven(&store, id).unwrap());
    assert!(store.journal().model_requests(id).unwrap().is_empty());
    println!("NATIVE_AGENT_SERVICE_TOOLS {}", f.root.display());
}
