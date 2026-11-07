//! Chat Completions compatibility. Partial calls never become executable.
use crate::{
    DecoderLimits, GatewayError, Message, ModelResponse, Protocol, RequestLimits, Result, SseEvent,
    StopReason, ToolCall, TurnRequest, Usage,
};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

pub fn build_request(request: &TurnRequest, limits: RequestLimits) -> Result<Value> {
    request.validate_with_limits(limits)?;
    let mut messages = vec![json!({"role":"system","content":request.system_prompt})];
    let mut pending = BTreeSet::new();
    for message in &request.messages {
        match message {
            Message::User { text } => {
                if !pending.is_empty() {
                    return Err(GatewayError::invalid("unresolved chat tools"));
                }
                messages.push(json!({"role":"user","content":text}));
            }
            Message::UserImages { text, images } => {
                if !pending.is_empty() {
                    return Err(GatewayError::invalid("unresolved chat tools"));
                }
                let mut content = vec![json!({"type":"text","text":text})];
                content.extend(images.iter().map(|i|json!({"type":"image_url","image_url":{"url":i.data_url(),"detail":"high"}})));
                messages.push(json!({"role":"user","content":content}));
            }
            Message::Assistant { response } => {
                if !pending.is_empty() || response.requested_model != request.model {
                    return Err(GatewayError::protocol("chat continuation identity"));
                }
                validate_continuation(response)?;
                for call in &response.calls {
                    if !pending.insert(call.id.clone()) {
                        return Err(GatewayError::protocol("duplicate chat call"));
                    }
                }
                messages.push(response.continuation[0].clone());
            }
            Message::ToolResult {
                call_id,
                content,
                is_error,
            } => {
                if !pending.remove(call_id) {
                    return Err(GatewayError::invalid("unmatched chat tool result"));
                }
                messages.push(json!({"role":"tool","tool_call_id":call_id,"content":if *is_error {json!({"is_error":true,"content":content}).to_string()} else {content.clone()}}));
            }
            Message::ToolImages { .. } => {
                return Err(GatewayError::unsupported(
                    "chat tool-result images require a provider-specific capability",
                ));
            }
        }
    }
    if !pending.is_empty() {
        return Err(GatewayError::invalid("missing chat tool results"));
    }
    let tools=request.tools.iter().map(|t|json!({"type":"function","function":{"name":t.name,"description":t.description,"parameters":t.parameters,"strict":t.strict}})).collect::<Vec<_>>();
    let mut body = json!({"model":request.model,"messages":messages,"stream":true,"stream_options":{"include_usage":true},"max_completion_tokens":request.max_output_tokens});
    if !tools.is_empty() {
        body["tools"] = json!(tools);
        body["parallel_tool_calls"] = json!(false);
    }
    if let Some(schema) = &request.output_schema {
        crate::schema::validate_schema(schema)?;
        body["response_format"] = json!({"type":"json_schema","json_schema":{"name":"melissa_output","strict":true,"schema":schema}});
    }
    Ok(body)
}

fn assistant(text: &str, calls: &[ToolCall], refusal: Option<&str>) -> Value {
    let mut message = json!({"role":"assistant","content":text});
    if !calls.is_empty() {
        message["tool_calls"]=json!(calls.iter().map(|c|json!({"id":c.id,"type":"function","function":{"name":c.name,"arguments":c.arguments}})).collect::<Vec<_>>());
    }
    if let Some(refusal) = refusal {
        message["refusal"] = json!(refusal);
    }
    message
}
pub(crate) fn validate_continuation(response: &ModelResponse) -> Result<()> {
    if response.protocol != Protocol::OpenAiChatCompletions
        || response.continuation
            != vec![assistant(
                &response.text,
                &response.calls,
                response.refusal.as_deref(),
            )]
    {
        return Err(GatewayError::protocol(
            "chat continuation differs from completed response",
        ));
    }
    if !matches!(
        response.stop,
        StopReason::EndTurn | StopReason::ToolCalls | StopReason::Refusal
    ) {
        return Err(GatewayError::protocol("incomplete chat history"));
    }
    Ok(())
}

