//! Opt-in live provider check: real model -> Gateway -> repository broker ->
//! `fs_replace` -> filesystem. Ignored by default; it spends real tokens.
//!
//! MELISSA_LIVE_ENDPOINT  full HTTPS endpoint, e.g. https://host/v1/chat/completions
//! MELISSA_LIVE_PROTOCOL  open_ai_chat_completions | open_ai_responses | anthropic_messages
//! MELISSA_LIVE_MODEL     exact model id
//! MELISSA_LIVE_API_KEY   credential (read from the environment only)
use melissa_core_contracts::{TaskState, catalog::Catalog, wire::TaskContractV1};
use melissa_local_store::{LocalStore, StoreConfig, StoreKey};
use melissa_model_gateway::{
    DecoderLimits, Protocol,
    agent_loop::{Gateway, GatewayPolicy, RunSpec, RunStatus},
    routing::{
        Capabilities, Measurements, ModelRegistration, PriceSource, RateCard, RouteRequirements,
    },
    transport::{ApiKey, EndpointTrust, ProviderEndpoint},
};
use melissa_repository::{
    Limits, PatchPolicy, Repository,
    tools::{RepositoryDispatcher, RepositoryToolPolicy},
};
use std::{
    collections::BTreeSet,
    path::Path,
    sync::{Arc, atomic::AtomicBool},
};
use tokio::sync::Semaphore;

const TASK: &str = "task:live-provider";
const PROFILE: &str = "melissa.code.implementer";
const RESOURCE: &str = "provider:live";
const AUTH: &str = "authorization:provider:live";

fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} is required for the live test"))
}

fn store(root: &Path) -> LocalStore {
    let mut s =
        LocalStore::open(root, StoreKey::from_bytes([42; 32]), StoreConfig::default()).unwrap();
    let mut task: TaskContractV1 = serde_json::from_slice(include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/code01/wire/task-contract-v1.json"
    )))
    .unwrap();
    task.task_id = TASK.into();
    task.selected_profile_ids = vec![PROFILE.into()];
    task.allowed_effects = vec![
        "workspace.read".into(),
        "workspace.apply_patch_scoped".into(),
    ];
    task.allowed_resource_ids.push(RESOURCE.into());
    task.authorization_refs.push(AUTH.into());
    task.limits.max_input_tokens = 400_000;
    task.limits.max_output_tokens = 40_000;
    task.limits.max_cost_micro_usd = 2_000_000;
    task.limits.max_output_bytes = 16 * 1024 * 1024;
    task.limits.max_disk_bytes = 16 * 1024 * 1024;
    s.journal_mut().create_task(&task).unwrap();
    for state in [
        TaskState::Planned,
        TaskState::Queued,
        TaskState::Leased,
        TaskState::Running,
    ] {
        let version = s.journal().task(TASK).unwrap().record.version;
        s.transition_task(TASK, version, state, None).unwrap();
    }
    s
}

