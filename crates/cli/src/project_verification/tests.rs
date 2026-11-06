use super::*;
use crate::{
    app::{bytes, now},
    project::tests::Fixture,
    project::{Command, Contract, Environment},
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    sync::{Arc, Mutex, atomic::AtomicBool},
};
fn python(args: &[&str]) -> Command {
    Command {
        executable: "python".into(),
        argv: args.iter().map(|s| s.to_string()).collect(),
        cwd: ".".into(),
    }
}
fn options(kind: Kind, path: &str, format: &str) -> Check {
    Check {
        kind,
        repetitions: 1,
        report: if path.is_empty() {
            None
        } else {
            Some(Report {
                path: path.into(),
                format: format.into(),
            })
        },
    }
}
fn runner(
    f: &Fixture,
    id: &str,
    store: &melissa_local_store::LocalStore,
    contract: Contract,
) -> runner::Runner {
    runner::Runner {
        home: f.app.home.clone(),
        task: id.into(),
        profile: "melissa.code.implementer".into(),
        contract,
        expires: now().unwrap() + 600_000,
        store: store.connect().unwrap(),
        final_check: false,
    }
}
#[test]
fn unknown_and_partial_runs_never_become_passing_validation() {
    assert_eq!(
        melissa_verification::aggregate(&[], 1),
        melissa_verification::Outcome::NotRun
    );
    assert!(!Kind::Lint.tests());
    assert!(Kind::Nextest.tests());
}
#[test]
fn legacy_contract_and_approved_configuration_keep_separate_authority() {
    let old = json!({"extensions":null,"environment":Environment::default(),"checks":{"check":python(&["-c","pass"])},"sync":[]});
    let contract: Contract = serde_json::from_value(old.clone()).unwrap();
    let serialized = serde_json::to_value(&contract).unwrap();
    assert!(serialized.get("verification").is_none());
    let mut spec = Spec::default();
    spec.after_edit.push("not-approved".into());
    assert!(spec.validate(&contract.checks).is_err());
    let f = Fixture::new(false);
    let path = f.root.join("verification.json");
    fs::write(
        &path,
        bytes(&json!({"checks":{"check":{"kind":"type","repetitions":1}},"after_edit":["check"]}))
            .unwrap(),
    )
    .unwrap();
    settings::configure(&f.app, &f.source, &path).unwrap();
    let reopened = crate::app::App::open(&f.app.home).unwrap();
    assert_eq!(
        settings::load(&reopened, &f.source)
            .unwrap()
            .unwrap()
            .after_edit,
        vec!["check"]
    );
    assert!(contract.verification.is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "real native multi-module reproduction, scoped edit, early check and regression; no AI"]
async fn native_reproduction_scoped_fix_early_checks_and_regression_keep_one_task() {
    use melissa_model_gateway::{
        ToolCall,
        tools::{ToolContext, ToolDispatcher},
    };
    use melissa_repository::{
        ByteEdit, FilePatch, PatchPolicy, PatchProposal,
        tools::{RepositoryDispatcher, RepositoryToolPolicy},
    };
    let f = Fixture::new(true);
    fs::write(f.source.join("main.py"), "def add(a,b):\n    return a-b\n").unwrap();
    fs::write(
        f.source.join("service.py"),
        "from main import add\ndef total():\n    return add(2,3)\n",
    )
    .unwrap();
    fs::create_dir(f.source.join("tests")).unwrap();
    fs::write(
        f.source.join("tests/test_total.py"),
        "from service import total\ndef test_total():\n    assert total()==5\n",
    )
    .unwrap();
    fs::write(f.source.join("pytest.ini"), "[pytest]\npythonpath = .\n").unwrap();
    fs::write(f.source.join("notes.txt"), "unrelated user work\r\n").unwrap();
    let checks = BTreeMap::from([
        (
            "syntax".into(),
            python(&["-I", "-m", "py_compile", "main.py", "service.py"]),
        ),
        (
            "tests".into(),
            python(&[
                "-I",
                "-m",
                "pytest",
                "tests/test_total.py",
                "-q",
                "--junitxml=reports/tests.xml",
            ]),
        ),
    ]);
    let spec = Spec {
        checks: BTreeMap::from([
            ("syntax".into(), options(Kind::Syntax, "", "")),
            (
                "tests".into(),
                options(Kind::Regression, "reports/tests.xml", "junit"),
            ),
        ]),
        after_edit: vec!["syntax".into()],
        reproduce: vec!["tests".into()],
        debug: None,
    };
    let contract = Contract {
        verification: Some(spec),
        extensions: None,
        environment: Environment::default(),
        checks,
        sync: vec![],
    };
    let value = f.contract_task(contract.clone());
    let (v, store) = f.running_value(value);
    let id = v["task_id"].as_str().unwrap();
    let before_budget = store.journal().model_budget(id).unwrap();
    let host = Arc::new(Mutex::new(runner(&f, id, &store, contract.clone())));
    let baseline = host
        .lock()
        .unwrap()
        .run(
            runner::Request {
                names: vec!["tests".into()],
                baseline: None,
            },
            true,
            "reproduction",
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
    assert_eq!(baseline["outcome"], "FAIL");
    assert_eq!(baseline["operation_status"], "PASS");
    assert_eq!(baseline["failure_reproduced"], true);
    assert_eq!(
        baseline["checks"][0]["attempts"][0]["test_counts"]["failed"],
        1
    );
    let cached = host
        .lock()
        .unwrap()
        .run(
            runner::Request {
                names: vec!["tests".into()],
                baseline: None,
            },
            true,
            "reproduction",
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
    assert_eq!(
        cached, baseline,
        "same invocation must return the existing receipt without launching again"
    );
    let replay = baseline.to_string();
    for private_field in [
        "raw_stdout_hex",
        "raw_stderr_hex",
        "report_hex",
        "executable",
        "argv",
    ] {
        assert!(!replay.contains(private_field));
    }
    let repo = Arc::new(
        melissa_repository::Repository::open(&f.source, Default::default(), vec![]).unwrap(),
    );
    let e = f.app.execution(&store, id).unwrap().unwrap();
    assert!(e.recipe.writable_paths.contains("main.py"));
    let task = store.journal().task(id).unwrap().contract;
    let (version, old) = repo.read("main.py").unwrap();
    let pos = String::from_utf8(old.clone()).unwrap().find("a-b").unwrap();
    let patch = PatchProposal {
        schema_version: "melissa.repository-patch/v1".into(),
        root_identity: repo.root_identity().into(),
        files: vec![FilePatch {
            before: version,
            edits: vec![ByteEdit {
                start: pos as u64,
                end: (pos + 3) as u64,
                expected: b"a-b".to_vec(),
                replacement: b"a+b".to_vec(),
            }],
        }],
    };
    let dispatcher = RepositoryDispatcher::new(
        repo.reopen().unwrap(),
        store.connect().unwrap(),
        RepositoryToolPolicy {
            read_repository: true,
            enabled_tools: ["fs_patch".into()].into_iter().collect(),
            files: PatchPolicy {
                task_id: id.into(),
                profile_id: "melissa.code.implementer".into(),
                principal_id: "cli:operator".into(),
                resource_id: task.workspace_ref.clone(),
                authorization_ref: task.authorization_refs[0].clone(),
                expires_at_ms: now().unwrap() + 600_000,
                epoch: 0,
                writable_paths: e.recipe.writable_paths.clone(),
                protected_paths: e.recipe.protected_paths.clone(),
            },
        },
    )
    .unwrap();
    let mut edited = crate::development::CheckedEdits {
        inner: Box::new(dispatcher),
        repo: repo.clone(),
        store: store.connect().unwrap(),
        task: id.into(),
        checker: None,
        project_verifier: Some(host.clone()),
        language: melissa_build_test::full::Language::Python,
        context: None,
    };
    let ctx = ToolContext {
        task_id: id.into(),
        profile_id: "melissa.code.implementer".into(),
        run_id: "phase6-run".into(),
        request_id: "phase6-request".into(),
        invocation_id: "phase6-fix".into(),
        attempt_id: "phase6-fix-attempt".into(),
        generation: 1,
    };
    let applied = edited
        .execute(
            ToolCall {
                id: "phase6-fix".into(),
                name: "fs_patch".into(),
                arguments: serde_json::to_string(&patch).unwrap(),
            },
            ctx,
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .unwrap();
    let edit: Value = serde_json::from_str(&applied.content).unwrap();
    assert_eq!(edit["mutation_applied"], true);
    assert_eq!(edit["after_edit"]["project"]["outcome"], "PASS", "{edit}");
    assert!(matches!(
        host.lock().unwrap().run(
            runner::Request {
                names: vec!["tests".into()],
                baseline: None
            },
            true,
            "reproduction",
            Arc::new(AtomicBool::new(false))
        ),
        Err(crate::Error::Conflict(_))
    ));
    let fixed = host
        .lock()
        .unwrap()
        .run(
            runner::Request {
                names: vec!["tests".into()],
                baseline: Some(baseline["receipt_id"].as_str().unwrap().into()),
            },
            false,
            "regression",
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
    assert_eq!(fixed["outcome"], "PASS", "{fixed}");
    assert_eq!(fixed["regression_confirmed"], true);
    assert_eq!(fixed["task_accepted"], false);
    let all = host
        .lock()
        .unwrap()
        .run(
            Default::default(),
            false,
            "all-final-checks",
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
    assert_eq!(all["all_required_checks_selected"], true);
    assert_eq!(all["outcome"], "PASS");
    let frozen =
        melissa_code_quality::source::FrozenSource::capture(&repo, &AtomicBool::new(false))
            .unwrap();
    let mut final_store = store.connect().unwrap();
    let final_gate = crate::project::task::verify(
        &f.app,
        &mut final_store,
        &task,
        &frozen,
        &repo,
        &contract,
        now().unwrap() + 120_000,
        Arc::new(AtomicBool::new(false)),
    )
    .unwrap();
    assert_eq!(
        final_gate["acceptance_kind"],
        "host_source_bound_project_checks"
    );
    assert!(
        final_gate["validation_receipt"]
            .as_str()
            .unwrap()
            .starts_with("verification:")
    );
    assert_eq!(final_gate["private_oracle"], false);

    assert_ne!(baseline["source_digest"], fixed["source_digest"]);
    assert_eq!(baseline["environment_digest"], fixed["environment_digest"]);
    assert_eq!(
        fs::read(f.source.join("notes.txt")).unwrap(),
        b"unrelated user work\r\n"
    );
    assert!(store.journal().model_requests(id).unwrap().is_empty());
    assert_eq!(
        serde_json::to_value(before_budget).unwrap(),
        serde_json::to_value(store.journal().model_budget(id).unwrap()).unwrap()
    );
    fs::write(
        f.root.join("repro-regression-result.json"),
        bytes(&json!({"baseline":baseline,"fixed":fixed,"final":all})).unwrap(),
    )
    .unwrap();
    println!(
        "NATIVE_REPRO_FIX {}",
        json!({"task":id,"before":"FAIL","after":"PASS","regression":true,"paid_calls":0,"evidence":f.root})
    );
}

fn rust_project(f: &Fixture) {
    fs::create_dir(f.source.join("src")).unwrap();
    fs::write(
        f.source.join("Cargo.toml"),
        "[package]\nname='fixture_math'\nversion='0.0.0'\nedition='2021'\n",
    )
    .unwrap();
    fs::write(
        f.source.join("Cargo.lock"),
        "version = 4\n[[package]]\nname='fixture_math'\nversion='0.0.0'\n",
    )
    .unwrap();
    fs::write(f.source.join("src/lib.rs"),"/// ```\n/// assert_eq!(fixture_math::add(2,3),5);\n/// ```\npub fn add(a:i32,b:i32)->i32{a+b}\n#[cfg(test)]mod tests{#[test]fn add_positive(){assert_eq!(super::add(2,3),5)}#[test]fn add_negative(){assert_eq!(super::add(-2,3),1)}}\n").unwrap();
    fs::create_dir(f.source.join(".config")).unwrap();
    fs::write(f.source.join(".config/nextest.toml"),"[profile.melissa]\nretries=0\n[profile.melissa.junit]\npath='results.xml'\nstore-success-output=true\n").unwrap();
}
fn cargo(args: &[&str]) -> Command {
    Command {
        executable: "cargo".into(),
        argv: args.iter().map(|s| s.to_string()).collect(),
        cwd: ".".into(),
    }
}
#[test]
#[ignore = "real nextest, llvm-cov and cargo-mutants in reviewed image; no AI"]
fn native_rust_nextest_coverage_mutation_and_cargo_doctest_are_separate_checks() {
    let f = Fixture::new(true);
    rust_project(&f);
    let mut environment = Environment {
        image_id: std::env::var("MELISSA_PHASE06_IMAGE").unwrap(),
        ..Default::default()
    };
    environment.limits.wall_time_ms = 600_000;
    environment.limits.cpu_time_ms = 600_000;
    let checks = BTreeMap::from([
        ("cargo".into(), cargo(&["test", "--locked", "--offline"])),
        (
            "doctest".into(),
            cargo(&["test", "--doc", "--locked", "--offline"]),
        ),
        (
            "nextest".into(),
            cargo(&[
                "nextest",
                "run",
                "--locked",
                "--offline",
                "--profile",
                "melissa",
            ]),
        ),
        (
            "coverage".into(),
            cargo(&[
                "llvm-cov",
                "--json",
                "--summary-only",
                "--output-path",
                "coverage.json",
                "--locked",
                "--offline",
            ]),
        ),
        (
            "mutation".into(),
            cargo(&[
                "mutants",
                "--in-place",
                "--file",
                "src/lib.rs",
                "--re",
                "add",
                "--timeout",
                "30",
                "--output",
                ".",
            ]),
        ),
    ]);
    let spec = Spec {
        checks: BTreeMap::from([
            ("cargo".into(), options(Kind::Test, "", "")),
            ("doctest".into(), options(Kind::Doctest, "", "")),
            (
                "nextest".into(),
                options(Kind::Nextest, "target/nextest/melissa/results.xml", "junit"),
            ),
            (
                "coverage".into(),
                options(Kind::Coverage, "coverage.json", "llvm_json"),
            ),
            (
                "mutation".into(),
                options(Kind::Mutation, "mutants.out/outcomes.json", "mutants_json"),
            ),
        ]),
        ..Default::default()
    };
    let contract = Contract {
        verification: Some(spec),
        extensions: None,
        environment,
        checks,
        sync: vec![],
    };
    let value = f.contract_task(contract.clone());
    let (v, store) = f.running_value(value);
    let id = v["task_id"].as_str().unwrap();
    let mut r = runner(&f, id, &store, contract);
    let result = r
        .run(
            Default::default(),
            false,
            "rust-quality",
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
    fs::write(
        f.root.join("rust-quality-result.json"),
        bytes(&result).unwrap(),
    )
    .unwrap();
    println!(
        "NATIVE_RUST_QUALITY {}",
        json!({"evidence":f.root,"result":result})
    );
    assert_eq!(result["outcome"], "PASS", "{result}");
    assert_eq!(result["checks"].as_array().unwrap().len(), 5);
    assert!(store.journal().model_requests(id).unwrap().is_empty());
}

#[test]
#[ignore = "real native prerequisites and skipped test inventory; no AI"]
fn native_missing_tool_and_skipped_tests_are_not_passing_acceptance() {
    for case in ["missing", "skipped"] {
        let f = Fixture::new(true);
        fs::write(f.source.join("test_skip.py"),"import pytest\n@pytest.mark.skip(reason='explicit fixture skip')\ndef test_not_run():\n    assert False\n").unwrap();
        let (command, check, expected) = if case == "missing" {
            (
                cargo(&["nextest", "run", "--offline"]),
                options(Kind::Nextest, "missing.xml", "junit"),
                "BLOCKED",
            )
        } else {
            (
                python(&[
                    "-I",
                    "-m",
                    "pytest",
                    "test_skip.py",
                    "-q",
                    "--junitxml=skipped.xml",
                ]),
                options(Kind::Test, "skipped.xml", "junit"),
                "NOT_RUN",
            )
        };
        let contract = Contract {
            verification: Some(Spec {
                checks: BTreeMap::from([("tests".into(), check)]),
                ..Default::default()
            }),
            extensions: None,
            environment: Environment::default(),
            checks: BTreeMap::from([("tests".into(), command)]),
            sync: vec![],
        };
        let value = f.contract_task(contract.clone());
        let (v, mut store) = f.running_value(value);
        let id = v["task_id"].as_str().unwrap();
        let mut r = runner(&f, id, &store, contract.clone());
        let result = r
            .run(
                Default::default(),
                false,
                "prerequisite-or-skip",
                Arc::new(AtomicBool::new(false)),
            )
            .unwrap();
        assert_eq!(result["outcome"], expected, "{result}");
        assert_eq!(result["requires_reconciliation"], false);
        let repo =
            melissa_repository::Repository::open(&f.source, Default::default(), vec![]).unwrap();
        let source =
            melissa_code_quality::source::FrozenSource::capture(&repo, &AtomicBool::new(false))
                .unwrap();
        let task = store.journal().task(id).unwrap().contract;
        assert!(
            crate::project::task::verify(
                &f.app,
                &mut store,
                &task,
                &source,
                &repo,
                &contract,
                now().unwrap() + 120_000,
                Arc::new(AtomicBool::new(false))
            )
            .is_err()
        );
        assert!(store.journal().model_requests(id).unwrap().is_empty());
        println!("NATIVE_NOT_ACCEPTED {case} {expected}");
    }
}

#[test]
#[ignore = "actual nextest failed-then-passing retry; no AI"]
fn native_nextest_recovered_retry_is_flaky_not_a_clean_pass() {
    let f = Fixture::new(true);
    rust_project(&f);
    fs::write(f.source.join("src/lib.rs"),"#[test]fn recovered_retry(){let p=std::path::Path::new(\"retry.flag\");if !p.exists(){std::fs::write(p,b\"first\").unwrap();panic!(\"first actual attempt fails\");}}\n").unwrap();
    let environment = Environment {
        image_id: std::env::var("MELISSA_PHASE06_IMAGE").unwrap(),
        ..Default::default()
    };
    let contract = Contract {
        verification: Some(Spec {
            checks: BTreeMap::from([(
                "tests".into(),
                options(Kind::Nextest, "target/nextest/melissa/results.xml", "junit"),
            )]),
            ..Default::default()
        }),
        extensions: None,
        environment,
        checks: BTreeMap::from([(
            "tests".into(),
            cargo(&[
                "nextest",
                "run",
                "--offline",
                "--profile",
                "melissa",
                "--retries",
                "1",
            ]),
        )]),
        sync: vec![],
    };
    let value = f.contract_task(contract.clone());
    let (v, store) = f.running_value(value);
    let id = v["task_id"].as_str().unwrap();
    let mut r = runner(&f, id, &store, contract);
    let result = r
        .run(
            Default::default(),
            false,
            "real-retry",
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
    assert_eq!(result["checks"][0]["attempts"][0]["command_exit_code"], 0);
    assert_eq!(result["outcome"], "FLAKY", "{result}");
    assert!(store.journal().model_requests(id).unwrap().is_empty());
    fs::write(f.root.join("flaky-result.json"), bytes(&result).unwrap()).unwrap();
    println!("NATIVE_FLAKY {}", f.root.display());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "actual general-CLI debug over the selected immutable project image; no AI"]
async fn native_project_debug_is_exposed_only_from_reviewed_config() {
    use melissa_model_gateway::{
        ToolCall,
        tools::{ToolContext, ToolDispatcher},
    };
    let f = Fixture::new(true);
    fs::write(
        f.source.join("main.py"),
        "value=1\nanswer=value+2\nprint(answer)\n",
    )
    .unwrap();
    let contract = Contract {
        verification: Some(Spec {
            debug: Some(melissa_build_test::debug::Launch {
                language: melissa_build_test::full::Language::Python,
                entry: "main.py".into(),
                cwd: ".".into(),
                argv: vec![],
                tsconfig: None,
            }),
            ..Default::default()
        }),
        extensions: None,
        environment: Environment {
            image_id: std::env::var("MELISSA_PHASE06_IMAGE").unwrap(),
            ..Default::default()
        },
        checks: BTreeMap::from([(
            "syntax".into(),
            python(&["-I", "-m", "py_compile", "main.py"]),
        )]),
        sync: vec![],
    };
    let value = f.contract_task(contract);
    let (v, store) = f.running_value(value);
    let id = v["task_id"].as_str().unwrap();
    let task = store.journal().task(id).unwrap().contract;
    let mut development = crate::project::tools::NavigationOnly(Box::new(
        crate::development::DevelopmentTools::new(
            &f.app,
            &store,
            &task,
            "melissa.code.implementer",
            now().unwrap() + 120_000,
            &["process.exec".into(), "code.diagnostics".into()]
                .into_iter()
                .collect(),
            "",
            true,
        )
        .unwrap(),
    ));
    assert!(
        development
            .definitions()
            .iter()
            .any(|d| d.definition.name == "code_debug_start")
    );
    let context = |name: &str| ToolContext {
        task_id: id.into(),
        profile_id: "melissa.code.implementer".into(),
        run_id: "debug-project-run".into(),
        request_id: "debug-project-request".into(),
        invocation_id: format!("debug:{name}"),
        attempt_id: format!("debug-attempt:{name}"),
        generation: 1,
    };
    let start = development
        .execute(
            ToolCall {
                id: "start".into(),
                name: "code_debug_start".into(),
                arguments: json!({"breakpoints":[{"path":"main.py","line":2}]}).to_string(),
            },
            context("start"),
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .unwrap();
    let value: Value = serde_json::from_str(&start.content).unwrap();
    assert!(start.success, "{value}");
    let sid = value["session_id"].as_str().unwrap();
    let close = development
        .execute(
            ToolCall {
                id: "close".into(),
                name: "code_debug_command".into(),
                arguments: json!({"session_id":sid,"operation":{"command":"close"}}).to_string(),
            },
            context("close"),
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .unwrap();
    assert!(close.success, "{}", close.content);
    assert!(store.journal().model_requests(id).unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unfinished_intent_and_wrong_artifact_never_replay_or_become_model_evidence() {
    use melissa_model_gateway::{ToolCall, tools::ToolDispatcher};
    let f = Fixture::new(false);
    let contract = Contract {
        verification: Some(Spec::default()),
        extensions: None,
        environment: Environment::default(),
        checks: BTreeMap::from([("check".into(), python(&["-c", "pass"]))]),
        sync: vec![],
    };
    let value = f.contract_task(contract.clone());
    let (v, mut store) = f.running_value(value);
    let id = v["task_id"].as_str().unwrap();
    let intent = format!("verification:{}:intent", blake3::hash(b"unfinished"));
    crate::app::put(&mut store, id, &intent, "verification.intent", b"{}").unwrap();
    let mut r = runner(&f, id, &store, contract);
    assert!(matches!(
        r.run(
            Default::default(),
            false,
            "unfinished",
            Arc::new(AtomicBool::new(false))
        ),
        Err(crate::Error::Reconciliation(_))
    ));
    let mut tools = super::tools::Tools(Arc::new(Mutex::new(r)));
    let page = ToolCall {
        id: "page".into(),
        name: "verification_result_page".into(),
        arguments: json!({"artifact_id":intent,"offset":0,"max_bytes":100}).to_string(),
    };
    assert!(tools.validate_call(&page).is_err());
    let ctx = melissa_model_gateway::tools::ToolContext {
        task_id: id.into(),
        profile_id: "melissa.code.implementer".into(),
        run_id: "unknown-run".into(),
        request_id: "unknown-request".into(),
        invocation_id: "unfinished".into(),
        attempt_id: "unknown-attempt".into(),
        generation: 1,
    };
    let unknown = tools
        .execute(
            ToolCall {
                id: "unfinished".into(),
                name: "verify_change".into(),
                arguments: "{}".into(),
            },
            ctx,
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .unwrap();
    assert!(!unknown.success);
    assert!(unknown.requires_reconciliation);

    assert!(store.journal().model_requests(id).unwrap().is_empty());
}

#[test]
#[ignore = "real executable and PowerShell helper scripts, queued approval then safe pause and same-task regression; no AI"]
fn native_operator_helpers_preserve_failure_and_same_task_after_pause() {
    use std::process::{Command as Process, Output};
    let cli = std::env::var("MELISSA_PHASE06_CLI").expect("build the current CLI first");
    let f = Fixture::new(true);
    fs::write(f.source.join("main.py"), "def add(a,b):\n    return a-b\n").unwrap();
    fs::write(
        f.source.join("test_total.py"),
        "from main import add\ndef test_total():\n    assert add(2,3)==5\n",
    )
    .unwrap();
    fs::write(f.source.join("pytest.ini"), "[pytest]\npythonpath = .\n").unwrap();
    let spec = Spec {
        checks: BTreeMap::from([("tests".into(), options(Kind::Test, "tests.xml", "junit"))]),
        reproduce: vec!["tests".into()],
        ..Default::default()
    };
    let contract = Contract {
        verification: Some(spec.clone()),
        extensions: None,
        environment: Environment::default(),
        checks: BTreeMap::from([(
            "tests".into(),
            python(&[
                "-I",
                "-m",
                "pytest",
                "test_total.py",
                "-q",
                "--junitxml=tests.xml",
            ]),
        )]),
        sync: vec![],
    };
    let config = f.root.join("verification.json");
    fs::write(&config, bytes(&spec).unwrap()).unwrap();
    let configured = Process::new(&cli)
        .args([
            "--home",
            f.app.home.to_str().unwrap(),
            "--json",
            "verification",
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
    let v = f.contract_task(contract);
    let id = v["task_id"].as_str().unwrap();
    f.app
        .approve(
            id,
            v["approval"]["request_id"].as_str().unwrap(),
            v["approval"]["digest"].as_str().unwrap(),
        )
        .unwrap();
    let before = f.app.show(id).unwrap();
    let helper = |name: &str, baseline: Option<&str>| -> Output {
        let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../scripts")
            .join(name);
        let mut command = Process::new("pwsh");
        command
            .args(["-NoProfile", "-NonInteractive", "-File"])
            .arg(script)
            .args([
                "-TaskId",
                id,
                "-Checks",
                "tests",
                "-Melissa",
                &cli,
                "-MelissaHome",
                f.app.home.to_str().unwrap(),
            ]);
        if let Some(b) = baseline {
            command.args(["-Baseline", b]);
        }
        command.output().unwrap()
    };
    let reproduction = helper("reproduce-failure.ps1", None);
    fs::write(
        f.root.join("operator-reproduce.stdout"),
        &reproduction.stdout,
    )
    .unwrap();
    fs::write(
        f.root.join("operator-reproduce.stderr"),
        &reproduction.stderr,
    )
    .unwrap();
    assert!(
        reproduction.status.success(),
        "{}",
        String::from_utf8_lossy(&reproduction.stderr)
    );
    let baseline: Value = serde_json::from_slice(&reproduction.stdout).unwrap();
    assert_eq!(baseline["result"]["outcome"], "FAIL");
    assert_eq!(baseline["state"], "paused");
    let failing = helper("verify-change.ps1", None);
    assert!(!failing.status.success());
    fs::write(f.root.join("operator-fail.stdout"), &failing.stdout).unwrap();
    fs::write(f.root.join("operator-fail.stderr"), &failing.stderr).unwrap();
    let failed: Value = serde_json::from_slice(&failing.stdout).unwrap();
    assert_eq!(failed["result"]["operation_status"], "FAIL");
    fs::write(f.source.join("main.py"), "def add(a,b):\n    return a+b\n").unwrap();
    let fixed = helper(
        "verify-change.ps1",
        Some(baseline["result"]["receipt_id"].as_str().unwrap()),
    );
    fs::write(f.root.join("operator-fixed.stdout"), &fixed.stdout).unwrap();
    fs::write(f.root.join("operator-fixed.stderr"), &fixed.stderr).unwrap();
    assert!(
        fixed.status.success(),
        "{}",
        String::from_utf8_lossy(&fixed.stderr)
    );
    let result: Value = serde_json::from_slice(&fixed.stdout).unwrap();
    assert_eq!(result["result"]["regression_confirmed"], true);
    assert_eq!(result["result"]["outcome"], "PASS");
    assert_eq!(result["state"], "paused");
    assert_eq!(result["task_id"], id);
    let after = f.app.show(id).unwrap();
    for field in ["model_budget", "expires_at_ms", "runtime_digest"] {
        assert_eq!(before[field], after[field]);
    }
    let store = f.app.store(id).unwrap();
    assert!(store.journal().model_requests(id).unwrap().is_empty());
    println!("NATIVE_OPERATOR_HELPERS {}", f.root.display());
}

#[test]
#[ignore = "real standard runner output, stale reports and physical output capacity; no AI"]
fn native_default_python_counts_and_unusable_reports_remain_explicit() {
    for case in ["pytest", "unittest", "stale", "capacity"] {
        let f = Fixture::new(true);
        fs::write(
            f.source.join("test_count.py"),
            "import warnings\ndef test_count():\n    warnings.warn(\"fixture warning\")\n    assert 2+3==5\n",
        )
        .unwrap();
        fs::write(f.source.join("test_stdlib.py"),"import unittest\nclass Count(unittest.TestCase):\n    def test_count(self):\n        self.assertEqual(2+3,5)\n").unwrap();
        let mut environment = Environment {
            executables: BTreeMap::from([(
                "python".into(),
                "/opt/melissa-python/bin/python".into(),
            )]),
            ..Default::default()
        };
        let (command, check, expected) = match case {
            "pytest" => (
                python(&["-I", "-m", "pytest", "test_count.py", "-q"]),
                options(Kind::Test, "", ""),
                "PASS",
            ),
            "unittest" => (
                python(&["-I", "-m", "unittest", "discover", "-p", "test_stdlib.py"]),
                options(Kind::Test, "", ""),
                "PASS",
            ),
            "stale" => {
                fs::write(
                    f.source.join("old.xml"),
                    "<testsuite tests='1'><testcase name='stale'/></testsuite>",
                )
                .unwrap();
                (
                    python(&["-I", "-c", "print('no report refresh')"]),
                    options(Kind::Test, "old.xml", "junit"),
                    "INCONCLUSIVE",
                )
            }
            _ => {
                environment.limits.output_bytes = 131072;
                (
                    python(&["-I", "-c", "print('x'*100000)"]),
                    options(Kind::Syntax, "", ""),
                    "BLOCKED",
                )
            }
        };
        let contract = Contract {
            verification: Some(Spec {
                checks: BTreeMap::from([("tests".into(), check)]),
                ..Default::default()
            }),
            extensions: None,
            environment,
            checks: BTreeMap::from([("tests".into(), command)]),
            sync: vec![],
        };
        let v = f.contract_task(contract.clone());
        let (v, store) = f.running_value(v);
        let id = v["task_id"].as_str().unwrap();
        let mut r = runner(&f, id, &store, contract);
        let result = r
            .run(
                Default::default(),
                false,
                "default-counts",
                Arc::new(AtomicBool::new(false)),
            )
            .unwrap();
        assert_eq!(result["outcome"], expected, "{case}: {result}");
        if expected == "PASS" {
            assert_eq!(
                result["checks"][0]["attempts"][0]["test_counts"]["passed"],
                1
            );
        }
        fs::write(
            f.root.join("standard-counts-result.json"),
            bytes(&result).unwrap(),
        )
        .unwrap();
        assert!(store.journal().model_requests(id).unwrap().is_empty());
        println!(
            "NATIVE_DEFAULT_COUNTS {case} {expected} {}",
            f.root.display()
        );
    }
}

#[test]
fn reviewed_launch_does_not_require_or_grant_debug_to_a_navigation_only_role() {
    use melissa_model_gateway::tools::ToolDispatcher;
    for configured in [false, true] {
        let f = Fixture::new(false);
        let contract = Contract {
            verification: Some(Spec {
                debug: configured.then(|| melissa_build_test::debug::Launch {
                    language: melissa_build_test::full::Language::Python,
                    entry: "main.py".into(),
                    cwd: ".".into(),
                    argv: vec![],
                    tsconfig: None,
                }),
                ..Default::default()
            }),
            extensions: None,
            environment: Environment::default(),
            checks: BTreeMap::from([(
                "syntax".into(),
                python(&["-I", "-m", "py_compile", "main.py"]),
            )]),
            sync: vec![],
        };
        let v = f.contract_task(contract);
        let (v, store) = f.running_value(v);
        let id = v["task_id"].as_str().unwrap();
        let task = store.journal().task(id).unwrap().contract;
        let tools = ["code.references".into()].into_iter().collect();
        let development = crate::development::DevelopmentTools::new(
            &f.app,
            &store,
            &task,
            "melissa.code.implementer",
            now().unwrap() + 120_000,
            &tools,
            "",
            false,
        )
        .unwrap();
        assert!(
            !development
                .definitions()
                .iter()
                .any(|d| d.definition.name == "code_debug_start")
        );
        assert!(store.journal().model_requests(id).unwrap().is_empty());
    }
}
