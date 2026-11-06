#![cfg(windows)]
mod support;
use melissa_build_test::{
    RustOperation,
    debug::{Breakpoint, DebugCommand, DebugPlan, DebugState, Launch, Project},
    full::Language,
};
use std::sync::{Arc, Mutex, atomic::AtomicBool};
fn run(launch: Launch, files: &[(&str, &str)], line: u64) {
    scenario(
        launch,
        files,
        line,
        [
            ("node".into(), "24.22.0".into()),
            ("rust".into(), "1.98.1".into()),
        ]
        .into_iter()
        .collect(),
        true,
    );
}
fn scenario(
    launch: Launch,
    files: &[(&str, &str)],
    line: u64,
    versions: std::collections::BTreeMap<String, String>,
    expected: bool,
) {
    let mut limits = support::limits();
    limits.export_bytes = 32 * 1024 * 1024;
    let mut fixture = support::Fixture::new(
        "pub fn initial(){}\n",
        RustOperation::Build,
        files,
        limits.clone(),
    );
    let source = fixture.repo.snapshot(&support::no()).unwrap();
    let project = Project {
        launch: launch.clone(),
        image_id: melissa_build_test::toolchain::COMPLETE_IMAGE.into(),
        executables: Default::default(),
        rust_version: versions.get("rust").cloned(),
        versions,
    };
    let plan = DebugPlan::prepare_with_launch(
        &fixture.repo,
        &source,
        launch.language,
        launch.entry.clone(),
        vec![Breakpoint {
            path: launch.entry.clone(),
            line,
        }],
        None,
        limits,
        &support::no(),
        Some(&project),
    )
    .unwrap();
    let permit = support::permit_for(&mut fixture, plan.request(), plan.snapshot());
    let mut session = plan
        .start(
            Arc::new(fixture.repo),
            Arc::new(Mutex::new(fixture.broker)),
            fixture.task.task_id,
            permit,
            support::identity(),
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
    let stopped = session.state() == DebugState::Stopped;
    let initial = session.last_reply().cloned();
    let stack = if stopped {
        Some(session.command(DebugCommand::Stack {}).unwrap())
    } else {
        None
    };
    if stopped {
        assert!(session.command(DebugCommand::Close {}).unwrap().ok);
    }
    let outcome = session.finish().unwrap();
    if let Some(root) = std::env::var_os("MELISSA_PHASE06_EVIDENCE") {
        let path = std::path::PathBuf::from(root).join(format!(
            "debug-{}-{}",
            launch.language.wire(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(path.join("result.json"),serde_json::to_vec_pretty(&serde_json::json!({"launch":launch,"initial":initial,"stack":stack,"success":outcome.success,"source_current":outcome.host_source_current,"binary_authenticated":outcome.binary_bytes_authenticated})).unwrap()).unwrap();
        if let Some(r) = &outcome.execution.report {
            std::fs::write(path.join("native-archive.tar"), &r.archive).unwrap();
        }
    }
    if !expected {
        assert!(!stopped);
        assert!(!outcome.success);
        assert!(format!("{initial:?}").contains("runtime_version_mismatch"));
        return;
    }
    assert!(stopped, "{initial:?}");
    let stack = stack.unwrap();
    assert!(stack.ok, "{stack:?}");
    assert!(
        stack.body.as_ref().unwrap()["stackFrames"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["source"]["path"]
                .as_str()
                .is_some_and(|p| p.ends_with(&launch.entry))),
        "{stack:?}"
    );
    assert!(outcome.success && outcome.host_source_current && outcome.binary_bytes_authenticated);
    let native = outcome.execution.report.unwrap();
    assert!(native.cleanup_confirmed && native.tree_confirmed_stopped);
}
#[test]
fn native_python_project_cwd_args_and_imported_module_use_reviewed_launch() {
    run(
        Launch {
            language: Language::Python,
            entry: "pkg/main.py".into(),
            cwd: "pkg".into(),
            argv: vec!["3".into()],
            tsconfig: None,
        },
        &[
            (
                "pkg/main.py",
                "import sys\nfrom helper import value\nanswer=value()+int(sys.argv[1])\nprint(answer)\n",
            ),
            ("pkg/helper.py", "def value():\n    return 4\n"),
        ],
        3,
    );
}
#[test]
fn native_typescript_nested_config_extends_and_sourcemap_stop_in_original_source() {
    run(
        Launch {
            language: Language::TypeScript,
            entry: "apps/demo/main.ts".into(),
            cwd: "apps/demo".into(),
            argv: vec![],
            tsconfig: Some("apps/demo/tsconfig.json".into()),
        },
        &[
            (
                "tsconfig.base.json",
                r#"{"compilerOptions":{"target":"ES2022","module":"CommonJS","strict":true}}"#,
            ),
            (
                "apps/demo/tsconfig.json",
                r#"{"extends":"../../tsconfig.base.json","include":["*.ts"]}"#,
            ),
            (
                "apps/demo/main.ts",
                "import {calc} from './helper';\nconst result=calc(2);\nconsole.log(result);\n",
            ),
            (
                "apps/demo/helper.ts",
                "export function calc(value:number){return value+3;}\n",
            ),
        ],
        2,
    );
}
#[test]
fn native_rust_nested_manifest_selects_the_real_binary_entry() {
    run(
        Launch {
            language: Language::Rust,
            entry: "apps/demo/src/main.rs".into(),
            cwd: "apps/demo".into(),
            argv: vec!["3".into()],
            tsconfig: None,
        },
        &[
            (
                "apps/demo/Cargo.toml",
                "[package]\nname='nested_debug'\nversion='0.0.0'\nedition='2021'\n",
            ),
            (
                "apps/demo/Cargo.lock",
                "version = 4\n[[package]]\nname='nested_debug'\nversion='0.0.0'\n",
            ),
            (
                "apps/demo/src/main.rs",
                "fn main(){\n    let value=std::env::args().nth(1).unwrap();\n    println!(\"{}\",value);\n}\n",
            ),
        ],
        2,
    );
}

#[test]
fn native_requested_runtime_mismatch_never_starts_a_different_version_silently() {
    scenario(
        Launch {
            language: Language::Python,
            entry: "main.py".into(),
            cwd: ".".into(),
            argv: vec![],
            tsconfig: None,
        },
        &[("main.py", "value=1\nprint(value)\n")],
        2,
        [("python".into(), "0.0.0".into())].into_iter().collect(),
        false,
    );
}
