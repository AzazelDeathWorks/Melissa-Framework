#![cfg(windows)]
mod support;
use melissa_repository::{Limits, PatchBroker, Repository, structural::Query};
use std::{fs, sync::atomic::AtomicBool};
fn no() -> AtomicBool {
    AtomicBool::new(false)
}

#[test]
fn early_ignore_rules_cover_direct_reads_reopen_nested_rules_and_evidence_scope() {
    let d = tempfile::tempdir().unwrap();
    fs::create_dir_all(d.path().join("pkg/cache")).unwrap();
    fs::create_dir_all(d.path().join("pkg/node_modules")).unwrap();
    fs::write(d.path().join(".gitignore"), "*.tmp\npkg/cache/\n").unwrap();
    fs::write(d.path().join(".ignore"), "*.log\n").unwrap();
    fs::write(d.path().join("pkg/.gitignore"), "!keep.tmp\n").unwrap();
    for path in ["a.tmp", "a.log", "pkg/cache/huge", "pkg/node_modules/huge"] {
        fs::File::create(d.path().join(path))
            .unwrap()
            .set_len(9 * 1024 * 1024)
            .unwrap();
    }
    fs::write(d.path().join("pkg/keep.tmp"), "visible").unwrap();
    fs::write(d.path().join("main.py"), "x=1\n").unwrap();
    fs::write(d.path().join(".env"), "PRIVATE_FIXTURE").unwrap();
    let r = Repository::open(
        d.path(),
        Limits {
            max_file_bytes: 1024,
            ..Limits::default()
        },
        vec![],
    )
    .unwrap();
    let snap = r.snapshot(&no()).unwrap(); // Oversized ignored bodies must never be read.
    assert!(snap.files.iter().any(|f| f.path == "pkg/keep.tmp"));
    for repo in [&r, &r.reopen().unwrap()] {
        for path in [
            "a.tmp",
            "a.log",
            "pkg/cache/huge",
            "pkg/node_modules/huge",
            ".env",
        ] {
            assert!(repo.read(path).is_err(), "{path}");
        }
    }
    let old = r.source_visibility_digest().unwrap();
    fs::write(d.path().join(".ignore"), "*.log\nmain.py\n.ignore\n").unwrap();
    assert_ne!(r.source_visibility_digest().unwrap(), old);
    assert!(r.read("main.py").is_err());
    assert!(r.validate_snapshot(&snap, &no()).is_err());
    // Full package/Git capture retains its separate explicit host contract.
    assert!(
        Repository::open_for_git(
            d.path(),
            Limits {
                max_file_bytes: 16 * 1024 * 1024,
                ..Limits::default()
            }
        )
        .unwrap()
        .read("a.tmp")
        .is_ok()
    );
}

#[test]
fn large_repository_materialization_measures_reuse_and_rejects_mutation() {
    let d = tempfile::tempdir().unwrap();
    for i in 0..1400 {
        fs::write(
            d.path().join(format!("f{i:04}.py")),
            format!("def value_{i}():\n    return {i}\n"),
        )
        .unwrap();
    }
    let r = Repository::open(d.path(), Limits::default(), vec![]).unwrap();
    let start = std::time::Instant::now();
    let cold = r.scan_index(&Default::default(), &no()).unwrap();
    let warm = r.scan_index(&cold.files, &no()).unwrap();
    let snap = r.snapshot(&no()).unwrap();
    let first = r.worker_input(&snap, &no()).unwrap();
    let again = r.reopen().unwrap().worker_input(&snap, &no()).unwrap();
    assert_eq!(first.snapshot.files.len(), 1400);
    assert_eq!(
        first.snapshot.digest().unwrap(),
        again.snapshot.digest().unwrap()
    );
    assert!(warm.changed_bytes.is_empty());
    println!(
        "SCALE_MEASUREMENT {}",
        serde_json::json!({"files":1400,"source_bytes":snap.total_bytes,"cold_index_read":cold.bytes_read,"warm_index_read":warm.bytes_read,"worker_first":first.materialization,"worker_repeat":again.materialization,"elapsed_ms":start.elapsed().as_millis(),"independent_worker_full_transfer":true})
    );
    fs::write(d.path().join("f0000.py"), "def value_0():\n    return 9\n").unwrap();
    assert!(r.worker_input(&snap, &no()).is_err());
    let current = r.snapshot(&no()).unwrap();
    let next = r.worker_input(&current, &no()).unwrap();
    assert!(
        std::str::from_utf8(&next.snapshot.files["f0000.py"])
            .unwrap()
            .contains("return 9")
    );
    fs::remove_file(d.path().join("f0001.py")).unwrap();
    let current = r.snapshot(&no()).unwrap();
    assert!(
        !r.worker_input(&current, &no())
            .unwrap()
            .snapshot
            .files
            .contains_key("f0001.py")
    );
}

