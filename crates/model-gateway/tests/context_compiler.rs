//! Synthetic native protocol histories, no provider/model API calls.
use melissa_core_contracts::{TaskState, wire::TaskContractV1};
use melissa_local_store::{LocalStore, StoreConfig, StoreKey};
use melissa_model_gateway::{
    context_compiler::*,
    tools::ToolContext,
    transcript::MessageLog,
    transport::{ApiKey, EndpointTrust, HttpTransport, ProviderEndpoint},
    *,
};
use serde_json::{Value, json};
use std::sync::atomic::AtomicBool;
fn setup(path: &std::path::Path) -> (LocalStore, String, String) {
    let mut store = LocalStore::open(
        path,
        StoreKey::from_bytes([220; 32]),
        StoreConfig::default(),
    )
    .unwrap()
    .with_pinned_cas_inventory()
    .unwrap();
    let task: TaskContractV1 = serde_json::from_slice(include_bytes!(
        "../../../tests/fixtures/code01/wire/task-contract-v1.json"
    ))
    .unwrap();
    store.journal_mut().create_task(&task).unwrap();
    for state in [
        TaskState::Planned,
        TaskState::Queued,
        TaskState::Leased,
        TaskState::Running,
    ] {
        let version = store.journal().task(&task.task_id).unwrap().record.version;
        store
            .transition_task(&task.task_id, version, state, None)
            .unwrap();
    }
    (store, task.task_id, task.selected_profile_ids[0].clone())
}
fn request(protocol: Protocol, turns: usize) -> TurnRequest {
    let mut messages = vec![Message::User {
        text: "Preserve the task, Türkçe and 中文 instructions.".into(),
    }];
    for n in 0..turns {
        let id = format!("call{n}");
        let continuation = match protocol {
            Protocol::OpenAiResponses => vec![
                json!({"type":"reasoning","id":format!("reason{n}"),"summary":[],"encrypted_content":"opaque-signature"}),
                json!({"type":"function_call","id":format!("fc{n}"),"call_id":id,"name":"inspect","arguments":"{}","status":"completed"}),
            ],
            Protocol::AnthropicMessages => vec![
                json!({"type":"thinking","thinking":"opaque reasoning","signature":"signed-original"}),
                json!({"type":"tool_use","id":id,"name":"inspect","input":{}}),
            ],
        };
        messages.push(Message::Assistant {
            response: ModelResponse {
                protocol,
                requested_model: "fixture".into(),
                model: "fixture".into(),
                response_id: format!("r{n}"),
                text: String::new(),
                calls: vec![ToolCall {
                    id: id.clone(),
                    name: "inspect".into(),
                    arguments: "{}".into(),
                }],
                stop: StopReason::ToolCalls,
                usage: Usage::default(),
                continuation,
                refusal: None,
            },
        });
        messages.push(Message::ToolResult {
            call_id: id,
            content: format!("observation {n}\n{}", "ü漢字 ".repeat(1024)),
            is_error: false,
        });
    }
    TurnRequest {
        model: "fixture".into(),
        system_prompt: "Pinned system and skills stay complete.".into(),
        messages,
        tools: vec![
            history_definition().definition,
            ToolDefinition {
                name: "inspect".into(),
                description: "owned fixture".into(),
                parameters: json!({"type":"object","properties":{},"required":[],"additionalProperties":false}),
                strict: false,
            },
        ],
        max_output_tokens: 1024,
        output_schema: None,
    }
}
fn endpoint(protocol: Protocol) -> ProviderEndpoint {
    ProviderEndpoint::new(
        "fixture".into(),
        "provider:fixture".into(),
        "authorization:fixture".into(),
        protocol,
        EndpointTrust::LoopbackFixture,
        "http://127.0.0.1:1/unused",
        ApiKey::new("owned-loopback-not-a-live-credential".into()).unwrap(),
    )
    .unwrap()
}
#[test]
fn long_native_histories_keep_pairs_opaque_bytes_and_recover_archived_ranges() {
    for protocol in [Protocol::OpenAiResponses, Protocol::AnthropicMessages] {
        let dir = tempfile::tempdir().unwrap();
        let (mut store, task, profile) = setup(dir.path());
        let transport = HttpTransport::new(true).unwrap();
        let original = request(protocol, 320);
        let mut working = original.clone();
        let mut compiler = ContextCompiler::default();
        let (_, report) = compiler
            .compile(
                &mut working,
                &transport,
                &endpoint(protocol),
                600_000,
                &mut store,
                &task,
                "run",
                vec![],
            )
            .unwrap();
        assert!(report.conservative_input_units + report.reserved_output_tokens <= 600_000);
        assert!(report.exact_token_count.is_none());
        assert!(!report.archived.is_empty());
        assert_eq!(working.messages.len(), original.messages.len());
        for (a, b) in original.messages.iter().zip(&working.messages) {
            if !matches!(a, Message::ToolResult { .. }) {
                assert_eq!(a, b);
            }
        }
        assert_eq!(
            &working.messages[working.messages.len() - 8..],
            &original.messages[original.messages.len() - 8..]
        );
        let record = &report.archived[0];
        let ctx = ToolContext {
            task_id: task.clone(),
            profile_id: profile,
            run_id: "run".into(),
            request_id: "request".into(),
            invocation_id: "read".into(),
            attempt_id: "read:attempt".into(),
            generation: 1,
        };
        let page = read_history(
            &store,
            &ctx,
            &json!({"artifact_ref":record.artifact_ref,"start_byte":0,"max_bytes":65536})
                .to_string(),
            &AtomicBool::new(false),
        )
        .unwrap();
        let page: Value = serde_json::from_str(&page.content).unwrap();
        let full = store.get_artifact(&task, &record.artifact_ref).unwrap();
        assert_eq!(page["content"].as_str().unwrap().as_bytes(), full);
        let (reused, used) = compiler.working_messages(&original.messages, &task, "run");
        assert!(!used.is_empty());
        assert_eq!(reused, working.messages);
        let mut foreign = ctx.clone();
        foreign.run_id = "foreign".into();
        assert!(
            read_history(
                &store,
                &foreign,
                &json!({"artifact_ref":record.artifact_ref,"start_byte":0,"max_bytes":10})
                    .to_string(),
                &AtomicBool::new(false)
            )
            .is_err()
        );
        assert_eq!(store.journal().model_requests(&task).unwrap().len(), 0);
    }
}
#[test]
fn output_reservation_exact_host_bound_and_required_overflow_never_dispatch() {
    let dir = tempfile::tempdir().unwrap();
    let (mut store, task, _) = setup(dir.path());
    let mut compiler = ContextCompiler::default();
    let transport = HttpTransport::new(true).unwrap();
    let mut req = request(Protocol::OpenAiResponses, 0);
    let (_, report) = compiler
        .compile(
            &mut req,
            &transport,
            &endpoint(Protocol::OpenAiResponses),
            100_000,
            &mut store,
            &task,
            "r",
            vec![],
        )
        .unwrap();
    let bound = report.conservative_input_units + report.reserved_output_tokens;
    compiler
        .compile(
            &mut req,
            &transport,
            &endpoint(Protocol::OpenAiResponses),
            bound,
            &mut store,
            &task,
            "r",
            vec![],
        )
        .unwrap();
    assert!(
        compiler
            .compile(
                &mut req,
                &transport,
                &endpoint(Protocol::OpenAiResponses),
                bound - 1,
                &mut store,
                &task,
                "r",
                vec![]
            )
            .is_err()
    );
    assert!(store.journal().model_requests(&task).unwrap().is_empty());
}
#[test]
fn message_objects_append_once_restore_exactly_and_reject_rewrites() {
    let dir = tempfile::tempdir().unwrap();
    let (mut store, task, _) = setup(dir.path());
    let mut log = MessageLog::default();
    let original = request(Protocol::AnthropicMessages, 6);
    let first = log
        .save(&mut store, &task, "transcript-run", &original.messages[..5])
        .unwrap();
    let count = store.list_artifacts(&task).unwrap().len();
    let repeat = log
        .save(&mut store, &task, "transcript-run", &original.messages[..5])
        .unwrap();
    assert_eq!(count, store.list_artifacts(&task).unwrap().len());
    assert_eq!(first.message_refs, repeat.message_refs);
    let full = log
        .save(&mut store, &task, "transcript-run", &original.messages)
        .unwrap();
    assert_eq!(
        MessageLog::restore(&store, &task, "transcript-run", &full).unwrap(),
        original.messages
    );
    assert!(MessageLog::restore(&store, &task, "foreign-run", &full).is_err());
    let mut changed = original.messages.clone();
    changed[0] = Message::User {
        text: "rewritten".into(),
    };
    assert!(
        log.save(&mut store, &task, "transcript-run", &changed)
            .is_err()
    );
    let mut truncated = full;
    truncated.total_bytes -= 1;
    assert!(MessageLog::restore(&store, &task, "transcript-run", &truncated).is_err());
}
#[test]
fn registered_fixture_counters_are_protocol_bound_and_unknown_images_are_explicit() {
    struct Counter(Protocol);
    impl NativeTokenCounter for Counter {
        fn policy_id(&self) -> &str {
            if self.0 == Protocol::OpenAiResponses {
                "fixture-utf8-v1"
            } else {
                "fixture-unicode-scalars-v1"
            }
        }
        fn count(&self, ep: &ProviderEndpoint, model: &str, wire: &Value) -> Result<Option<u64>> {
            if ep.protocol != self.0 || model != "fixture" {
                return Ok(None);
            }
            let raw = serde_json::to_string(wire).unwrap();
            Ok(Some(if self.0 == Protocol::OpenAiResponses {
                raw.len() as u64
            } else {
                raw.chars().count() as u64
            }))
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let (mut store, task, _) = setup(dir.path());
    let transport = HttpTransport::new(true).unwrap();
    for protocol in [Protocol::OpenAiResponses, Protocol::AnthropicMessages] {
        let mut compiler = ContextCompiler::default()
            .with_counter(Box::new(Counter(protocol)))
            .unwrap();
        let mut req = request(protocol, 0);
        let (_, report) = compiler
            .compile(
                &mut req,
                &transport,
                &endpoint(protocol),
                100_000,
                &mut store,
                &task,
                "counter",
                vec![],
            )
            .unwrap();
        let cap = report.exact_token_count.unwrap() + req.max_output_tokens;
        assert_eq!(report.capacity_check, "host_tokenizer");
        assert!(
            compiler
                .compile(
                    &mut req,
                    &transport,
                    &endpoint(protocol),
                    cap,
                    &mut store,
                    &task,
                    "counter",
                    vec![]
                )
                .is_ok()
        );
        assert!(
            compiler
                .compile(
                    &mut req,
                    &transport,
                    &endpoint(protocol),
                    cap - 1,
                    &mut store,
                    &task,
                    "counter",
                    vec![]
                )
                .is_err()
        );
    }
    let mut req = request(Protocol::OpenAiResponses, 0);
    req.messages = vec![Message::UserImages {
        text: "inspect screenshot".into(),
        images: vec![
            vision::InlinePng::new(include_bytes!("fixtures/code10-real-browser.png").to_vec())
                .unwrap(),
        ],
    }];
    let original = req.messages.clone();
    let (_, report) = ContextCompiler::default()
        .compile(
            &mut req,
            &transport,
            &endpoint(Protocol::OpenAiResponses),
            2048,
            &mut store,
            &task,
            "image",
            vec![],
        )
        .unwrap();
    assert_eq!(req.messages, original);
    assert_eq!(report.capacity_check, "not_measured_multimodal");
    assert!(report.exact_token_count.is_none());
}
