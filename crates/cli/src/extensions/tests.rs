//! Controlled local transports and native workers; no live provider inference.
use super::*;
use crate::{
    app::bytes,
    project::{Command, Contract, Environment, tests::Fixture},
};
use melissa_model_gateway::{
    ToolCall,
    tools::{ToolContext, ToolDispatcher},
};
use std::{
    fs,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
fn python(code: &str) -> Command {
    Command {
        executable: "python".into(),
        argv: vec!["-c".into(), code.into()],
        cwd: ".".into(),
    }
}
fn contract(bundle: Bundle) -> Contract {
    Contract {
        verification: None,
        extensions: Some(bundle),
        environment: Environment::default(),
        checks: BTreeMap::from([("test".into(), python("pass"))]),
        sync: vec![],
    }
}
fn skill(root: &Path, name: &str, body: &str) {
    fs::create_dir_all(root.join(name).join("references")).unwrap();
    fs::write(root.join(name).join("SKILL.md"),format!("---\nname: {name}\ndescription: Detailed migration testing procedures\nallowed-tools: host.root-anything\n---\n{body}\n")).unwrap();
    fs::write(
        root.join(name).join("references/detail.txt"),
        "exact reference contents\n",
    )
    .unwrap();
}
fn ctx(task: &str, id: &str) -> ToolContext {
    ToolContext {
        task_id: task.into(),
        profile_id: "melissa.code.implementer".into(),
        run_id: "fixture-run".into(),
        request_id: "fixture-no-provider".into(),
        invocation_id: id.into(),
        attempt_id: format!("attempt-{id}"),
        generation: 1,
    }
}
fn call(name: &str, value: Value) -> ToolCall {
    ToolCall {
        id: "local-fixture-call".into(),
        name: name.into(),
        arguments: value.to_string(),
    }
}
#[test]
fn dynamic_catalog_pins_full_instructions_precedence_resources_and_import_updates() {
    let f = Fixture::new(false);
    let user = f.app.home.join("skills");
    let project = f.source.join(".melissa/skills");
    skill(&user, "migration-check", "user complete instructions");
    skill(
        &project,
        "migration-check",
        "project full instructions and scope rules",
    );
    fs::write(
        project.join("migration-check/.env"),
        "OWNED_FIXTURE_HIDDEN=not-a-credential",
    )
    .unwrap();
    let repo = melissa_repository::Repository::open(&f.source, Default::default(), vec![]).unwrap();
    let bundle = f
        .app
        .freeze_extensions(&repo, "migration testing")
        .unwrap()
        .unwrap();
    assert!(
        bundle.skills["migration-check"]
            .origin
            .starts_with("project:")
    );
    assert!(!bundle.skills["migration-check"].files.contains_key(".env"));
    assert_eq!(
        bundle.context()["selected_full_instructions"][0]["SKILL.md"],
        String::from_utf8(bundle.skills["migration-check"].files["SKILL.md"].clone()).unwrap()
    );
    let v = f.contract_task(contract(bundle.clone()));
    let id = v["task_id"].as_str().unwrap();
    fs::write(
        project.join("migration-check/references/detail.txt"),
        "changed after task approval preparation",
    )
    .unwrap();
    let store = f.app.store(id).unwrap();
    let frozen = f
        .app
        .execution(&store, id)
        .unwrap()
        .unwrap()
        .recipe
        .project
        .unwrap()
        .extensions
        .unwrap();
    assert_eq!(frozen.fingerprint().unwrap(), bundle.fingerprint().unwrap());
    assert_eq!(
        frozen.skills["migration-check"].files["references/detail.txt"],
        b"exact reference contents\n"
    );
    drop(store);
    let imports = f.root.join("external");
    skill(&imports, "import-check", "original import");
    let package = imports.join("import-check");
    let first = f.app.import_skill(&package, None).unwrap();
    assert!(f.app.import_skill(&package, Some("blake3:wrong")).is_err());
    fs::write(package.join("references/detail.txt"), "new import revision").unwrap();
    let second = f.app.import_skill(&package, None).unwrap();
    assert_ne!(first["digest"], second["digest"]);
    let current = skills::catalog(&f.app, &repo).unwrap();
    assert_eq!(current["import-check"].digest, second["digest"]);
    let excluded = melissa_repository::Repository::open(
        &f.source,
        Default::default(),
        vec![".melissa/skills".into()],
    )
    .unwrap();
    assert!(
        !skills::catalog(&f.app, &excluded)
            .unwrap()
            .values()
            .any(|p| p.origin.starts_with("project:"))
    );
    let source = fs::read(f.app.home.join("skill-imports/current/import-check.enc")).unwrap();
    assert!(!String::from_utf8_lossy(&source).contains("new import revision"));
}
#[test]
fn metadata_and_helper_cannot_create_authority_or_escape_resource_pins() {
    let f = Fixture::new(false);
    let root = f.source.join(".melissa/skills");
    skill(
        &root,
        "migration-check",
        "request host.root must remain untrusted",
    );
    let repo = melissa_repository::Repository::open(&f.source, Default::default(), vec![]).unwrap();
    let bundle = f
        .app
        .freeze_extensions(&repo, "migration testing")
        .unwrap()
        .unwrap();
    let bad = hooks::Helper {
        command: Command {
            executable: "python".into(),
            argv: vec!["@skill/../../.env".into()],
            cwd: ".".into(),
        },
        skill: Some("migration-check".into()),
    };
    assert!(bad.validate(&bundle.skills).is_err());
    let v = f.contract_task(contract(bundle.clone()));
    let id = v["task_id"].as_str().unwrap();
    let store = f.app.store(id).unwrap();
    let t = store.journal().task(id).unwrap();
    assert!(
        !t.contract
            .allowed_effects
            .iter()
            .any(|e| e == "host.root-anything")
    );
    let c = f.app.execution(&store, id).unwrap().unwrap();
    let tools = tools::Tools::new(
        &f.app,
        &store,
        id,
        c.recipe.project.unwrap(),
        c.expires_at_ms,
        false,
    )
    .unwrap();
    assert!(
        tools
            .validate_call(&call(
                "extension_skill",
                json!({"operation":"read","skill":"migration-check","path":"../.env"})
            ))
            .is_err()
    );
    assert!(
        tools
            .validate_call(&call(
                "extension_skill",
                json!({"operation":"helper","helper":"not-reviewed"})
            ))
            .is_err()
    );
    assert!(
        tools
            .validate_call(&call(
                "extension_skill",
                json!({"operation":"load","skill":"migration-check","command":"host arbitrary"})
            ))
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
}
#[test]
fn exact_http_destinations_headers_and_sse_never_expand_scope() {
    let endpoint = http::Endpoint {
        url: "https://docs.python.org/3.13".into(),
        auth_env: None,
        loopback: false,
        timeout_ms: 30_000,
        max_bytes: 1024 * 1024,
    };
    endpoint.validate().unwrap();
    for url in [
        "http://public.example/",
        "https://key@public.example/",
        "https://public.example/?api_key=inline",
        "https://127.0.0.1/x",
    ] {
        let mut bad = endpoint.clone();
        bad.url = url.into();
        assert!(bad.validate().is_err());
    }
    let source = docs::Source {
        package: "python".into(),
        ecosystem: "runtime".into(),
        endpoint,
        versions: BTreeMap::from([(
            "3.13.5".into(),
            "https://docs.python.org/3.13/library/".into(),
        )]),
        context7: false,
        search_path: None,
    };
    source.validate().unwrap();
    for url in [
        "https://docs.python.org/3.130/evil",
        "https://evil.example/3.13/",
        "https://docs.python.org/3.13/../secret",
    ] {
        assert!(source.check_url(url).is_err());
    }
    assert_eq!(
        headers::value("line1\nline2"),
        "=?base64?bGluZTEKbGluZTI=?="
    );
    let schema = json!({"type":"object","properties":{"nested":{"type":"object","properties":{"region":{"type":"string","x-mcp-header":"Region"}}}}});
    assert_eq!(
        headers::parameters(&schema, &json!({"nested":{"region":"hello 世界"}})).unwrap()[0].0,
        "Mcp-Param-Region"
    );
    for bad in [
        json!({"type":"object","x-mcp-header":"Root"}),
        json!({"type":"object","properties":{"x":{"type":"number","x-mcp-header":"Unsafe"}}}),
        json!({"type":"object","allOf":[{"properties":{"x":{"type":"string","x-mcp-header":"Unsafe"}}}]}),
        json!({"type":"object","properties":{"x":{"type":"string","x-mcp-header":"x\r\nHost"}}}),
    ] {
        assert!(headers::parameters(&bad, &json!({})).is_err());
    }
    let raw=b"event: message\ndata: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\"}\n\ndata: {\"jsonrpc\":\"2.0\",\"id\":7,\"result\":{\"value\":42}}\n\n";
    assert_eq!(
        mcp::sse_response(raw, &json!(7)).unwrap()["result"]["value"],
        42
    );
    assert!(mcp::sse_response(raw, &json!(8)).is_err());
    assert!(mcp::sse_response(b"data: {\"id\":7}", &json!(7)).is_err());
}
#[test]
fn ten_workflow_templates_are_importable_pinned_packages_and_cli_flags_are_strict() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("skills/workflows");
    let manifest: Value =
        serde_json::from_slice(&fs::read(root.join("manifest.json")).unwrap()).unwrap();
    let packages = manifest["packages"].as_array().unwrap();
    assert_eq!(packages.len(), 10);
    let catalog = crate::resources::catalog().unwrap();
    for p in packages {
        let name = p["name"].as_str().unwrap();
        let repo =
            melissa_repository::Repository::open(&root.join(name), Default::default(), vec![])
                .unwrap();
        let package = skills::Package::read_from(&repo, "", "test-template".into()).unwrap();
        assert_eq!(package.name, name);
        assert!(package.files.contains_key("references/acceptance.md"));
        assert!(package.files.contains_key("assets/result.template.json"));
        assert!(
            catalog
                .profiles
                .contains_key(p["owner_profile"].as_str().unwrap())
        );
    }
    for args in [
        vec!["extension", "list", "--workspace", "."],
        vec![
            "extension",
            "import-skill",
            "--path",
            "my-skill",
            "--digest",
            "blake3:reviewed",
        ],
        vec![
            "extension",
            "resolve",
            "--task",
            "task_owned",
            "--source",
            "python",
            "--query",
            "typing",
        ],
    ] {
        crate::args::parse(args.into_iter().map(std::ffi::OsString::from)).unwrap();
    }
    for args in [
        vec!["extension", "list", "--path", "wrong"],
        vec!["extension", "resolve", "--task", "x", "--source", "y"],
        vec![
            "extension",
            "configure",
            "--workspace",
            ".",
            "--path",
            "x",
            "--command",
            "shell",
        ],
    ] {
        assert!(crate::args::parse(args.into_iter().map(std::ffi::OsString::from)).is_err());
    }
}
#[tokio::test]
async fn unapproved_mismatched_or_interrupted_extension_intents_never_start_a_helper() {
    let f = Fixture::new(false);
    let mut bundle = Bundle::default();
    bundle.config.helpers.insert(
        "owned".into(),
        hooks::Helper {
            command: python("print('must not start')"),
            skill: None,
        },
    );
    let v = f.contract_task(contract(bundle));
    let id = v["task_id"].as_str().unwrap();
    let mut store = f.app.store(id).unwrap();
    let e = f.app.execution(&store, id).unwrap().unwrap();
    let c = e.recipe.project.unwrap();
    let mut tools =
        tools::Tools::new(&f.app, &store, id, c.clone(), e.expires_at_ms, true).unwrap();
    let request = call(
        "extension_skill",
        json!({"operation":"helper","helper":"owned","input":{}}),
    );
    assert!(
        tools
            .execute(
                request.clone(),
                ctx(id, "unapproved"),
                Arc::new(AtomicBool::new(false))
            )
            .await
            .is_err()
    );
    let mut changed = c.clone();
    changed
        .environment
        .versions
        .insert("python".into(), "9.9.9".into());
    assert!(tools::Tools::new(&f.app, &store, id, changed, e.expires_at_ms, true).is_err());
    drop(tools);
    drop(store);
    f.app
        .approve(
            id,
            v["approval"]["request_id"].as_str().unwrap(),
            v["approval"]["digest"].as_str().unwrap(),
        )
        .unwrap();
    store = f.app.store(id).unwrap();
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
    let intent = format!("extension:call:{}:intent", blake3::hash(b"interrupted"));
    crate::app::put(
        &mut store,
        id,
        &intent,
        "extension.call.intent",
        &bytes(&json!({"fixture":"simulated interrupted pending call"})).unwrap(),
    )
    .unwrap();
    let mut tools = tools::Tools::new(&f.app, &store, id, c, e.expires_at_ms, true).unwrap();
    let result = tools
        .execute(
            request,
            ctx(id, "interrupted"),
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .unwrap();
    assert!(!result.success && result.requires_reconciliation);
    assert!(
        !f.app
            .home
            .join("tasks")
            .join(id)
            .join("project-sessions")
            .exists()
    );
    assert!(store.journal().model_requests(id).unwrap().is_empty());
}
#[tokio::test]
async fn actual_cli_workflow_node_binds_extension_tools_and_rejects_missing_effect() {
    let f = Fixture::new(false);
    let mut bundle = Bundle::default();
    bundle.config.helpers.insert(
        "owned".into(),
        hooks::Helper {
            command: python("pass"),
            skill: None,
        },
    );
    let (v, store) = f.running_value(f.contract_task(contract(bundle)));
    let id = v["task_id"].as_str().unwrap();
    let execution = f.app.execution(&store, id).unwrap().unwrap();
    let task = store.journal().task(id).unwrap().contract;
    let plan = crate::engine::plan(&task, &execution).unwrap();
    assert!(
        plan.nodes
            .iter()
            .all(|n| n.tools.contains("extensions.use")
                && n.effects.contains("extensions.use_scoped"))
    );
    let source = task.input_snapshot_digest.clone();
    let policy = execution.settings_digest.clone();
    let refresh = Arc::new(move || {
        Ok(melissa_workflow::scheduler::Freshness {
            workspace_digest: source.clone(),
            policy_digest: policy.clone(),
        })
    });
    let cancel = Arc::new(AtomicBool::new(false));
    let node = plan.nodes[0].id.clone();
    let mut scheduler = melissa_workflow::scheduler::Scheduler::create_until(
        store.connect().unwrap(),
        plan,
        &crate::resources::catalog().unwrap(),
        refresh,
        cancel.clone(),
        execution.expires_at_ms,
    )
    .unwrap();
    let packet = scheduler
        .acquire(&node, "offline-extension-node", 60_000)
        .unwrap();
    let shared = Arc::new(Mutex::new(scheduler));
    let c = execution.recipe.project.unwrap();
    let mut forged = packet.clone();
    forged.node.effects.remove("extensions.use_scoped");
    let tools = tools::Tools::new(
        &f.app,
        &store,
        id,
        c.clone(),
        execution.expires_at_ms,
        false,
    )
    .unwrap();
    assert!(
        melissa_workflow::gate::NodeDispatcher::new(shared.clone(), forged, Box::new(tools))
            .is_err()
    );
    let tools = tools::Tools::new(&f.app, &store, id, c, execution.expires_at_ms, false).unwrap();
    let mut context = ctx(id, "node-catalog");
    context.profile_id = packet.node.profile.clone();
    context.run_id = packet.lease.run.clone();
    let mut dispatcher =
        melissa_workflow::gate::NodeDispatcher::new(shared, packet, Box::new(tools)).unwrap();
    let result = dispatcher
        .execute(
            call("extension_skill", json!({"operation":"catalog"})),
            context,
            cancel,
        )
        .await
        .unwrap();
    assert!(result.success, "{}", result.content);
    assert!(store.journal().model_requests(id).unwrap().is_empty());
}
#[derive(Clone)]
struct Request {
    method: String,
    path: String,
    headers: String,
    body: Value,
}
struct Reply {
    status: u16,
    content_type: &'static str,
    body: String,
    headers: Vec<(&'static str, String)>,
}
fn reply(body: Value) -> Reply {
    Reply {
        status: 200,
        content_type: "application/json",
        body: body.to_string(),
        headers: vec![],
    }
}
struct HttpFixture {
    url: String,
    requests: Arc<Mutex<Vec<Request>>>,
    stop: Arc<AtomicBool>,
    join: Option<thread::JoinHandle<()>>,
}
impl HttpFixture {
    fn new(handler: impl Fn(&Request) -> Reply + Send + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        listener.set_nonblocking(true).unwrap();
        let requests = Arc::new(Mutex::new(vec![]));
        let observed = requests.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let join = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(180);
            while !stopped.load(Ordering::Acquire) && Instant::now() < deadline {
                let Ok((mut stream, _)) = listener.accept() else {
                    thread::sleep(Duration::from_millis(5));
                    continue;
                };
                // Windows accepted sockets can inherit FIONBIO. Explicitly
                // select blocking IO before read_exact; otherwise startup races
                // look like truncated HTTP responses and discard requests.
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                if let Some(request) = read_request(&mut stream) {
                    observed.lock().unwrap().push(request.clone());
                    let reply = handler(&request);
                    let headers = reply
                        .headers
                        .iter()
                        .map(|(k, v)| format!("{k}: {v}\r\n"))
                        .collect::<String>();
                    let raw = format!(
                        "HTTP/1.1 {} Fixture\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n{}\r\n{}",
                        reply.status,
                        reply.content_type,
                        reply.body.len(),
                        headers,
                        reply.body
                    );
                    let _ = stream.write_all(raw.as_bytes());
                }
            }
        });
        Self {
            url,
            requests,
            stop,
            join: Some(join),
        }
    }
    fn endpoint(&self, path: &str) -> http::Endpoint {
        http::Endpoint {
            url: format!("{}{path}", self.url),
            auth_env: None,
            loopback: true,
            timeout_ms: 5000,
            max_bytes: 1024 * 1024,
        }
    }
}
impl Drop for HttpFixture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(join) = self.join.take() {
            join.join().unwrap();
        }
    }
}
fn read_request(stream: &mut TcpStream) -> Option<Request> {
    let mut raw = vec![];
    let mut byte = [0];
    while !raw.ends_with(b"\r\n\r\n") {
        stream.read_exact(&mut byte).ok()?;
        raw.push(byte[0]);
        if raw.len() > 65536 {
            return None;
        }
    }
    let headers = String::from_utf8(raw).ok()?;
    let first = headers
        .lines()
        .next()?
        .split_whitespace()
        .collect::<Vec<_>>();
    let len = headers
        .lines()
        .find_map(|l| {
            l.to_ascii_lowercase()
                .strip_prefix("content-length:")
                .map(str::trim)
                .and_then(|v| v.parse::<usize>().ok())
        })
        .unwrap_or(0);
    let mut body = vec![0; len];
    stream.read_exact(&mut body).ok()?;
    Some(Request {
        method: first[0].into(),
        path: first[1].into(),
        body: serde_json::from_slice(&body).unwrap_or(Value::Null),
        headers,
    })
}
fn schema() -> Value {
    json!({"type":"object","properties":{"message":{"type":"string","x-mcp-header":"Message"}},"required":["message"],"additionalProperties":false})
}
fn server(endpoint: http::Endpoint, version: &str, writes: bool) -> mcp::Server {
    mcp::Server {
        connection: mcp::Transport::Http { endpoint },
        protocol_version: version.into(),
        tools: BTreeMap::from([(
            "echo".into(),
            mcp::Tool {
                description: "Owned fixture echo".into(),
                input_schema: schema(),
                writes,
            },
        )]),
    }
}
fn mcp_reply(request: &Request, version: &str, bad_schema: bool, sse: bool) -> Reply {
    if request.method == "DELETE" {
        return Reply {
            status: 204,
            body: String::new(),
            ..reply(Value::Null)
        };
    }
    let id = &request.body["id"];
    let method = request.body["method"].as_str().unwrap_or("");
    if method == "notifications/initialized" {
        return Reply {
            status: 202,
            body: String::new(),
            ..reply(Value::Null)
        };
    }
    let result = match method {
        "initialize" => {
            json!({"protocolVersion":version,"capabilities":{"tools":{}},"serverInfo":{"name":"owned-fixture","version":"1"}})
        }
        "server/discover" => {
            json!({"protocolVersions":[version],"capabilities":{"tools":{}},"serverInfo":{"name":"owned-fixture","version":"1"}})
        }
        "tools/list" => {
            json!({"tools":[{"name":"echo","inputSchema":if bad_schema{json!({"type":"object"})}else{schema()}},{"name":"unreviewed","inputSchema":{"type":"object"}}]})
        }
        _ => {
            json!({"resultType":"complete","content":[{"type":"text","text":request.body["params"]["arguments"]["message"]}],"isError":false})
        }
    };
    let mut r = reply(json!({"jsonrpc":"2.0","id":id,"result":result}));
    if method == "initialize" {
        r.headers.push(("Mcp-Session-Id", "owned-session".into()));
    }
    if sse && method == "tools/call" {
        r.body = format!("data: {}\n\n", r.body);
        r.content_type = "text/event-stream";
    }
    r
}
#[tokio::test(flavor = "multi_thread")]
async fn http_mcp_legacy_and_modern_validate_schema_headers_lifecycle_and_exact_once_receipts() {
    for version in ["2025-11-25", "2026-07-28"] {
        let v = version.to_owned();
        let http = HttpFixture::new(move |r| mcp_reply(r, &v, false, true));
        let f = Fixture::new(false);
        let mut bundle = Bundle::default();
        bundle.config.mcp.insert(
            "owned".into(),
            server(http.endpoint("/mcp"), version, false),
        );
        let (v, store) = f.running_value(f.contract_task(contract(bundle)));
        let id = v["task_id"].as_str().unwrap();
        let e = f.app.execution(&store, id).unwrap().unwrap();
        let mut tools = tools::Tools::new(
            &f.app,
            &store,
            id,
            e.recipe.project.unwrap(),
            e.expires_at_ms,
            true,
        )
        .unwrap();
        let request = call(
            "extension_mcp",
            json!({"server":"owned","tool":"echo","arguments":{"message":"merhaba 世界"}}),
        );
        let output = tools
            .execute(
                request.clone(),
                ctx(id, "mcp-once"),
                Arc::new(AtomicBool::new(false)),
            )
            .await
            .unwrap();
        assert!(output.success, "{}", output.content);
        assert!(!output.requires_reconciliation);
        assert!(output.content.contains("merhaba 世界"));
        let count = http.requests.lock().unwrap().len();
        let duplicate = tools
            .execute(
                request,
                ctx(id, "mcp-once"),
                Arc::new(AtomicBool::new(false)),
            )
            .await
            .unwrap();
        assert_eq!(duplicate.content, output.content);
        assert_eq!(http.requests.lock().unwrap().len(), count);
        assert!(
            tools
                .validate_call(&call(
                    "extension_mcp",
                    json!({"server":"owned","tool":"unreviewed","arguments":{}})
                ))
                .is_err()
        );
        tools.close().await.unwrap();
        assert!(
            store
                .list_artifacts(id)
                .unwrap()
                .iter()
                .any(|a| a.logical_type == "cli.extension.lifecycle")
        );
        let requests = http.requests.lock().unwrap();
        if version == "2026-07-28" {
            assert!(!requests.iter().any(|r| r.body["method"] == "initialize"));
            let called = requests
                .iter()
                .find(|r| r.body["method"] == "tools/call")
                .unwrap();
            assert!(
                called
                    .headers
                    .to_ascii_lowercase()
                    .contains("mcp-method: tools/call")
            );
            assert!(called.headers.contains("=?base64?"));
            assert_eq!(
                called.body["params"]["_meta"]["io.modelcontextprotocol/protocolVersion"],
                version
            );
        } else {
            assert!(requests.iter().any(|r| r.method == "DELETE"));
        }
        assert!(store.journal().model_requests(id).unwrap().is_empty());
    }
}
#[tokio::test(flavor = "multi_thread")]
async fn remote_schema_drift_readonly_write_and_ambiguous_call_are_not_success_or_replayed() {
    let http = HttpFixture::new(|r| mcp_reply(r, "2025-11-25", true, false));
    let f = Fixture::new(false);
    let mut b = Bundle::default();
    b.config.mcp.insert(
        "owned".into(),
        server(http.endpoint("/mcp"), "2025-11-25", false),
    );
    let (v, store) = f.running_value(f.contract_task(contract(b)));
    let id = v["task_id"].as_str().unwrap();
    let e = f.app.execution(&store, id).unwrap().unwrap();
    let mut tools = tools::Tools::new(
        &f.app,
        &store,
        id,
        e.recipe.project.unwrap(),
        e.expires_at_ms,
        false,
    )
    .unwrap();
    let output = tools
        .execute(
            call(
                "extension_mcp",
                json!({"server":"owned","tool":"echo","arguments":{"message":"never dispatched"}}),
            ),
            ctx(id, "schema-drift"),
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .unwrap();
    assert!(!output.success);
    assert!(
        !http
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|r| r.body["method"] == "tools/call")
    );
    tools.close().await.unwrap();
    drop(tools);
    let remote = HttpFixture::new(|r| {
        if r.body["method"] == "tools/call" {
            Reply {
                status: 500,
                body: "failure after dispatch".into(),
                ..reply(Value::Null)
            }
        } else {
            mcp_reply(r, "2026-07-28", false, false)
        }
    });
    let f = Fixture::new(false);
    let mut b = Bundle::default();
    b.config.mcp.insert(
        "owned".into(),
        server(remote.endpoint("/mcp"), "2026-07-28", true),
    );
    let (v, store) = f.running_value(f.contract_task(contract(b)));
    let id = v["task_id"].as_str().unwrap();
    let e = f.app.execution(&store, id).unwrap().unwrap();
    let mut tools = tools::Tools::new(
        &f.app,
        &store,
        id,
        e.recipe.project.unwrap(),
        e.expires_at_ms,
        false,
    )
    .unwrap();
    let request = call(
        "extension_mcp",
        json!({"server":"owned","tool":"echo","arguments":{"message":"owned"}}),
    );
    assert!(tools.validate_call(&request).is_err());
    tools.writes = true;
    let a = tools
        .execute(
            request.clone(),
            ctx(id, "unknown-once"),
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .unwrap();
    assert!(!a.success && a.requires_reconciliation);
    let _ = tools
        .execute(
            request,
            ctx(id, "unknown-once"),
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .unwrap();
    let methods = remote
        .requests
        .lock()
        .unwrap()
        .iter()
        .map(|r| r.body["method"].clone())
        .collect::<Vec<_>>();
    assert_eq!(
        methods.iter().filter(|m| **m == "tools/call").count(),
        1,
        "{}; methods={methods:?}",
        a.content
    );
    tools.close().await.unwrap();
}
#[tokio::test(flavor = "multi_thread")]
async fn large_mcp_result_is_reopenable_in_pages_without_truncation_or_private_artifact_access() {
    let payload = "Z".repeat(700_000);
    let server_payload = payload.clone();
    let http = HttpFixture::new(move |r| {
        if r.body["method"] == "tools/call" {
            reply(
                json!({"jsonrpc":"2.0","id":r.body["id"],"result":{"content":[{"type":"text","text":server_payload}],"isError":false}}),
            )
        } else {
            mcp_reply(r, "2026-07-28", false, false)
        }
    });
    let f = Fixture::new(false);
    let mut bundle = Bundle::default();
    bundle.config.mcp.insert(
        "owned".into(),
        server(http.endpoint("/mcp"), "2026-07-28", false),
    );
    let (v, store) = f.running_value(f.contract_task(contract(bundle)));
    let id = v["task_id"].as_str().unwrap();
    let e = f.app.execution(&store, id).unwrap().unwrap();
    let mut tools = tools::Tools::new(
        &f.app,
        &store,
        id,
        e.recipe.project.unwrap(),
        e.expires_at_ms,
        false,
    )
    .unwrap();
    assert!(
        tools
            .definitions()
            .iter()
            .any(|d| d.definition.name == "extension_result_page")
    );
    let output = tools
        .execute(
            call(
                "extension_mcp",
                json!({"server":"owned","tool":"echo","arguments":{"message":"large"}}),
            ),
            ctx(id, "large"),
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .unwrap();
    assert!(output.success, "{}", output.content);
    let brief: Value = serde_json::from_str(&output.content).unwrap();
    assert_eq!(brief["full_result_retained"], true);
    let artifact = brief["artifact_id"].as_str().unwrap().to_string();
    let mut cursor = 0;
    let mut raw = vec![];
    loop {
        let output = tools
            .execute(
                call(
                    "extension_result_page",
                    json!({"artifact_id":artifact,"offset":cursor,"max_bytes":32768}),
                ),
                ctx(id, &format!("page-{cursor}")),
                Arc::new(AtomicBool::new(false)),
            )
            .await
            .unwrap();
        assert!(output.success, "{}", output.content);
        let v: Value = serde_json::from_str(&output.content).unwrap();
        let page = &v["result"];
        let hex = page["hex"].as_str().unwrap();
        for i in (0..hex.len()).step_by(2) {
            raw.push(u8::from_str_radix(&hex[i..i + 2], 16).unwrap());
        }
        cursor = page["next_offset"].as_u64().unwrap();
        if page["has_more"] == false {
            break;
        }
    }
    let full: Value = serde_json::from_slice(&raw).unwrap();
    assert_eq!(full["result"]["result"]["content"][0]["text"], payload);
    assert!(
        tools
            .validate_call(&call(
                "extension_result_page",
                json!({"artifact_id":"cli:execution:v1"})
            ))
            .is_err()
    );
    tools.close().await.unwrap();
    assert!(store.journal().model_requests(id).unwrap().is_empty());
}
#[tokio::test(flavor = "multi_thread")]
#[ignore = "real isolated MCP stdio server lifecycle over the native project broker; no AI"]
async fn native_stdio_mcp_survives_turns_and_closes_owned_process_tree() {
    let f = Fixture::new(true);
    fs::write(f.source.join("mcp_fixture.py"),r#"import json,sys
count=0
for line in sys.stdin:
 r=json.loads(line)
 if 'id' not in r: continue
 m=r['method']
 if m=='initialize': result={'protocolVersion':'2025-11-25','capabilities':{'tools':{}},'serverInfo':{'name':'owned-stdio','version':'1'}}
 elif m=='tools/list': result={'tools':[{'name':'echo','inputSchema':{'type':'object','properties':{'message':{'type':'string','x-mcp-header':'Message'}},'required':['message'],'additionalProperties':False}}]}
 else:
  count+=1
  result={'content':[{'type':'text','text':str(count)+':'+r['params']['arguments']['message']}],'isError':False}
 print(json.dumps({'jsonrpc':'2.0','id':r['id'],'result':result}),flush=True)
"#).unwrap();
    let mut b = Bundle::default();
    let mut s = server(
        http::Endpoint {
            url: "https://unused.invalid".into(),
            auth_env: None,
            loopback: false,
            timeout_ms: 1000,
            max_bytes: 4096,
        },
        "2025-11-25",
        false,
    );
    s.connection = mcp::Transport::Stdio {
        command: Command {
            executable: "python".into(),
            argv: vec!["mcp_fixture.py".into()],
            cwd: ".".into(),
        },
    };
    b.config.mcp.insert("owned".into(), s);
    let (v, store) = f.running_value(f.contract_task(contract(b)));
    let id = v["task_id"].as_str().unwrap();
    let e = f.app.execution(&store, id).unwrap().unwrap();
    let mut tools = tools::Tools::new(
        &f.app,
        &store,
        id,
        e.recipe.project.unwrap(),
        e.expires_at_ms,
        true,
    )
    .unwrap();
    for i in 1..=2 {
        let output = tools
            .execute(
                call(
                    "extension_mcp",
                    json!({"server":"owned","tool":"echo","arguments":{"message":"persisted"}}),
                ),
                ctx(id, &format!("stdio-{i}")),
                Arc::new(AtomicBool::new(false)),
            )
            .await
            .unwrap();
        assert!(output.success, "{}", output.content);
        assert!(output.content.contains(&format!("{i}:persisted")));
    }
    let session = fs::read_dir(f.app.home.join("tasks").join(id).join("project-sessions"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .file_name()
        .to_string_lossy()
        .to_string();
    assert!(
        f.app
            .project_input(id, &session, "bypass-mcp", "stdin", "7b7d0a")
            .is_err()
    );
    assert!(
        f.app
            .project_input(id, &session, "bypass-eof", "eof", "")
            .is_err()
    );
    let large = "L".repeat(64000);
    let output = tools
        .execute(
            call(
                "extension_mcp",
                json!({"server":"owned","tool":"echo","arguments":{"message":large}}),
            ),
            ctx(id, "stdio-large"),
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .unwrap();
    assert!(output.success, "{}", output.content);
    assert!(output.content.contains(&format!("3:{large}")));
    tools.close().await.unwrap();
    assert!(crate::engine::native_stop_proven(&store, id).unwrap());
    assert!(store.journal().model_requests(id).unwrap().is_empty());
    fs::write(
        f.root.join("stdio-result.json"),
        bytes(&json!({"status":"PASS","task":id,"owned_processes_stopped":true,"paid_calls":0}))
            .unwrap(),
    )
    .unwrap();
}
#[tokio::test(flavor = "multi_thread")]
#[ignore = "real native installed-version probe plus owned HTTP documentation/Context7 fixtures; no external service"]
async fn native_docs_pin_installed_version_and_context7_request_and_reject_redirects() {
    for context7 in [false, true] {
        let http = HttpFixture::new(|r| {
            if r.path.contains("redirect") {
                Reply {
                    status: 302,
                    headers: vec![("Location", "https://unreviewed.invalid/".into())],
                    ..reply(Value::Null)
                }
            } else {
                reply(
                    json!({"text":"Owned exact-version documentation","source":"https://docs.python.org/3.13/"}),
                )
            }
        });
        let f = Fixture::new(true);
        let mut b = Bundle::default();
        let endpoint = http.endpoint(if context7 { "" } else { "/docs" });
        let source = docs::Source {
            package: "python".into(),
            ecosystem: "runtime".into(),
            endpoint,
            versions: BTreeMap::from([(
                "3.13.5".into(),
                if context7 {
                    "/python/cpython/3.13.5".into()
                } else {
                    format!("{}/docs/3.13.5", http.url)
                },
            )]),
            context7,
            search_path: if context7 {
                None
            } else {
                Some("/search".into())
            },
        };
        b.config.docs.insert("python".into(), source.clone());
        let (v, store) = f.running_value(f.contract_task(contract(b)));
        let id = v["task_id"].as_str().unwrap();
        let e = f.app.execution(&store, id).unwrap().unwrap();
        let mut tools = tools::Tools::new(
            &f.app,
            &store,
            id,
            e.recipe.project.unwrap(),
            e.expires_at_ms,
            false,
        )
        .unwrap();
        let result = tools
            .execute(
                call(
                    "docs_resolve",
                    json!({"source":"python","query":"type hints"}),
                ),
                ctx(id, "docs-version"),
                Arc::new(AtomicBool::new(false)),
            )
            .await
            .unwrap();
        assert!(result.success, "{}", result.content);
        let result: Value = serde_json::from_str(&result.content).unwrap();
        assert_eq!(result["result"]["version"], "3.13.5");
        assert_eq!(result["result"]["version_basis"], "native_installed_probe");
        assert!(result["result"]["version_evidence"]["native"]["tree_confirmed_stopped"] == true);
        if context7 {
            let requests = http.requests.lock().unwrap();
            assert!(requests.iter().any(|r| r.path.contains("/api/v2/context?")
                && r.path.contains("libraryId=%2Fpython%2Fcpython%2F3.13.5")));
        } else {
            assert!(
                http.requests
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|r| r.path == "/docs/3.13.5")
            );
            let redirect = tools
                .execute(
                    call(
                        "docs_read",
                        json!({"source":"python","url":format!("{}/docs/redirect",http.url)}),
                    ),
                    ctx(id, "redirect"),
                    Arc::new(AtomicBool::new(false)),
                )
                .await
                .unwrap();
            assert!(!redirect.success);
        }
        let mut mismatch = source;
        mismatch.versions = BTreeMap::from([(
            "0.0.0".into(),
            if context7 {
                "/python/cpython/0.0.0".into()
            } else {
                format!("{}/docs/0.0.0", http.url)
            },
        )]);
        let requests = http.requests.lock().unwrap().len();
        let mismatch = docs::resolve(
            &mut tools.runner,
            "mismatch",
            &mismatch,
            "test",
            "different-version",
            Arc::new(AtomicBool::new(false)),
        )
        .await;
        assert!(mismatch.is_err());
        assert_eq!(http.requests.lock().unwrap().len(), requests);
        tools.close().await.unwrap();
        assert!(store.journal().model_requests(id).unwrap().is_empty());
        fs::write(f.root.join("docs-result.json"), bytes(&result).unwrap()).unwrap();
    }
}
struct Mutator {
    path: std::path::PathBuf,
    count: Arc<AtomicUsize>,
}
impl ToolDispatcher for Mutator {
    fn definitions(&self) -> Vec<melissa_model_gateway::tools::BoundTool> {
        vec![]
    }
    fn validate_call(&self, _: &ToolCall) -> melissa_model_gateway::Result<()> {
        Ok(())
    }
    fn execute<'a>(
        &'a mut self,
        _: ToolCall,
        _: ToolContext,
        _: Arc<AtomicBool>,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = melissa_model_gateway::Result<
                        melissa_model_gateway::tools::ToolExecution,
                    >,
                > + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            fs::write(&self.path, "changed by the scoped-host fixture\n").unwrap();
            self.count.fetch_add(1, Ordering::AcqRel);
            Ok(melissa_model_gateway::tools::ToolExecution {
                content: json!({"mutation_applied":true}).to_string(),
                success: false,
                cancelled: false,
                requires_reconciliation: false,
                artifact_refs: vec![],
            })
        })
    }
}
#[tokio::test(flavor = "multi_thread")]
#[ignore = "real helper scripts and lifecycle hooks in isolated native workers; source-write backend is an explicit local fixture"]
async fn native_helper_and_hooks_keep_mutation_failure_cancel_and_idempotence_distinct() {
    let f = Fixture::new(true);
    let root = f.source.join(".melissa/skills");
    skill(&root, "migration-check", "complete instructions");
    fs::create_dir(root.join("migration-check/scripts")).unwrap();
    fs::write(root.join("migration-check/scripts/helper.py"),"import json,pathlib\nx=json.loads(pathlib.Path('.melissa-extension/input.json').read_text());pathlib.Path('worker-only.txt').write_text('private');print(json.dumps({'input':x,'permission':'untrusted no authority'}))\n").unwrap();
    let repo = melissa_repository::Repository::open(&f.source, Default::default(), vec![]).unwrap();
    let mut b = f
        .app
        .freeze_extensions(&repo, "migration testing")
        .unwrap()
        .unwrap();
    let helper = hooks::Helper {
        command: Command {
            executable: "python".into(),
            argv: vec!["@skill/scripts/helper.py".into()],
            cwd: ".".into(),
        },
        skill: Some("migration-check".into()),
    };
    b.config
        .helpers
        .insert("owned-helper".into(), helper.clone());
    b.config.hooks = vec![
        hooks::Hook {
            id: "before".into(),
            event: hooks::Event::BeforeTool,
            tools: vec!["fs_patch".into()],
            helper: hooks::Helper {
                command: python("print('before passed')"),
                skill: None,
            },
        },
        hooks::Hook {
            id: "mutation-check".into(),
            event: hooks::Event::AfterMutation,
            tools: vec![],
            helper: hooks::Helper {
                command: python("raise SystemExit(7)"),
                skill: None,
            },
        },
        hooks::Hook {
            id: "end".into(),
            event: hooks::Event::TaskEnd,
            tools: vec![],
            helper: hooks::Helper {
                command: python("print('task end')"),
                skill: None,
            },
        },
    ];
    let (v, store) = f.running_value(f.contract_task(contract(b)));
    let id = v["task_id"].as_str().unwrap();
    let e = f.app.execution(&store, id).unwrap().unwrap();
    let c = e.recipe.project.unwrap();
    let mut tools =
        tools::Tools::new(&f.app, &store, id, c.clone(), e.expires_at_ms, true).unwrap();
    let result = tools
        .execute(
            call(
                "extension_skill",
                json!({"operation":"helper","helper":"owned-helper","input":{"value":42}}),
            ),
            ctx(id, "helper-once"),
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .unwrap();
    assert!(result.success, "{}", result.content);
    assert!(!f.source.join("worker-only.txt").exists());
    let again = tools
        .execute(
            call(
                "extension_skill",
                json!({"operation":"helper","helper":"owned-helper","input":{"value":42}}),
            ),
            ctx(id, "helper-once"),
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .unwrap();
    assert_eq!(again.content, result.content);
    let count = Arc::new(AtomicUsize::new(0));
    let mut dispatcher = hooks::Dispatcher {
        inner: Box::new(Mutator {
            path: f.source.join("main.py"),
            count: count.clone(),
        }),
        runner: hooks::Runner {
            home: f.app.home.clone(),
            task: id.into(),
            contract: c,
            expires: e.expires_at_ms,
            store: store.connect().unwrap(),
        },
    };
    let result = dispatcher
        .execute(
            call("fs_patch", json!({})),
            ctx(id, "actual-patch"),
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .unwrap();
    assert!(!result.success);
    let result: Value = serde_json::from_str(&result.content).unwrap();
    assert_eq!(result["mutation_applied"], true);
    assert_eq!(result["retry_mutation"], false);
    assert_eq!(count.load(Ordering::Acquire), 1);
    assert!(
        fs::read_to_string(f.source.join("main.py"))
            .unwrap()
            .starts_with("changed by")
    );
    dispatcher
        .runner
        .event(
            hooks::Event::TaskEnd,
            "task_end",
            "fixture-finished",
            json!({"status":"failed"}),
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .unwrap();
    let cancelled = dispatcher
        .runner
        .helper(
            "cancel-before-start",
            &helper,
            json!({}),
            Arc::new(AtomicBool::new(true)),
        )
        .await
        .unwrap();
    assert_eq!(cancelled["status"], "NOT_RUN");
    tools.close().await.unwrap();
    assert!(crate::engine::native_stop_proven(&store, id).unwrap());
    assert!(store.journal().model_requests(id).unwrap().is_empty());
    fs::write(f.root.join("hook-result.json"), bytes(&result).unwrap()).unwrap();
}
#[tokio::test(flavor = "multi_thread")]
#[ignore = "operator docs-resolve driver uses native probe and owned HTTP fixture; no model"]
async fn native_operator_docs_helper_keeps_same_task_budget_and_safe_resume_checkpoint() {
    let http = HttpFixture::new(|_| reply(json!({"text":"native operator fixture"})));
    let f = Fixture::new(true);
    let mut bundle = Bundle::default();
    bundle.config.docs.insert(
        "python".into(),
        docs::Source {
            package: "python".into(),
            ecosystem: "runtime".into(),
            endpoint: http.endpoint("/docs"),
            versions: BTreeMap::from([("3.13.5".into(), format!("{}/docs/3.13.5", http.url))]),
            context7: false,
            search_path: None,
        },
    );
    let v = f.contract_task(contract(bundle));
    let id = v["task_id"].as_str().unwrap();
    assert!(f.app.resolve_docs(id, "python", "typing").await.is_err());
    f.app
        .approve(
            id,
            v["approval"]["request_id"].as_str().unwrap(),
            v["approval"]["digest"].as_str().unwrap(),
        )
        .unwrap();
    let before = f.app.show(id).unwrap();
    let result = f.app.resolve_docs(id, "python", "typing").await.unwrap();
    assert_eq!(result["state"], "paused", "{result}");
    assert_eq!(result["model_calls"], 0);
    assert_eq!(result["result"]["status"], "PASS", "{result}");
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
}
#[tokio::test(flavor = "multi_thread")]
#[ignore = "prepared uv image and real project virtual environment; no model or external documentation service"]
async fn native_docs_probe_uses_uv_project_environment_and_ignores_source_module_shadowing() {
    let image = std::env::var("MELISSA_PHASE03_IMAGE").expect("verified multi-tool image required");
    let http = HttpFixture::new(|_| reply(json!({"text":"uv environment fixture"})));
    let f = Fixture::new(true);
    fs::write(f.source.join("pyproject.toml"),"[project]\nname='phase04-fixture'\nversion='0.1.0'\nrequires-python='>=3.13'\n[tool.uv]\npackage=false\n").unwrap();
    fs::write(f.source.join("uv.lock"),"version = 1\nrevision = 3\nrequires-python = \">=3.13\"\n[[package]]\nname = \"phase04-fixture\"\nversion = \"0.1.0\"\nsource = { virtual = \".\" }\n").unwrap();
    fs::write(
        f.source.join("json.py"),
        "raise RuntimeError('source module must not shadow trusted probe imports')\n",
    )
    .unwrap();
    let repo = melissa_repository::Repository::open(&f.source, Default::default(), vec![]).unwrap();
    let discovery = crate::project::discovery::discover(&repo).unwrap();
    let mut bundle = Bundle::default();
    bundle.config.docs.insert(
        "python".into(),
        docs::Source {
            package: "python".into(),
            ecosystem: "runtime".into(),
            endpoint: http.endpoint("/docs"),
            versions: BTreeMap::from([("3.13.5".into(), format!("{}/docs/3.13.5", http.url))]),
            context7: false,
            search_path: None,
        },
    );
    let mut c = contract(bundle);
    c.environment.image_id = image;
    c.environment
        .executables
        .insert("uv".into(), "/usr/local/bin/uv".into());
    c.sync = discovery.sync;
    let (v, store) = f.running_value(f.contract_task(c));
    let id = v["task_id"].as_str().unwrap();
    let e = f.app.execution(&store, id).unwrap().unwrap();
    let mut tools = tools::Tools::new(
        &f.app,
        &store,
        id,
        e.recipe.project.unwrap(),
        e.expires_at_ms,
        false,
    )
    .unwrap();
    let output = tools
        .execute(
            call("docs_resolve", json!({"source":"python","query":"typing"})),
            ctx(id, "uv-docs"),
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .unwrap();
    assert!(output.success, "{}", output.content);
    let result: Value = serde_json::from_str(&output.content).unwrap();
    assert!(
        result["result"]["installed_environment"]["environment_prefix"]
            .as_str()
            .unwrap()
            .ends_with("/.venv")
    );
    assert!(store.journal().model_requests(id).unwrap().is_empty());
    tools.close().await.unwrap();
}
