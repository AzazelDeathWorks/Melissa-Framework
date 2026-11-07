use super::*;
use crate::{
    app::{bytes, decode, nonce, now},
    project::{Command as Process, Contract, Environment, tests::Fixture},
    services::{
        self,
        host::{Host, Operation},
    },
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    sync::{Arc, atomic::AtomicBool},
};
fn scenario() -> Check {
    serde_json::from_value(json!({"viewports":[{"name":"narrow","width":390,"height":844},{"name":"wide","width":1280,"height":800}],"accessibility":true,"visual":[],"steps":[
        {"action":"navigate","path":"/?reset=1"},{"action":"wait","selector":"#empty","state":"visible","timeout_ms":10000},{"action":"checkpoint","name":"empty"},
        {"action":"fill","selector":"#name","value":"synthetic-note"},{"action":"press","selector":"#name","key":"Enter"},{"action":"wait","selector":".row","state":"visible","timeout_ms":10000},{"action":"assert_text","selector":".row","text":"synthetic-note"},{"action":"checkpoint","name":"main"},
        {"action":"click","selector":"#slow"},{"action":"wait","selector":"#loading","state":"visible","timeout_ms":10000},{"action":"checkpoint","name":"loading"},{"action":"wait","selector":"#loading","state":"hidden","timeout_ms":10000},
        {"action":"click","selector":"#fail"},{"action":"wait","selector":"#error","state":"visible","timeout_ms":10000},{"action":"assert_text","selector":"#error","text":"Could not load notes"},{"action":"checkpoint","name":"error"},
        {"action":"navigate","path":"/?keep=1"},{"action":"wait","selector":".row","state":"visible","timeout_ms":10000},{"action":"assert_text","selector":".row","text":"synthetic-note"},{"action":"checkpoint","name":"persisted"}
    ]})).unwrap()
}
fn setup(f: &Fixture) -> Contract {
    fs::write(f.source.join("app.py"), include_str!("fixture_app.py")).unwrap();
    fs::write(
        f.source.join("schema.sql"),
        "CREATE TABLE items(id INTEGER PRIMARY KEY,name TEXT NOT NULL);",
    )
    .unwrap();
    let image = std::env::var("MELISSA_PHASE08_IMAGE").unwrap_or_else(|_| {
        "sha256:fd0648f92f70597674b18baf8d9537bd1db8fbcfe8b38da2c5ca864ccf328ded".into()
    });
    let mut environment = Environment {
        image_id: image,
        ..Default::default()
    };
    environment.limits.memory_bytes = 2 * 1024 * 1024 * 1024;
    environment.limits.disk_bytes = 512 * 1024 * 1024;
    environment.limits.output_bytes = 32 * 1024 * 1024;
    Contract {
        services: Some(services::Spec {
            browser: Some(Spec {
                service: "web".into(),
                allowed_services: vec![],
                websocket: false,
                service_workers: false,
                devtools: false,
                checks: BTreeMap::from([("flow".into(), scenario())]),
            }),
            backend: melissa_container_executor::CreationBackend::Compose,
            services: BTreeMap::from([(
                "web".into(),
                services::Service {
                    command: Process {
                        executable: "python".into(),
                        argv: vec!["-I".into(), "app.py".into()],
                        cwd: ".".into(),
                    },
                    port: 38171,
                    ready: services::Ready {
                        path: "/health".into(),
                        status: 200,
                        timeout_ms: 20000,
                    },
                    depends_on: vec!["database".into()],
                },
            )]),
            database: Some(services::Data {
                engine: melissa_web_development::plan::Database::Sqlite,
                schema: Some("schema.sql".into()),
                sample: None,
                provenance: "synthetic".into(),
            }),
            ..Default::default()
        }),
        environment,
        checks: BTreeMap::from([(
            "smoke".into(),
            Process {
                executable: "python".into(),
                argv: vec!["-c".into(), "print('source-check')".into()],
                cwd: ".".into(),
            },
        )]),
        sync: vec![],
        extensions: None,
        verification: None,
    }
}
fn owner(f: &Fixture, c: Contract) -> (Value, Host) {
    let (v, store) = f.running_value(f.contract_task(c.clone()));
    let id = v["task_id"].as_str().unwrap();
    let host = Host::new(
        &f.app,
        &store,
        id,
        "melissa.code.implementer",
        c,
        now().unwrap() + 600000,
    )
    .unwrap();
    (v, host)
}
fn call(host: &mut Host, op: Operation) -> Value {
    let sid = host.session_id().map(str::to_string);
    let r = host.run(
        op,
        sid.as_deref(),
        &format!("frontend:{}", nonce().unwrap()),
        Arc::new(AtomicBool::new(false)),
    );
    if let Err(e) = &r {
        if let Some(id) = host.session_id() {
            let app = crate::app::App::open(&host.home).unwrap();
            let page = app.project_poll(&host.task, id, 0, 256 * 1024).unwrap();
            fs::write(
                host.home.parent().unwrap().join("frontend-failure.json"),
                bytes(&page).unwrap(),
            )
            .unwrap();
            println!(
                "NATIVE_FRONTEND_FAILURE {} {}",
                host.home.parent().unwrap().display(),
                page["state"]["result"]
            );
        }
        panic!("{e}")
    };
    r.unwrap()
}
fn browser(host: &mut Host, command: Command) -> Value {
    call(host, Operation::Browser { command })
}
fn simple(c: &mut Contract) {
    let check = c
        .services
        .as_mut()
        .unwrap()
        .browser
        .as_mut()
        .unwrap()
        .checks
        .get_mut("flow")
        .unwrap();
    check.viewports.truncate(1);
    check.steps=serde_json::from_value(json!([{"action":"navigate","path":"/?reset=1"},{"action":"wait","selector":"#empty","state":"visible","timeout_ms":10000},{"action":"checkpoint","name":"empty"}])).unwrap();
}
fn save(f: &Fixture, name: &str, value: &Value) {
    fs::write(f.root.join(name), bytes(value).unwrap()).unwrap();
    println!("NATIVE_FRONTEND_EVIDENCE {} {}", name, f.root.display());
}
#[test]
fn browser_config_rejects_foreign_services_unreviewed_capabilities_and_invalid_baselines() {
    let f = Fixture::new(false);
    let mut c = setup(&f);
    c.validate().unwrap();
    c.services
        .as_mut()
        .unwrap()
        .browser
        .as_mut()
        .unwrap()
        .allowed_services
        .push("external".into());
    assert!(c.validate().is_err());
    let mut c = setup(&f);
    let b = c.services.as_mut().unwrap().browser.as_mut().unwrap();
    assert!(
        Command::Devtools {
            page: "main".into(),
            mode: "performance".into()
        }
        .validate(b)
        .is_err()
    );
    b.checks.get_mut("flow").unwrap().visual.push(Visual {
        viewport: "narrow".into(),
        checkpoint: "main".into(),
        baseline_id: "../../other".into(),
        max_diff_ratio: 0.0,
    });
    assert!(c.validate().is_err());
    assert!(
        Command::Navigate {
            page: "main".into(),
            path: "//external.invalid/".into()
        }
        .validate(c.services.as_ref().unwrap().browser.as_ref().unwrap())
        .is_err()
    );
}
#[test]
#[ignore = "real owned Chromium + API/SQLite, responsive states, axe, screenshots and trace; no model"]
fn native_frontend_api_database_flow_records_states_and_source_bound_artifacts() {
    let f = Fixture::new(true);
    let c = setup(&f);
    let (v, mut h) = owner(&f, c);
    assert_eq!(call(&mut h, Operation::Start)["outcome"], "PASS");
    let result = browser(
        &mut h,
        Command::Verify {
            name: "flow".into(),
        },
    );
    save(&f, "frontend-flow.json", &result);
    assert_eq!(result["outcome"], "PASS");
    assert_eq!(result["body"]["functionality"], "PASS");
    assert_eq!(result["body"]["accessibility"], "PASS");
    assert_eq!(result["body"]["visual"], "NOT_RUN");
    assert_eq!(result["body"]["viewports"].as_array().unwrap().len(), 2);
    for viewport in result["body"]["viewports"].as_array().unwrap() {
        assert_eq!(viewport["checkpoints"].as_array().unwrap().len(), 5);
    }
    let artifacts = result["browser_artifacts"].as_object().unwrap();
    assert_eq!(artifacts.len(), 10);
    for image in artifacts.values() {
        let raw = h
            .store
            .get_artifact(&h.task, image["artifact_id"].as_str().unwrap())
            .unwrap();
        assert!(raw.starts_with(b"\x89PNG"));
        assert_eq!(baseline::sha(&raw), image["sha256"]);
        assert_eq!(image["source_digest"], result["source_digest"]);
    }
    let query = call(
        &mut h,
        Operation::Database {
            command: melissa_web_development::protocol::Command::Query {
                connection: "verify".into(),
                sql: "SELECT name FROM items".into(),
                parameters: vec![],
            },
        },
    );
    assert_eq!(query["body"]["rows"], json!([["synthetic-note"]]));
    let close = call(&mut h, Operation::Close);
    save(&f, "frontend-close.json", &close);
    assert_eq!(close["outcome"], "PASS");
    assert!(
        close["browser_artifacts"]
            .as_object()
            .unwrap()
            .values()
            .any(|a| a["kind"] == "trace")
    );
    assert!(
        close["browser_artifacts"]
            .as_object()
            .unwrap()
            .values()
            .any(|a| a["kind"] == "browser_events")
    );
    assert!(
        h.store
            .journal()
            .model_requests(v["task_id"].as_str().unwrap())
            .unwrap()
            .is_empty()
    );
}
#[test]
#[ignore = "real accepted visual reference, passing/failed regression and immutable cross-project baseline; no model"]
fn native_visual_baseline_requires_operator_acceptance_and_never_updates_on_failure() {
    let f = Fixture::new(true);
    let mut c = setup(&f);
    simple(&mut c);
    let (_, mut h) = owner(&f, c.clone());
    assert_eq!(call(&mut h, Operation::Start)["outcome"], "PASS");
    let capture = browser(
        &mut h,
        Command::Verify {
            name: "flow".into(),
        },
    );
    assert_eq!(capture["outcome"], "PASS");
    let image = capture["browser_artifacts"]
        .as_object()
        .unwrap()
        .values()
        .find(|a| a["kind"] == "png")
        .unwrap();
    assert!(
        baseline::accept(
            &f.app,
            &h.task,
            baseline::Accept {
                artifact_id: image["artifact_id"].as_str().unwrap().into(),
                name: "empty".into(),
                digest: "wrong".into()
            }
        )
        .is_err()
    );
    let capture_task = h.task.clone();
    call(&mut h, Operation::Close);
    drop(h);
    assert!(
        baseline::accept(
            &f.app,
            &capture_task,
            baseline::Accept {
                artifact_id: image["artifact_id"].as_str().unwrap().into(),
                name: "empty".into(),
                digest: "wrong".into()
            }
        )
        .is_err()
    );
    let accepted = baseline::accept(
        &f.app,
        &capture_task,
        baseline::Accept {
            artifact_id: image["artifact_id"].as_str().unwrap().into(),
            name: "empty".into(),
            digest: image["review_digest"].as_str().unwrap().into(),
        },
    )
    .unwrap();
    let id = accepted["baseline_id"].as_str().unwrap();
    let path = f
        .app
        .home
        .join("visual-baselines")
        .join(format!("{id}.enc"));
    let original = fs::read(&path).unwrap();
    c.services
        .as_mut()
        .unwrap()
        .browser
        .as_mut()
        .unwrap()
        .checks
        .get_mut("flow")
        .unwrap()
        .visual
        .push(Visual {
            viewport: "narrow".into(),
            checkpoint: "empty".into(),
            baseline_id: id.into(),
            max_diff_ratio: 0.0,
        });
    let (_, mut second) = owner(&f, c.clone());
    call(&mut second, Operation::Start);
    let pass = browser(
        &mut second,
        Command::Verify {
            name: "flow".into(),
        },
    );
    save(&f, "visual-pass.json", &pass);
    assert_eq!(pass["body"]["visual"], "PASS");
    call(&mut second, Operation::Close);
    let text = fs::read_to_string(f.source.join("app.py")).unwrap();
    fs::write(
        f.source.join("app.py"),
        text.replace("BACKGROUND = '#ffffff'", "BACKGROUND = '#ffdd00'"),
    )
    .unwrap();
    call(&mut second, Operation::Start);
    let failed = browser(
        &mut second,
        Command::Verify {
            name: "flow".into(),
        },
    );
    save(&f, "visual-fail.json", &failed);
    assert_eq!(failed["outcome"], "FAIL");
    assert_eq!(failed["body"]["functionality"], "PASS");
    assert_eq!(failed["body"]["accessibility"], "PASS");
    assert_eq!(failed["body"]["visual"], "FAIL");
    assert_eq!(failed["body"]["baseline_updated"], false);
    call(&mut second, Operation::Close);
    assert_eq!(fs::read(path).unwrap(), original);
    let other = f.root.join("other-project");
    fs::create_dir(&other).unwrap();
    fs::write(other.join("main.py"), "print('unrelated')").unwrap();
    let repo = melissa_repository::Repository::open(&other, Default::default(), vec![]).unwrap();
    assert!(
        baseline::load(
            &f.app,
            &repo,
            c.services.as_ref().unwrap().browser.as_ref().unwrap()
        )
        .is_err()
    );
    save(&f, "baseline-accepted.json", &accepted);
}
#[test]
#[ignore = "real axe violation and functional failure are independent; no model"]
fn native_accessibility_and_functionality_failures_are_separate_results() {
    let f = Fixture::new(true);
    let mut c = setup(&f);
    simple(&mut c);
    let text = fs::read_to_string(f.source.join("app.py")).unwrap();
    fs::write(
        f.source.join("app.py"),
        text.replace("BAD_A11Y = False", "BAD_A11Y = True"),
    )
    .unwrap();
    let (_, mut h) = owner(&f, c.clone());
    call(&mut h, Operation::Start);
    let result = browser(
        &mut h,
        Command::Verify {
            name: "flow".into(),
        },
    );
    save(&f, "axe-fail.json", &result);
    assert_eq!(result["outcome"], "FAIL");
    assert_eq!(result["body"]["functionality"], "PASS");
    assert_eq!(result["body"]["accessibility"], "FAIL");
    call(&mut h, Operation::Close);
    fs::write(f.source.join("app.py"), text).unwrap();
    let check = c
        .services
        .as_mut()
        .unwrap()
        .browser
        .as_mut()
        .unwrap()
        .checks
        .get_mut("flow")
        .unwrap();
    check.steps.insert(
        2,
        Step::AssertText {
            selector: "#empty".into(),
            text: "wrong expected text".into(),
        },
    );
    let (_, mut h) = owner(&f, c);
    call(&mut h, Operation::Start);
    let result = browser(
        &mut h,
        Command::Verify {
            name: "flow".into(),
        },
    );
    save(&f, "function-fail.json", &result);
    assert_eq!(result["body"]["functionality"], "FAIL");
    assert_eq!(result["body"]["accessibility"], "NOT_RUN");
    call(&mut h, Operation::Close);
}
#[test]
#[ignore = "real WebSocket module replacement, service worker and Chrome DevTools MCP; no model"]
fn native_browser_profiles_support_hmr_service_worker_and_optional_devtools() {
    let f = Fixture::new(true);
    let mut c = setup(&f);
    let b = c.services.as_mut().unwrap().browser.as_mut().unwrap();
    b.websocket = true;
    b.service_workers = true;
    b.devtools = true;
    let (_, mut h) = owner(&f, c);
    call(&mut h, Operation::Start);
    let caps = browser(&mut h, Command::Capabilities);
    assert_eq!(caps["body"]["websocket"], true);
    assert_eq!(caps["body"]["service_workers"], true);
    browser(
        &mut h,
        Command::Navigate {
            page: "main".into(),
            path: "/capabilities?reset=1".into(),
        },
    );
    browser(
        &mut h,
        Command::Fill {
            page: "main".into(),
            selector: "#name".into(),
            value: "preserved-input".into(),
        },
    );
    browser(
        &mut h,
        Command::Click {
            page: "main".into(),
            selector: "#hmr".into(),
        },
    );
    // Locator wait is fixed JS inside Playwright; repeatedly inspecting would hide races.
    let mut seen = false;
    for _ in 0..100 {
        let value = browser(
            &mut h,
            Command::Elements {
                page: "main".into(),
                selector: "#version,#swresult".into(),
            },
        );
        let values = value["body"]["elements"].as_array().unwrap();
        if values.iter().any(|v| v["text"] == "module-v2")
            && values.iter().any(|v| v["text"] == "service-worker-live")
        {
            seen = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(30));
    }
    assert!(seen, "actual SW and module update did not arrive");
    let input = browser(
        &mut h,
        Command::Elements {
            page: "main".into(),
            selector: "#name".into(),
        },
    );
    assert_eq!(input["body"]["elements"][0]["value"], "preserved-input");
    let diagnostic = browser(
        &mut h,
        Command::Devtools {
            page: "main".into(),
            mode: "diagnostics".into(),
        },
    );
    save(&f, "devtools-diagnostics.json", &diagnostic);
    assert_eq!(diagnostic["outcome"], "PASS");
    let trace = browser(
        &mut h,
        Command::Devtools {
            page: "main".into(),
            mode: "performance".into(),
        },
    );
    save(&f, "devtools-performance.json", &trace);
    assert_eq!(trace["outcome"], "PASS");
    assert!(
        trace["browser_artifacts"]
            .as_object()
            .unwrap()
            .values()
            .any(|a| a["kind"] == "performance_trace")
    );
    call(&mut h, Operation::Close);
    let mut c = setup(&f);
    simple(&mut c);
    let (_, mut h) = owner(&f, c);
    call(&mut h, Operation::Start);
    let caps = browser(&mut h, Command::Capabilities);
    assert_eq!(caps["body"]["websocket"], false);
    assert_eq!(caps["body"]["service_workers"], false);
    assert!(
        Operation::Browser {
            command: Command::Devtools {
                page: "main".into(),
                mode: "diagnostics".into()
            }
        }
        .validate(h.contract.services.as_ref().unwrap())
        .is_err()
    );
    call(&mut h, Operation::Close);
}
#[test]
#[ignore = "real browser missing SDK blocks before project service launch; no model"]
fn native_missing_browser_sdk_remains_blocked_without_running_project() {
    let f = Fixture::new(true);
    let mut c = setup(&f);
    c.environment.image_id =
        "sha256:d853da333281e062219bfdffe0fce754dbf01742153032e2e3ae3c71a71aeb39".into();
    let (_, mut h) = owner(&f, c);
    let r = call(&mut h, Operation::Start);
    assert_eq!(r["outcome"], "BLOCKED");
    assert_eq!(r["body"]["command_started"], false);
    assert!(
        h.store
            .journal()
            .model_requests(&h.task)
            .unwrap()
            .is_empty()
    );
    save(&f, "browser-not-run.json", &r);
}

