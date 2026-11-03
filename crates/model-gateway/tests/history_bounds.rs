use melissa_model_gateway::{Message, RequestLimits, TurnRequest};

#[test]
fn message_count_covers_long_runs_while_serialized_bytes_remain_bounded() {
    let mut request = TurnRequest {
        model: "fixture".into(),
        system_prompt: "host".into(),
        messages: vec![
            Message::User {
                text: "turn".into()
            };
            257
        ],
        tools: vec![],
        max_output_tokens: 128,
        output_schema: None,
    };
    assert!(request.validate().is_ok());
    assert!(
        request
            .validate_with_limits(RequestLimits {
                max_history_bytes: 10
            })
            .is_err()
    );
    request.messages.resize(
        RequestLimits::MAX_MESSAGES + 1,
        Message::User {
            text: "turn".into(),
        },
    );
    assert!(request.validate().is_err());
}
