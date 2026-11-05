#![cfg(windows)]
mod support;
use melissa_build_test::{
    RustOperation,
    full::Language,
    semantic::{SemanticMethod, SemanticPlan, SemanticQuery, SemanticReport},
};
fn run(
    language: Language,
    query: SemanticQuery,
    files: &[(&str, &str)],
    scripts: bool,
) -> SemanticReport {
    let mut limits = support::limits();
    limits.export_bytes = 32 * 1024 * 1024;
    let mut f = support::Fixture::new(
        "pub fn initial(){}\n",
        RustOperation::Build,
        files,
        limits.clone(),
    );
    let source = f.repo.snapshot(&support::no()).unwrap();
    let p = SemanticPlan::prepare(
        &f.repo,
        &source,
        language,
        query,
        scripts,
        limits,
        &support::no(),
    )
    .unwrap();
    let permit = support::permit_for(&mut f, p.request(), p.snapshot());
    let r = p
        .execute(
            &f.repo,
            &mut f.broker,
            &f.task.task_id,
            &permit,
            &support::identity(),
            &support::no(),
        )
        .unwrap();
    assert!(r.success, "{}", serde_json::to_string_pretty(&r).unwrap());
    assert!(
        r.source_current
            && r.locations_authenticated
            && !r.task_completed
            && !r.protected_acceptance
    );
    r
}
#[test]
fn actual_python_and_typescript_semantic_definitions_resolve_same_named_modules() {
    let py = [
        ("src/calc.py", "def same() -> int:\n    return 1\n"),
        ("src/other.py", "def same() -> int:\n    return 2\n"),
        ("src/main.py", "from calc import same\nvalue = same()\n"),
    ];
    let r = run(
        Language::Python,
        SemanticQuery {
            method: SemanticMethod::Definition,
            path: "src/main.py".into(),
            line: 1,
            character: 9,
        },
        &py,
        false,
    );
    assert!(
        r.result
            .unwrap()
            .locations
            .iter()
            .any(|l| l.uri.ends_with("/src/calc.py"))
    );
    for (language, ext) in [(Language::JavaScript, "js"), (Language::TypeScript, "ts")] {
        let calc = format!("src/calc.{ext}");
        let other = format!("src/other.{ext}");
        let main = format!("src/main.{ext}");
        let files = [
            ("package.json", "{\"type\":\"module\"}"),
            (calc.as_str(), "export function same(){ return 1; }\n"),
            (other.as_str(), "export function same(){ return 2; }\n"),
            (
                main.as_str(),
                "import { same } from './calc.js';\nexport const value = same();\n",
            ),
        ];
        let r = run(
            language,
            SemanticQuery {
                method: SemanticMethod::Definition,
                path: main.clone(),
                line: 1,
                character: 22,
            },
            &files,
            false,
        );
        eprintln!(
            "{} definition={}",
            language.wire(),
            serde_json::to_string(&r).unwrap()
        );
        assert!(
            r.result
                .unwrap()
                .locations
                .iter()
                .any(|l| l.uri.ends_with(&calc))
        );
    }
}

