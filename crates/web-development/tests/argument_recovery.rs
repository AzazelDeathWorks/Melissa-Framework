mod support;
use melissa_model_gateway::{ErrorKind, ToolCall, tools::ToolDispatcher};
use melissa_repository::{Limits, Repository};
use melissa_web_development::{protocol::Service, tools::WebDispatcher};
use serde_json::{Value, json};
use support::*;

fn call(raw: &str, session: Value) -> ToolCall {
    ToolCall {
        id: "call:portable-recovery".into(),
        name: "web_session".into(),
        arguments: json!({"session_id":session,"operation_json":raw}).to_string(),
    }
}

#[test]
fn malformed_portable_json_can_be_corrected_without_converting_authority_denials() {
    let f = Fixture::new(Service::Browser, None, &[]);
    let d = WebDispatcher::new(
        Repository::open(&f.root(), Limits::default(), vec![]).unwrap(),
        f.store.connect().unwrap(),
        f.catalog.clone(),
        f.policy.clone(),
        f.registration.clone(),
        limits(),
    )
    .unwrap()
    .with_portable_model_wire()
    .unwrap();
    for raw in [
        r#"{\"operation\": {\"command\": \"start\"}, \"session_id\": null}"#,
        r#"{"operation":{"command":"start"},"session_id":null}"#,
        r#"{"command":"unknown-operation"}"#,
        r#"{"command":"start","command":"close"}"#,
    ] {
        let error = d.validate_call(&call(raw, Value::Null)).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Invalid, "{raw}: {error}");
        assert!(!error.message.is_empty());
        d.validate_call(&call(r#"{"command":"start"}"#, Value::Null))
            .unwrap();
    }
    // No model argument may gain an existing session or an external target.
    assert_eq!(
        d.validate_call(&call(r#"{"command":"start"}"#, json!("web:foreign")))
            .unwrap_err()
            .kind,
        ErrorKind::Authorization
    );
    assert_eq!(
        d.validate_call(&call(
            r#"{"command":"navigate","page":"main","path":"https://outside.example/"}"#,
            Value::Null
        ))
        .unwrap_err()
        .kind,
        ErrorKind::Authorization
    );
    d.revocation().revoke();
    assert_eq!(
        d.validate_call(&call(r#"{"command":"start"}"#, Value::Null))
            .unwrap_err()
            .kind,
        ErrorKind::Authorization
    );
    assert!(
        f.store
            .journal()
            .events(&f.ctx.task_id)
            .unwrap()
            .iter()
            .all(|e| e.invocation.is_none())
    );
}
