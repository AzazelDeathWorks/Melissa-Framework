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

fn intelligent() -> ContextCompiler {
    ContextCompiler::default()
        .with_intelligence(Default::default())
        .unwrap()
}

#[test]
fn intelligence_preserves_both_native_protocols_and_reduces_long_working_views() {
    for protocol in [Protocol::OpenAiResponses, Protocol::AnthropicMessages] {
        let dir = tempfile::tempdir().unwrap();
        let (mut store, task, _) = setup(dir.path());
        let original = request(protocol, 120);
        let original_bytes = serde_json::to_vec(&original).unwrap().len();
        let baseline_wire_bytes = serde_json::to_vec(
            &HttpTransport::new(true)
                .unwrap()
                .request_body(&endpoint(protocol), &original)
                .unwrap(),
        )
        .unwrap()
        .len();
        let mut working = original.clone();
        let (_, report) = intelligent()
            .compile(
                &mut working,
                &HttpTransport::new(true).unwrap(),
                &endpoint(protocol),
                2_000_000,
                &mut store,
                &task,
                "smart",
                vec![],
            )
            .unwrap();
        assert_eq!(working.max_output_tokens, original.max_output_tokens);
        assert_eq!(working.system_prompt, original.system_prompt);
        assert_eq!(working.messages.len(), original.messages.len());
        for (a, b) in original.messages.iter().zip(&working.messages) {
            if !matches!(a, Message::ToolResult { .. }) {
                assert_eq!(a, b);
            }
        }
        assert_eq!(
            &original.messages[original.messages.len() - 8..],
            &working.messages[working.messages.len() - 8..]
        );
        assert!(serde_json::to_vec(&working).unwrap().len() < original_bytes / 2);
        let info = report.intelligence.unwrap();
        println!(
            "CONTEXT_METRIC {}",
            json!({"protocol":protocol,"history_turns":120,"baseline_native_bytes":baseline_wire_bytes,
            "compiled_native_bytes":report.native_body_bytes,"build_microseconds":info.build_microseconds,"output_tokens":report.reserved_output_tokens,
            "evidence_class":"synthetic protocol history; not model quality or billed token measurement"})
        );
        assert!(info.working_history_bytes < info.original_history_bytes);
        assert!(!info.omissions.is_empty());
        assert!(info.checkpoint_refs.is_empty());
        assert_eq!(
            info.stages["milestone_checkpoint"],
            "preserve_native_continuation"
        );
        assert!(
            info.sections
                .iter()
                .any(|s| s.section == "provider_continuation" && s.serialized_bytes > 0)
        );
        for record in report.archived {
            assert_eq!(
                store
                    .get_artifact(&task, &record.artifact_ref)
                    .unwrap()
                    .len(),
                record.original_bytes
            );
        }
        assert!(store.journal().model_requests(&task).unwrap().is_empty());
    }
}

