//! Read-only Gateway routes. Host task/role/run/expiry are not wire fields.
use crate::{
    Error, Result, bounded,
    profiles::ReviewProfiles,
    review::{AnchorRequest, ReviewSession, Side, Submission},
};
use melissa_core_contracts::TaskState;
use melissa_local_store::{ArtifactRequest, LocalStore};
use melissa_model_gateway::{
    ErrorKind, GatewayError, ToolCall, ToolDefinition, strict_json,
    tools::{BoundTool, ToolContext, ToolDispatcher, ToolExecution},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

/// Trusted Rust configuration, intentionally not deserializable.
pub struct ReviewConfig {
    pub task_id: String,
    pub profile_id: String,
    pub run_id: String,
    pub resource_id: String,
    pub authorization_ref: String,
    pub expires_at_ms: u64,
}
struct Host {
    session: ReviewSession,
    store: LocalStore,
    profiles: ReviewProfiles,
    config: ReviewConfig,
    revoked: bool,
}
#[derive(Clone)]
pub struct ReviewDispatcher {
    host: Arc<Mutex<Host>>,
}
const ROUTES: &[(&str, &str, &str)] = &[
    (
        "review_scope",
        "fs.read",
        "Inspect this frozen baseline/final source and changed paths; this is not an atomic filesystem snapshot.",
    ),
    (
        "review_read",
        "fs.read",
        "Read a source-bound UTF-8 page with registered literal secrets redacted. Offsets refer to original bytes.",
    ),
    (
        "review_anchor",
        "fs.read",
        "Get exact before/after byte-span digest and actual line numbers without echoing source text.",
    ),
    (
        "review_submit",
        "evidence.attach",
        "Persist located candidate findings and limitations. This validates locations, not defect truth or task/release acceptance.",
    ),
];
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Read {
    side: Side,
    path: String,
    offset: usize,
    max_bytes: usize,
}
enum Input {
    Scope,
    Read(Read),
    Anchor(AnchorRequest),
    Submit(Submission),
}
fn parse(c: &ToolCall) -> melissa_model_gateway::Result<Input> {
    if !bounded(&c.id, 256) || c.arguments.len() > 2 * 1024 * 1024 {
        return Err(GatewayError::invalid("review argument bounds"));
    }
    let v = strict_json(&c.arguments)?;
    let result = match c.name.as_str() {
        "review_scope" => serde_json::from_value::<Empty>(v).map(|_| Input::Scope),
        "review_read" => serde_json::from_value(v).map(Input::Read),
        "review_anchor" => serde_json::from_value(v).map(Input::Anchor),
        "review_submit" => serde_json::from_value(v).map(Input::Submit),
        _ => return Err(GatewayError::unsupported("unbound review function")),
    };
    result.map_err(|_| GatewayError::invalid("exact review argument fields required"))
}
fn map(e: Error) -> GatewayError {
    let kind = match e {
        Error::Invalid(_) | Error::Json(_) => ErrorKind::Invalid,
        Error::Denied(_) | Error::Stale(_) => ErrorKind::Authorization,
        Error::Cancelled => ErrorKind::Cancelled,
        Error::Store(_) | Error::AfterAppend => ErrorKind::Store,
        _ => ErrorKind::Tool,
    };
    GatewayError::new(kind, e.to_string())
}
fn schema(wire: &str) -> Value {
    let side = json!({"type":"string","enum":["before","after"]});
    let path = json!({"type":"string","minLength":1,"maxLength":2048});
    let integer = json!({"type":"integer","minimum":0});
    let text = json!({"type":"string","minLength":1,"maxLength":8192});
    let hash = json!({"type":"string","pattern":"^blake3:[a-f0-9]{64}$"});
    let anchor = json!({"type":"object","additionalProperties":false,
        "properties":{"side":side,"path":path,"start":integer,"end":integer,
          "file_digest":hash,"span_digest":hash,"start_line":{"type":"integer","minimum":1},"end_line":{"type":"integer","minimum":1}},
        "required":["side","path","start","end","file_digest","span_digest","start_line","end_line"]});
    let finding = json!({"type":"object","additionalProperties":false,"properties":{
        "id":{"type":"string","minLength":1,"maxLength":128},"severity":{"type":"string","enum":["critical","high","medium","low"]},
        "title":{"type":"string","minLength":1,"maxLength":2048},"trigger":text,"impact":text,"proposed_verification":text,"anchor":anchor},
        "required":["id","severity","title","trigger","impact","proposed_verification","anchor"]});
    match wire {
        "review_scope" => {
            json!({"type":"object","properties":{},"required":[],"additionalProperties":false})
        }
        "review_read" => {
            json!({"type":"object","properties":{"side":side,"path":path,"offset":integer,"max_bytes":{"type":"integer","minimum":1,"maximum":65536}},"required":["side","path","offset","max_bytes"],"additionalProperties":false})
        }
        "review_anchor" => {
            json!({"type":"object","properties":{"side":side,"path":path,"start":integer,"end":integer},"required":["side","path","start","end"],"additionalProperties":false})
        }
        _ => json!({"type":"object","properties":{"source_digest":hash,"baseline_digest":hash,
            "findings":{"type":"array","maxItems":128,"items":finding},"limitations":{"type":"array","minItems":1,"maxItems":128,"items":text}},
            "required":["source_digest","baseline_digest","findings","limitations"],"additionalProperties":false}),
    }
}
impl Host {
    fn authority(&self, wire: &str) -> Result {
        let canonical = ROUTES
            .iter()
            .find(|r| r.0 == wire)
            .ok_or(Error::Denied("review route"))?
            .1;
        let c = &self.config;
        let profile = self
            .profiles
            .profile(&c.profile_id)
            .ok_or(Error::Denied("unknown reviewer profile"))?;
        let t = self.store.journal().task(&c.task_id)?;
        if self.revoked
            || t.record.state != TaskState::Running
            || t.record.unknown_invocation_id.is_some()
            || t.contract.workspace_ref != c.resource_id
            || !t.contract.allowed_resource_ids.contains(&c.resource_id)
            || !t.contract.authorization_refs.contains(&c.authorization_ref)
            || !t.contract.selected_profile_ids.contains(&c.profile_id)
            || t.contract.input_snapshot_digest != self.session.source().snapshot().digest
            || melissa_execution_broker::unix_ms().map_err(|_| Error::Denied("trusted clock"))?
                >= c.expires_at_ms
            || !profile.tool_ids.iter().any(|id| id == canonical)
        {
            return Err(Error::Denied("review task/profile/resource/expiry"));
        }
        let effect = if wire == "review_submit" {
            "evidence.append"
        } else {
            "workspace.read"
        };
        if !t.contract.allowed_effects.iter().any(|e| e == effect)
            || !profile
                .authority
                .capability_ceiling
                .iter()
                .any(|e| e == effect)
        {
            return Err(Error::Denied("review effect intersection"));
        }
        Ok(())
    }
    fn execute(
        &mut self,
        call: ToolCall,
        ctx: ToolContext,
        cancel: &AtomicBool,
    ) -> Result<ToolExecution> {
        self.authority(&call.name)?;
        if ctx.task_id != self.config.task_id
            || ctx.profile_id != self.config.profile_id
            || ctx.run_id != self.config.run_id
            || ctx.generation == 0
            || [&ctx.request_id, &ctx.invocation_id, &ctx.attempt_id]
                .iter()
                .any(|s| !bounded(s, 256))
        {
            return Err(Error::Denied("review invocation identity"));
        }
        if cancel.load(Ordering::Acquire) {
            return Err(Error::Cancelled);
        }
        let input = parse(&call).map_err(|_| Error::Invalid("review arguments"))?;
        let append = matches!(input, Input::Submit(_));
        let result = match input {
            Input::Scope => self.session.scope(cancel)?,
            Input::Read(r) => {
                self.session
                    .read_page(r.side, &r.path, r.offset, r.max_bytes, cancel)?
            }
            Input::Anchor(r) => serde_json::to_value(self.session.anchor(&r, cancel)?)?,
            Input::Submit(r) => serde_json::to_value(self.session.submit(r, cancel)?)?,
        };
        let content =
            json!({"schema_version":"melissa.quality-tool-result/v1","task_id":ctx.task_id,
            "result":result,"task_accepted":false,"protected_acceptance":false})
            .to_string();
        let ceiling = self
            .store
            .journal()
            .task(&ctx.task_id)?
            .contract
            .limits
            .max_output_bytes;
        if content.len() as u64 > ceiling {
            return Err(Error::Invalid(
                "review response exceeds task output envelope; request a smaller page",
            ));
        }
        self.session.assert_current(cancel)?;
        self.authority(&call.name)?;
        let mut refs = vec![];
        if append {
            let id = format!("quality:{}:located-candidate", ctx.invocation_id);
            self.store
                .put_artifact(
                    &ArtifactRequest {
                        artifact_id: id.clone(),
                        task_id: ctx.task_id,
                        content_type: "application/json".into(),
                        logical_type: "quality.located_candidate".into(),
                        schema_version: "v1".into(),
                        retain_until_ms: 0,
                        legal_hold: true,
                    },
                    content.as_bytes(),
                )
                .map_err(|_| Error::AfterAppend)?;
            refs.push(id);
        }
        if cancel.load(Ordering::Acquire)
            || self.session.assert_current(cancel).is_err()
            || self.authority(&call.name).is_err()
        {
            return Err(if append {
                Error::AfterAppend
            } else {
                Error::Cancelled
            });
        }
        Ok(ToolExecution {
            content,
            success: true,
            cancelled: false,
            requires_reconciliation: false,
            artifact_refs: refs,
        })
    }
}
impl ReviewDispatcher {
    pub fn new(
        session: ReviewSession,
        store: LocalStore,
        profiles: ReviewProfiles,
        config: ReviewConfig,
    ) -> Result<Self> {
        if !matches!(
            config.profile_id.as_str(),
            "melissa.code.reviewer"
                | "melissa.code.security-reviewer"
                | "melissa.core.independent-qa"
        ) || [
            &config.task_id,
            &config.run_id,
            &config.resource_id,
            &config.authorization_ref,
        ]
        .iter()
        .any(|s| !bounded(s, 256))
        {
            return Err(Error::Denied("explicit review host identity required"));
        }
        let host = Host {
            session,
            store,
            profiles,
            config,
            revoked: false,
        };
        host.authority("review_scope")?;
        Ok(Self {
            host: Arc::new(Mutex::new(host)),
        })
    }
    pub fn revoke(&self) -> Result {
        self.host
            .lock()
            .map_err(|_| Error::Denied("review host unavailable"))?
            .revoked = true;
        Ok(())
    }
}
impl ToolDispatcher for ReviewDispatcher {
    fn definitions(&self) -> Vec<BoundTool> {
        let Ok(h) = self.host.lock() else {
            return vec![];
        };
        ROUTES
            .iter()
            .filter(|r| h.authority(r.0).is_ok())
            .map(|r| BoundTool {
                canonical_id: r.1.into(),
                definition: ToolDefinition {
                    name: r.0.into(),
                    description: r.2.into(),
                    parameters: schema(r.0),
                    strict: false,
                },
            })
            .collect()
    }
    fn validate_call(&self, c: &ToolCall) -> melissa_model_gateway::Result<()> {
        self.host
            .lock()
            .map_err(|_| GatewayError::new(ErrorKind::Tool, "review host unavailable"))?
            .authority(&c.name)
            .map_err(map)?;
        parse(c).map(|_| ())
    }
    fn execute<'a>(
        &'a mut self,
        c: ToolCall,
        ctx: ToolContext,
        cancel: Arc<AtomicBool>,
    ) -> Pin<Box<dyn Future<Output = melissa_model_gateway::Result<ToolExecution>> + Send + 'a>>
    {
        let host = self.host.clone();
        Box::pin(async move {
            let result = tokio::task::spawn_blocking(move || {
                host.lock()
                    .map_err(|_| Error::Denied("review host unavailable"))?
                    .execute(c, ctx, &cancel)
            })
            .await;
            match result {
                Ok(Ok(v))=>Ok(v),
                Ok(Err(Error::AfterAppend))|Err(_)=>Ok(ToolExecution{content:json!({"status":"append_requires_inspection","requires_reconciliation":true,"automatic_retry_allowed":false,"task_accepted":false}).to_string(),success:false,cancelled:false,requires_reconciliation:true,artifact_refs:vec![]}),
                Ok(Err(e))=>Err(map(e)),
            }
        })
    }
}
