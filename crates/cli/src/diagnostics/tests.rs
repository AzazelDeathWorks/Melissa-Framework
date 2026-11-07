use super::*;
use crate::project::tests::Fixture;
use std::{
    fs,
    sync::{Arc, atomic::AtomicBool},
};
fn task(f: &Fixture) -> String {
    f.app
        .create(
            &f.source,
            "Review SUPPORT_CANARY_GOAL",
            "melissa.code.reviewer",
            vec![],
        )
        .unwrap()["task_id"]
        .as_str()
        .unwrap()
        .into()
}
#[test]
fn error_classes_separate_non_execution_uncertainty_and_model_failures() {
    for (error, class, status) in [
        (Error::Usage("x"), "input", "NOT_RUN"),
        (
            Error::Model(melissa_model_gateway::ErrorKind::Budget),
            "policy",
            "BLOCKED",
        ),
        (
            Error::Model(melissa_model_gateway::ErrorKind::ProviderUnavailable),
            "environment",
            "BLOCKED",
        ),
        (
            Error::Model(melissa_model_gateway::ErrorKind::Tool),
            "tool",
            "FAIL",
        ),
        (
            Error::Model(melissa_model_gateway::ErrorKind::Invalid),
            "input",
            "NOT_RUN",
        ),
        (Error::Denied("x"), "policy", "BLOCKED"),
        (Error::Unavailable("x"), "environment", "BLOCKED"),
        (Error::Verification("x"), "acceptance", "FAIL"),
        (Error::Repository, "tool", "BLOCKED"),
        (Error::Reconciliation("x"), "uncertain", "INCONCLUSIVE"),
        (
            Error::Model(melissa_model_gateway::ErrorKind::Protocol),
            "model",
            "FAIL",
        ),
    ] {
        let v = explain(&error);
        assert_eq!(v["class"], class);
        assert_eq!(v["status"], status);
        assert_eq!(v["automatic_retry"], false);
    }
}
#[test]
fn support_allowlist_removes_canary_secrets_paths_settings_and_raw_errors_without_altering_evidence()
 {
    let f = Fixture::new(false);
    let id = task(&f);
    fs::write(f.source.join(".env"), "SUPPORT_CANARY_ENV").unwrap();
    let mut store = f.app.store(&id).unwrap();
    crate::app::put(&mut store,&id,"fixture:error","run.error",&bytes(&json!({"error_kind":"unavailable","error":"SUPPORT_CANARY_ERROR https://user:password@private.example/token","requires_reconciliation":false})).unwrap()).unwrap();
    {
        use melissa_model_gateway::{
            ErrorKind, ToolCall,
            adapter::{AdapterMetadata, Status, ToolPreflight, ToolReceipt},
            tools::ToolContext,
        };
        let pre = ToolPreflight::new(
            &ToolCall {
                id: "blocked".into(),
                name: "unknown".into(),
                arguments: "SUPPORT_CANARY_ARGS".into(),
            },
            &[],
            Status::Blocked,
            AdapterMetadata::default(),
        );
        let receipt = ToolReceipt::not_run(
            &ToolContext {
                task_id: id.clone(),
                profile_id: "melissa.code.reviewer".into(),
                run_id: "offline".into(),
                request_id: "NEVER_DISPATCHED_FIXTURE".into(),
                invocation_id: "blocked".into(),
                attempt_id: "blocked-attempt".into(),
                generation: 1,
            },
            pre,
            Some(ErrorKind::Authorization),
        );
        store
            .put_artifact(
                &melissa_local_store::ArtifactRequest {
                    artifact_id: "blocked:receipt".into(),
                    task_id: id.clone(),
                    content_type: "application/json".into(),
                    logical_type: "tool.receipt".into(),
                    schema_version: "v1".into(),
                    retain_until_ms: 0,
                    legal_hold: true,
                },
                &bytes(&receipt).unwrap(),
            )
            .unwrap();
    }
    let before = store.journal().events(&id).unwrap();
    let original = store.get_artifact(&id, "fixture:error").unwrap();
    drop(store);
    let report = f.app.diagnose(&id).unwrap();
    assert_eq!(report["model_test_status"], "NOT_RUN");
    assert!(
        report["issues"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["class"] == "environment" && i["status"] == "BLOCKED")
    );
    assert!(
        report["issues"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["class"] == "policy"
                && i["status"] == "BLOCKED"
                && i["dispatch_started"] == false)
    );
    let output = f.root.join("support");
    f.app.support(&id, &output).unwrap();
    let raw = fs::read_to_string(output.join("support.json")).unwrap();
    for forbidden in [
        "SUPPORT_CANARY",
        "password",
        "private.example",
        "offline-fixture",
        "provider.example.invalid",
        f.source.to_str().unwrap(),
        f.app.home.to_str().unwrap(),
    ] {
        assert!(!raw.contains(forbidden), "{forbidden}");
    }
    let manifest: Value =
        serde_json::from_slice(&fs::read(output.join("manifest.json")).unwrap()).unwrap();
    assert_eq!(manifest["files"][0]["digest"], digest(raw.as_bytes()));
    assert!(f.app.support(&id, &output).is_err());
    assert!(f.app.support(&id, &f.source.join("leak")).is_err());
    let store = f.app.store(&id).unwrap();
    assert_eq!(store.journal().events(&id).unwrap(), before);
    assert_eq!(store.get_artifact(&id, "fixture:error").unwrap(), original);
    assert!(store.journal().model_requests(&id).unwrap().is_empty());
}
#[test]
fn host_process_measurement_is_real_scoped_and_persists_in_authenticated_control() {
    let f = Fixture::new(false);
    let id = task(&f);
    let control = crate::control::Control::new(
        &f.app,
        &id,
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicBool::new(false)),
    )
    .unwrap();
    let mut data = vec![13u8; 4 * 1024 * 1024];
    for byte in &mut data {
        *byte = byte.wrapping_add(1);
    }
    std::hint::black_box(&data);
    control
        .publish("paused", json!({}), json!({}), json!({}), json!({}))
        .unwrap();
    let sample = f.app.live(&id).unwrap().host_measurement.unwrap();
    assert!(sample.latest.working_set_bytes.unwrap() > 0);
    assert!(
        sample
            .latest
            .process_lifetime_peak_working_set_bytes
            .unwrap()
            >= sample.latest.working_set_bytes.unwrap()
    );
    assert!(sample.read_transfer_delta.is_some());
    let metrics = f.app.metrics(&id).unwrap();
    assert!(metrics["metrics"]["peak_host_rss_bytes"].as_u64().unwrap() > 0);
    assert!(metrics["metrics"]["disk_read_bytes"].is_null());
}
#[tokio::test]
async fn otlp_wire_uses_hex_ids_numeric_enums_and_single_attempt_http_with_partial_failure() {
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };
    let f = Fixture::new(false);
    let id = task(&f);
    let body = f.app.otlp(&id).unwrap();
    let span = &body["resourceSpans"][0]["scopeSpans"][0]["spans"][0];
    assert_eq!(span["traceId"].as_str().unwrap().len(), 32);
    assert_eq!(span["spanId"].as_str().unwrap().len(), 16);
    assert_eq!(span["kind"], 1);
    assert!(span["startTimeUnixNano"].is_string());
    assert!(!body.to_string().contains("SUPPORT_CANARY"));
    for (response, expected) in [
        ("{}", "PASS"),
        (
            r#"{"partialSuccess":{"rejectedSpans":"1","errorMessage":"do not leak this"}}"#,
            "FAIL",
        ),
        ("broken-json", "INCONCLUSIVE"),
        ("[]", "INCONCLUSIVE"),
        (r#"{"partialSuccess":"wrong shape"}"#, "INCONCLUSIVE"),
    ] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let response = response.to_owned();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut input = vec![];
            let header_end;
            loop {
                let mut chunk = [0u8; 4096];
                let n = stream.read(&mut chunk).await.unwrap();
                assert!(n > 0);
                input.extend_from_slice(&chunk[..n]);
                if let Some(p) = input.windows(4).position(|s| s == b"\r\n\r\n") {
                    header_end = p + 4;
                    break;
                }
            }
            let headers = String::from_utf8_lossy(&input[..header_end]).to_lowercase();
            assert!(headers.starts_with("post /v1/traces"));
            assert!(headers.contains("content-type: application/json"));
            let length = headers
                .lines()
                .find_map(|l| l.strip_prefix("content-length:"))
                .unwrap()
                .trim()
                .parse::<usize>()
                .unwrap();
            while input.len() - header_end < length {
                let mut chunk = [0u8; 4096];
                let n = stream.read(&mut chunk).await.unwrap();
                assert!(n > 0);
                input.extend_from_slice(&chunk[..n]);
            }
            let _: Value = serde_json::from_slice(&input[header_end..]).unwrap();
            let reply = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                response.len(),
                response
            );
            stream.write_all(reply.as_bytes()).await.unwrap();
        });
        let result = f
            .app
            .export_otlp(&id, None, Some(&format!("http://{address}/v1/traces")))
            .await
            .unwrap();
        server.await.unwrap();
        assert_eq!(result["status"], expected);
        assert!(!result.to_string().contains("do not leak"));
        assert_eq!(result["provider_model_calls"], 0);
    }
    for endpoint in [
        "https://collector.example/v1/traces",
        "http://localhost:4318/v1/traces",
        "http://127.0.0.1:4318/private",
        "http://user:pass@127.0.0.1:4318/v1/traces",
    ] {
        assert!(f.app.export_otlp(&id, None, Some(endpoint)).await.is_err());
    }
    let out = f.root.join("otlp.json");
    assert_eq!(
        f.app.export_otlp(&id, Some(&out), None).await.unwrap()["delivery"]["status"],
        "NOT_RUN"
    );
    assert!(f.app.export_otlp(&id, Some(&out), None).await.is_err());
}