#[derive(Default)]
struct PartialCall {
    id: String,
    name: String,
    arguments: String,
}
pub struct ChatDecoder {
    requested: String,
    id: Option<String>,
    model: Option<String>,
    text: String,
    refusal: String,
    calls: BTreeMap<u64, PartialCall>,
    finish: Option<String>,
    done: bool,
    usage: Usage,
    limits: DecoderLimits,
    bytes: usize,
}
impl ChatDecoder {
    pub fn new(requested: &str, limits: DecoderLimits) -> Self {
        Self {
            requested: requested.into(),
            id: None,
            model: None,
            text: String::new(),
            refusal: String::new(),
            calls: BTreeMap::new(),
            finish: None,
            done: false,
            usage: Usage::default(),
            limits,
            bytes: 0,
        }
    }
    pub fn push(&mut self, event: &SseEvent) -> Result<()> {
        self.limits.validate()?;
        if self.done {
            return Err(GatewayError::protocol("chat data after DONE"));
        }
        if event.data.trim() == "[DONE]" {
            if self.finish.is_none() {
                return Err(GatewayError::protocol("chat DONE before finish"));
            }
            self.done = true;
            return Ok(());
        }
        self.bytes = self
            .bytes
            .checked_add(event.data.len())
            .ok_or_else(|| GatewayError::protocol("chat bytes overflow"))?;
        if event.data.len() > self.limits.max_event_bytes
            || self.bytes > self.limits.max_total_bytes
        {
            return Err(GatewayError::protocol("chat envelope bounds"));
        }
        let v = crate::strict_json(&event.data)?;
        if v.get("error").is_some() {
            return Err(GatewayError::protocol("chat provider error"));
        }
        for (field, slot) in [("id", &mut self.id), ("model", &mut self.model)] {
            let value = v[field]
                .as_str()
                .filter(|s| !s.is_empty() && s.len() <= 256)
                .ok_or_else(|| GatewayError::protocol("chat identity missing"))?;
            if slot.as_ref().is_some_and(|prior| prior != value) {
                return Err(GatewayError::protocol("chat identity changed"));
            }
            *slot = Some(value.into());
        }
        if let Some(usage) = v.get("usage").filter(|u| !u.is_null()) {
            if self.usage.input_tokens.is_some() {
                return Err(GatewayError::protocol("duplicate chat usage"));
            }
            let input = usage["prompt_tokens"]
                .as_u64()
                .ok_or_else(|| GatewayError::protocol("chat input usage"))?;
            let output = usage["completion_tokens"]
                .as_u64()
                .ok_or_else(|| GatewayError::protocol("chat output usage"))?;
            if let Some(total) = usage["total_tokens"].as_u64()
                && input.checked_add(output) != Some(total)
            {
                return Err(GatewayError::protocol("chat usage total"));
            }
            self.usage = Usage {
                input_tokens: Some(input),
                output_tokens: Some(output),
                cached_input_tokens: usage["prompt_tokens_details"]["cached_tokens"].as_u64(),
                reasoning_output_tokens: usage["completion_tokens_details"]["reasoning_tokens"]
                    .as_u64(),
                ..Usage::default()
            };
            if self.usage.cached_input_tokens.is_some_and(|n| n > input)
                || self
                    .usage
                    .reasoning_output_tokens
                    .is_some_and(|n| n > output)
            {
                return Err(GatewayError::protocol("chat usage subsets"));
            }
        }
        let choices = v["choices"]
            .as_array()
            .ok_or_else(|| GatewayError::protocol("chat choices missing"))?;
        if choices.len() > 1 {
            return Err(GatewayError::unsupported("multiple chat choices"));
        }
        for choice in choices {
            if self.finish.is_some() || choice["index"].as_u64() != Some(0) {
                return Err(GatewayError::protocol(
                    "chat choice after finish or wrong index",
                ));
            }
            let d = &choice["delta"];
            if d.get("role").is_some_and(|r| r != "assistant") {
                return Err(GatewayError::protocol("chat role changed"));
            }
            if let Some(s) = d["content"].as_str() {
                self.text.push_str(s);
            }
            if let Some(s) = d["refusal"].as_str() {
                self.refusal.push_str(s);
            }
            if self.text.len().saturating_add(self.refusal.len()) > self.limits.max_output_bytes {
                return Err(GatewayError::protocol("chat text bounds"));
            }
            if let Some(parts) = d["tool_calls"].as_array() {
                for p in parts {
                    let index = p["index"]
                        .as_u64()
                        .ok_or_else(|| GatewayError::protocol("chat tool index"))?;
                    if index >= self.limits.max_items as u64 {
                        return Err(GatewayError::protocol("chat tool bounds"));
                    }
                    if p.get("type").is_some_and(|t| t != "function") {
                        return Err(GatewayError::unsupported("chat non-function tool"));
                    }
                    let c = self.calls.entry(index).or_default();
                    if let Some(id) = p["id"].as_str() {
                        if !c.id.is_empty() && c.id != id {
                            return Err(GatewayError::protocol("chat call id changed"));
                        }
                        c.id = id.into();
                    }
                    if let Some(s) = p["function"]["name"].as_str() {
                        c.name.push_str(s);
                    }
                    if let Some(s) = p["function"]["arguments"].as_str() {
                        c.arguments.push_str(s);
                    }
                    if c.id.len() > 256
                        || c.name.len() > 64
                        || c.arguments.len() > self.limits.max_argument_bytes
                    {
                        return Err(GatewayError::protocol("chat call bounds"));
                    }
                }
            }
            if let Some(reason) = choice["finish_reason"].as_str() {
                self.finish = Some(reason.into());
            }
        }
        Ok(())
    }
    pub fn finish(self) -> Result<ModelResponse> {
        if !self.done {
            return Err(GatewayError::protocol(
                "chat stream interrupted before DONE",
            ));
        }
        let stop = match self.finish.as_deref() {
            Some("stop") => StopReason::EndTurn,
            Some("tool_calls") => StopReason::ToolCalls,
            Some("length") => StopReason::MaxTokens,
            Some("content_filter") => StopReason::Refusal,
            _ => return Err(GatewayError::protocol("unknown chat stop reason")),
        };
        let mut calls = vec![];
        let mut ids = BTreeSet::new();
        if stop == StopReason::ToolCalls {
            for (expected, (index, c)) in self.calls.into_iter().enumerate() {
                if index != expected as u64
                    || c.id.is_empty()
                    || !ids.insert(c.id.clone())
                    || !crate::valid_tool_name(&c.name)
                    || !crate::strict_json(&c.arguments)?.is_object()
                {
                    return Err(GatewayError::protocol("invalid completed chat tool"));
                }
                calls.push(ToolCall {
                    id: c.id,
                    name: c.name,
                    arguments: c.arguments,
                });
            }
            if calls.is_empty() {
                return Err(GatewayError::protocol("chat tools finish without tools"));
            }
        } else if stop == StopReason::EndTurn && !self.calls.is_empty() {
            return Err(GatewayError::protocol("chat calls without tool finish"));
        }
        let refusal = (!self.refusal.is_empty()).then_some(self.refusal);
        if refusal.is_some() && !calls.is_empty() {
            return Err(GatewayError::protocol("chat refusal with tools"));
        }
        let stop = if refusal.is_some() {
            StopReason::Refusal
        } else {
            stop
        };
        let continuation = vec![assistant(&self.text, &calls, refusal.as_deref())];
        Ok(ModelResponse {
            protocol: Protocol::OpenAiChatCompletions,
            requested_model: self.requested,
            model: self
                .model
                .ok_or_else(|| GatewayError::protocol("missing chat model"))?,
            response_id: self
                .id
                .ok_or_else(|| GatewayError::protocol("missing chat id"))?,
            text: self.text,
            calls,
            stop,
            usage: self.usage,
            continuation,
            refusal,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn event(v: Value) -> SseEvent {
        SseEvent {
            event: None,
            data: v.to_string(),
        }
    }
    fn chunk(delta: Value, finish: Value) -> SseEvent {
        event(
            json!({"id":"c1","model":"test","choices":[{"index":0,"delta":delta,"finish_reason":finish}]}),
        )
    }
    fn done() -> SseEvent {
        SseEvent {
            event: None,
            data: "[DONE]".into(),
        }
    }
    #[test]
    fn split_tools_need_finish_and_done() {
        let mut d = ChatDecoder::new("test", DecoderLimits::default());
        d.push(&chunk(json!({"tool_calls":[{"index":0,"id":"t1","type":"function","function":{"name":"read","arguments":"{\"path\":"}}]}),Value::Null)).unwrap();
        d.push(&chunk(
            json!({"tool_calls":[{"index":0,"function":{"arguments":"\"x\"}"}}]}),
            json!("tool_calls"),
        ))
        .unwrap();
        d.push(&done()).unwrap();
        let r = d.finish().unwrap();
        assert_eq!(r.calls[0].arguments, "{\"path\":\"x\"}");
        validate_continuation(&r).unwrap();
    }
    #[test]
    fn interrupted_and_truncated_never_expose_calls() {
        let mut d = ChatDecoder::new("test", DecoderLimits::default());
        d.push(&chunk(json!({"content":"hello"}), json!("stop")))
            .unwrap();
        assert!(d.finish().is_err());
        let mut d = ChatDecoder::new("test", DecoderLimits::default());
        d.push(&chunk(json!({"tool_calls":[{"index":0,"id":"x","function":{"name":"write","arguments":"{"}}]}),json!("length"))).unwrap();
        d.push(&done()).unwrap();
        let r = d.finish().unwrap();
        assert!(r.calls.is_empty());
        assert_eq!(r.stop, StopReason::MaxTokens);
    }
    #[test]
    fn usage_missing_is_unknown_and_duplicate_rejected() {
        let mut d = ChatDecoder::new("test", DecoderLimits::default());
        d.push(&chunk(json!({"content":"ok"}), json!("stop")))
            .unwrap();
        d.push(&done()).unwrap();
        assert_eq!(d.finish().unwrap().usage.input_tokens, None);
        let mut d = ChatDecoder::new("test", DecoderLimits::default());
        let u = event(
            json!({"id":"c1","model":"test","choices":[],"usage":{"prompt_tokens":3,"completion_tokens":2,"total_tokens":5}}),
        );
        d.push(&u).unwrap();
        assert!(d.push(&u).is_err());
    }
}
