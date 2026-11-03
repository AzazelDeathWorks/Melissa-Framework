#[path = "../../web-development/tests/support/mod.rs"]
mod support;
use melissa_model_gateway::{ToolCall, tools::ToolDispatcher};
use melissa_recovery::reconcile::inspect_start_only_web_effect;
use melissa_repository::{Limits, Repository};
use melissa_web_development::{
    plan::Database,
    protocol::{ApiRequest, Command, Method, Service},
    tools::WebDispatcher,
};
use serde_json::{Value, json};
use support::*;

#[test]
fn interrupted_startup_uses_native_exit_evidence_and_application_commands_remain_unknown() {
    for application_command in [false, true] {
        let mut f = Fixture::new(Service::Api, Some(Database::Sqlite), &[]);
        let mut session = f.start();
        if application_command {
            assert!(
                session
                    .command(Command::Api {
                        request: ApiRequest {
                            session: "alice".into(),
                            method: Method::Get,
                            path: "/api/notes".into(),
                            headers: Default::default(),
                            body: None,
                            timeout_ms: 3000
                        }
                    })
                    .unwrap()
                    .ok
            );
        }
        drop(session);
        let result =
            inspect_start_only_web_effect(&mut f.store, &f.ctx.task_id, &f.ctx.invocation_id);
        if application_command {
            assert!(result.is_err());
            assert!(
                f.store
                    .journal()
                    .task(&f.ctx.task_id)
                    .unwrap()
                    .record
                    .unknown_invocation_id
                    .is_some()
            );
        } else if let Ok(proof) = result {
            assert_eq!(proof["proof"]["application_commands_dispatched"], 0);
            assert_eq!(proof["automatic_replay_allowed"], false);
            assert_eq!(proof["proof"]["task_accepted"], false);
            assert!(
                f.store
                    .journal()
                    .task(&f.ctx.task_id)
                    .unwrap()
                    .record
                    .unknown_invocation_id
                    .is_none()
            );
            assert!(
                inspect_start_only_web_effect(&mut f.store, &f.ctx.task_id, &f.ctx.invocation_id)
                    .is_err()
            );
        } else {
            // Drop races EOF cleanup against the native cancellation kill.
            // The existing proof requires Cancelled AND exit0. EOF can win
            // with Exited/0, or cancellation can kill with Cancelled/137.
            // Preserve Unknown in both ineligible cases; do not weaken policy.
            let raw = f
                .store
                .get_artifact(&f.ctx.task_id, &format!("{}:web-final", f.ctx.attempt_id))
                .unwrap();
            let outcome: Value = serde_json::from_slice(&raw).unwrap();
            assert!(
                (outcome["worker"]["termination"] == "exited"
                    && outcome["worker"]["exit_code"] == 0)
                    || (outcome["worker"]["termination"] == "cancelled"
                        && outcome["worker"]["exit_code"] == 137)
            );
            assert_eq!(outcome["worker"]["cleanup_confirmed"], true);
            assert_eq!(outcome["worker"]["tree_confirmed_stopped"], true);
            assert_eq!(
                result.unwrap_err().to_string(),
                "Store: native cancelled startup/cleanup not proven"
            );
            assert!(
                f.store
                    .journal()
                    .task(&f.ctx.task_id)
                    .unwrap()
                    .record
                    .unknown_invocation_id
                    .is_some()
            );
            println!(
                "Native ineligible termination={} exit={} retained as Unknown; no replay or reconciliation admitted",
                outcome["worker"]["termination"], outcome["worker"]["exit_code"]
            );
        }
    }
}

#[tokio::test]
async fn rejected_api_cookie_jar_handle_can_close_without_inventing_unknown_effect() {
    let f = Fixture::new(Service::Api, Some(Database::Sqlite), &[]);
    let mut d = WebDispatcher::new(
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
    let call = |session: Value, op: Value| ToolCall {
        id: "call:owned-start".into(),
        name: "web_session".into(),
        arguments: json!({"session_id":session,"operation_json":op.to_string()}).to_string(),
    };
    let r = d
        .execute(
            call(Value::Null, json!({"command":"start"})),
            f.ctx.clone(),
            f.cancelled.clone(),
        )
        .await
        .unwrap();
    let id = serde_json::from_str::<Value>(&r.content).unwrap()["session_id"].clone();
    let api = |jar: Value| json!({"command":"api","request":{"session":jar,"method":"get","path":"/api/notes","headers":{},"body":null,"timeout_ms":3000}});
    assert!(d.validate_call(&call(id.clone(), api(id.clone()))).is_err());
    assert!(
        d.validate_call(&call(id.clone(), api(json!("alice"))))
            .is_ok()
    );
    let out = d.finish_owned().unwrap().unwrap();
    assert!(out.success && out.cleanup_confirmed && !out.requires_reconciliation);
    assert!(d.finish_owned().unwrap().is_none());
    assert!(
        f.store
            .journal()
            .task(&f.ctx.task_id)
            .unwrap()
            .record
            .unknown_invocation_id
            .is_none()
    );
}