#[test]
fn completed_plain_turns_checkpoint_without_losing_decisions_failures_or_resume_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let (mut store, task, _) = setup(dir.path());
    let mut original = request(Protocol::OpenAiResponses, 120);
    for m in &mut original.messages {
        if let Message::Assistant { response } = m {
            response
                .continuation
                .retain(|v| v["type"] == "function_call");
        }
    }
    if let Message::Assistant { response } = &mut original.messages[3] {
        response.text = "Decision: preserve existing public API".into();
        response.continuation.insert(0,json!({"id":"decision1","type":"message","role":"assistant","status":"completed", "content":[{"type":"output_text","text":response.text,"annotations":[]}]}));
    }
    if let Message::ToolResult {
        is_error, content, ..
    } = &mut original.messages[6]
    {
        *is_error = true;
        *content = "Regression still fails; NOT accepted".into();
    }
    let original_bytes = serde_json::to_vec(&original.messages).unwrap();
    let log = MessageLog::default()
        .save(&mut store, &task, "durable-smart", &original.messages)
        .unwrap();
    let mut working = original.clone();
    let transport = HttpTransport::new(true).unwrap();
    let ep = endpoint(Protocol::OpenAiResponses);
    let (_, r) = intelligent()
        .compile(
            &mut working,
            &transport,
            &ep,
            2_000_000,
            &mut store,
            &task,
            "smart-plain",
            vec![],
        )
        .unwrap();
    assert!(working.messages.len() < original.messages.len() / 2);
    working.validate_structure().unwrap();
    assert!(working.messages.contains(&original.messages[3]));
    assert!(working.messages.contains(&original.messages[6]));
    let info = r.intelligence.unwrap();
    assert_eq!(info.checkpoint_refs.len(), 1);
    let evidence = store.get_artifact(&task, &info.checkpoint_refs[0]).unwrap();
    let evidence: Value = serde_json::from_slice(&evidence).unwrap();
    assert!(evidence["groups"].as_array().unwrap().len() > 16);
    let restored = MessageLog::restore(&store, &task, "durable-smart", &log).unwrap();
    assert_eq!(serde_json::to_vec(&restored).unwrap(), original_bytes);
    let mut resumed = original;
    let (_, again) = intelligent()
        .compile(
            &mut resumed,
            &transport,
            &ep,
            2_000_000,
            &mut store,
            &task,
            "smart-plain",
            vec![],
        )
        .unwrap();
    assert_eq!(resumed, working);
    assert_eq!(
        again.intelligence.unwrap().checkpoint_refs,
        info.checkpoint_refs
    );
}

#[test]
fn normalized_failed_test_keeps_failure_source_and_full_verbatim_evidence() {
    let dir = tempfile::tempdir().unwrap();
    let (mut store, task, _) = setup(dir.path());
    let mut req = request(Protocol::OpenAiResponses, 1);
    if let Message::Assistant { response } = &mut req.messages[1] {
        response.calls[0].name = "code_project_tests".into();
        response.continuation[1]["name"] = json!("code_project_tests");
    }
    let raw=json!({"schema_version":"melissa.sdk-result/v1","status":"failed","raw_worker_artifact_id":"raw:1",
        "source_digest":"blake3:source","host_source_current":false,"details":{
            "runs":[{"id":"fails","status":"failed"},{"id":"passes","status":"passed"},{"id":"skip","status":"skipped"}],
            "steps":[{"stage":"tests","exit_code":101,"stdout_hex":"aa".repeat(20000)}],
            "diagnostics":[{"code":"E0308","file":"src/lib.rs","line":9,"level":"error","message":"wrong type"}],
            "truncated":true,"error":null,"discovered":[{},{}]},"warnings":["incomplete output"],"protocol_error":null}).to_string();
    req.messages[2] = Message::ToolResult {
        call_id: "call0".into(),
        content: raw.clone(),
        is_error: true,
    };
    let (_, r) = intelligent()
        .compile(
            &mut req,
            &HttpTransport::new(true).unwrap(),
            &endpoint(Protocol::OpenAiResponses),
            200_000,
            &mut store,
            &task,
            "normalize",
            vec![],
        )
        .unwrap();
    let Message::ToolResult {
        content, is_error, ..
    } = &req.messages[2]
    else {
        panic!()
    };
    assert!(*is_error);
    let v: Value = serde_json::from_str(content).unwrap();
    assert_eq!(v["observation"]["status"], "failed");
    assert_eq!(v["observation"]["passed_count"], 1);
    assert_eq!(v["observation"]["failed_count"], 1);
    assert_eq!(v["observation"]["skipped_count"], 1);
    assert_eq!(v["observation"]["source_truncated"], true);
    assert_eq!(v["observation"]["host_source_current"], false);
    assert_eq!(
        v["observation"]["diagnostics"][0]["diagnostic_code"],
        "E0308"
    );
    assert_eq!(
        store
            .get_artifact(&task, &r.archived[0].artifact_ref)
            .unwrap(),
        raw.as_bytes()
    );
    assert!(content.len() < raw.len() / 4);
}

