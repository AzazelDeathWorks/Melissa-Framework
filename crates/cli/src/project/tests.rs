//! Model-free discovery, approvals, real broker processes and retained evidence.
use super::*;
use crate::{
    app::{App, bytes, nonce, now},
    conversation::Mode,
};
use melissa_repository::Repository;
use serde_json::{Value, json};
use std::{
    fs,
    path::PathBuf,
    sync::{Arc, atomic::AtomicBool},
    time::Duration,
};
pub(crate) struct Fixture {
    _temp: Option<tempfile::TempDir>,
    pub(crate) root: PathBuf,
    pub(crate) app: App,
    pub(crate) source: PathBuf,
}
impl Fixture {
    pub(crate) fn new(native: bool) -> Self {
        let (temp, root) = if native && let Some(e) = std::env::var_os("MELISSA_PHASE03_EVIDENCE") {
            let p = PathBuf::from(e).join(format!("native-{}", nonce().unwrap()));
            fs::create_dir_all(&p).unwrap();
            (None, p)
        } else {
            let t = tempfile::tempdir().unwrap();
            let p = t.path().to_path_buf();
            (Some(t), p)
        };
        let source = root.join("project");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("main.py"), "print('source')\n").unwrap();
        let home = root.join("home");
        App::init(&home).unwrap();
        let app = App::open(&home).unwrap();
        let config = root.join("settings.json");
        fs::write(&config,bytes(&json!({"schema_version":"melissa.cli-settings/v1","provider_id":"offline-fixture","endpoint":"https://provider.example.invalid/v1/responses","protocol":"open_ai_responses","model":"offline-fixture","credential":{"kind":"environment","name":"MELISSA_PHASE03_NO_CREDENTIAL"},"context_tokens":1048576,"max_output_tokens":393216,"capabilities":{"tool_calling":true,"structured_output":false,"vision":false,"streaming":true},"json_object":true,"native_json_schema":false,"single_attempt":false,"rate_card":{"source":{"kind":"verified_host_rate_card","source_url":"https://provider.example.invalid/pricing","checked_at_unix_ms":1},"input_micro_usd_per_million":300000,"output_micro_usd_per_million":1200000,"cached_input_micro_usd_per_million":null,"cache_creation_micro_usd_per_million":null}})).unwrap()).unwrap();
        app.configure(&config).unwrap();
        Self {
            _temp: temp,
            root,
            app,
            source,
        }
    }
    fn task(&self, command: Command, mut environment: Environment) -> Value {
        if let Ok(image) = std::env::var("MELISSA_PHASE03_IMAGE") {
            environment.image_id = image;
        }
        self.contract_task(Contract {
            services: None,
            verification: None,
            extensions: None,
            environment,
            checks: BTreeMap::from([("test".into(), command)]),
            sync: vec![],
        })
    }
    pub(crate) fn contract_task(&self, contract: Contract) -> Value {
        let path = self.root.join(format!("draft-{}.json", nonce().unwrap()));
        let preparation = self
            .app
            .prepare(&self.source, "main.py hatasını düzelt", &path, None)
            .unwrap();
        self.app
            .create_project(
                &self.source,
                "main.py hatasını düzelt",
                Mode::Develop,
                preparation["preparation"]["adaptive_plan"].clone(),
                contract,
                5_000_000,
                3_600_000,
            )
            .unwrap()
    }
    fn running(
        &self,
        command: Command,
        environment: Environment,
    ) -> (Value, melissa_local_store::LocalStore) {
        self.running_value(self.task(command, environment))
    }
    pub(crate) fn running_value(&self, value: Value) -> (Value, melissa_local_store::LocalStore) {
        let id = value["task_id"].as_str().unwrap();
        self.app
            .approve(
                id,
                value["approval"]["request_id"].as_str().unwrap(),
                value["approval"]["digest"].as_str().unwrap(),
            )
            .unwrap();
        let mut store = self.app.store(id).unwrap();
        let t = store.journal().task(id).unwrap();
        let leased = store
            .transition_task(
                id,
                t.record.version,
                melissa_core_contracts::TaskState::Leased,
                None,
            )
            .unwrap();
        store
            .transition_task(
                id,
                leased.record.version,
                melissa_core_contracts::TaskState::Running,
                None,
            )
            .unwrap();
        (value, store)
    }
}
#[test]
fn approved_project_scope_changes_only_paths_after_pause_without_changing_contract_budget_or_deadline()
 {
    let f = Fixture::new(false);
    let v = f.task(python("pass"), Environment::default());
    let id = v["task_id"].as_str().unwrap();
    let before = f.app.show(id).unwrap();
    let delta = f
        .app
        .propose_scope(
            id,
            Default::default(),
            std::collections::BTreeSet::from(["postgres".into()]),
        )
        .unwrap();
    let hash = delta["brief"]["pending_scope"]["digest"].as_str().unwrap();
    f.app.approve_scope(id, hash).unwrap();
    let delta = f
        .app
        .propose_scope(
            id,
            std::collections::BTreeSet::from(["new.py".into()]),
            Default::default(),
        )
        .unwrap();
    f.app
        .approve_scope(
            id,
            delta["brief"]["pending_scope"]["digest"].as_str().unwrap(),
        )
        .unwrap();
    let store = f.app.store(id).unwrap();
    let e = f.app.execution(&store, id).unwrap().unwrap();
    let writes = task::effective_writes(&f.app, id, &e.recipe).unwrap();
    assert!(writes.contains("new.py"));
    assert!(!e.recipe.writable_paths.contains("new.py"));
    drop(store);
    let after = f.app.show(id).unwrap();
    for field in [
        "task_id",
        "approval",
        "model_budget",
        "expires_at_ms",
        "runtime_digest",
        "source_digest",
    ] {
        assert_eq!(before[field], after[field]);
    }
}
#[test]
#[ignore = "real immutable multi-version image plus native npm/pnpm/yarn/uv/mise adapters; no AI"]
fn native_environment_matrix_uses_project_versions_and_locked_package_managers() {
    let image =
        std::env::var("MELISSA_PHASE03_IMAGE").expect("set the verified native test image ID");
    for kind in [
        "node24", "node22", "mise22", "npm", "pnpm", "yarn", "uv", "pip", "cargo",
    ] {
        let f = Fixture::new(true);
        let mut environment = Environment {
            image_id: image.clone(),
            ..Environment::default()
        };
        environment
            .executables
            .insert("uv".into(), "/usr/local/bin/uv".into());
        environment.executables.insert(
            "pnpm".into(),
            "/opt/melissa-project-tools/node_modules/.bin/pnpm".into(),
        );
        environment.executables.insert(
            "yarn".into(),
            "/opt/melissa-project-tools/node_modules/.bin/yarn".into(),
        );
        environment
            .executables
            .insert("mise".into(), "/opt/project-mise/mise/bin/mise".into());
        let command = match kind {
            "node24" | "node22" | "mise22" => {
                let version = if kind == "node24" {
                    "24.22.0"
                } else {
                    "22.14.0"
                };
                environment.versions.insert("node".into(), version.into());
                if kind == "node22" {
                    environment
                        .executables
                        .insert("node".into(), "/opt/project-node22/bin/node".into());
                }
                environment.use_mise = kind == "mise22";
                fs::write(f.source.join(".node-version"), format!("{version}\n")).unwrap();
                fs::write(f.source.join("sum.cjs"), "exports.sum=(a,b)=>a+b;\n").unwrap();
                fs::write(f.source.join("runtime.test.cjs"),format!("const test=require('node:test'); const assert=require('node:assert/strict'); const {{sum}}=require('./sum.cjs'); test('project behavior and selected version',()=>{{assert.equal(sum(19,23),42); assert.equal(process.versions.node,{version:?});}});\n")).unwrap();
                Command {
                    executable: "node".into(),
                    argv: vec!["--test".into(), "runtime.test.cjs".into()],
                    cwd: ".".into(),
                }
            }
            "npm" | "pnpm" | "yarn" => {
                let manager = if kind == "pnpm" {
                    ",\"packageManager\":\"pnpm@9.15.9\""
                } else if kind == "yarn" {
                    ",\"packageManager\":\"yarn@1.22.22\""
                } else {
                    ""
                };
                fs::write(f.source.join("package.json"),format!("{{\"name\":\"phase03-fixture\",\"version\":\"1.0.0\",\"scripts\":{{\"test\":\"node test.cjs\"}}{manager}}}")).unwrap();
                fs::write(
                    f.source.join("test.cjs"),
                    "if(2+2!==4)process.exit(7); console.log('native package check');\n",
                )
                .unwrap();
                match kind {"pnpm"=>fs::write(f.source.join("pnpm-lock.yaml"),"lockfileVersion: '9.0'\nsettings:\n  autoInstallPeers: true\n  excludeLinksFromLockfile: false\nimporters:\n  .: {}\n").unwrap(),"yarn"=>fs::write(f.source.join("yarn.lock"),"# yarn lockfile v1\n").unwrap(),_=>fs::write(f.source.join("package-lock.json"),r#"{"name":"phase03-fixture","version":"1.0.0","lockfileVersion":3,"requires":true,"packages":{"":{"name":"phase03-fixture","version":"1.0.0"}}}"#).unwrap()};
                Command {
                    executable: kind.into(),
                    argv: vec!["run".into(), "test".into()],
                    cwd: ".".into(),
                }
            }
            "uv" => {
                fs::write(f.source.join("pyproject.toml"),"[project]\nname='phase03-fixture'\nversion='0.1.0'\nrequires-python='>=3.13'\n[tool.uv]\npackage=false\n").unwrap();
                fs::write(f.source.join("uv.lock"),"version = 1\nrevision = 3\nrequires-python = \">=3.13\"\n[[package]]\nname = \"phase03-fixture\"\nversion = \"0.1.0\"\nsource = { virtual = \".\" }\n").unwrap();
                Command {
                    executable: "uv".into(),
                    argv: vec![
                        "run",
                        "--offline",
                        "--no-sync",
                        "--no-python-downloads",
                        "python",
                        "-c",
                        "print('uv environment checked')",
                    ]
                    .into_iter()
                    .map(String::from)
                    .collect(),
                    cwd: ".".into(),
                }
            }
            "pip" => {
                fs::write(
                    f.source.join("requirements.txt"),
                    "# no external dependencies\n",
                )
                .unwrap();
                fs::write(f.source.join("prepare_wheel.py"), r#"import hashlib, pathlib, zipfile
p=pathlib.Path('phase03_dep-0.1.0-py3-none-any.whl')
with zipfile.ZipFile(p,'w') as z:
 z.writestr('phase03_dep.py','VALUE=42\n')
 z.writestr('phase03_dep-0.1.0.dist-info/METADATA','Metadata-Version: 2.1\nName: phase03-dep\nVersion: 0.1.0\n')
 z.writestr('phase03_dep-0.1.0.dist-info/WHEEL','Wheel-Version: 1.0\nGenerator: owned-fixture\nRoot-Is-Purelib: true\nTag: py3-none-any\n')
 z.writestr('phase03_dep-0.1.0.dist-info/RECORD','')
pathlib.Path('requirements.txt').write_text('./'+p.name+' --hash=sha256:'+hashlib.sha256(p.read_bytes()).hexdigest()+'\n')
"#).unwrap();
                python(
                    "import phase03_dep,sys; assert phase03_dep.VALUE==42; assert '.melissa-project/venvs/' in sys.prefix; print('private pip dependency checked')",
                )
            }
            _ => {
                fs::create_dir(f.source.join("src")).unwrap();
                fs::write(
                    f.source.join("Cargo.toml"),
                    "[package]\nname='phase03-fixture'\nversion='0.1.0'\nedition='2024'\n",
                )
                .unwrap();
                fs::write(
                    f.source.join("Cargo.lock"),
                    "version = 4\n[[package]]\nname = \"phase03-fixture\"\nversion = \"0.1.0\"\n",
                )
                .unwrap();
                fs::write(
                    f.source.join("src/main.rs"),
                    "fn main() {}\n#[test] fn behavior() { assert_eq!(2+2,4); }\n",
                )
                .unwrap();
                environment.versions.insert("rust".into(), "1.98.1".into());
                Command {
                    executable: "cargo".into(),
                    argv: vec!["test", "--locked", "--offline"]
                        .into_iter()
                        .map(String::from)
                        .collect(),
                    cwd: ".".into(),
                }
            }
        };
        let repo = Repository::open(&f.source, Default::default(), vec![]).unwrap();
        let discovery = discovery::discover(&repo).unwrap();
        for (k, v) in &discovery.requested_versions {
            environment
                .versions
                .entry(k.clone())
                .or_insert_with(|| v.clone());
        }
        let mut sync = discovery.sync;
        if kind == "pip" {
            sync.insert(
                0,
                Command {
                    executable: "python".into(),
                    argv: vec!["prepare_wheel.py".into()],
                    cwd: ".".into(),
                },
            );
        }
        let contract = Contract {
            services: None,
            verification: None,
            extensions: None,
            environment,
            checks: BTreeMap::from([("test".into(), command.clone())]),
            sync,
        };
        let value = f.contract_task(contract.clone());
        let (value, store) = f.running_value(value);
        let id = value["task_id"].as_str().unwrap();
        let mut runtime = runtime::Runtime::new(
            &f.app,
            id,
            "melissa.code.implementer",
            contract,
            now().unwrap() + 180_000,
        )
        .unwrap();
        let sid = runtime
            .start(
                &f.app,
                &store,
                &repo,
                command,
                "",
                Arc::new(AtomicBool::new(false)),
                false,
                false,
            )
            .unwrap();
        let result = runtime::Runtime::wait(&f.app, id, &sid, &AtomicBool::new(false)).unwrap();
        runtime.close().unwrap();
        fs::write(
            f.root.join("matrix-result.json"),
            bytes(&json!({"kind":kind,"session_id":sid,"result":result})).unwrap(),
        )
        .unwrap();
        assert_eq!(
            result["status"],
            "PASS",
            "{kind}: {result}; retained {}",
            f.root.display()
        );
        assert!(store.journal().model_requests(id).unwrap().is_empty());
        println!("native matrix PASS: {kind}");
    }
}
fn python(code: &str) -> Command {
    Command {
        executable: "python".into(),
        argv: vec!["-c".into(), code.into()],
        cwd: ".".into(),
    }
}
#[test]
fn mailbox_reopen_rejects_corruption_and_never_accepts_lost_or_expired_owners() {
    let f = Fixture::new(false);
    let v = f.task(python("pass"), Environment::default());
    let id = v["task_id"].as_str().unwrap();
    let sid = nonce().unwrap();
    let d = disk::Disk::open(&f.app, id, &sid, true).unwrap();
    let mut state = disk::State {
        private_input: false,
        expires_at_ms: now().unwrap() + 60_000,
        task_id: id.into(),
        session_id: sid.clone(),
        invocation_id: "fixture".into(),
        source_digest: "fixture-source".into(),
        environment_digest: "fixture-env".into(),
        owner: crate::control::ProcessIdentity { pid: 0, created: 0 },
        phase: "preparing".into(),
        events: 0,
        chain: String::new(),
        controls: 0,
        command_started: false,
        stdout_bytes: 0,
        stderr_bytes: 0,
        result: None,
    };
    d.write("state.enc", &state, false).unwrap();
    drop(d);
    let reopened = App::open(&f.app.home).unwrap();
    let lost = reopened.project_poll(id, &sid, 0, 4096).unwrap();
    assert_eq!(lost["execution_status"], "INCONCLUSIVE");
    assert_eq!(lost["requires_reconciliation"], true);
    assert_eq!(lost["task_accepted"], false);
    assert!(
        reopened
            .project_input(id, &sid, "lost", "stdin", "00")
            .is_err()
    );
    let d = disk::Disk::open(&f.app, id, &sid, true).unwrap();
    state.owner = crate::control::ProcessIdentity::current().unwrap();
    state.expires_at_ms = now().unwrap() - 1;
    d.write("state.enc", &state, true).unwrap();
    d.event(json!({"event":"fixture-output","value":"retained"}))
        .unwrap();
    drop(d);
    assert_eq!(
        reopened.project_poll(id, &sid, 0, 4096).unwrap()["execution_status"],
        "INCONCLUSIVE"
    );
    let event = f
        .app
        .home
        .join("tasks")
        .join(id)
        .join("project-sessions")
        .join(&sid)
        .join("event-00000000000000000000.enc");
    let mut cipher = fs::read(&event).unwrap();
    let last = cipher.len() - 1;
    cipher[last] ^= 1;
    fs::write(event, cipher).unwrap();
    assert!(reopened.project_poll(id, &sid, 0, 4096).is_err());
}
#[test]
fn manifests_select_real_manager_locks_versions_and_monorepo_commands_without_running_code() {
    let f = Fixture::new(false);
    fs::write(f.source.join("package.json"),r#"{"packageManager":"pnpm@9.15.9","scripts":{"test":"node test.js","build":"node build.js"},"engines":{"node":">=22 <25"}}"#).unwrap();
    fs::write(f.source.join("pnpm-lock.yaml"), "lockfileVersion: '9.0'\n").unwrap();
    fs::write(f.source.join(".node-version"), "22.14.0\n").unwrap();
    fs::write(f.source.join(".env"), "NEVER_READ=private-fixture").unwrap();
    let repo = Repository::open(&f.source, Default::default(), vec![]).unwrap();
    let d = discovery::discover(&repo).unwrap();
    assert_eq!(d.requested_versions["node"], "22.14.0");
    assert_eq!(d.requested_versions["pnpm"], "9.15.9");
    assert_eq!(d.commands["pnpm-test"].argv, vec!["run", "test"]);
    assert_eq!(
        d.sync[0].argv,
        vec!["install", "--offline", "--frozen-lockfile"]
    );
    assert!(!d.manifests.contains_key(".env"));
    let explicit = d
        .contract_with_checks(
            Environment::default(),
            Some(BTreeMap::from([("custom".into(), python("pass"))])),
        )
        .unwrap();
    assert_eq!(explicit.environment.versions["node"], "22.14.0");
    assert_eq!(explicit.environment.versions["pnpm"], "9.15.9");
    fs::write(f.source.join("yarn.lock"), "").unwrap();
    assert!(discovery::discover(&repo).unwrap().issues.is_empty());
    fs::write(
        f.source.join("package.json"),
        r#"{"scripts":{"test":"node test.js"}}"#,
    )
    .unwrap();
    assert!(!discovery::discover(&repo).unwrap().issues.is_empty());
    assert!(
        discovery::discover(&repo)
            .unwrap()
            .contract_with_checks(
                Environment::default(),
                Some(BTreeMap::from([("custom".into(), python("pass"))]))
            )
            .is_err()
    );
}
#[test]
fn empty_new_project_has_zero_initial_paths_and_keeps_reviewable_scope_on_same_task() {
    let f = Fixture::new(false);
    let root = f.root.join("empty");
    fs::create_dir(&root).unwrap();
    let goal = "Create a new Python application";
    let draft = f
        .app
        .prepare(&root, goal, &f.root.join("empty-plan.json"), None)
        .unwrap();
    let v = f
        .app
        .create_project(
            &root,
            goal,
            Mode::Develop,
            draft["preparation"]["adaptive_plan"].clone(),
            Contract {
                services: None,
                verification: None,
                extensions: None,
                environment: Environment::default(),
                checks: BTreeMap::from([("test".into(), python("pass"))]),
                sync: vec![],
            },
            5_000_000,
            3_600_000,
        )
        .unwrap();
    let id = v["task_id"].as_str().unwrap();
    let store = f.app.store(id).unwrap();
    let e = f.app.execution(&store, id).unwrap().unwrap();
    assert!(e.recipe.writable_paths.is_empty());
    assert!(e.recipe.agents.iter().any(|s| s.write));
    assert!(
        task::effective_writes(&f.app, id, &e.recipe)
            .unwrap()
            .is_empty()
    );
    drop(store);
    let delta = f
        .app
        .propose_scope(
            id,
            std::collections::BTreeSet::from(["main.py".into()]),
            Default::default(),
        )
        .unwrap();
    f.app
        .approve_scope(
            id,
            delta["brief"]["pending_scope"]["digest"].as_str().unwrap(),
        )
        .unwrap();
    assert_eq!(
        task::effective_writes(&f.app, id, &e.recipe).unwrap(),
        std::collections::BTreeSet::from(["main.py".into()])
    );
    assert_eq!(v["model_budget"], f.app.show(id).unwrap()["model_budget"]);
}
#[test]
fn readonly_project_role_rejects_raw_commands_before_creating_a_session() {
    use melissa_model_gateway::tools::ToolDispatcher;
    let f = Fixture::new(false);
    let v = f.task(python("pass"), Environment::default());
    let id = v["task_id"].as_str().unwrap();
    let store = f.app.store(id).unwrap();
    let contract = f
        .app
        .execution(&store, id)
        .unwrap()
        .unwrap()
        .recipe
        .project
        .unwrap();
    let dispatcher = tools::ProjectTools::new(
        &f.app,
        &store,
        id,
        "melissa.code.test-engineer",
        Arc::new(Repository::open(&f.source, Default::default(), vec![]).unwrap()),
        contract,
        now().unwrap() + 60_000,
        false,
    )
    .unwrap();
    let mut call = melissa_model_gateway::ToolCall {
        id: "read-only".into(),
        name: "project_process".into(),
        arguments: json!({"operation":"start","command":python("pass")}).to_string(),
    };
    assert!(dispatcher.validate_call(&call).is_err());
    call.arguments = json!({"operation":"check","name":"test"}).to_string();
    dispatcher.validate_call(&call).unwrap();
    assert!(
        !f.app
            .home
            .join("tasks")
            .join(id)
            .join("project-sessions")
            .exists()
    );
}
#[test]
fn per_project_environment_reopens_isolated_and_recipe_has_no_private_oracle_or_quality_cut() {
    let f = Fixture::new(false);
    let mut e = Environment::default();
    e.versions.insert("python".into(), "3.13.5".into());
    e.limits.wall_time_ms = 3_600_001;
    e.limits.cpu_time_ms = 3_600_001;
    f.app.save_environment(&f.source, e.clone()).unwrap();
    let reopened = App::open(&f.app.home).unwrap();
    assert_eq!(
        reopened.project_environment(&f.source).unwrap().versions,
        e.versions
    );
    let other = f.root.join("other");
    fs::create_dir(&other).unwrap();
    assert!(
        reopened
            .project_environment(&other)
            .unwrap()
            .versions
            .is_empty()
    );
    let value = f.task(python("pass"), e);
    assert_eq!(value["execution_plan"]["private_oracle"], false);
    assert!(value["execution_plan"]["project"].is_object());
    let id = value["task_id"].as_str().unwrap();
    let store = f.app.store(id).unwrap();
    let execution = f.app.execution(&store, id).unwrap().unwrap();
    assert_eq!(execution.recipe.schema_version, "melissa.cli-recipe/v3");
    assert!(execution.negative_files.is_empty());
    assert_eq!(
        store
            .journal()
            .task(id)
            .unwrap()
            .contract
            .limits
            .max_output_tokens,
        i64::MAX as u64
    );
    assert!(store.journal().model_requests(id).unwrap().is_empty());
}
#[tokio::test]
async fn unapproved_native_operations_do_not_resolve_model_keys_or_start_workers() {
    let f = Fixture::new(false);
    let v = f.task(python("pass"), Environment::default());
    let id = v["task_id"].as_str().unwrap();
    assert!(
        f.app
            .run_project_operation(id, "preflight", None)
            .await
            .is_err()
    );
    assert!(
        !f.app
            .home
            .join("tasks")
            .join(id)
            .join("project-sessions")
            .exists()
    );
    assert_eq!(f.app.show(id).unwrap()["state"], "awaiting_approval");
}
#[test]
#[ignore = "real isolated Docker process sessions; no model or paid provider"]
fn native_project_stdio_cancellation_and_foreign_task_isolation() {
    let a = Fixture::new(true);
    let b = Fixture::new(true);
    let command = python(
        "import sys; print('ready',flush=True); value=sys.stdin.buffer.read(); sys.stdout.buffer.write(b'echo:'+value);sys.stdout.flush(); print('stderr-kept',file=sys.stderr,flush=True)",
    );
    let (ta, sa) = a.running(command.clone(), Environment::default());
    let (tb, sb) = b.running(command.clone(), Environment::default());
    let aid = ta["task_id"].as_str().unwrap();
    let bid = tb["task_id"].as_str().unwrap();
    let repo_a = Repository::open(&a.source, Default::default(), vec![]).unwrap();
    let repo_b = Repository::open(&b.source, Default::default(), vec![]).unwrap();
    let ca = a
        .app
        .execution(&sa, aid)
        .unwrap()
        .unwrap()
        .recipe
        .project
        .unwrap();
    let cb = b
        .app
        .execution(&sb, bid)
        .unwrap()
        .unwrap()
        .recipe
        .project
        .unwrap();
    let mut ra = runtime::Runtime::new(
        &a.app,
        aid,
        "melissa.code.implementer",
        ca,
        now().unwrap() + 120_000,
    )
    .unwrap();
    let mut rb = runtime::Runtime::new(
        &b.app,
        bid,
        "melissa.code.implementer",
        cb,
        now().unwrap() + 120_000,
    )
    .unwrap();
    let sid_a = ra
        .start(
            &a.app,
            &sa,
            &repo_a,
            command.clone(),
            "",
            Arc::new(AtomicBool::new(false)),
            false,
            false,
        )
        .unwrap();
    let sid_b = rb
        .start(
            &b.app,
            &sb,
            &repo_b,
            command,
            "",
            Arc::new(AtomicBool::new(false)),
            false,
            false,
        )
        .unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(40);
    loop {
        let pa = a.app.project_poll(aid, &sid_a, 0, 32768).unwrap();
        let pb = b.app.project_poll(bid, &sid_b, 0, 32768).unwrap();
        if pa["state"]["command_started"] == true && pb["state"]["command_started"] == true {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "{pa}\n{pb}");
        std::thread::sleep(Duration::from_millis(40));
    }
    assert!(
        a.app
            .project_input(aid, &sid_b, "foreign", "cancel", "")
            .is_err()
    );
    a.app
        .project_input(aid, &sid_a, "cancel-a", "cancel", "")
        .unwrap();
    b.app
        .project_input(bid, &sid_b, "input-once", "stdin", "68656c6c6f0a")
        .unwrap();
    assert_eq!(
        b.app
            .project_input(bid, &sid_b, "input-once", "stdin", "68656c6c6f0a")
            .unwrap()["duplicate"],
        true
    );
    b.app
        .project_input(bid, &sid_b, "input-eof", "eof", "")
        .unwrap();
    let result_a = runtime::Runtime::wait(&a.app, aid, &sid_a, &AtomicBool::new(false)).unwrap();
    let result_b = runtime::Runtime::wait(&b.app, bid, &sid_b, &AtomicBool::new(false)).unwrap();
    assert_eq!(result_b["status"], "PASS", "{result_b}");
    assert_ne!(result_a["status"], "PASS");
    assert_eq!(result_a["tree_confirmed_stopped"], true);
    assert_eq!(result_b["tree_confirmed_stopped"], true);
    let output = b.app.project_poll(bid, &sid_b, 0, 256 * 1024).unwrap();
    assert!(output.to_string().contains("6563686f3a68656c6c6f0a"));
    assert!(output.to_string().contains("7374646572722d6b6570740a"));
    assert!(
        output["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["value"]["event"] == "input_delivered_to_pipe"
                && e["value"]["request_id"] == "input-once")
    );
    assert!(
        output["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["value"]["event"] == "input_eof" && e["value"]["request_id"] == "input-eof")
    );
    assert_eq!(
        a.app
            .project_input(aid, &sid_a, "cancel-again", "cancel", "")
            .unwrap()["already_stopped"],
        true
    );
    ra.close().unwrap();
    rb.close().unwrap();
    assert!(crate::engine::native_stop_proven(&sa, aid).unwrap());
    assert!(crate::engine::native_stop_proven(&sb, bid).unwrap());
    assert!(sa.journal().model_requests(aid).unwrap().is_empty());
    assert!(sb.journal().model_requests(bid).unwrap().is_empty());
}
#[test]
#[ignore = "real selected worker version mismatch; no model or paid provider"]
fn native_project_version_mismatch_is_blocked_and_command_never_runs() {
    let f = Fixture::new(true);
    let mut env = Environment::default();
    env.versions.insert("python".into(), "9.99.99".into());
    let command = python("raise RuntimeError('this command must never run')");
    let (t, store) = f.running(command.clone(), env);
    let id = t["task_id"].as_str().unwrap();
    let contract = f
        .app
        .execution(&store, id)
        .unwrap()
        .unwrap()
        .recipe
        .project
        .unwrap();
    let repo = Repository::open(&f.source, Default::default(), vec![]).unwrap();
    let mut r = runtime::Runtime::new(
        &f.app,
        id,
        "melissa.code.implementer",
        contract,
        now().unwrap() + 120_000,
    )
    .unwrap();
    let sid = r
        .start(
            &f.app,
            &store,
            &repo,
            command,
            "",
            Arc::new(AtomicBool::new(false)),
            false,
            false,
        )
        .unwrap();
    let result = runtime::Runtime::wait(&f.app, id, &sid, &AtomicBool::new(false)).unwrap();
    r.close().unwrap();
    assert_eq!(result["status"], "BLOCKED", "{result}");
    assert_eq!(result["command_started"], false);
    assert_eq!(result["tree_confirmed_stopped"], true);
}
#[tokio::test(flavor = "multi_thread")]
#[ignore = "real operator command and automatic preflight before any model credential; no AI"]
async fn native_operator_command_pauses_same_task_and_failed_preflight_never_contacts_provider() {
    let f = Fixture::new(true);
    let command = python("print('operator-native')");
    let v = f.task(command.clone(), Environment::default());
    let id = v["task_id"].as_str().unwrap();
    f.app
        .approve(
            id,
            v["approval"]["request_id"].as_str().unwrap(),
            v["approval"]["digest"].as_str().unwrap(),
        )
        .unwrap();
    let before = f.app.show(id).unwrap();
    let result = f
        .app
        .run_project_operation(id, "exec", Some(command))
        .await
        .unwrap();
    assert_eq!(result["result"]["status"], "PASS", "{result}");
    assert_eq!(result["state"], "paused");
    let after = f.app.show(id).unwrap();
    assert_eq!(before["model_budget"], after["model_budget"]);
    assert_eq!(before["expires_at_ms"], after["expires_at_ms"]);
    assert_eq!(f.app.live(id).unwrap().phase, "paused");
    assert!(
        f.app
            .store(id)
            .unwrap()
            .journal()
            .model_requests(id)
            .unwrap()
            .is_empty()
    );
    let bad = Fixture::new(true);
    let mut environment = Environment::default();
    environment.versions.insert("python".into(), "9.9.9".into());
    let v = bad.task(python("pass"), environment);
    let id = v["task_id"].as_str().unwrap();
    bad.app
        .approve(
            id,
            v["approval"]["request_id"].as_str().unwrap(),
            v["approval"]["digest"].as_str().unwrap(),
        )
        .unwrap();
    assert!(bad.app.run_task(id, false).await.is_err());
    let state = bad.app.show(id).unwrap();
    assert_eq!(state["state"], "blocked", "{state}");
    assert!(
        bad.app
            .store(id)
            .unwrap()
            .journal()
            .model_requests(id)
            .unwrap()
            .is_empty()
    );
    assert_eq!(state["task_accepted"], false);
}
#[test]
#[ignore = "real source-bound final project check and stdout byte paging; no AI"]
fn native_final_checks_bind_source_and_environment_and_keep_binary_output_pages() {
    let f = Fixture::new(true);
    let command = python("import sys; sys.stdout.buffer.write(bytes(range(256))*200)");
    let (v, mut store) = f.running(command, Environment::default());
    let id = v["task_id"].as_str().unwrap();
    let e = f.app.execution(&store, id).unwrap().unwrap();
    let project = e.recipe.project.unwrap();
    let task = store.journal().task(id).unwrap().contract;
    let repo = Repository::open(&f.source, Default::default(), vec![]).unwrap();
    let source =
        melissa_code_quality::source::FrozenSource::capture(&repo, &AtomicBool::new(false))
            .unwrap();
    let receipt = task::verify(
        &f.app,
        &mut store,
        &task,
        &source,
        &repo,
        &project,
        e.expires_at_ms,
        Arc::new(AtomicBool::new(false)),
    )
    .unwrap();
    assert_eq!(receipt["source_digest"], source.snapshot().digest);
    assert_eq!(receipt["private_oracle"], false);
    let sid = receipt["checks"][0]["session_id"].as_str().unwrap();
    let mut cursor = 0;
    let mut data = vec![];
    loop {
        let page = f.app.project_poll(id, sid, cursor, 16384).unwrap();
        for event in page["events"].as_array().unwrap() {
            let event = &event["value"];
            if event["event"] == "stdout" {
                let hex = event["hex"].as_str().unwrap();
                for n in (0..hex.len()).step_by(2) {
                    data.push(u8::from_str_radix(&hex[n..n + 2], 16).unwrap());
                }
            }
        }
        let next = page["next_cursor"].as_u64().unwrap();
        assert!(next >= cursor);
        cursor = next;
        if page["has_more"] == false {
            break;
        }
    }
    assert_eq!(data, (0..200).flat_map(|_| 0u8..=255).collect::<Vec<_>>());
    assert!(store.journal().model_requests(id).unwrap().is_empty());
    fs::write(f.source.join("main.py"), "changed\n").unwrap();
    assert!(
        task::verify(
            &f.app,
            &mut store,
            &task,
            &source,
            &repo,
            &project,
            e.expires_at_ms,
            Arc::new(AtomicBool::new(false))
        )
        .is_err()
    );
}

#[test]
#[ignore = "real owned Docker session and large source transfer; no AI"]
fn native_phase05_large_project_reuses_one_source_transfer_during_stdio_session() {
    let f = Fixture::new(true);
    for i in 0..1400 {
        fs::write(
            f.source.join(format!("module_{i}.py")),
            "# visible source\n",
        )
        .unwrap();
    }
    let command = python(
        "import sys,pathlib; print('ready',flush=True); sys.stdin.buffer.read(); print(len(list(pathlib.Path('.').glob('module_*.py'))),flush=True)",
    );
    let (v, store) = f.running(command.clone(), Environment::default());
    let id = v["task_id"].as_str().unwrap();
    let contract = f
        .app
        .execution(&store, id)
        .unwrap()
        .unwrap()
        .recipe
        .project
        .unwrap();
    let repo = Repository::open(&f.source, Default::default(), vec![]).unwrap();
    let mut runtime = runtime::Runtime::new(
        &f.app,
        id,
        "melissa.code.implementer",
        contract,
        now().unwrap() + 120_000,
    )
    .unwrap();
    let sid = runtime
        .start(
            &f.app,
            &store,
            &repo,
            command,
            "phase05-session",
            Arc::new(AtomicBool::new(false)),
            false,
            false,
        )
        .unwrap();
    for _ in 0..3 {
        f.app.project_poll(id, &sid, 0, 32 * 1024).unwrap();
    }
    f.app
        .project_input(id, &sid, "phase05-eof", "eof", "")
        .unwrap();
    let result = runtime::Runtime::wait(&f.app, id, &sid, &AtomicBool::new(false)).unwrap();
    runtime.close().unwrap();
    assert_eq!(result["status"], "PASS", "{result}");
    let events = f.app.project_poll(id, &sid, 0, 256 * 1024).unwrap();
    let prepared: Vec<_> = events["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["value"]["event"] == "prepared")
        .collect();
    assert_eq!(prepared.len(), 1);
    assert_eq!(prepared[0]["value"]["materialization"]["files"], 1401);
    assert!(events.to_string().contains("313430300a")); // Actual worker stdout: 1400 newline.
    assert!(store.journal().model_requests(id).unwrap().is_empty());
    let evidence = json!({"status":result["status"],"source_transfers":prepared.len(),"polls_without_retransfer":3,"materialization":prepared[0]["value"]["materialization"],"paid_calls":0});
    fs::write(
        f.root.join("phase05-session-result.json"),
        bytes(&evidence).unwrap(),
    )
    .unwrap();
    println!("NATIVE_SESSION_REUSE {evidence}");
}