#[test]
fn actual_monorepo_alias_definition_references_and_type_definition_use_lsp() {
    let files = [
        (
            "package.json",
            r#"{"private":true,"workspaces":["packages/*"]}"#,
        ),
        (
            "tsconfig.json",
            r#"{"files":[],"references":[{"path":"packages/shared"},{"path":"packages/app"}]}"#,
        ),
        (
            "tsconfig.base.json",
            r#"{"compilerOptions":{"target":"ES2022","module":"ESNext","moduleResolution":"Bundler","baseUrl":".","paths":{"@shared/*":["packages/shared/src/*"]},"strict":true,"composite":true}}"#,
        ),
        (
            "packages/shared/tsconfig.json",
            r#"{"extends":"../../tsconfig.base.json","include":["src/**/*.ts"]}"#,
        ),
        (
            "packages/app/tsconfig.json",
            r#"{"extends":"../../tsconfig.base.json","include":["*.ts"],"references":[{"path":"../shared"}]}"#,
        ),
        (
            "packages/shared/package.json",
            r#"{"name":"shared","version":"1.0.0"}"#,
        ),
        (
            "packages/shared/src/calc.ts",
            "export interface Item { value: number }\nexport function create(): Item { return {value:1}; }\nexport class Counter implements Item { value=1; }\n",
        ),
        (
            "packages/app/package.json",
            r#"{"name":"app","version":"1.0.0"}"#,
        ),
        (
            "packages/app/main.ts",
            "import { create } from '@shared/calc';\nconst item = create();\nconsole.log(item.value);\n",
        ),
        (
            "packages/app/other.ts",
            "import { create } from '@shared/calc';\nexport const other = create();\n",
        ),
    ];
    let query = |method, path: &str, line, character| SemanticQuery {
        method,
        path: path.into(),
        line,
        character,
    };
    let definition = run(
        Language::TypeScript,
        query(SemanticMethod::Definition, "packages/app/main.ts", 1, 15),
        &files,
        false,
    );
    assert!(
        definition
            .result
            .unwrap()
            .locations
            .iter()
            .any(|l| l.uri.ends_with("/packages/shared/src/calc.ts"))
    );
    let references = run(
        Language::TypeScript,
        query(
            SemanticMethod::References,
            "packages/shared/src/calc.ts",
            1,
            18,
        ),
        &files,
        false,
    );
    let locations = references.result.unwrap().locations;
    for p in ["/packages/app/main.ts", "/packages/app/other.ts"] {
        assert!(
            locations.iter().any(|l| l.uri.ends_with(p)),
            "missing true caller {p}"
        );
    }
    let types = run(
        Language::TypeScript,
        query(
            SemanticMethod::TypeDefinition,
            "packages/app/main.ts",
            2,
            14,
        ),
        &files,
        false,
    );
    assert!(
        types
            .result
            .unwrap()
            .locations
            .iter()
            .any(|l| l.uri.ends_with("/packages/shared/src/calc.ts"))
    );
    let implementations = run(
        Language::TypeScript,
        query(
            SemanticMethod::Implementation,
            "packages/shared/src/calc.ts",
            0,
            18,
        ),
        &files,
        false,
    );
    let data = implementations.result.unwrap();
    assert!(
        data.locations
            .iter()
            .any(|l| l.uri.ends_with("/packages/shared/src/calc.ts"))
    );
    eprintln!("NATIVE_IMPLEMENTATION {}", data.result);
    assert!(
        data.result
            .as_array()
            .unwrap()
            .iter()
            .any(|location| location["range"]["start"]["line"] == 2)
    );
}