#[test]
fn typed_search_and_diff_preserve_source_identity_and_unknowns() {
    let hits=(0..100).map(|i|json!({"path":format!("src/{i}.rs"),"line":i+1,"byte_offset":i,"digest":"blake3:file"})).collect::<Vec<_>>();
    let raw=json!({"schema_version":"melissa.repository-tool-result/v1","source_visibility_digest":"policy",
        "result":{"hits":hits,"truncated":true,"snapshot_digest":"blake3:tree"}}).to_string();
    let view = melissa_model_gateway::observations::normalize("fs_search", &raw).unwrap();
    assert_eq!(view["match_count"], 100);
    assert_eq!(view["matches"].as_array().unwrap().len(), 16);
    assert_eq!(view["matches"][0]["content_digest"], "blake3:file");
    assert!(view["matches"][0]["match_excerpt"].is_null());
    assert!(melissa_model_gateway::observations::normalize("fs_read", &raw).is_none());
    let raw = json!({"operation":{"operation":"git","operation_unused":false,"operation_details":{},"unused":0},"stdout":""});
    assert!(melissa_model_gateway::observations::normalize("code_git", &raw.to_string()).is_none());
    let raw = json!({"operation":{"service":"git","operation":{"operation":"diff","staged":false}},"stdout":"diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1 +1 @@\n-old\n+new\n","source_digest":"blake3:tree","source_current":true,"success":true,"stderr":""});
    let view =
        melissa_model_gateway::observations::normalize("code_git", &raw.to_string()).unwrap();
    assert_eq!(view["changed_files"][0]["new_display_path"], "b/a");
    assert_eq!(view["current_hunks"][0]["header"], "@@ -1 +1 @@");
    assert!(
        view["current_hunks"][0]["excerpt"]
            .as_str()
            .unwrap()
            .contains("+new")
    );
    assert_eq!(view["task_accepted"], false);
}

#[test]
fn intelligence_rejects_conflicting_schemas_overbudget_sections_and_broken_pairs() {
    let dir = tempfile::tempdir().unwrap();
    let (mut store, task, _) = setup(dir.path());
    let transport = HttpTransport::new(true).unwrap();
    let ep = endpoint(Protocol::OpenAiResponses);
    let mut req = request(Protocol::OpenAiResponses, 0);
    req.tools.push(req.tools[1].clone());
    intelligent()
        .compile(
            &mut req,
            &transport,
            &ep,
            100_000,
            &mut store,
            &task,
            "dedup",
            vec![],
        )
        .unwrap();
    assert_eq!(req.tools.len(), 2);
    let mut conflict = req.tools[1].clone();
    conflict.description = "different contract".into();
    req.tools.push(conflict);
    assert!(
        intelligent()
            .compile(
                &mut req,
                &transport,
                &ep,
                100_000,
                &mut store,
                &task,
                "conflict",
                vec![]
            )
            .is_err()
    );
    let mut policy = melissa_model_gateway::context_intelligence::ContextPolicy::default();
    policy
        .section_byte_limits
        .insert("system_and_specialist".into(), 1);
    let mut req = request(Protocol::OpenAiResponses, 0);
    let original = req.clone();
    assert!(
        ContextCompiler::default()
            .with_intelligence(policy)
            .unwrap()
            .compile(
                &mut req,
                &transport,
                &ep,
                100_000,
                &mut store,
                &task,
                "budget",
                vec![]
            )
            .is_err()
    );
    assert_eq!(req, original);
    let mut req = request(Protocol::OpenAiResponses, 1);
    req.messages.pop();
    assert!(
        intelligent()
            .compile(
                &mut req,
                &transport,
                &ep,
                100_000,
                &mut store,
                &task,
                "broken",
                vec![]
            )
            .is_err()
    );
    assert!(store.journal().model_requests(&task).unwrap().is_empty());
}