#[test]
fn structural_multi_file_proposals_use_scoped_patch_and_reject_stale_preimages() {
    let (_d, r, mut store, policy) = support::fixture(&["a.py", "b.py"]);
    fs::write(
        r.root_path().join("a.py"),
        "# legacy(7)\nx = api.legacy(1)\n",
    )
    .unwrap();
    fs::write(
        r.root_path().join("b.py"),
        "s='api.legacy(2)'\ny = api.legacy(3)\n",
    )
    .unwrap();
    let q = Query {
        language: "python".into(),
        paths: vec!["a.py".into(), "b.py".into()],
        pattern: "api.legacy($A)".into(),
        rewrite: Some("api.current($A)".into()),
    };
    let report = r.structural(&q, &no()).unwrap();
    assert_eq!(report.matches.len(), 2);
    let patch = report.proposal.unwrap();
    let stale = patch.clone();
    let mut broker = PatchBroker::new(&r, &mut store, policy).unwrap();
    let permit = broker
        .authorize("phase05-apply", "phase05-apply-attempt", patch)
        .unwrap();
    let receipt = broker.execute(permit, &no()).unwrap();
    assert_eq!(receipt.files.len(), 2);
    assert!(
        fs::read_to_string(r.root_path().join("a.py"))
            .unwrap()
            .contains("# legacy(7)\nx = api.current(1)")
    );
    assert!(
        fs::read_to_string(r.root_path().join("b.py"))
            .unwrap()
            .contains("s='api.legacy(2)'\ny = api.current(3)")
    );
    let stale_permit = broker
        .authorize("phase05-stale", "phase05-stale-attempt", stale)
        .unwrap();
    assert!(broker.execute(stale_permit, &no()).is_err());
    let mut invalid = q;
    invalid.rewrite = Some("$UNKNOWN".into());
    assert!(r.structural(&invalid, &no()).is_err());
}

#[test]
fn structural_grammars_preserve_literal_dollars_and_reject_ambiguous_or_unbound_edits() {
    let d = tempfile::tempdir().unwrap();
    let r = Repository::open(d.path(), Limits::default(), vec![]).unwrap();
    for (language, path, source) in [
        ("rust", "a.rs", "fn f(){old(1);}"),
        ("python", "a.py", "x=old(1)\n"),
        ("javascript", "a.js", "const x=old(1);"),
        ("typescript", "a.ts", "const x: number=old(1);"),
        ("tsx", "a.tsx", "const x=<div>{old(1)}</div>;"),
    ] {
        fs::write(d.path().join(path), source).unwrap();
        let report = r
            .structural(
                &Query {
                    language: language.into(),
                    paths: vec![path.into()],
                    pattern: "old($A)".into(),
                    rewrite: Some("current($A)".into()),
                },
                &no(),
            )
            .unwrap();
        assert_eq!(report.matches.len(), 1, "{language}");
        assert_eq!(
            report.proposal.unwrap().files[0].edits[0].replacement,
            b"current(1)"
        );
    }
    fs::write(d.path().join("dollar.js"), "const x=price('$5');\n").unwrap();
    let q = Query {
        language: "javascript".into(),
        paths: vec!["dollar.js".into()],
        pattern: "price('$5')".into(),
        rewrite: None,
    };
    assert_eq!(r.structural(&q, &no()).unwrap().matches.len(), 1);
    fs::write(d.path().join("nested.py"), "x=old(old(1))\n").unwrap();
    assert!(
        r.structural(
            &Query {
                language: "python".into(),
                paths: vec!["nested.py".into()],
                pattern: "old($A)".into(),
                rewrite: Some("current($A)".into())
            },
            &no()
        )
        .is_err()
    );
    assert!(
        r.structural(
            &Query {
                language: "javascript".into(),
                paths: vec!["dollar.js".into()],
                pattern: "price('$A')".into(),
                rewrite: Some("$UNKNOWN".into())
            },
            &no()
        )
        .is_err()
    );
}

