//! Optional presentation-only public text observation. Reasoning, tool
//! arguments, provider headers and credentials never enter this channel.
use crate::{Protocol, SseEvent};
use serde::{Deserialize, Serialize};
use std::{
    future::Future,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VisibleDelta {
    pub stream_id: u64,
    pub text: String,
}
pub type Observer = Arc<dyn Fn(VisibleDelta) + Send + Sync>;
#[derive(Clone, Debug)]
pub struct ToolEvent {
    pub id: String,
    pub name: String,
    pub state: String,
}
pub type ToolObserver = Arc<dyn Fn(ToolEvent) + Send + Sync>;
tokio::task_local! {static TOOL_OBSERVER:ToolObserver;}
pub async fn observe_tools<F: Future>(observer: ToolObserver, future: F) -> F::Output {
    TOOL_OBSERVER.scope(observer, future).await
}
pub(crate) fn publish_tool(id: &str, name: &str, state: &str) {
    let _ = TOOL_OBSERVER.try_with(|observer| {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            observer(ToolEvent {
                id: id.into(),
                name: name.into(),
                state: state.into(),
            })
        }));
    });
}
tokio::task_local! {static OBSERVER:Observer;}
static NEXT: AtomicU64 = AtomicU64::new(1);
pub(crate) fn next_stream() -> u64 {
    NEXT.fetch_add(1, Ordering::Relaxed)
}
pub async fn observe<F: Future>(observer: Observer, future: F) -> F::Output {
    OBSERVER.scope(observer, future).await
}
pub(crate) fn publish(stream_id: u64, protocol: Protocol, event: &SseEvent) {
    if OBSERVER.try_with(|_| ()).is_err() {
        return;
    }
    if let Some(text) = public_text(protocol, event) {
        let _ = OBSERVER.try_with(|observer| {
            // A failed view must not interrupt an admitted model operation.
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                observer(VisibleDelta { stream_id, text })
            }));
        });
    }
}
fn public_text(protocol: Protocol, event: &SseEvent) -> Option<String> {
    let v = crate::strict_json(&event.data).ok()?;
    let value = match protocol {
        Protocol::OpenAiChatCompletions => v["choices"][0]["delta"]["content"].as_str(),
        Protocol::OpenAiResponses if v["type"] == "response.output_text.delta" => {
            v["delta"].as_str()
        }
        Protocol::AnthropicMessages
            if v["type"] == "content_block_delta" && v["delta"]["type"] == "text_delta" =>
        {
            v["delta"]["text"].as_str()
        }
        _ => None,
    }?;
    Some(value.to_owned())
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn event(v: serde_json::Value) -> SseEvent {
        SseEvent {
            event: None,
            data: v.to_string(),
        }
    }
    #[test]
    fn only_user_visible_text_is_admitted() {
        assert_eq!(
            public_text(
                Protocol::OpenAiResponses,
                &event(json!({"type":"response.output_text.delta","delta":"Türkçe\n🦀"}))
            ),
            Some("Türkçe\n🦀".into())
        );
        for kind in [
            "response.reasoning_text.delta",
            "response.reasoning_summary_text.delta",
            "response.function_call_arguments.delta",
        ] {
            assert!(
                public_text(
                    Protocol::OpenAiResponses,
                    &event(json!({"type":kind,"delta":"private"}))
                )
                .is_none()
            );
        }
        assert_eq!(
            public_text(
                Protocol::AnthropicMessages,
                &event(
                    json!({"type":"content_block_delta","delta":{"type":"text_delta","text":"hello"}})
                )
            ),
            Some("hello".into())
        );
        assert!(public_text(Protocol::AnthropicMessages,&event(json!({"type":"content_block_delta","delta":{"type":"thinking_delta","thinking":"private"}}))).is_none());
    }
    #[tokio::test]
    async fn observation_is_task_scoped_and_does_not_escape_to_other_runs() {
        let captured = Arc::new(std::sync::Mutex::new(vec![]));
        let copy = captured.clone();
        let observer: Observer = Arc::new(move |delta| copy.lock().unwrap().push(delta));
        let e = event(json!({"type":"response.output_text.delta","delta":"visible"}));
        observe(observer, async {
            publish(7, Protocol::OpenAiResponses, &e);
        })
        .await;
        publish(8, Protocol::OpenAiResponses, &e);
        let c = captured.lock().unwrap();
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].stream_id, 7);
    }
}