#[test]
fn intelligence_binds_opaque_history_to_provider_model_and_source_visibility() {
    let dir = tempfile::tempdir().unwrap();
    let (mut store, task, profile) = setup(dir.path());
    let transport = HttpTransport::new(true).unwrap();
    let ep = endpoint(Protocol::OpenAiResponses);
    let mut compiler = intelligent();
    compiler
        .set_source_scope("source-policy-one".into())
        .unwrap();
    let mut req = request(Protocol::OpenAiResponses, 80);
    let (_, report) = compiler
        .compile(
            &mut req,
            &transport,
            &ep,
            2_000_000,
            &mut store,
            &task,
            "bound",
            vec![],
        )
        .unwrap();
    let record = &report.archived[0];
    let context = ToolContext {
        task_id: task.clone(),
        profile_id: profile,
        run_id: "bound".into(),
        request_id: "r".into(),
        invocation_id: "i".into(),
        attempt_id: "a".into(),
        generation: 1,
    };
    let args =
        json!({"artifact_ref":record.artifact_ref,"start_byte":0,"max_bytes":128}).to_string();
    assert!(
        read_history_scoped(
            &store,
            &context,
            &args,
            &AtomicBool::new(false),
            "source-policy-one"
        )
        .is_ok()
    );
    assert!(
        read_history_scoped(
            &store,
            &context,
            &args,
            &AtomicBool::new(false),
            "source-policy-two"
        )
        .is_err()
    );
    assert!(read_history(&store, &context, &args, &AtomicBool::new(false)).is_err());
    let mut other = ep.clone();
    other.provider_id = "foreign-provider".into();
    assert!(
        compiler
            .compile(
                &mut request(Protocol::OpenAiResponses, 80),
                &transport,
                &other,
                2_000_000,
                &mut store,
                &task,
                "bound",
                vec![]
            )
            .is_err()
    );
    let mut other = request(Protocol::OpenAiResponses, 80);
    other.model = "foreign-model".into();
    assert!(
        compiler
            .compile(
                &mut other,
                &transport,
                &ep,
                2_000_000,
                &mut store,
                &task,
                "bound",
                vec![]
            )
            .is_err()
    );
    compiler
        .set_source_scope("source-policy-two".into())
        .unwrap();
    assert!(
        compiler
            .compile(
                &mut request(Protocol::OpenAiResponses, 80),
                &transport,
                &ep,
                2_000_000,
                &mut store,
                &task,
                "bound",
                vec![]
            )
            .is_err()
    );
}

#[test]
fn a_new_scope_record_cannot_promote_an_old_unscoped_observation() {
    let dir = tempfile::tempdir().unwrap();
    let (mut store, task, profile) = setup(dir.path());
    let transport = HttpTransport::new(true).unwrap();
    let endpoint = endpoint(Protocol::OpenAiResponses);
    let (_, old) = ContextCompiler::default()
        .compile(
            &mut request(Protocol::OpenAiResponses, 80),
            &transport,
            &endpoint,
            150_000,
            &mut store,
            &task,
            "legacy",
            vec![],
        )
        .unwrap();
    let record = &old.archived[0];
    let original = store.get_artifact(&task, &record.artifact_ref).unwrap();
    let mut compiler = intelligent();
    compiler.set_source_scope("current-policy".into()).unwrap();
    compiler
        .compile(
            &mut request(Protocol::OpenAiResponses, 0),
            &transport,
            &endpoint,
            150_000,
            &mut store,
            &task,
            "legacy",
            vec![],
        )
        .unwrap();
    let context = ToolContext {
        task_id: task.clone(),
        profile_id: profile,
        run_id: "legacy".into(),
        request_id: "r".into(),
        invocation_id: "i".into(),
        attempt_id: "a".into(),
        generation: 1,
    };
    let args =
        json!({"artifact_ref":record.artifact_ref,"start_byte":0,"max_bytes":128}).to_string();
    assert!(
        read_history_scoped(
            &store,
            &context,
            &args,
            &AtomicBool::new(false),
            "current-policy"
        )
        .is_err()
    );
    assert_eq!(
        store.get_artifact(&task, &record.artifact_ref).unwrap(),
        original
    );
    assert!(
        compiler
            .compile(
                &mut request(Protocol::OpenAiResponses, 80),
                &transport,
                &endpoint,
                2_000_000,
                &mut store,
                &task,
                "legacy",
                vec![]
            )
            .is_err()
    );
}

