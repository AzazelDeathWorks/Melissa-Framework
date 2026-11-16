//! Opt-in live provider check: real model -> Gateway -> repository broker ->
//! `fs_replace` -> filesystem. Ignored by default; it spends real tokens.
//!
//! MELISSA_LIVE_ENDPOINT  full HTTPS endpoint, e.g. https://host/v1/chat/completions
//! MELISSA_LIVE_PROTOCOL  open_ai_chat_completions | open_ai_responses | anthropic_messages
//! MELISSA_LIVE_MODEL     exact model id
//! MELISSA_LIVE_API_KEY   credential (read from the environment only)
//! MELISSA_LIVE_OUT       (A/B only) JSONL output path; MELISSA_LIVE_REPS repetitions,
//!                        MELISSA_LIVE_CASES / MELISSA_LIVE_ARMS comma-separated ids
//!
//! Summarize A/B output with `python tools/edit_ab_stats.py OUT.jsonl`.
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

struct Case {
    id: &'static str,
    file: &'static str,
    before: &'static str,
    goal: &'static str,
    must: &'static [&'static str],
    /// At least one must appear; lets equivalent correct fixes pass.
    must_any: &'static [&'static str],
    must_not: &'static [&'static str],
}

const CASES: &[Case] = &[
    Case {
        id: "py_operator",
        file: "calc.py",
        before: "def add(a, b):\n    return a - b\n\n\ndef mul(a, b):\n    return a * b\n",
        goal: "add() subtracts; make it add.",
        must: &["return a + b", "return a * b"],
        must_any: &[],
        must_not: &["return a - b"],
    },
    Case {
        id: "js_off_by_one",
        file: "sum.js",
        before: "export function sum(xs) {\n  let total = 0;\n  for (let i = 0; i <= xs.length; i++) {\n    total += xs[i];\n  }\n  return total;\n}\n",
        goal: "sum() reads past the end of the array; fix the loop bound.",
        must: &["i < xs.length", "total += xs[i];"],
        must_any: &[],
        must_not: &["<="],
    },
    Case {
        id: "rust_duplicate_context",
        file: "limits.rs",
        before: "pub const READ_TIMEOUT_MS: u64 = 1000;\npub const WRITE_TIMEOUT_MS: u64 = 1000;\npub const IDLE_TIMEOUT_MS: u64 = 1000;\n",
        goal: "Set only WRITE_TIMEOUT_MS to 2500; leave the others at 1000.",
        must: &[
            "READ_TIMEOUT_MS: u64 = 1000;",
            "WRITE_TIMEOUT_MS: u64 = 2500;",
            "IDLE_TIMEOUT_MS: u64 = 1000;",
        ],
        must_any: &[],
        must_not: &[],
    },
    Case {
        id: "go_insert_line",
        file: "main.go",
        before: "package main\n\nimport (\n\t\"fmt\"\n)\n\nfunc main() {\n\tfmt.Println(strings.ToUpper(\"hi\"))\n}\n",
        goal: "main.go uses strings without importing it; add the \"strings\" import inside the existing import block.",
        must: &["\t\"fmt\"\n", "\t\"strings\"\n", "strings.ToUpper"],
        must_any: &[],
        must_not: &[],
    },
    Case {
        id: "crlf_python",
        file: "greet.py",
        before: "def greet(name):\r\n    return \"Hello \" + nam\r\n",
        goal: "greet() references an undefined variable; return \"Hello \" + name. Keep CRLF line endings.",
        must: &["return \"Hello \" + name\r\n"],
        must_any: &[],
        must_not: &["nam\r\n"],
    },
    Case {
        id: "utf8_turkish",
        file: "mesajlar.py",
        before: "HATA = \"Dosya bulunamadi\"\nUYARI = \"Disk dolmak üzere\"\n",
        goal: "Fix the Turkish spelling in HATA to \"Dosya bulunamadı\" (dotless ı); keep UYARI unchanged.",
        must: &[
            "HATA = \"Dosya bulunamadı\"",
            "UYARI = \"Disk dolmak üzere\"",
        ],
        must_any: &[],
        must_not: &["bulunamadi\""],
    },
    Case {
        id: "multiline_body",
        file: "stack.py",
        before: "class Stack:\n    def __init__(self):\n        self.items = []\n\n    def pop(self):\n        item = self.items[0]\n        del self.items[0]\n        return item\n\n    def push(self, item):\n        self.items.append(item)\n",
        goal: "pop() removes from the bottom; make it remove and return the last item (LIFO). Do not change push().",
        must: &["def push(self, item):\n        self.items.append(item)\n"],
        must_any: &["self.items.pop()", "self.items[-1]"],
        must_not: &["self.items[0]"],
    },
    Case {
        id: "toml_section",
        file: "config.toml",
        before: "[server]\nport = 8080\nworkers = 4\n\n[metrics]\nport = 9090\nenabled = false\n",
        goal: "Enable metrics and change only the metrics port to 9100; the server port stays 8080.",
        must: &[
            "[server]\nport = 8080\n",
            "[metrics]\nport = 9100\nenabled = true\n",
        ],
        must_any: &[],
        must_not: &[],
    },
];