#[tokio::test]
#[ignore = "spends real provider tokens; set MELISSA_LIVE_* and run with --ignored"]
async fn live_model_fixes_a_bug_through_fs_replace() {
    let protocol: Protocol =
        serde_json::from_value(serde_json::Value::String(env("MELISSA_LIVE_PROTOCOL"))).unwrap();
    let endpoint = env("MELISSA_LIVE_ENDPOINT");
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("repo");
    std::fs::create_dir(&root).unwrap();
    let before = "def add(a, b):\n    return a - b\n\n\ndef mul(a, b):\n    return a * b\n";
    std::fs::write(root.join("calc.py"), before).unwrap();
    std::fs::write(root.join("notes.txt"), "do not touch\n").unwrap();
    let repo = Repository::open(&root, Limits::default(), vec![]).unwrap();

    let mut g = Gateway::new(
        store(&dir.path().join("store")),
        Catalog::load_definitions(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")).unwrap(),
        vec![ModelRegistration {
            registration_id: "live-route".into(),
            endpoint: ProviderEndpoint::new(
                "live-provider".into(),
                RESOURCE.into(),
                AUTH.into(),
                protocol,
                if endpoint.starts_with("http://127.0.0.1") {
                    EndpointTrust::LocalLoopback
                } else {
                    EndpointTrust::RemoteHttps
                },
                &endpoint,
                ApiKey::from_env("MELISSA_LIVE_API_KEY").unwrap(),
            )
            .unwrap(),
            model_id: env("MELISSA_LIVE_MODEL"),
            capabilities: Capabilities {
                tool_calling: true,
                structured_output: true,
                vision: false,
                streaming: true,
            },
            categories: BTreeSet::from(["coding".into()]),
            context_tokens: 128_000,
            max_output_tokens: 8192,
            timeout_ms: 120_000,
            max_concurrency: 1,
            rate_card: Some(RateCard {
                source: PriceSource::OperatorDeclared {
                    note: "live test rate card".into(),
                },
                input_micro_usd_per_million: 1_000_000,
                output_micro_usd_per_million: 4_000_000,
                cached_input_micro_usd_per_million: None,
                cache_creation_micro_usd_per_million: None,
            }),
            measurements: Measurements::default(),
            slots: Arc::new(Semaphore::new(1)),
        }],
        GatewayPolicy {
            allowed_registrations: BTreeSet::from(["live-route".into()]),
            max_requests: 10,
            max_turns: 8,
            max_tools: 8,
            max_wall_time_ms: 300_000,
            expires_at_unix_ms: melissa_execution_broker::unix_ms().unwrap() + 600_000,
            allow_fallback: false,
            max_backoff_ms: 2_000,
            decoder_limits: DecoderLimits::default(),
        },
        false,
    )
    .unwrap();
    let t = g.store().journal().task(TASK).unwrap().contract;
    let mut dispatcher = RepositoryDispatcher::new(
        repo,
        g.store().connect().unwrap(),
        RepositoryToolPolicy {
            files: PatchPolicy {
                task_id: TASK.into(),
                principal_id: "principal:live-host".into(),
                profile_id: PROFILE.into(),
                resource_id: t.workspace_ref,
                authorization_ref: t.authorization_refs[0].clone(),
                expires_at_ms: melissa_execution_broker::unix_ms().unwrap() + 600_000,
                epoch: 0,
                writable_paths: ["calc.py".into()].into(),
                protected_paths: ["notes.txt".into()].into(),
            },
            read_repository: true,
            enabled_tools: ["fs_read".into(), "fs_replace".into()].into(),
        },
    )
    .unwrap();
    let spec = RunSpec {
        run_id: "live_fs_replace".into(),
        task_id: TASK.into(),
        profile_id: PROFILE.into(),
        user_message: "calc.py has a bug: add() subtracts. Read calc.py with fs_read, fix it \
                       with fs_replace, then reply with one short sentence."
            .into(),
        route: RouteRequirements {
            category: "coding".into(),
            tools: true,
            structured_output: false,
            vision: false,
            minimum_success_ppm: None,
            maximum_p95_ms: None,
        },
        max_output_tokens: 4096,
        output_schema: None,
        required_tool_names: BTreeSet::from(["fs_replace".into()]),
    };
    let started = std::time::Instant::now();
    let r = g
        .run(&spec, &mut dispatcher, Arc::new(AtomicBool::new(false)))
        .await
        .unwrap();
    eprintln!(
        "live result: status={:?} requests={} tool_calls={} elapsed_ms={} answer={:?}",
        r.status,
        r.request_ids.len(),
        r.tool_calls,
        started.elapsed().as_millis(),
        r.answer
    );
    assert_eq!(r.status, RunStatus::FinalCandidate);
    assert!(!r.task_accepted);
    let after = std::fs::read_to_string(root.join("calc.py")).unwrap();
    assert!(
        after.contains("return a + b"),
        "calc.py was not fixed:\n{after}"
    );
    assert!(after.contains("return a * b"));
    assert_eq!(
        std::fs::read_to_string(root.join("notes.txt")).unwrap(),
        "do not touch\n"
    );
    g.store().integrity_check().unwrap();
}