#[test]
fn current_goal_and_milestone_are_rebuilt_without_changing_required_inputs() {
    let dir = tempfile::tempdir().unwrap();
    let (mut store, task, _) = setup(dir.path());
    let mut compiler = intelligent();
    let state = melissa_model_gateway::context_intelligence::TurnState {
        task_id: task.clone(),
        run_id: "current".into(),
        profile_id: "p".into(),
        goal: "Keep Unicode 中文 goal".into(),
        task_version: 4,
        task_state: "Running".into(),
        turn: 2,
        successful_tools: Default::default(),
        remaining_required_tools: Default::default(),
        milestone: None,
        budget: json!({"fixture":true}),
    };
    compiler.set_turn_state(state.clone());
    let original = request(Protocol::OpenAiResponses, 0);
    let mut working = original.clone();
    let (_, report) = compiler
        .compile(
            &mut working,
            &HttpTransport::new(true).unwrap(),
            &endpoint(Protocol::OpenAiResponses),
            100_000,
            &mut store,
            &task,
            "current",
            vec![],
        )
        .unwrap();
    assert_eq!(working.messages[0], original.messages[0]);
    assert_eq!(working.messages.len(), 2);
    let Message::User { text } = &working.messages[1] else {
        panic!()
    };
    let v: Value = serde_json::from_str(text).unwrap();
    assert_eq!(v["current_goal_and_milestone"]["turn"], 2);
    assert!(
        report
            .intelligence
            .unwrap()
            .sections
            .iter()
            .any(|s| s.section == "goal_and_milestone" && s.serialized_bytes > 0)
    );
    let mut state = state;
    state.turn = 3;
    state.successful_tools.insert("code_tests".into());
    compiler.set_turn_state(state);
    let mut working = original;
    compiler
        .compile(
            &mut working,
            &HttpTransport::new(true).unwrap(),
            &endpoint(Protocol::OpenAiResponses),
            100_000,
            &mut store,
            &task,
            "current",
            vec![],
        )
        .unwrap();
    let Message::User { text } = &working.messages[1] else {
        panic!()
    };
    assert!(text.contains("code_tests"));
    assert!(text.contains("\"turn\":3"));
}

#[test]
fn invalid_old_native_history_cannot_be_hidden_by_a_milestone() {
    let dir = tempfile::tempdir().unwrap();
    let (mut store, task, _) = setup(dir.path());
    let mut req = request(Protocol::OpenAiResponses, 100);
    for m in &mut req.messages {
        if let Message::Assistant { response } = m {
            response
                .continuation
                .retain(|v| v["type"] == "function_call");
        }
    }
    if let Message::Assistant { response } = &mut req.messages[1] {
        response.continuation[0]["arguments"] = json!("{\"tampered\":true}");
    }
    let error = intelligent()
        .compile(
            &mut req,
            &HttpTransport::new(true).unwrap(),
            &endpoint(Protocol::OpenAiResponses),
            2_000_000,
            &mut store,
            &task,
            "invalid-old",
            vec![],
        )
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Invalid);
    assert!(error.message.contains("disagrees"));
    assert!(store.journal().model_requests(&task).unwrap().is_empty());
}