struct Outcome {
    status: RunStatus,
    task_accepted: bool,
    success: bool,
    requests: usize,
    tool_calls: u32,
    input_tokens: u64,
    output_tokens: u64,
    elapsed_ms: u128,
    protected_intact: bool,
}

/// One fresh store, workspace and gateway per run so arms never share state.
/// One edit surface: the tools the model sees and how the prompt names them.
struct Arm {
    id: &'static str,
    tools: &'static [&'static str],
    required: &'static str,
    portable_patch_wire: bool,
    how: &'static str,
}

const ARMS: &[Arm] = &[
    Arm {
        id: "fs_replace",
        tools: &["fs_read", "fs_replace"],
        required: "fs_replace",
        portable_patch_wire: false,
        how: "Read FILE with fs_read, change it with fs_replace",
    },
    // Raw native proposal; fs_snapshot is the only surface exposing root_identity.
    Arm {
        id: "fs_patch",
        tools: &["fs_read", "fs_snapshot", "fs_patch"],
        required: "fs_patch",
        portable_patch_wire: false,
        how: "Read FILE with fs_read, take root_identity from fs_snapshot, then change it with fs_patch using exact byte offsets from the read",
    },
    // The CLI recipe path: unified diff -> native proposal -> portable fs_patch.
    Arm {
        id: "unified",
        tools: &["fs_read", "code_unified_propose", "fs_patch"],
        required: "fs_patch",
        portable_patch_wire: true,
        how: "Read FILE with fs_read, write a unified diff and pass it to code_unified_propose with line_endings preserve_source, then apply the returned proposal with fs_patch, serializing it exactly once into proposal_json",
    },
];