#[test]
#[ignore = "real independent final gate runs reviewed frontend acceptance on the final source; no model"]
fn native_final_gate_requires_browser_acceptance_and_preserves_failure() {
    let f = Fixture::new(true);
    let mut c = setup(&f);
    simple(&mut c);
    fs::write(f.source.join("test_smoke.py"),"import unittest\nclass Smoke(unittest.TestCase):\n def test_source(self): self.assertTrue(True)\n").unwrap();
    c.checks.get_mut("smoke").unwrap().argv = vec![
        "-I".into(),
        "-m".into(),
        "unittest".into(),
        "discover".into(),
        "-p".into(),
        "test_smoke.py".into(),
    ];
    c.services
        .as_mut()
        .unwrap()
        .acceptance
        .push(services::Required::Browser {
            name: "flow".into(),
        });
    let (v, mut store) = f.running_value(f.contract_task(c.clone()));
    let id = v["task_id"].as_str().unwrap();
    let task = store.journal().task(id).unwrap().contract;
    let repo = melissa_repository::Repository::open(&f.source, Default::default(), vec![]).unwrap();
    let frozen =
        melissa_code_quality::source::FrozenSource::capture(&repo, &AtomicBool::new(false))
            .unwrap();
    let passed = crate::project::task::verify(
        &f.app,
        &mut store,
        &task,
        &frozen,
        &repo,
        &c,
        now().unwrap() + 600000,
        Arc::new(AtomicBool::new(false)),
    )
    .unwrap();
    assert_eq!(passed["service_checks"][1]["body"]["accessibility"], "PASS");
    save(&f, "browser-final-pass.json", &passed);
    let text = fs::read_to_string(f.source.join("app.py")).unwrap();
    fs::write(
        f.source.join("app.py"),
        text.replace("BAD_A11Y = False", "BAD_A11Y = True"),
    )
    .unwrap();
    let frozen =
        melissa_code_quality::source::FrozenSource::capture(&repo, &AtomicBool::new(false))
            .unwrap();
    assert!(
        crate::project::task::verify(
            &f.app,
            &mut store,
            &task,
            &frozen,
            &repo,
            &c,
            now().unwrap() + 600000,
            Arc::new(AtomicBool::new(false))
        )
        .is_err()
    );
    assert!(crate::engine::native_stop_proven(&store, id).unwrap());
    assert!(store.journal().model_requests(id).unwrap().is_empty());
}