#[test]
fn cargo_diagnostics_and_large_views_keep_failure_counts_and_never_claim_acceptance() {
    let raw=json!({"schema_version":"melissa.rust-build-test/v1","outcome":"failed","raw_logs":[{"stage":"cargo check","exit_code":101}],
        "counts":{"passed":4,"failed":1,"ignored":2},"test_runs":[{"tests":{"pass":"passed","fail":"failed","skip":"ignored"}}],
        "diagnostics":(0..128).map(|i|json!({"code":"E0308","level":"error","message":"x".repeat(2000),
            "spans":[{"file":"src/lib.rs","line":i+1,"end_line":i+2,"primary":true}]})).collect::<Vec<_>>(),
        "host_source_current":true,"truncated":false}).to_string();
    let view = melissa_model_gateway::observations::normalize("code_check", &raw).unwrap();
    assert_eq!(view["counts"]["failed"], 1);
    assert_eq!(view["counts"]["ignored"], 2);
    assert_eq!(view["diagnostics"][0]["affected_file"], "src/lib.rs");
    assert_eq!(view["diagnostics"][0]["line_range"], json!([1, 2]));
    assert!(serde_json::to_vec(&view).unwrap().len() <= 12 * 1024);
    assert_eq!(view["view_is_partial"], true);
    assert_eq!(view["protected_acceptance"], false);
    assert!(
        melissa_model_gateway::observations::normalize(
            "code_check",
            "{\"status\":\"passed\",\"status\":\"failed\"}"
        )
        .is_none()
    );
}

