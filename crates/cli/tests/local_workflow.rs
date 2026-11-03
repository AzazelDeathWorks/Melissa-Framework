#![cfg(windows)]
use melissa_cli::{Error, app::App};
use melissa_code_quality::vault;
use melissa_core_contracts::TaskState;
use melissa_local_store::{LocalStore, StoreConfig};
use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

struct Fixture {
    _temp: tempfile::TempDir,
    home: PathBuf,
    root: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("özel state");
        let root = temp.path().join("kod projesi");
        fs::create_dir(&root).unwrap();
        fs::write(root.join("main.txt"), "kaynak\r\n").unwrap();
        App::init(&home).unwrap();
        Self {
            _temp: temp,
            home,
            root,
        }
    }
    fn app(&self) -> App {
        App::open(&self.home).unwrap()
    }
    fn task(&self) -> Value {
        self.app()
            .create(
                &self.root,
                "Hataları düzelt 🦀",
                "melissa.code.implementer",
                vec![],
            )
            .unwrap()
    }
    fn store(&self, id: &str) -> LocalStore {
        LocalStore::open(
            &self.home.join("tasks").join(id).join("store"),
            vault::open_key(&self.home.join("store-key.dpapi")).unwrap(),
            StoreConfig::default(),
        )
        .unwrap()
    }
}
fn id(v: &Value) -> &str {
    v["task_id"].as_str().unwrap()
}
fn approve(app: &App, v: &Value) -> melissa_cli::Result<Value> {
    app.approve(
        id(v),
        v["approval"]["request_id"].as_str().unwrap(),
        v["approval"]["digest"].as_str().unwrap(),
    )
}
fn contains_bytes(root: &Path, needle: &[u8]) -> bool {
    fs::read_dir(root).unwrap().any(|e| {
        let p = e.unwrap().path();
        if p.is_dir() {
            contains_bytes(&p, needle)
        } else {
            fs::read(p)
                .unwrap()
                .windows(needle.len())
                .any(|b| b == needle)
        }
    })
}

#[test]
fn encrypted_task_reopens_with_real_state_events_and_exact_review_approval() {
    let f = Fixture::new();
    let key = fs::read(f.home.join("store-key.dpapi")).unwrap();
    assert_eq!(App::init(&f.home).unwrap()["status"], "already_initialized");
    assert_eq!(fs::read(f.home.join("store-key.dpapi")).unwrap(), key);
    let v = f.task();
    assert_eq!(v["state"], "awaiting_approval");
    assert_eq!(v["version"], 2);
    assert_eq!(v["task_accepted"], false);
    assert_eq!(v["model_budget"]["active_requests"], 0);
    assert_eq!(f.app().show(id(&v)).unwrap(), v);
    assert_eq!(
        f.app().list().unwrap()["tasks"].as_array().unwrap().len(),
        1
    );
    assert!(!contains_bytes(&f.home, "Hataları düzelt 🦀".as_bytes()));
    assert!(!contains_bytes(&f.home, "kaynak\r\n".as_bytes()));
    let q = approve(&f.app(), &v).unwrap();
    assert_eq!(q["state"], "queued");
    assert_eq!(q["worker_started"], false);
    assert_eq!(q["API_calls"], 0);
    assert!(approve(&f.app(), &v).is_err());
    let events = f.app().events(id(&v)).unwrap();
    assert!(events["events"].as_array().unwrap().len() >= 4);
    let s = f.store(id(&v));
    let c = s.journal().task(id(&v)).unwrap().contract;
    assert_eq!(c.allowed_effects, ["workspace.read", "evidence.append"]);
    assert_eq!(c.limits.max_cost_micro_usd, 0);
    assert_eq!(c.limits.max_output_tokens, i64::MAX as u64);
}

#[test]
fn approval_rejects_wrong_request_digest_task_and_changed_source() {
    let f = Fixture::new();
    let a = f.task();
    let b = f.task();
    let app = f.app();
    assert!(
        app.approve(id(&a), "wrong", a["approval"]["digest"].as_str().unwrap())
            .is_err()
    );
    assert!(
        app.approve(
            id(&a),
            a["approval"]["request_id"].as_str().unwrap(),
            "wrong"
        )
        .is_err()
    );
    assert!(
        app.approve(
            id(&b),
            a["approval"]["request_id"].as_str().unwrap(),
            a["approval"]["digest"].as_str().unwrap()
        )
        .is_err()
    );
    fs::write(f.root.join("main.txt"), "changed").unwrap();
    assert!(approve(&app, &a).is_err());
    assert_eq!(app.show(id(&a)).unwrap()["state"], "awaiting_approval");
    assert!(
        app.evidence(id(&a), None, None).unwrap()["artifacts"]
            .as_array()
            .unwrap()
            .iter()
            .all(|v| v["logical_type"] != "cli.approval.decision")
    );
}