#[test]
fn uncertain_journal_fixture_never_becomes_a_completed_model_span_or_triggers_transport() {
    // Synthetic ledger transitions only; no Gateway/HTTP/provider is constructed.
    use melissa_local_store::journal::ModelReservation;
    let f = Fixture::new(false);
    let value = f
        .app
        .create(
            &f.source,
            "Unknown outcome fixture",
            "melissa.code.reviewer",
            vec![],
        )
        .unwrap();
    let (value, mut store) = f.running_value(value);
    let id = value["task_id"].as_str().unwrap();
    let time = now().unwrap();
    let version = store.journal().task(id).unwrap().record.version;
    store
        .journal_mut()
        .reserve_model_request(
            &ModelReservation {
                schema_version: "melissa.model-reservation/v1".into(),
                request_id: "synthetic-unknown".into(),
                task_id: id.into(),
                profile_id: "melissa.code.reviewer".into(),
                provider_id: "offline-ledger-fixture".into(),
                model_id: "NO_TRANSPORT".into(),
                request_digest: digest(b"synthetic request identity"),
                pricing_digest: Some(digest(b"zero cost fixture")),
                reserved_input_tokens: 1,
                reserved_output_tokens: 1,
                reserved_cost_micro_usd: 0,
                created_at_unix_ms: time,
            },
            version,
        )
        .unwrap();
    store
        .journal_mut()
        .dispatch_model_request("synthetic-unknown", 0, time)
        .unwrap();
    store
        .journal_mut()
        .mark_model_request_unknown(
            "synthetic-unknown",
            1,
            "Synthetic interrupted ledger; no actual HTTP or inference",
            time,
        )
        .unwrap();
    let before = store.journal().model_request("synthetic-unknown").unwrap();
    drop(store);
    let output = f.app.otlp(id).unwrap();
    let spans = output["resourceSpans"][0]["scopeSpans"][0]["spans"]
        .as_array()
        .unwrap();
    assert!(spans.iter().all(|s| s["name"] != "melissa.model.request"));
    assert!(
        spans[0]["attributes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|a| a["key"] == "melissa.pending_model_spans_not_closed"
                && a["value"]["intValue"] == "1")
    );
    let diagnosis = f.app.diagnose(id).unwrap();
    assert_eq!(diagnosis["status"], "INCONCLUSIVE");
    assert_eq!(
        f.app
            .store(id)
            .unwrap()
            .journal()
            .model_request("synthetic-unknown")
            .unwrap(),
        before
    );
}