#[test]
#[ignore = "real CLI helper and authenticated loopback preview; stale source, close and cancellation; no model"]
fn native_browser_helper_and_foreground_preview_bind_source_and_stop_on_cancel() {
    use std::{
        io::{BufRead, BufReader, Read, Write},
        net::TcpStream,
        process::{Command as Run, Stdio},
        time::{Duration, Instant},
    };
    let cli = std::env::var("MELISSA_PHASE08_CLI").expect("build latest CLI first");
    let f = Fixture::new(true);
    let mut c = setup(&f);
    simple(&mut c);
    let v = f.contract_task(c);
    let id = v["task_id"].as_str().unwrap();
    f.app
        .approve(
            id,
            v["approval"]["request_id"].as_str().unwrap(),
            v["approval"]["digest"].as_str().unwrap(),
        )
        .unwrap();
    let helper = Run::new("pwsh")
        .args([
            "-NoLogo",
            "-NoProfile",
            "-File",
            concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../scripts/browser-verify.ps1"
            ),
            "-Task",
            id,
            "-Check",
            "flow",
            "-Melissa",
            &cli,
            "-HomeDirectory",
            f.app.home.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    fs::write(f.root.join("browser-helper.stdout"), &helper.stdout).unwrap();
    fs::write(f.root.join("browser-helper.stderr"), &helper.stderr).unwrap();
    assert!(
        helper.status.success(),
        "{}",
        String::from_utf8_lossy(&helper.stderr)
    );
    let value: Value = decode(&helper.stdout).unwrap();
    assert_eq!(value["outcome"], "PASS");
    assert_eq!(value["model_calls"], 0);
    struct Owned(std::process::Child);
    impl Drop for Owned {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let mut child = Owned(
        Run::new(&cli)
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
            .unwrap(),
    );
    let mut reader = BufReader::new(child.0.stdout.take().unwrap());
    let event = loop {
        let mut line = String::new();
        assert!(
            reader.read_line(&mut line).unwrap() > 0,
            "preview did not start"
        );
        let v: Value = serde_json::from_str(line.trim()).unwrap();
        if v["event"] == "preview_ready" {
            break v;
        }
    };
    let url = url::Url::parse(event["url"].as_str().unwrap()).unwrap();
    let address = format!("127.0.0.1:{}", url.port().unwrap());
    let request = |path: &str, extra: &str| {
        let mut stream = TcpStream::connect(&address).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(30)))
            .unwrap();
        write!(
            stream,
            "GET {path} HTTP/1.1\r\nHost: {address}\r\n{extra}Connection: close\r\n\r\n"
        )
        .unwrap();
        let mut reply = String::new();
        stream.read_to_string(&mut reply).unwrap();
        reply
    };
    assert!(request("/", "").starts_with("HTTP/1.1 403"));
    let granted = request(url.path(), "");
    assert!(granted.starts_with("HTTP/1.1 303"));
    let cookie = granted
        .lines()
        .find_map(|l| l.strip_prefix("Set-Cookie: "))
        .unwrap()
        .split(';')
        .next()
        .unwrap();
    let headers = format!("Cookie: {cookie}\r\n");
    let page = request("/", &headers);
    assert!(page.starts_with("HTTP/1.1 200"));
    assert!(page.contains("Synthetic notes"));
    assert!(
        request(
            "/",
            &format!("{headers}Origin: https://external.invalid\r\n")
        )
        .starts_with("HTTP/1.1 403")
    );
    fs::write(
        f.source.join("main.py"),
        "print('changed while preview was live')\n",
    )
    .unwrap();
    assert!(request("/", &headers).starts_with("HTTP/1.1 409"));
    f.app.cancel(id).unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    while child.0.try_wait().unwrap().is_none() {
        assert!(
            Instant::now() < deadline,
            "preview cancellation did not stop foreground owner"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    let status = child.0.try_wait().unwrap().unwrap();
    let mut error = String::new();
    child
        .0
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut error)
        .unwrap();
    let mut remaining = String::new();
    reader.read_to_string(&mut remaining).unwrap();
    fs::write(f.root.join("preview-owner.stderr"), &error).unwrap();
    fs::write(f.root.join("preview-owner-tail.stdout"), &remaining).unwrap();
    assert!(
        status.success(),
        "foreground owner failed instead of acknowledging cancellation: {error}; {remaining}"
    );
    assert!(TcpStream::connect(&address).is_err());
    let store = f.app.store(id).unwrap();
    assert_eq!(
        store.journal().task(id).unwrap().record.state,
        melissa_core_contracts::TaskState::Cancelled
    );
    assert!(crate::engine::native_stop_proven(&store, id).unwrap());
    assert!(store.journal().model_requests(id).unwrap().is_empty());
    save(
        &f,
        "preview-proof.json",
        &json!({"authenticated_http":true,"cross_origin_denied":true,"stale_source_denied":true,"cancel_closed_listener":true,"model_calls":0}),
    );
}

#[test]
#[ignore = "real browser tool output has native PNG identity, explicit stale status and no foreign authority; no model"]
fn native_browser_image_tool_preserves_ownership_and_historical_source_status() {
    use melissa_model_gateway::{
        ToolCall,
        tools::{ToolContext, ToolDispatcher},
    };
    use std::sync::Mutex;
    let f = Fixture::new(true);
    let mut c = setup(&f);
    simple(&mut c);
    let (_, mut h) = owner(&f, c);
    call(&mut h, Operation::Start);
    let receipt = browser(
        &mut h,
        Command::Verify {
            name: "flow".into(),
        },
    );
    assert_eq!(receipt["outcome"], "PASS");
    let image = receipt["browser_artifacts"]
        .as_object()
        .unwrap()
        .values()
        .find(|a| a["kind"] == "png")
        .unwrap();
    let raw_id = image["artifact_id"].as_str().unwrap().to_owned();
    let task = h.task.clone();
    let shared = Arc::new(Mutex::new(h));
    let mut tools = services::tools::Tools(shared.clone());
    for definition in tools.definitions() {
        jsonschema::validator_for(&definition.definition.parameters).unwrap();
    }
    let invoke = ToolCall {
        id: "image".into(),
        name: "frontend_image".into(),
        arguments: json!({"artifact_id":raw_id}).to_string(),
    };
    let context = ToolContext {
        task_id: task.clone(),
        profile_id: "melissa.code.implementer".into(),
        run_id: "owned-image".into(),
        request_id: "request".into(),
        invocation_id: "native-image-1".into(),
        attempt_id: "attempt".into(),
        generation: 1,
    };
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let result = runtime
        .block_on(tools.execute(
            invoke.clone(),
            context.clone(),
            Arc::new(AtomicBool::new(false)),
        ))
        .unwrap();
    assert!(result.success);
    let value: Value = serde_json::from_str(&result.content).unwrap();
    assert_eq!(value["source_current"], true);
    assert_eq!(value["service_image"]["artifact_id"], raw_id);
    let mut wrong = context.clone();
    wrong.profile_id = "melissa.code.reviewer".into();
    assert!(
        runtime
            .block_on(tools.execute(invoke.clone(), wrong, Arc::new(AtomicBool::new(false))))
            .is_err()
    );
    assert!(tools.validate_call(&ToolCall{id:"private".into(),name:"frontend_image".into(),arguments:json!({"artifact_id":format!("{}:intent",receipt["receipt_id"].as_str().unwrap())}).to_string()}).is_err());
    assert!(tools.validate_call(&ToolCall{id:"preview".into(),name:"project_services".into(),arguments:json!({"session_id":receipt["session_id"],"command":{"operation":"preview","service":"web","method":"GET","path":"/","content_type":null,"body_hex":""}}).to_string()}).is_err());
    fs::write(f.source.join("main.py"), "print('new source')\n").unwrap();
    let result = runtime
        .block_on(tools.execute(invoke, context, Arc::new(AtomicBool::new(false))))
        .unwrap();
    let value: Value = serde_json::from_str(&result.content).unwrap();
    assert_eq!(value["source_current"], false);
    let mut h = shared.lock().unwrap();
    call(&mut h, Operation::Close);
    assert!(h.store.journal().model_requests(&task).unwrap().is_empty());
    save(&f, "frontend-image-tool.json", &value);
}
