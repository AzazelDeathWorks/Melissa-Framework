use melissa_model_gateway::{
    transport::{ApiKey, EndpointTrust, HttpTransport, PromptCache, ProviderEndpoint},
    *,
};
use serde_json::json;
fn endpoint(p: Protocol) -> ProviderEndpoint {
    ProviderEndpoint::new(
        "fixture".into(),
        "workspace:fixture".into(),
        "auth:fixture".into(),
        p,
        EndpointTrust::LoopbackFixture,
        "http://127.0.0.1:9/test",
        ApiKey::new("not-a-credential".into()).unwrap(),
    )
    .unwrap()
}
fn request() -> TurnRequest {
    TurnRequest {
        model: "fixture".into(),
        system_prompt: "stable specialist instructions".into(),
        messages: vec![Message::User {
            text: "fix the code".into(),
        }],
        tools: ["z_tool", "a_tool"]
            .into_iter()
            .map(|name| ToolDefinition {
                name: name.into(),
                description: "read data".into(),
                parameters: json!({"type":"object","properties":{},"additionalProperties":false}),
                strict: false,
            })
            .collect(),
        max_output_tokens: 393216,
        output_schema: None,
    }
}
#[test]
fn unknown_cache_stays_disabled_and_tool_order_is_stable_without_reducing_capacity() {
    let r = request();
    let e = endpoint(Protocol::OpenAiResponses);
    let a = HttpTransport::body(&e, &r).unwrap();
    let mut b = r.clone();
    b.tools.reverse();
    assert_eq!(a, HttpTransport::body(&e, &b).unwrap());
    assert!(a.get("prompt_cache_key").is_none());
    assert_eq!(a["max_output_tokens"], 393216);
    assert_eq!(a["tools"][0]["name"], "a_tool");
}
#[test]
fn explicit_provider_cache_hints_bind_static_prefix_and_never_claim_hits() {
    let r = request();
    let e = endpoint(Protocol::OpenAiResponses)
        .with_prompt_cache(PromptCache::OpenaiMemory)
        .unwrap();
    let a = HttpTransport::body(&e, &r).unwrap();
    assert_eq!(a["prompt_cache_key"].as_str().unwrap().len(), 64);
    let mut b = r.clone();
    b.messages.push(Message::User {
        text: "continue".into(),
    });
    assert_eq!(
        a["prompt_cache_key"],
        HttpTransport::body(&e, &b).unwrap()["prompt_cache_key"]
    );
    b.system_prompt.push_str(" changed");
    assert_ne!(
        a["prompt_cache_key"],
        HttpTransport::body(&e, &b).unwrap()["prompt_cache_key"]
    );
    assert_eq!(a["prompt_cache_retention"], "in-memory");
    assert!(
        endpoint(Protocol::AnthropicMessages)
            .with_prompt_cache(PromptCache::OpenaiMemory)
            .is_err()
    );
    assert!(
        endpoint(Protocol::OpenAiResponses)
            .with_prompt_cache(PromptCache::AnthropicEphemeral)
            .is_err()
    );
    let claude = endpoint(Protocol::AnthropicMessages)
        .with_prompt_cache(PromptCache::AnthropicEphemeral)
        .unwrap();
    let a = HttpTransport::body(&claude, &r).unwrap();
    assert_eq!(a["system"][0]["cache_control"], json!({"type":"ephemeral"}));
    assert_eq!(a["system"][0]["text"], r.system_prompt);
    assert_eq!(a["max_tokens"], 393216);
    assert_eq!(
        a["messages"],
        HttpTransport::body(&endpoint(Protocol::AnthropicMessages), &r).unwrap()["messages"]
    );
}
