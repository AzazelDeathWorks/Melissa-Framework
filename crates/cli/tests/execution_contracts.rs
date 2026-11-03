#![cfg(windows)]
use melissa_cli::{app::App, recipe::Recipe, settings::Settings};
use serde_json::{Value, json};
use std::{fs, path::PathBuf};
struct Fixture {
    temp: tempfile::TempDir,
    home: PathBuf,
    source: PathBuf,
    config: PathBuf,
    recipe: Recipe,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("state");
        let source = temp.path().join("project");
        let negative = temp.path().join("negative");
        fs::create_dir(&source).unwrap();
        fs::create_dir(&negative).unwrap();
        for root in [&source, &negative] {
            fs::write(
                root.join("main.py"),
                b"import sys\nprint(int(sys.argv[1])-1)\n",
            )
            .unwrap();
        }
        fs::write(source.join("dirty.txt"), b"untouched user work\r\n").unwrap();
        let config = temp.path().join("config.json");
        fs::write(&config,serde_json::to_vec(&json!({"schema_version":"melissa.cli-settings/v1","provider_id":"operator-selected","endpoint":"https://provider.example.invalid/v1/responses","protocol":"open_ai_responses","model":"operator-model","credential":{"kind":"environment","name":"MELISSA_TEST_NONEXISTENT_KEY"},"context_tokens":1048576,"max_output_tokens":393216,"capabilities":{"tool_calling":true,"structured_output":false,"vision":false,"streaming":true},"json_object":true,"native_json_schema":false,"single_attempt":false,"rate_card":{"source":{"kind":"verified_host_rate_card","source_url":"https://provider.example.invalid/pricing","checked_at_unix_ms":1},"input_micro_usd_per_million":300000,"output_micro_usd_per_million":1200000,"cached_input_micro_usd_per_million":null,"cache_creation_micro_usd_per_million":null}})).unwrap()).unwrap();
        let recipe:Recipe=serde_json::from_value(json!({"schema_version":"melissa.cli-recipe/v1","agents":[{"profile":"melissa.code.implementer","goal":"Fix the program","write":true},{"profile":"melissa.code.reviewer","goal":"Inspect final source","write":false}],"writable_paths":["main.py"],"protected_paths":["dirty.txt"],"language":"python","entry":"main.py","negative_workspace":negative,"checks":[{"id":"one","input":1,"expected":1},{"id":"two","input":2,"expected":2}],"max_cost_micro_usd":5000000,"wall_time_ms":1800000})).unwrap();
        App::init(&home).unwrap();
        let app = App::open(&home).unwrap();
        app.configure(&config).unwrap();
        Self {
            temp,
            home,
            source,
            config,
            recipe,
        }
    }
    pub fn app(&self) -> App {
        App::open(&self.home).unwrap()
    }
    pub fn task(&self) -> Value {
        self.app()
            .create_execution(
                &self.source,
                "Fix code and preserve dirty work",
                vec![],
                self.recipe.clone(),
            )
            .unwrap()
    }
}
#[test]
fn settings_import_and_display_never_resolve_or_disclose_the_credential_reference() {
    let f = Fixture::new();
    let v = f.app().configuration().unwrap();
    assert_eq!(v["secret_resolved"], false);
    assert!(!v.to_string().contains("MELISSA_TEST_NONEXISTENT_KEY"));
    assert!(
        !fs::read(f.home.join("settings.enc"))
            .unwrap()
            .windows(b"operator-model".len())
            .any(|w| w == b"operator-model")
    );
    let before = v["settings_digest"].clone();
    f.app().configure(&f.config).unwrap();
    assert_eq!(f.app().configuration().unwrap()["settings_digest"], before);
}
#[test]
fn recipe_plan_has_exact_write_scope_and_a_protected_oracle_without_model_answers() {
    let f = Fixture::new();
    let v = f.task();
    assert_eq!(v["state"], "awaiting_approval");
    let plan = f.app().progress(v["task_id"].as_str().unwrap()).unwrap();
    assert_eq!(
        plan["workflow"]["plan"]["nodes"].as_array().unwrap().len(),
        2
    );
    assert!(
        !plan["workflow"]["plan"]
            .to_string()
            .contains("\"expected\"")
    );
    assert_eq!(v["execution_plan"]["writable_paths"], json!(["main.py"]));
    assert_eq!(v["execution_plan"]["expected_answers_sent_to_model"], false);
    assert!(
        v["approval"]["meaning"]
            .as_str()
            .unwrap()
            .contains("paid model")
    );
}
#[tokio::test(flavor = "multi_thread")]
async fn unapproved_execution_cannot_resolve_a_key_or_start_a_model() {
    let f = Fixture::new();
    let v = f.task();
    let id = v["task_id"].as_str().unwrap();
    assert!(f.app().run_task(id, false).await.is_err());
    let current = f.app().show(id).unwrap();
    assert_eq!(current["state"], "awaiting_approval");
    assert_eq!(current["model_budget"]["known_cost_micro_usd"], 0);
    assert!(!f.home.join("tasks").join(id).join("control").exists());
}
#[test]
fn changed_settings_or_source_invalidates_execution_approval() {
    let f = Fixture::new();
    let v = f.task();
    let id = v["task_id"].as_str().unwrap();
    let mut s: Value = serde_json::from_slice(&fs::read(&f.config).unwrap()).unwrap();
    s["model"] = json!("different-model");
    fs::write(&f.config, serde_json::to_vec(&s).unwrap()).unwrap();
    f.app().configure(&f.config).unwrap();
    assert!(
        f.app()
            .approve(
                id,
                v["approval"]["request_id"].as_str().unwrap(),
                v["approval"]["digest"].as_str().unwrap()
            )
            .is_err()
    );
    assert_eq!(f.app().show(id).unwrap()["state"], "awaiting_approval");
}
#[test]
fn unsafe_or_ambiguous_recipe_and_inline_api_secrets_are_rejected() {
    let f = Fixture::new();
    let original = f.recipe;
    for path in [
        "../outside",
        "/",
        "C:/file",
        "AGENTS.md",
        "dirty.txt",
        "DIRTY.TXT",
    ] {
        let mut r = original.clone();
        r.writable_paths = std::collections::BTreeSet::from([path.into()]);
        assert!(r.validate().is_err(), "{path}");
    }
    let mut r = original.clone();
    r.checks[1].input = r.checks[0].input.clone();
    assert!(r.validate().is_err());
    let mut s: Value = serde_json::from_slice(&fs::read(&f.config).unwrap()).unwrap();
    s["api_key"] = json!("DO_NOT_ACCEPT_INLINE_KEYS");
    assert!(serde_json::from_value::<Settings>(s).is_err());
}
#[test]
fn byte_diff_keeps_binary_and_crlf_and_delivery_cannot_be_invented() {
    let f = Fixture::new();
    let v = f.task();
    let id = v["task_id"].as_str().unwrap();
    let changed = b"binary\0\xff\r\n";
    fs::write(f.source.join("main.py"), changed).unwrap();
    let diff = f.app().diff_content(id).unwrap();
    assert_eq!(diff["changes"][0]["after_bytes"], json!(changed));
    assert!(
        f.app()
            .delivery(id, Some(&f.temp.path().join("fake-package")))
            .is_err()
    );
    assert!(!f.temp.path().join("fake-package").exists());
}
#[test]
fn runtime_bundle_contains_valid_profiles_and_skills_without_history_or_user_settings() {
    let c = melissa_cli::resources::catalog().unwrap();
    assert_eq!(c.profiles.len(), 18);
    assert_eq!(c.workers.len(), 3);
    assert_eq!(c.tools.tools.len(), 57);
    assert_eq!(c.modules.modules.len(), 2);
    for p in c.profiles.values() {
        assert!(matches!(
            p.module,
            melissa_core_contracts::Module::Core | melissa_core_contracts::Module::Code
        ));
        assert!(
            p.handoffs
                .iter()
                .all(|h| c.profiles.contains_key(&h.profile_id))
        );
    }
    for t in &c.tools.tools {
        assert!(matches!(
            t.owner_module,
            melissa_core_contracts::Module::Core | melissa_core_contracts::Module::Code
        ));
        assert!(t.referenced_by.iter().all(|p| c.profiles.contains_key(p)));
        assert!(
            t.worker_candidates
                .iter()
                .all(|w| c.workers.contains_key(w))
        );
    }
    for module in ["cyber", "game", "science", "fit", "repack"] {
        assert!(
            melissa_cli::resources::FILES
                .iter()
                .all(|(p, _)| !p.starts_with(&format!("catalog/agents/{module}/")))
        );
    }
    assert!(c.backlog.items.is_empty());
    assert!(
        melissa_cli::resources::FILES
            .iter()
            .all(|(p, _)| !p.starts_with("reports/")
                && !p.contains("settings")
                && !p.contains("backlog"))
    );
    let tools = c.profiles["melissa.code.implementer"]
        .tool_ids
        .iter()
        .cloned()
        .collect();
    let capabilities = c.profiles["melissa.code.implementer"]
        .authority
        .capability_ceiling
        .iter()
        .cloned()
        .collect();
    let (pins, registry, policy) =
        melissa_cli::resources::skills("melissa.code.implementer", tools, capabilities).unwrap();
    let material = registry
        .load(&pins[0], &policy)
        .unwrap()
        .material()
        .unwrap();
    assert!(material.files.contains_key("SKILL.md"));
    assert!(!material.files.keys().any(|p| p.starts_with("evals/")));
}
#[test]
fn private_recipe_and_key_reference_inside_project_are_refused_before_inference() {
    let f = Fixture::new();
    let private = f.source.join("private-recipe.json");
    fs::write(&private, serde_json::to_vec(&f.recipe).unwrap()).unwrap();
    let result = f.app().create_execution(
        &f.source,
        "must refuse private recipe",
        vec![],
        Recipe::read(&private).unwrap(),
    );
    assert!(matches!(result, Err(melissa_cli::Error::Denied(_))));
    let mut settings: Value = serde_json::from_slice(&fs::read(&f.config).unwrap()).unwrap();
    settings["credential"] = json!({"kind":"file","path":f.source.join("key-never-read.txt")});
    fs::write(&f.config, serde_json::to_vec(&settings).unwrap()).unwrap();
    f.app().configure(&f.config).unwrap();
    assert!(!f.source.join("key-never-read.txt").exists());
    assert!(matches!(
        f.app().create_execution(
            &f.source,
            "must refuse in-source key reference",
            vec![],
            f.recipe.clone()
        ),
        Err(melissa_cli::Error::Denied(_))
    ));
    assert_eq!(fs::read_dir(f.home.join("tasks")).unwrap().count(), 0);
}
#[test]
fn forward_slash_external_file_reference_does_not_read_a_key_during_task_creation() {
    let f = Fixture::new();
    let mut settings: Value = serde_json::from_slice(&fs::read(&f.config).unwrap()).unwrap();
    let private = f.temp.path().join("external-key-not-created.txt");
    settings["credential"] =
        json!({"kind":"file","path":private.to_string_lossy().replace('\\',"/")});
    fs::write(&f.config, serde_json::to_vec(&settings).unwrap()).unwrap();
    f.app().configure(&f.config).unwrap();
    let recipe_path = f.temp.path().join("outside-recipe.json");
    fs::write(&recipe_path, serde_json::to_vec(&f.recipe).unwrap()).unwrap();
    let recipe = Recipe::read(&PathBuf::from(
        recipe_path.to_string_lossy().replace('\\', "/"),
    ))
    .unwrap();
    let task = f
        .app()
        .create_execution(
            &f.source,
            "create only; never read missing credential",
            vec![],
            recipe,
        )
        .unwrap();
    assert_eq!(task["state"], "awaiting_approval");
    assert_eq!(task["model_budget"]["known_cost_micro_usd"], 0);
    assert!(!private.exists());
}
