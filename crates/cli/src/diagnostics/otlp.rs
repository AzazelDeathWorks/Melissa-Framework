//! Explicit OTLP/HTTP JSON export from authenticated journal times, not replay.
use crate::{
    Error, Result,
    app::{App, bytes, now},
    digest,
};
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::Path, time::Duration};
fn hex(id: &str, length: usize) -> String {
    blake3::hash(id.as_bytes()).to_hex()[..length].to_owned()
}
fn attribute(key: &str, value: Value) -> Value {
    let value = if let Some(n) = value.as_u64() {
        json!({"intValue":n.to_string()})
    } else if let Some(b) = value.as_bool() {
        json!({"boolValue":b})
    } else {
        json!({"stringValue":value.as_str().unwrap_or("unknown")})
    };
    json!({"key":key,"value":value})
}
#[allow(clippy::too_many_arguments)]
fn span(
    task: &str,
    key: &str,
    parent: Option<&str>,
    name: &str,
    start: u64,
    end: u64,
    failed: bool,
    attributes: Vec<Value>,
) -> Result<Value> {
    if end < start {
        return Err(Error::Store);
    }
    let mut value = json!({"traceId":hex(task,32),"spanId":hex(&format!("{task}:{key}"),16),"name":name,"kind":1,"startTimeUnixNano":start.checked_mul(1_000_000).ok_or(Error::Store)?.to_string(),"endTimeUnixNano":end.checked_mul(1_000_000).ok_or(Error::Store)?.to_string(),"attributes":attributes,"status":{"code":if failed{2}else{0}}});
    if let Some(parent) = parent {
        value["parentSpanId"] = json!(hex(&format!("{task}:{parent}"), 16));
    }
    Ok(value)
}
impl App {
    pub fn otlp(&self, id: &str) -> Result<Value> {
        let store = self.store(id)?;
        let events = store.journal().events(id)?;
        let first = events.first().ok_or(Error::Store)?;
        let last = events.last().ok_or(Error::Store)?;
        let task = store.journal().task(id)?;
        let mut spans = vec![span(
            id,
            "journal",
            None,
            "melissa.task.journal_window",
            first.created_at_ms,
            last.created_at_ms,
            task.record.state == melissa_core_contracts::TaskState::Failed,
            vec![
                attribute("melissa.task_ref", json!(digest(id.as_bytes()))),
                attribute("melissa.task_version", json!(task.record.version)),
                attribute("melissa.evidence_snapshot", json!(true)),
            ],
        )?];
        let mut starts = BTreeMap::new();
        for event in &events {
            if let Some(attempt) = &event.attempt {
                use melissa_local_store::journal::AttemptState;
                if attempt.state == AttemptState::Dispatched {
                    starts.insert(attempt.attempt_id.clone(), event.created_at_ms);
                }
                if matches!(
                    attempt.state,
                    AttemptState::Succeeded | AttemptState::Failed | AttemptState::Cancelled
                ) && let Some(start) = starts.remove(&attempt.attempt_id)
                {
                    spans.push(span(
                        id,
                        &attempt.attempt_id,
                        Some("journal"),
                        "melissa.tool.attempt",
                        start,
                        event.created_at_ms,
                        attempt.state != AttemptState::Succeeded,
                        vec![
                            attribute(
                                "melissa.invocation_ref",
                                json!(digest(attempt.invocation_id.as_bytes())),
                            ),
                            attribute(
                                "melissa.tool_ref",
                                json!(digest(
                                    event
                                        .invocation
                                        .as_ref()
                                        .map(|i| i.tool_id.as_bytes())
                                        .unwrap_or(b"unknown")
                                )),
                            ),
                        ],
                    )?);
                }
            }
        }
        let mut pending = 0;
        for request in store.journal().model_requests(id)? {
            if let Some(start) = request.dispatched_at_unix_ms {
                if let Some(settlement) = request.settlement.filter(|s| {
                    !matches!(
                        s.state,
                        melissa_local_store::journal::ModelRequestState::OutcomeUnknown
                    )
                }) {
                    spans.push(span(
                        id,
                        &request.reservation.request_id,
                        Some("journal"),
                        "melissa.model.request",
                        start,
                        settlement.completed_at_unix_ms,
                        !matches!(
                            settlement.state,
                            melissa_local_store::journal::ModelRequestState::Succeeded
                        ),
                        vec![
                            attribute(
                                "melissa.request_ref",
                                json!(digest(request.reservation.request_id.as_bytes())),
                            ),
                            attribute(
                                "melissa.usage_known",
                                json!(
                                    settlement.usage.input_tokens.is_some()
                                        && settlement.usage.output_tokens.is_some()
                                ),
                            ),
                        ],
                    )?);
                } else {
                    pending += 1;
                }
            }
        }
        spans[0]["attributes"]
            .as_array_mut()
            .ok_or(Error::Store)?
            .push(attribute(
                "melissa.pending_model_spans_not_closed",
                json!(pending),
            ));
        let result = json!({"resourceSpans":[{"resource":{"attributes":[attribute("service.name",json!("melissa-cli")),attribute("service.version",json!(env!("CARGO_PKG_VERSION")))]},"scopeSpans":[{"scope":{"name":"melissa.journal","version":"1.0.0"},"spans":spans}]}]});
        if bytes(&result)?.len() > 32 * 1024 * 1024 {
            return Err(Error::Unavailable(
                "OTLP görünümü büyük; eksik trace tam diye aktarılmadı.",
            ));
        }
        Ok(result)
    }
    pub async fn export_otlp(
        &self,
        id: &str,
        out: Option<&Path>,
        endpoint: Option<&str>,
    ) -> Result<Value> {
        let body = self.otlp(id)?;
        let raw = bytes(&body)?;
        let mut exported = None;
        if let Some(out) = out {
            let (out, _parent) = crate::maintenance::destination(Some(self), Some(id), out)?;
            crate::maintenance::write_new(&out, &raw)?;
            exported = Some(out);
        }
        let mut status = "PASS";
        let mut delivery = json!({"status":"NOT_RUN","reason":"No collector endpoint selected"});
        if let Some(endpoint) = endpoint {
            let url =
                url::Url::parse(endpoint).map_err(|_| Error::Usage("OTLP adresi geçersiz."))?;
            let local = match url.host() {
                Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
                Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
                _ => false,
            };
            if !local
                || url.scheme() != "http"
                || url.path() != "/v1/traces"
                || !url.username().is_empty()
                || url.password().is_some()
                || url.query().is_some()
                || url.fragment().is_some()
            {
                return Err(Error::Denied(
                    "OTLP için açık loopback IP ve /v1/traces gerekli; kimlik/proxy/redirect kullanılmaz.",
                ));
            }
            let client = reqwest::Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(10))
                .build()
                .map_err(|_| Error::Io)?;
            delivery = match client
                .post(url)
                .header("Content-Type", "application/json")
                .body(raw.clone())
                .send()
                .await
            {
                Ok(mut response) => {
                    let code = response.status().as_u16();
                    let mut response_body = vec![];
                    let mut valid = true;
                    loop {
                        match response.chunk().await {
                            Ok(Some(chunk)) if response_body.len() + chunk.len() <= 65536 => {
                                response_body.extend_from_slice(&chunk)
                            }
                            Ok(None) => break,
                            _ => {
                                valid = false;
                                break;
                            }
                        }
                    }
                    let value = if valid {
                        melissa_model_gateway::strict_json(
                            std::str::from_utf8(&response_body).unwrap_or(""),
                        )
                        .ok()
                    } else {
                        None
                    };
                    let partial = value.as_ref().and_then(|v| v.get("partialSuccess"));
                    let rejected = partial
                        .and_then(|v| v.get("rejectedSpans"))
                        .map(|v| {
                            v.as_u64()
                                .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
                        })
                        .unwrap_or(Some(0));
                    let shape = value.as_ref().is_some_and(|v| v.is_object())
                        && partial.is_none_or(|p| {
                            p.is_object() && p.get("errorMessage").is_none_or(Value::is_string)
                        });
                    status = if code != 200 {
                        "FAIL"
                    } else if !shape || rejected.is_none() {
                        "INCONCLUSIVE"
                    } else if rejected.is_some_and(|n| n > 0) {
                        "FAIL"
                    } else {
                        "PASS"
                    };
                    json!({"status":status,"http_status":code,"rejected_spans":rejected,"warning":partial.and_then(|p|p["errorMessage"].as_str()).is_some_and(|s|!s.is_empty()),"automatic_retry":false,"response_text":"omitted"})
                }
                Err(_) => {
                    status = "INCONCLUSIVE";
                    json!({"status":status,"reason":"collector transport not settled; no automatic retry","automatic_retry":false})
                }
            };
        }
        Ok(
            json!({"command":"observe otlp","status":status,"out":exported,"delivery":delivery,"captured_at_ms":now()?,"body":if out.is_none()&&endpoint.is_none(){Some(body)}else{None},"provider_model_calls":0,"redacted":true}),
        )
    }
}