#[test]
fn impact_map_separates_transitive_imports_package_tests_and_unresolved_aliases() {
    let d = tempfile::tempdir().unwrap();
    fs::create_dir(d.path().join("tests")).unwrap();
    for (p, s) in [
        ("package.json", "{}"),
        ("calc.ts", "export function calc(){return 1;}"),
        (
            "main.ts",
            "import {calc} from './calc'; export const answer=calc();",
        ),
        (
            "app.ts",
            "import {answer} from './main'; console.log(answer);",
        ),
        ("tests/calc.test.ts", "function test_calc(){}"),
        ("alias.ts", "import {calc} from '@shared/calc'; calc();"),
    ] {
        fs::write(d.path().join(p), s).unwrap();
    }
    let r = Repository::open(d.path(), Limits::default(), vec![]).unwrap();
    let impact = r
        .impact_map(
            &melissa_repository::impact::Query {
                paths: vec!["calc.ts".into()],
            },
            &no(),
        )
        .unwrap();
    for path in ["calc.ts", "main.ts", "app.ts", "tests/calc.test.ts"] {
        assert!(impact.candidates.iter().any(|c| c.path == path), "{path}");
    }
    assert!(!impact.complete_build_graph);
    assert!(
        impact
            .unresolved_imports
            .iter()
            .any(|(p, _)| p == "alias.ts")
    );
}

#[test]
fn native_structural_refactor_runs_real_callers_from_more_than_1024_files() {
    use melissa_execution_broker::{InvocationIdentity, policy::PermitRequest, unix_ms};
    let (_d, r, mut store, policy) = support::fixture(&["first.py", "second.py"]);
    for (p, s) in [
        (
            "api.py",
            "def legacy(x):\n    return x+1\ndef current(x):\n    return x+1\n",
        ),
        (
            "first.py",
            "import api\ndef first():\n    return api.legacy(4)\n",
        ),
        (
            "second.py",
            "import api\ndef second():\n    return api.legacy(9)\n",
        ),
        (
            "main.py",
            "from first import first\nfrom second import second\nassert first()==5 and second()==10\nprint('REAL_CALLERS_PASS')\n",
        ),
    ] {
        fs::write(r.root_path().join(p), s).unwrap();
    }
    for i in 0..1300 {
        fs::write(
            r.root_path().join(format!("source_{i}.py")),
            "# visible source\n",
        )
        .unwrap();
    }
    // Exercise bytes as well as count: source payload exceeds the old 64 MiB.
    for i in 0..9 {
        fs::write(
            r.root_path().join(format!("asset_{i}.bin")),
            vec![i as u8; 8 * 1024 * 1024],
        )
        .unwrap();
    }
    let proposal = r
        .structural(
            &Query {
                language: "python".into(),
                paths: vec!["first.py".into(), "second.py".into()],
                pattern: "api.legacy($A)".into(),
                rewrite: Some("api.current($A)".into()),
            },
            &no(),
        )
        .unwrap()
        .proposal
        .unwrap();
    let mut patches = PatchBroker::new(&r, &mut store, policy).unwrap();
    let permit = patches
        .authorize("native-refactor", "native-refactor-attempt", proposal)
        .unwrap();
    patches.execute(permit, &no()).unwrap();
    drop(patches);
    let source = r.snapshot(&no()).unwrap();
    let input = r.worker_input(&source, &no()).unwrap();
    let mut limits = support::limits();
    limits.export_bytes = 0;
    let request = melissa_container_executor::ContainerRequest {
        backend: Default::default(),
        schema_version: "melissa.container-exec/v1".into(),
        image_id: "sha256:fd18bccfbb336754152a58054354bcea114919c38c07ab40a25ced856f613c1e".into(),
        executable: "/usr/bin/python3".into(),
        argv: vec!["main.py".into()],
        cwd: ".".into(),
        env: Default::default(),
        limits,
    };
    let task = store
        .journal()
        .task("task:code05-closeout")
        .unwrap()
        .contract;
    let mut broker = support::container(store, &task, &request, &input.snapshot);
    let n = unix_ms().unwrap();
    let permit = broker
        .issue_permit(
            &task.task_id,
            request,
            &input.snapshot,
            PermitRequest {
                permit_id: "permit:phase05-refactor".into(),
                nonce: "nonce:phase05-refactor".into(),
                principal_id: "principal:closeout".into(),
                profile_id: task.selected_profile_ids[0].clone(),
                resource_id: task.workspace_ref.clone(),
                authorization_ref: "auth:closeout".into(),
                not_before_unix_ms: n,
                expires_at_unix_ms: n + 90_000,
            },
        )
        .unwrap();
    let result = broker
        .execute(
            &task.task_id,
            &permit,
            &input.snapshot,
            &InvocationIdentity {
                invocation_id: "inv:phase05-refactor".into(),
                attempt_id: "attempt:phase05-refactor".into(),
                generation: 1,
                idempotency_key: None,
            },
            &no(),
        )
        .unwrap();
    let report = result.report.unwrap();
    assert_eq!(
        report.exit_code,
        Some(0),
        "{}",
        String::from_utf8_lossy(&report.stderr)
    );
    assert!(String::from_utf8_lossy(&report.stdout).contains("REAL_CALLERS_PASS"));
    assert!(report.policy_verified && report.cleanup_confirmed && report.tree_confirmed_stopped);
    assert!(
        broker
            .store()
            .journal()
            .model_requests(&task.task_id)
            .unwrap()
            .is_empty()
    );
    println!(
        "NATIVE_REFACTOR {}",
        serde_json::json!({"files":input.snapshot.files.len(),"materialization":input.materialization,"exit_code":report.exit_code,"cleanup":report.cleanup_confirmed,"paid_calls":0})
    );
}