#[test]
fn explicit_exclusions_and_source_diffs_survive_reopening() {
    let f = Fixture::new();
    fs::create_dir(f.root.join("out")).unwrap();
    fs::write(f.root.join("out/ignored"), "ignore").unwrap();
    fs::write(f.root.join("deleted"), "deleted").unwrap();
    let v = f
        .app()
        .create(
            &f.root,
            "diff",
            "melissa.code.implementer",
            vec!["out".into(), "out".into()],
        )
        .unwrap();
    assert_eq!(v["source_files"], 2);
    assert!(
        f.app().diff(id(&v)).unwrap()["changes"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    approve(&f.app(), &v).unwrap();
    fs::write(f.root.join("main.txt"), "edited").unwrap();
    fs::remove_file(f.root.join("deleted")).unwrap();
    fs::write(f.root.join("added"), "new").unwrap();
    fs::write(f.root.join("out/ignored"), "excluded change").unwrap();
    let d = f.app().diff(id(&v)).unwrap();
    let changes = d["changes"].as_array().unwrap();
    assert_eq!(changes.len(), 3);
    assert!(
        changes
            .iter()
            .any(|c| c["path"] == "main.txt" && c["kind"] == "modified")
    );
    assert!(
        changes
            .iter()
            .any(|c| c["path"] == "added" && c["kind"] == "added")
    );
    assert!(
        changes
            .iter()
            .any(|c| c["path"] == "deleted" && c["kind"] == "deleted")
    );
    assert_eq!(d["raw_source_printed"], false);
}

#[test]
fn evidence_export_is_explicit_exact_and_never_overwrites() {
    let f = Fixture::new();
    let v = f.task();
    let artifacts = f.app().evidence(id(&v), None, None).unwrap();
    let a = artifacts["artifacts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["logical_type"] == "cli.source.bytes")
        .unwrap();
    let artifact = a["artifact_id"].as_str().unwrap();
    let output = f._temp.path().join("export.bin");
    let exported = f
        .app()
        .evidence(id(&v), Some(artifact), Some(&output))
        .unwrap();
    assert_eq!(exported["bytes"], "kaynak\r\n".len());
    assert_eq!(fs::read(&output).unwrap(), b"kaynak\r\n");
    assert!(
        f.app()
            .evidence(id(&v), Some(artifact), Some(&output))
            .is_err()
    );
    assert_eq!(fs::read(&output).unwrap(), b"kaynak\r\n");
    assert!(f.app().evidence(id(&v), Some(artifact), None).is_err());
    assert!(
        f.app()
            .evidence(
                id(&v),
                Some("does-not-exist"),
                Some(&output.with_extension("missing"))
            )
            .is_err()
    );
    assert!(!output.with_extension("missing").exists());
}

#[test]
fn cancellation_is_durable_idempotent_and_not_completion() {
    let f = Fixture::new();
    let v = f.task();
    assert_eq!(f.app().cancel(id(&v)).unwrap()["state"], "cancelled");
    let count = f.app().events(id(&v)).unwrap()["events"]
        .as_array()
        .unwrap()
        .len();
    assert_eq!(f.app().cancel(id(&v)).unwrap()["already_cancelled"], true);
    assert_eq!(
        f.app().events(id(&v)).unwrap()["events"]
            .as_array()
            .unwrap()
            .len(),
        count
    );
    assert_eq!(f.app().show(id(&v)).unwrap()["task_accepted"], false);
    assert!(approve(&f.app(), &v).is_err());
    let queued = f.task();
    approve(&f.app(), &queued).unwrap();
    assert_eq!(f.app().cancel(id(&queued)).unwrap()["state"], "cancelled");
}

#[test]
fn busy_or_leased_work_is_not_reported_as_stopped() {
    let f = Fixture::new();
    let v = f.task();
    approve(&f.app(), &v).unwrap();
    let mut s = f.store(id(&v));
    assert!(matches!(f.app().cancel(id(&v)), Err(Error::Unavailable(_))));
    assert_eq!(f.app().list().unwrap()["tasks"][0]["state"], "store_busy");
    let version = s.journal().task(id(&v)).unwrap().record.version;
    s.transition_task(id(&v), version, TaskState::Leased, None)
        .unwrap();
    drop(s);
    assert!(f.app().cancel(id(&v)).is_err());
    assert_eq!(f.app().show(id(&v)).unwrap()["state"], "leased");
}

#[test]
fn source_root_replacement_cannot_reuse_an_approval() {
    let f = Fixture::new();
    let v = f.task();
    fs::rename(&f.root, f.root.with_extension("preserved")).unwrap();
    fs::create_dir(&f.root).unwrap();
    fs::write(f.root.join("main.txt"), "kaynak\r\n").unwrap();
    assert!(approve(&f.app(), &v).is_err());
    assert!(f.app().diff(id(&v)).is_err());
}

#[test]
fn invalid_paths_profiles_or_incomplete_stores_do_not_create_replacement_records() {
    let f = Fixture::new();
    assert!(melissa_cli::app::absolute(&f.root.join("..")).is_err());
    for bad in [
        "../task_x",
        "task_short",
        "task_XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX",
        "task_🦀xxxxxxxxxxxxxxxxxxxxxxxxxxxx",
    ] {
        assert!(f.app().show(bad).is_err());
    }
    for goal in ["", "  ", "\0"] {
        assert!(
            f.app()
                .create(&f.root, goal, "melissa.code.implementer", vec![])
                .is_err()
        );
    }
    assert!(f.app().create(&f.root, "goal", "unknown", vec![]).is_err());
    assert!(
        f.app()
            .create(&f.root, "goal", "melissa.core.verify", vec![])
            .is_err()
    );
    assert!(
        f.app()
            .create(
                &f.root,
                "goal",
                "melissa.code.implementer",
                vec!["../outside".into()]
            )
            .is_err()
    );
    assert!(
        f.app()
            .create(f._temp.path(), "goal", "melissa.code.implementer", vec![])
            .is_err()
    );
    assert!(
        f.app()
            .create(
                &f.home.join("tasks"),
                "goal",
                "melissa.code.implementer",
                vec![]
            )
            .is_err()
    );
    let bad = "task_00000000000000000000000000000000";
    let dir = f.home.join("tasks").join(bad);
    fs::create_dir(&dir).unwrap();
    assert!(f.app().show(bad).is_err());
    assert!(!dir.join("store").exists());
}

#[test]
fn corrupt_home_manifest_or_foreign_data_is_preserved_and_rejected() {
    let f = Fixture::new();
    let foreign = f._temp.path().join("foreign");
    fs::create_dir(&foreign).unwrap();
    fs::write(foreign.join("keep"), "preserve").unwrap();
    assert!(App::init(&foreign).is_err());
    assert_eq!(fs::read(foreign.join("keep")).unwrap(), b"preserve");
    let path = f.home.join("home.json");
    let invalid = b"{\"schema_version\":\"melissa.cli-home/v1\",\"schema_version\":\"other\",\"installation_id\":\"00000000000000000000000000000000\"}";
    fs::write(&path, invalid).unwrap();
    assert!(App::open(&f.home).is_err());
    assert!(App::init(&f.home).is_err());
    assert_eq!(fs::read(&path).unwrap(), invalid);
}

#[test]
fn standalone_binary_reports_json_errors_and_persistent_task_state() {
    let f = Fixture::new();
    let binary = env!("CARGO_BIN_EXE_melissa");
    let output = Command::new(binary)
        .args(["--home"])
        .arg(&f.home)
        .args(["--json", "task", "new", "--workspace"])
        .arg(&f.root)
        .args(["--goal", "CLI üzerinden oluştur"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let v: Value = serde_json::from_slice(&output.stdout).unwrap();
    let show = Command::new(binary)
        .args(["--home"])
        .arg(&f.home)
        .args(["--json", "task", "show", id(&v)])
        .output()
        .unwrap();
    assert!(show.status.success());
    assert_eq!(serde_json::from_slice::<Value>(&show.stdout).unwrap(), v);
    let bad = Command::new(binary)
        .args(["--home"])
        .arg(&f.home)
        .args(["--json", "task", "show", "../outside"])
        .output()
        .unwrap();
    assert_eq!(bad.status.code(), Some(2));
    assert!(bad.stdout.is_empty());
    assert_eq!(
        serde_json::from_slice::<Value>(&bad.stderr).unwrap()["ok"],
        false
    );
    let catalog = Command::new(binary)
        .arg("--json")
        .arg("profiles")
        .env_remove("LOCALAPPDATA")
        .env_remove("MELISSA_HOME")
        .output()
        .unwrap();
    assert!(catalog.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&catalog.stdout).unwrap()["profiles"]
            .as_array()
            .unwrap()
            .len(),
        18
    );
}