#[test]
fn actual_existing_pyright_project_configuration_is_preserved_and_resolves_package() {
    let files = [
        (
            "pyrightconfig.json",
            r#"{"include":["packages"],"extraPaths":["packages/lib"]}"#,
        ),
        (
            "packages/lib/calc.py",
            "def answer() -> int:\n    return 42\n",
        ),
        (
            "packages/app/main.py",
            "from calc import answer\nvalue = answer()\n",
        ),
    ];
    let report = run(
        Language::Python,
        SemanticQuery {
            method: SemanticMethod::Definition,
            path: "packages/app/main.py".into(),
            line: 1,
            character: 10,
        },
        &files,
        false,
    );
    assert!(
        report
            .result
            .unwrap()
            .locations
            .iter()
            .any(|l| l.uri.ends_with("/packages/lib/calc.py"))
    );
}
#[test]
fn actual_cargo_rust_semantics_resolve_local_dependencies_and_sysroot_sources() {
    let files = [
        (
            "src/lib.rs",
            "mod alpha;\nmod beta;\npub fn run()->u32{alpha::same()}\npub fn make()->Vec<u32>{Vec::new()}\n",
        ),
        ("src/alpha.rs", "pub fn same()->u32{1}\n"),
        ("src/beta.rs", "pub fn same()->u32{2}\n"),
    ];
    let r = run(
        Language::Rust,
        SemanticQuery {
            method: SemanticMethod::Definition,
            path: "src/lib.rs".into(),
            line: 2,
            character: 25,
        },
        &files,
        false,
    );
    assert!(
        r.result
            .unwrap()
            .locations
            .iter()
            .any(|l| l.uri.ends_with("src/alpha.rs"))
    );
    let r = run(
        Language::Rust,
        SemanticQuery {
            method: SemanticMethod::Definition,
            path: "src/lib.rs".into(),
            line: 3,
            character: 16,
        },
        &files,
        false,
    );
    assert!(
        r.result
            .unwrap()
            .locations
            .iter()
            .any(|l| l.scope == "sdk" && l.uri.contains("/rust/library/"))
    );
}
#[test]
fn actual_python_diagnostics_come_from_semantic_server_without_executing_source() {
    let files = [(
        "src/bad.py",
        "from pathlib import Path\nPath('/work/CODE06_BUILD_SCRIPT_RAN').write_text('must not execute')\ndef answer() -> int:\n    return 'wrong'\n",
    )];
    let r = run(
        Language::Python,
        SemanticQuery {
            method: SemanticMethod::Diagnostics,
            path: "src/bad.py".into(),
            line: 3,
            character: 4,
        },
        &files,
        false,
    );
    let data = r.result.unwrap();
    assert!(data.result.to_string().contains("reportReturnType"));
    assert!(!data.execution_markers["CODE06_BUILD_SCRIPT_RAN"]);
}
#[test]
fn actual_cargo_path_dependency_build_script_and_proc_macro_require_executing_mode() {
    let files = [
        (
            "Cargo.toml",
            "[package]\nname=\"code06_fixture\"\nversion=\"0.0.0\"\nedition=\"2021\"\n[dependencies]\nhelper={path=\"helper\"}\nmacros={path=\"macros\"}\n",
        ),
        (
            "Cargo.lock",
            "version = 4\n[[package]]\nname=\"code06_fixture\"\nversion=\"0.0.0\"\ndependencies=[\"helper\",\"macros\"]\n[[package]]\nname=\"helper\"\nversion=\"0.0.0\"\n[[package]]\nname=\"macros\"\nversion=\"0.0.0\"\n",
        ),
        (
            "helper/Cargo.toml",
            "[package]\nname=\"helper\"\nversion=\"0.0.0\"\nedition=\"2021\"\n",
        ),
        ("helper/src/lib.rs", "pub fn same()->u32{42}\n"),
        (
            "macros/Cargo.toml",
            "[package]\nname=\"macros\"\nversion=\"0.0.0\"\nedition=\"2021\"\n[lib]\nproc-macro=true\n",
        ),
        (
            "macros/src/lib.rs",
            "use proc_macro::TokenStream;\n#[proc_macro]\npub fn make(_:TokenStream)->TokenStream{std::fs::write(\"/work/CODE06_PROC_MACRO_RAN\",b\"real expansion\").unwrap();\"pub fn generated()->u32{7}\".parse().unwrap()}\n",
        ),
        (
            "build.rs",
            "fn main(){std::fs::write(\"/work/CODE06_BUILD_SCRIPT_RAN\",b\"real build script\").unwrap();println!(\"cargo:rerun-if-changed=build.rs\");}\n",
        ),
        (
            "src/lib.rs",
            "macros::make!();\npub fn run()->u32{helper::same()+generated()}\n",
        ),
    ];
    for enabled in [false, true] {
        let r = run(
            Language::Rust,
            SemanticQuery {
                method: SemanticMethod::Definition,
                path: "src/lib.rs".into(),
                line: 1,
                character: 27,
            },
            &files,
            enabled,
        );
        let data = r.result.unwrap();
        eprintln!(
            "enabled={enabled} {}",
            serde_json::to_string(&data).unwrap()
        );
        assert!(
            data.locations
                .iter()
                .any(|l| l.uri.ends_with("helper/src/lib.rs"))
        );
        assert_eq!(data.execution_markers["CODE06_BUILD_SCRIPT_RAN"], enabled);
        assert_eq!(data.execution_markers["CODE06_PROC_MACRO_RAN"], enabled);
    }
}
#[test]
fn actual_semantic_references_symbols_and_hover_are_not_grammar_index_results() {
    for (language, path, body, line, character) in [
        (
            Language::Rust,
            "src/lib.rs",
            "pub fn answer()->u32{42}\npub fn run()->u32{answer()}\n",
            1,
            19,
        ),
        (
            Language::Python,
            "src/calc.py",
            "def answer() -> int:\n    return 42\nvalue = answer()\n",
            2,
            10,
        ),
        (
            Language::JavaScript,
            "src/calc.js",
            "export function answer(){return 42;}\nexport const value=answer();\n",
            1,
            21,
        ),
        (
            Language::TypeScript,
            "src/calc.ts",
            "export function answer():number{return 42;}\nexport const value=answer();\n",
            1,
            21,
        ),
    ] {
        for method in [
            SemanticMethod::References,
            SemanticMethod::Symbols,
            SemanticMethod::Hover,
        ] {
            let r = run(
                language,
                SemanticQuery {
                    method,
                    path: path.into(),
                    line,
                    character,
                },
                &[(path, body)],
                false,
            );
            let data = r.result.unwrap();
            assert!(
                !data.result.is_null() && data.result != serde_json::json!([]),
                "{} {:?}: {}",
                language.wire(),
                method,
                data.result
            );
            if method == SemanticMethod::References {
                assert!(
                    data.result.as_array().is_some_and(|a| a.len() >= 2),
                    "declaration and actual call reference required: {}",
                    data.result
                );
                assert!(
                    data.locations.iter().any(|l| l.uri.ends_with(path)),
                    "reference source bytes must be authenticated"
                );
            } else {
                assert!(
                    data.result.to_string().contains("answer"),
                    "{} {:?}: {}",
                    language.wire(),
                    method,
                    data.result
                );
            }
        }
    }
}