#[tokio::test]
async fn structural_dispatcher_pages_complete_evidence_and_labels_stale_source() {
    use melissa_model_gateway::{
        ToolCall,
        tools::{ToolContext, ToolDispatcher},
    };
    use melissa_repository::tools::{RepositoryDispatcher, RepositoryToolPolicy};
    use std::sync::Arc;
    let (_d, r, store, policy) = support::fixture(&[]);
    fs::write(
        r.root_path().join("large.py"),
        format!("x = api.legacy(\"{}\")\n", "q".repeat(150)).repeat(500),
    )
    .unwrap();
    let root = r.root_path().to_path_buf();
    let mut dispatcher = RepositoryDispatcher::new(
        r,
        store,
        RepositoryToolPolicy {
            files: policy.clone(),
            read_repository: true,
            enabled_tools: ["code_structural_propose", "fs_artifact_page"]
                .into_iter()
                .map(String::from)
                .collect(),
        },
    )
    .unwrap();
    let ctx = |i: &str| ToolContext {
        task_id: policy.task_id.clone(),
        profile_id: policy.profile_id.clone(),
        run_id: "run:phase05".into(),
        request_id: "request:phase05".into(),
        invocation_id: i.into(),
        attempt_id: format!("attempt:{i}"),
        generation: 1,
    };
    let output=dispatcher.execute(ToolCall {id:"proposal".into(),name:"code_structural_propose".into(),arguments:serde_json::json!({"language":"python","paths":["large.py"],"pattern":"api.legacy($A)","rewrite":"api.current($A)"}).to_string()},ctx("proposal"),Arc::new(no())).await.unwrap();
    let pointer: serde_json::Value = serde_json::from_str(&output.content).unwrap();
    assert_eq!(pointer["delivery"], "paged_result");
    let mut assembled = vec![];
    let mut cursor = 0;
    loop {
        let page=dispatcher.execute(ToolCall{id:format!("page{cursor}"),name:"fs_artifact_page".into(),arguments:serde_json::json!({"artifact_id":pointer["artifact_id"],"digest":pointer["digest"],"offset":cursor,"max_bytes":65536}).to_string()},ctx(&format!("page{cursor}")),Arc::new(no())).await.unwrap();
        let page: serde_json::Value = serde_json::from_str(&page.content).unwrap();
        let p = &page["result"];
        assert_eq!(p["source_freshness"], "CURRENT");
        assembled.extend(
            p["bytes"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_u64().unwrap() as u8),
        );
        if p["complete"] == true {
            break;
        }
        cursor = p["next_offset"].as_u64().unwrap();
    }
    assert_eq!(
        assembled.len() as u64,
        pointer["total_bytes"].as_u64().unwrap()
    );
    assert_eq!(
        format!("blake3:{}", blake3::hash(&assembled).to_hex()),
        pointer["digest"]
    );
    let report: serde_json::Value = serde_json::from_slice(&assembled).unwrap();
    assert_eq!(report["result"]["matches"].as_array().unwrap().len(), 500);
    fs::write(root.join("large.py"), "x=0\n").unwrap();
    let output=dispatcher.execute(ToolCall{id:"stale-page".into(),name:"fs_artifact_page".into(),arguments:serde_json::json!({"artifact_id":pointer["artifact_id"],"digest":pointer["digest"],"offset":0,"max_bytes":1024}).to_string()},ctx("stale-page"),Arc::new(no())).await.unwrap();
    let stale: serde_json::Value = serde_json::from_str(&output.content).unwrap();
    assert_eq!(stale["result"]["source_freshness"], "STALE");
    assert_eq!(fs::read_to_string(root.join("large.py")).unwrap(), "x=0\n");
}
