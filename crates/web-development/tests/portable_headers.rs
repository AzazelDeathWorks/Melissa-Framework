mod support;
use melissa_model_gateway::{ErrorKind, ToolCall, tools::ToolDispatcher};
use melissa_repository::{Limits, Repository};
use melissa_web_development::{protocol::Service, tools::WebDispatcher};
use serde_json::{Value, json};
use std::sync::{Arc, atomic::AtomicBool};
use support::*;

fn call(portable: bool, handle: &Value, operation: Value) -> ToolCall {
    ToolCall {
        id: "call:header-case".into(),
        name: "web_session".into(),
        arguments: if portable {
            json!({"session_id":handle,"operation_json":operation.to_string()})
        } else {
            json!({"session_id":handle,"operation":operation})
        }
        .to_string(),
    }
}

fn response_bodies(value: &Value, out: &mut Vec<Value>) {
    match value {
        Value::Object(fields) => {
            if let Some(hex) = fields.get("body_hex").and_then(Value::as_str) {
                let (pairs, remainder) = hex.as_bytes().as_chunks::<2>();
                assert!(remainder.is_empty(), "Odd native body hex length");
                let bytes = pairs
                    .iter()
                    .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
                    .collect::<Vec<_>>();
                out.push(serde_json::from_slice(&bytes).unwrap());
            }
            for child in fields.values() {
                response_bodies(child, out);
            }
        }
        Value::Array(values) => {
            for child in values {
                response_bodies(child, out);
            }
        }
        _ => {}
    }
}

async fn exercise(portable: bool) {
    let source = include_str!("fixtures/app.py").replace(
        "    def do_GET(self):\n",
        "    def do_GET(self):\n        if self.path=='/headers':return self.send(200,{'content':self.headers.get('Content-Type'),'auth':self.headers.get('Authorization'),'key':self.headers.get('Idempotency-Key'),'tenant':self.headers.get('X-Test-Tenant')})\n",
    );
    let f = Fixture::new(Service::Api, None, &[("app.py", &source)]);
    let mut d = WebDispatcher::new(
        Repository::open(&f.root(), Limits::default(), vec![]).unwrap(),
        f.store.connect().unwrap(),
        f.catalog.clone(),
        f.policy.clone(),
        f.registration.clone(),
        limits(),
    )
    .unwrap();
    if portable {
        d = d.with_portable_model_wire().unwrap();
    }
    let no = Arc::new(AtomicBool::new(false));
    let start = d
        .execute(
            call(portable, &Value::Null, json!({"command":"start"})),
            f.ctx.clone(),
            no.clone(),
        )
        .await
        .unwrap();
    assert!(start.success);
    let handle = serde_json::from_str::<Value>(&start.content).unwrap()["session_id"].clone();
    let request = |headers: Value| json!({"session":"headers","method":"get","path":"/headers","headers":headers,"body":null,"timeout_ms":5000});
    let mixed = request(
        json!({"Content-Type":"application/json; charset=UTF-8","AUTHORIZATION":"Bearer Alice_CaseSensitive","Idempotency-Key":"Keep_Case-9","X-Test-Tenant":"Tenant_A"}),
    );
    let operation = json!({"command":"api","request":mixed});
    let observed = d.validate_call(&call(portable, &handle, operation.clone()));
    // Close even on the expected before-fix failure; no orphan test process.
    if portable && let Err(error) = &observed {
        d.execute(
            call(portable, &handle, json!({"command":"close"})),
            f.ctx.clone(),
            no.clone(),
        )
        .await
        .unwrap();
        panic!("Portable API header validation failed: {error}");
    }
    if portable {
        let mut second = mixed.clone();
        second["session"] = json!("headers_second");
        for op in [
            operation,
            json!({"command":"api_concurrent","requests":[mixed,second]}),
        ] {
            let r = d
                .execute(call(portable, &handle, op), f.ctx.clone(), no.clone())
                .await
                .unwrap();
            assert!(r.success && !r.requires_reconciliation, "{}", r.content);
            let v: Value = serde_json::from_str(&r.content).unwrap();
            let mut bodies = vec![];
            response_bodies(&v, &mut bodies);
            assert!(!bodies.is_empty(), "Native API response body missing: {v}");
            for body in bodies {
                assert_eq!(
                    body,
                    json!({"content":"application/json; charset=UTF-8","auth":"Bearer Alice_CaseSensitive","key":"Keep_Case-9","tenant":"Tenant_A"})
                );
            }
        }
    } else {
        assert_eq!(observed.unwrap_err().kind, ErrorKind::Authorization);
    }
    for headers in [
        json!({"Authorization":"Bearer alice","authorization":"Bearer bob"}),
        json!({"Host":"outside.example"}),
        json!({"X-Test-Tenant":"alice\r\nHost: outside.example"}),
        json!({"content-type":"application/json\u{0000}"}),
        json!({"proxy-authorization":"Bearer denied"}),
    ] {
        for op in [
            json!({"command":"api","request":request(headers.clone())}),
            json!({"command":"api_concurrent","requests":[request(json!({})),request(headers.clone())]}),
        ] {
            assert_eq!(
                d.validate_call(&call(portable, &handle, op))
                    .unwrap_err()
                    .kind,
                ErrorKind::Authorization
            );
        }
    }
    let lower =
        json!({"command":"api","request":request(json!({"authorization":"Bearer ExactCase"}))});
    assert!(
        d.execute(call(portable, &handle, lower), f.ctx.clone(), no.clone())
            .await
            .unwrap()
            .success
    );
    let closed = d
        .execute(
            call(portable, &handle, json!({"command":"close"})),
            f.ctx.clone(),
            no,
        )
        .await
        .unwrap();
    assert!(closed.success && !closed.requires_reconciliation);
    assert_eq!(
        f.store.journal().attempt(&f.ctx.attempt_id).unwrap().state,
        melissa_local_store::journal::AttemptState::Succeeded
    );
}

#[tokio::test]
async fn portable_api_header_names_follow_http_case_rules_without_relaxing_guards() {
    exercise(true).await;
}

#[tokio::test]
async fn legacy_typed_api_header_validation_remains_strict() {
    exercise(false).await;
}
