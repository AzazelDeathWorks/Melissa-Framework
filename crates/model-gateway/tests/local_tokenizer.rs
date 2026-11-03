use melissa_model_gateway::{
    Protocol,
    local_tokenizer::*,
    transport::{ApiKey, EndpointTrust, ProviderEndpoint},
};
use serde_json::json;
fn fixture(dir: &std::path::Path) -> LocalTokenizerConfig {
    let bytes = include_bytes!("fixtures/local-tokenizer.json");
    let path = dir.join("tokenizer.json");
    std::fs::write(&path, bytes).unwrap();
    LocalTokenizerConfig {
        path,
        blake3: blake3::hash(bytes).to_hex().to_string(),
    }
}
fn endpoint() -> ProviderEndpoint {
    ProviderEndpoint::new(
        "fixture".into(),
        "provider:fixture".into(),
        "authorization:fixture".into(),
        Protocol::OpenAiResponses,
        EndpointTrust::LoopbackFixture,
        "http://127.0.0.1:1/unused",
        ApiKey::new("fixture-placeholder".into()).unwrap(),
    )
    .unwrap()
}
#[test]
fn real_tokenization_disables_artifact_padding_and_truncation() {
    let dir = tempfile::tempdir().unwrap();
    let config = fixture(dir.path());
    let ep = endpoint();
    let tokenizer =
        LocalWireTokenizer::load(&config, ep.url.as_str(), ep.protocol, "fixture").unwrap();
    // JSON quotes remain part of the first and last words. Four whitespace
    // pieces are expected; artifact truncation to 2 and padding to 128 are off.
    let result = tokenizer
        .measure(&ep, "fixture", &json!("hello world again extra"))
        .unwrap()
        .unwrap();
    assert_eq!(result.token_count, 4);
    assert_eq!(result.artifact_digest, format!("blake3:{}", config.blake3));
    assert!(result.method.contains("not provider input tokens"));
}
#[test]
fn artifact_is_authenticated_and_binding_is_exact() {
    let dir = tempfile::tempdir().unwrap();
    let config = fixture(dir.path());
    let ep = endpoint();
    let tokenizer =
        LocalWireTokenizer::load(&config, ep.url.as_str(), ep.protocol, "fixture").unwrap();
    assert!(
        tokenizer
            .measure(&ep, "other-model", &json!({}))
            .unwrap()
            .is_none()
    );
    let mut other = ep.clone();
    other.url.set_path("/other");
    assert!(
        tokenizer
            .measure(&other, "fixture", &json!({}))
            .unwrap()
            .is_none()
    );
    other = ep.clone();
    other.protocol = Protocol::AnthropicMessages;
    assert!(
        tokenizer
            .measure(&other, "fixture", &json!({}))
            .unwrap()
            .is_none()
    );
    let rebound =
        LocalWireTokenizer::load(&config, ep.url.as_str(), ep.protocol, "other-model").unwrap();
    assert_ne!(tokenizer.policy_id(), rebound.policy_id());
    std::fs::write(&config.path, b"{}").unwrap();
    assert!(LocalWireTokenizer::load(&config, ep.url.as_str(), ep.protocol, "fixture").is_err());
    // The verified in-memory artifact remains stable across file replacement.
    assert_eq!(
        tokenizer
            .measure(&ep, "fixture", &json!("a b c"))
            .unwrap()
            .unwrap()
            .token_count,
        3
    );
}
#[test]
fn over_limit_wire_is_not_truncated_into_a_fake_measurement() {
    let dir = tempfile::tempdir().unwrap();
    let config = fixture(dir.path());
    let ep = endpoint();
    let tokenizer =
        LocalWireTokenizer::load(&config, ep.url.as_str(), ep.protocol, "fixture").unwrap();
    assert!(
        tokenizer
            .measure(&ep, "fixture", &json!("x".repeat(2 * 1024 * 1024)))
            .unwrap()
            .is_none()
    );
}
#[test]
fn invalid_or_oversized_artifacts_fail_closed() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = fixture(dir.path());
    config.blake3 = "A".repeat(64);
    assert!(config.validate().is_err());
    config.blake3 = "0".repeat(64);
    let file = std::fs::File::create(&config.path).unwrap();
    file.set_len(32 * 1024 * 1024 + 1).unwrap();
    drop(file);
    assert!(
        LocalWireTokenizer::load(
            &config,
            endpoint().url.as_str(),
            Protocol::OpenAiResponses,
            "fixture"
        )
        .is_err()
    );
    std::fs::write(&config.path, b"{}").unwrap();
    config.blake3 = blake3::hash(b"{}").to_hex().to_string();
    assert!(
        LocalWireTokenizer::load(
            &config,
            endpoint().url.as_str(),
            Protocol::OpenAiResponses,
            "fixture"
        )
        .is_err()
    );
}