#[test]
fn external_summary_requires_exact_source_and_review_evidence_and_never_invokes_a_model() {
    use melissa_model_gateway::context_intelligence::ReviewedSummary;
    let dir = tempfile::tempdir().unwrap();
    let (mut store, task, _) = setup(dir.path());
    let original = request(Protocol::OpenAiResponses, 80);
    let Message::ToolResult { content: raw, .. } = &original.messages[2] else {
        panic!()
    };
    let source_digest = format!("blake3:{}", blake3::hash(raw.as_bytes()));
    let summary = ReviewedSummary {
        task_id: task.clone(),
        run_id: "summary".into(),
        call_id: "call0".into(),
        source_digest: source_digest.clone(),
        summary: "Earlier observation is historical; verify current source.".into(),
        evidence_ref: "summary:source".into(),
        review_ref: "summary:review".into(),
    };
    let review=json!({"approved":true,"task_id":task,"run_id":"summary","call_id":"call0","source_digest":source_digest,"summary_digest":format!("blake3:{}",blake3::hash(summary.summary.as_bytes()))}).to_string();
    for (id, data) in [
        (&summary.evidence_ref, raw.as_bytes()),
        (&summary.review_ref, review.as_bytes()),
    ] {
        store
            .put_artifact(
                &melissa_local_store::ArtifactRequest {
                    artifact_id: id.clone(),
                    task_id: task.clone(),
                    content_type: "application/json".into(),
                    logical_type: "context.review-fixture".into(),
                    schema_version: "v1".into(),
                    retain_until_ms: 0,
                    legal_hold: false,
                },
                data,
            )
            .unwrap();
    }
    let mut working = original.clone();
    let (_, report) = intelligent()
        .with_reviewed_summary(summary.clone())
        .compile(
            &mut working,
            &HttpTransport::new(true).unwrap(),
            &endpoint(Protocol::OpenAiResponses),
            2_000_000,
            &mut store,
            &task,
            "summary",
            vec![],
        )
        .unwrap();
    assert!(
        report.intelligence.unwrap().stages["model_assisted_summary"]
            .contains("reviewed_external_summary")
    );
    let Message::ToolResult { content, .. } = &working.messages[2] else {
        panic!()
    };
    assert!(content.contains("reviewed_summary"));
    let mut bad = summary;
    bad.summary = "An unreviewed claim".into();
    assert!(
        intelligent()
            .with_reviewed_summary(bad)
            .compile(
                &mut original.clone(),
                &HttpTransport::new(true).unwrap(),
                &endpoint(Protocol::OpenAiResponses),
                2_000_000,
                &mut store,
                &task,
                "summary",
                vec![]
            )
            .is_err()
    );
    assert!(store.journal().model_requests(&task).unwrap().is_empty());
}
#[test]
fn local_wire_measurements_never_become_exact_counts_or_modify_requests() {
    use melissa_model_gateway::local_tokenizer::{LocalTokenizerConfig, LocalWireTokenizer};
    let dir = tempfile::tempdir().unwrap();
    let (mut store, task, _) = setup(dir.path());
    let path = dir.path().join("tokenizer.json");
    let bytes = include_bytes!("fixtures/local-tokenizer.json");
    std::fs::write(&path, bytes).unwrap();
    for protocol in [Protocol::OpenAiResponses, Protocol::AnthropicMessages] {
        let ep = endpoint(protocol);
        let tokenizer = LocalWireTokenizer::load(
            &LocalTokenizerConfig {
                path: path.clone(),
                blake3: blake3::hash(bytes).to_hex().to_string(),
            },
            ep.url.as_str(),
            protocol,
            "fixture",
        )
        .unwrap();
        let mut compiler = ContextCompiler::default().with_wire_tokenizer(tokenizer);
        let transport = HttpTransport::new(true).unwrap();
        let mut req = request(protocol, 0);
        let expected = transport.request_body(&ep, &req).unwrap();
        let (actual, report) = compiler
            .compile(
                &mut req,
                &transport,
                &ep,
                100_000,
                &mut store,
                &task,
                "wire",
                vec![],
            )
            .unwrap();
        assert_eq!(expected, actual);
        assert!(report.wire_token_measurement.unwrap().token_count > 0);
        assert!(report.exact_token_count.is_none());
        assert_eq!(report.capacity_check, "conservative_text_units");
        assert_eq!(report.reserved_output_tokens, 1024);
        // The smaller wire token count cannot admit an oversized request.
        assert!(
            compiler
                .compile(
                    &mut req,
                    &transport,
                    &ep,
                    1100,
                    &mut store,
                    &task,
                    "wire",
                    vec![]
                )
                .is_err()
        );
        let mut req = request(protocol, 1);
        let (_, report) = compiler
            .compile(
                &mut req,
                &transport,
                &ep,
                100_000,
                &mut store,
                &task,
                "wire",
                vec![],
            )
            .unwrap();
        assert!(report.wire_token_measurement.is_none());
        assert_eq!(
            report.wire_tokenizer_status.as_deref(),
            Some("not_measured_images_or_provider_continuation")
        );
        assert!(store.journal().model_requests(&task).unwrap().is_empty());
    }
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
fn history_scope_rejects_prefix_related_runs_and_incomplete_utf8_pages() {
    let dir = tempfile::tempdir().unwrap();
    let (mut store, task, profile) = setup(dir.path());
    let body = "ö content";
    let hash = blake3::hash(body.as_bytes()).to_hex().to_string();
    let own = format!("{}{}", history_prefix("run"), hash);
    let nested = format!("{}{}", history_prefix("run:child"), hash);
    for id in [&own, &nested] {
        store
            .put_artifact(
                &melissa_local_store::ArtifactRequest {
                    artifact_id: id.clone(),
                    task_id: task.clone(),
                    content_type: "text/plain".into(),
                    logical_type: "context.archived-observation".into(),
                    schema_version: "v1".into(),
                    retain_until_ms: 0,
                    legal_hold: false,
                },
                body.as_bytes(),
            )
            .unwrap();
    }
    let mut context = ToolContext {
        task_id: task,
        profile_id: profile,
        run_id: "run".into(),
        request_id: "r".into(),
        invocation_id: "i".into(),
        attempt_id: "a".into(),
        generation: 1,
    };
    let page = |context: &ToolContext, id: &str, start: usize, size: usize| {
        read_history(
            &store,
            context,
            &json!({"artifact_ref":id,"start_byte":start,"max_bytes":size}).to_string(),
            &AtomicBool::new(false),
        )
    };
    assert!(page(&context, &nested, 0, 20).is_err());
    assert!(page(&context, &own, 0, 1).is_err());
    assert!(page(&context, &own, 1, 2).is_err());
    let result: Value = serde_json::from_str(&page(&context, &own, 0, 2).unwrap().content).unwrap();
    assert_eq!(result["content"], "ö");
    context.run_id = "run:child".into();
    assert!(page(&context, &nested, 0, 20).is_ok());
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
