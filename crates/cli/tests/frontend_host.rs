#![cfg(windows)]
use melissa_cli::{
    app::App,
    frontend::{Action, Host, Request, Response, pipe::Client},
};
use std::{fs, path::PathBuf};
struct Fixture {
    _temp: tempfile::TempDir,
    home: PathBuf,
    workspace: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let workspace = temp.path().join("Türkçe proje");
        fs::create_dir(&workspace).unwrap();
        fs::write(workspace.join("main.py"), "print('ok')\r\n").unwrap();
        Self {
            _temp: temp,
            home,
            workspace,
        }
    }
    fn host(&self) -> Host {
        Host::start(&self.home, &self.workspace).unwrap()
    }
}
fn req(id: &str, action: Action) -> Request {
    Request {
        version: 1,
        request_id: id.into(),
        after: 0,
        action,
    }
}
fn input(text: &str, revision: u64) -> Action {
    Action::Input {
        text: text.into(),
        expected_revision: revision,
        approval_digest: None,
    }
}
async fn command(h: &Host, id: &str, text: &str, r: u64) -> Response {
    h.request(req(id, input(text, r))).await.unwrap()
}

#[tokio::test]
async fn frontend_prepares_multiline_records_and_reopens_without_a_provider_call() {
    let f = Fixture::new();
    let h = f.host();
    assert!(command(&h, "mode", "/mode plan", 0).await.ok);
    let prepared = command(&h, "goal", "main.py incele\n\tSatır sonlarını koru 🦀", 1).await;
    assert!(prepared.ok, "{:?}", prepared.error);
    assert!(
        prepared.state["draft"]["goal"]
            .as_str()
            .unwrap()
            .contains("\n\t")
    );
    let created = command(&h, "create", "/create", 2).await;
    assert!(created.ok, "{:?}", created.error);
    let id = created.state["task_id"].as_str().unwrap();
    assert!(created.state["approval"].is_object());
    assert!(!created.state["executable"].as_bool().unwrap());
    let read = h
        .request(req(
            "logs",
            Action::Read {
                view: "logs".into(),
            },
        ))
        .await
        .unwrap();
    assert!(read.ok);
    let app = App::open(&f.home).unwrap();
    let before = app.show(id).unwrap();
    assert!(!before["execution_plan"].is_object());
    assert!(!command(&h, "no-run", "/run", 3).await.ok);
    assert_eq!(
        fs::read(f.workspace.join("main.py")).unwrap(),
        b"print('ok')\r\n"
    );
    assert_eq!(app.list().unwrap()["tasks"].as_array().unwrap().len(), 1);
}
#[tokio::test]
async fn frontend_exact_approval_replay_and_revision_protect_effects() {
    let f = Fixture::new();
    let h = f.host();
    command(&h, "goal", "main.py incele", 0).await;
    let task = command(&h, "create", "/create", 1).await;
    assert!(!command(&h, "wrong-approval", "/approve", 2).await.ok);
    let digest = task.state["approval"]["digest"].as_str().unwrap();
    let approved = h
        .request(req(
            "approve",
            Action::Input {
                text: "/approve".into(),
                expected_revision: 2,
                approval_digest: Some(digest.into()),
            },
        ))
        .await
        .unwrap();
    assert!(approved.ok, "{:?}", approved.error);
    let repeat = h
        .request(req(
            "approve",
            Action::Input {
                text: "/approve".into(),
                expected_revision: 2,
                approval_digest: Some(digest.into()),
            },
        ))
        .await
        .unwrap();
    assert!(repeat.ok);
    assert_eq!(repeat.state["revision"], approved.state["revision"]);
    assert!(!command(&h, "stale", "/mode review", 2).await.ok);
    let steer = req("steer-once", input("API'yi koru", 3));
    assert!(h.request(steer.clone()).await.unwrap().ok);
    assert!(h.request(steer).await.unwrap().ok);
    let read = h
        .request(req(
            "conversation",
            Action::Read {
                view: "conversation".into(),
            },
        ))
        .await
        .unwrap();
    assert_eq!(read.value["brief"]["messages"].as_array().unwrap().len(), 1);
    assert!(!command(&h, "steer-once", "Farklı istek", 4).await.ok);
    assert_eq!(read.state["revision"], 4);
}
#[tokio::test]
async fn named_pipe_clients_share_draft_task_and_reconnect_cursor() {
    let f = Fixture::new();
    let a = Client::connect_or_start(&f.home, &f.workspace)
        .await
        .unwrap();
    let b = Client::connect_or_start(&f.home, &f.workspace)
        .await
        .unwrap();
    let r = a
        .request(req("one", input("main.py incele", 0)))
        .await
        .unwrap();
    assert!(r.ok);
    let snapshot = b.request(req("snapshot", Action::Snapshot)).await.unwrap();
    assert_eq!(snapshot.epoch, r.epoch);
    assert_eq!(snapshot.state["draft"], r.state["draft"]);
    let created = b.request(req("two", input("/create", 1))).await.unwrap();
    assert!(created.ok);
    let mut reconnect = req("reconnect", Action::Snapshot);
    reconnect.after = r.cursor;
    let read = a.request(reconnect).await.unwrap();
    assert_eq!(read.state["task_id"], created.state["task_id"]);
    assert!(read.events.iter().all(|e| e.seq > r.cursor));
    assert!(!read.replay_reset);
}
#[tokio::test]
async fn unsupported_protocol_and_foreign_project_task_do_not_change_session() {
    let f = Fixture::new();
    let h = f.host();
    let mut bad = req("future", input("main.py incele", 0));
    bad.version = 2;
    let rejected = h.request(bad).await.unwrap();
    assert!(!rejected.ok);
    assert!(rejected.state["draft"].is_null());
    let other = f._temp.path().join("other");
    fs::create_dir(&other).unwrap();
    fs::write(other.join("x.txt"), "x").unwrap();
    let app = App::open(&f.home).unwrap();
    let task = app
        .create(&other, "İncele", "melissa.code.implementer", vec![])
        .unwrap();
    let denied = command(
        &h,
        "foreign",
        &format!("/open {}", task["task_id"].as_str().unwrap()),
        0,
    )
    .await;
    assert!(!denied.ok);
    assert!(denied.state["task_id"].is_null());
    assert!(
        h.request(req(
            "read-only",
            Action::Read {
                view: "memory".into()
            }
        ))
        .await
        .unwrap()
        .ok
    );
}