async fn run_case(case: &Case, arm: &Arm) -> Outcome {
    let protocol: Protocol =
        serde_json::from_value(serde_json::Value::String(env("MELISSA_LIVE_PROTOCOL"))).unwrap();
    let endpoint = env("MELISSA_LIVE_ENDPOINT");
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("repo");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join(case.file), case.before).unwrap();
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
    let dispatcher = RepositoryDispatcher::new(
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
                writable_paths: [case.file.into()].into(),
                protected_paths: ["notes.txt".into()].into(),
            },
            read_repository: true,
            enabled_tools: arm.tools.iter().map(|t| t.to_string()).collect(),
        },
    )
    .unwrap()
    // Same recovery surface as the CLI engine: write-free input faults and
    // stale pre-dispatch patches come back as observations, not stops.
    .with_read_error_observations()
    .unwrap()
    .with_denied_read_observations()
    .unwrap()
    .with_recovery_observations()
    .unwrap();
    let mut dispatcher = if arm.portable_patch_wire {
        dispatcher.with_portable_patch_wire().unwrap()
    } else {
        dispatcher
    };
    let spec = RunSpec {
        run_id: format!("live_{}_{}", case.id, arm.id),
        task_id: TASK.into(),
        profile_id: PROFILE.into(),
        user_message: format!(
            "{} {} {}, then reply with one short sentence.",
            case.file,
            case.goal,
            arm.how.replace("FILE", case.file)
        ),
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
        required_tool_names: BTreeSet::from([arm.required.into()]),
    };
    let started = std::time::Instant::now();
    let r = g
        .run(&spec, &mut dispatcher, Arc::new(AtomicBool::new(false)))
        .await
        .unwrap();
    let elapsed_ms = started.elapsed().as_millis();
    g.store().integrity_check().unwrap();
    let budget = g.store().journal().model_budget(TASK).unwrap();
    let after = std::fs::read(root.join(case.file)).unwrap();
    let after = String::from_utf8_lossy(&after);
    let success = r.status == RunStatus::FinalCandidate
        && case.must.iter().all(|m| after.contains(m))
        && (case.must_any.is_empty() || case.must_any.iter().any(|m| after.contains(m)))
        && !case.must_not.iter().any(|m| after.contains(m));
    Outcome {
        status: r.status,
        task_accepted: r.task_accepted,
        success,
        requests: r.request_ids.len(),
        tool_calls: r.tool_calls,
        input_tokens: budget.charged_input_tokens,
        output_tokens: budget.charged_output_tokens,
        elapsed_ms,
        protected_intact: std::fs::read(root.join("notes.txt")).unwrap() == b"do not touch\n",
    }
}

#[tokio::test]
#[ignore = "spends real provider tokens; set MELISSA_LIVE_* and run with --ignored"]
async fn live_model_fixes_a_bug_through_fs_replace() {
    let o = run_case(&CASES[0], &ARMS[0]).await;
    eprintln!(
        "live result: status={:?} requests={} tool_calls={} tokens={}/{} elapsed_ms={}",
        o.status, o.requests, o.tool_calls, o.input_tokens, o.output_tokens, o.elapsed_ms
    );
    assert!(o.success, "status={:?}", o.status);
    assert!(!o.task_accepted);
    assert!(o.protected_intact);
}

/// Paired comparison of edit surfaces: same model, cases and goal; only the
/// edit tools differ. Arm order rotates to spread provider drift.
#[tokio::test]
#[ignore = "spends real provider tokens; set MELISSA_LIVE_* and MELISSA_LIVE_OUT"]
async fn live_edit_tool_ab() {
    use std::io::Write;
    let reps: usize = std::env::var("MELISSA_LIVE_REPS")
        .ok()
        .map_or(1, |v| v.parse().unwrap());
    let mut out = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(env("MELISSA_LIVE_OUT"))
        .unwrap();
    let model = env("MELISSA_LIVE_MODEL");
    let only = std::env::var("MELISSA_LIVE_CASES").ok();
    let arm_ids = std::env::var("MELISSA_LIVE_ARMS").ok();
    for rep in 0..reps {
        for (i, case) in CASES.iter().enumerate() {
            if only
                .as_ref()
                .is_some_and(|ids| !ids.split(',').any(|id| id == case.id))
            {
                continue;
            }
            let mut arms: Vec<&Arm> = ARMS
                .iter()
                .filter(|a| {
                    arm_ids
                        .as_ref()
                        .is_none_or(|ids| ids.split(',').any(|id| id == a.id))
                })
                .collect();
            let n = arms.len();
            arms.rotate_left((rep + i) % n);
            for arm in arms {
                let o = run_case(case, arm).await;
                assert!(
                    o.protected_intact,
                    "{} {} touched a protected file",
                    case.id, arm.id
                );
                assert!(!o.task_accepted);
                let line = serde_json::json!({
                    "model": model, "case": case.id, "rep": rep, "arm": arm.id,
                    "success": o.success, "status": format!("{:?}", o.status),
                    "requests": o.requests, "tool_calls": o.tool_calls,
                    "input_tokens": o.input_tokens, "output_tokens": o.output_tokens,
                    "elapsed_ms": o.elapsed_ms as u64,
                });
                eprintln!("{line}");
                writeln!(out, "{line}").unwrap();
            }
        }
    }
}
