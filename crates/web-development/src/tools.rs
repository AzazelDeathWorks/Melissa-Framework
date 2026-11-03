//! Optional native Gateway tools. Session handles are bound to task/profile/run.
use crate::{
    Error, Result, identifier,
    plan::{Registration, WebPlan},
    protocol::{ApiRequest, Command, Service},
    session::{Policy, WebSession},
};
use melissa_container_executor::ContainerLimits;
use melissa_core_contracts::catalog::Catalog;
use melissa_local_store::LocalStore;
use melissa_model_gateway::{
    ErrorKind, GatewayError, ToolCall, ToolDefinition,
    tools::{BoundTool, ToolContext, ToolDispatcher, ToolExecution},
};
use melissa_repository::Repository;
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

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Call {
    session_id: Option<String>,
    operation: Command,
}
struct Host {
    repo: Arc<Repository>,
    store: LocalStore,
    catalog: Catalog,
    policy: Policy,
    registration: Registration,
    limits: ContainerLimits,
    session: Option<WebSession>,
    revoked: Arc<AtomicBool>,
    api_rules: Vec<crate::plan::ApiReconciliationRule>,
    portable_wire: bool,
}
pub struct WebDispatcher {
    host: Arc<Mutex<Host>>,
    revoked: Arc<AtomicBool>,
}
#[derive(Clone)]
pub struct Revocation {
    revoked: Arc<AtomicBool>,
}
impl Revocation {
    pub fn revoke(&self) {
        self.revoked.store(true, Ordering::Release);
    }
}
fn err(e: Error) -> GatewayError {
    GatewayError::new(
        match &e {
            Error::Arguments(_) => ErrorKind::Invalid,
            Error::AfterDispatch(_) => ErrorKind::Tool,
            _ => ErrorKind::Authorization,
        },
        e.to_string(),
    )
}
fn canonical(reg: &Registration) -> &'static str {
    if reg.services.contains(&Service::Browser) {
        "browser.navigate"
    } else if reg.services.contains(&Service::Api) {
        "code.tests"
    } else {
        "data.profile"
    }
}
fn canonicalize_portable_headers(request: &mut ApiRequest) -> Result {
    let mut headers = std::collections::BTreeMap::new();
    for (name, value) in std::mem::take(&mut request.headers) {
        let lower = name.to_ascii_lowercase();
        let name = if [
            "content-type",
            "authorization",
            "idempotency-key",
            "x-test-tenant",
        ]
        .contains(&lower.as_str())
        {
            lower
        } else {
            name
        };
        if headers.insert(name, value).is_some() {
            return Err(Error::Invalid("duplicate local API header name"));
        }
    }
    request.headers = headers;
    Ok(())
}
impl Host {
    fn uncertain(&mut self, message: &str) -> ToolExecution {
        self.revoked.store(true, Ordering::Release);
        let mut refs = vec![];
        let mut cleanup = false;
        if let Some(session) = self.session.take() {
            session.cancellation().store(true, Ordering::Release);
            refs = session.artifact_refs().to_vec();
            if let Ok(result) = session.finish() {
                cleanup = result.cleanup_confirmed;
                refs = result.artifact_refs;
            }
        }
        ToolExecution{content:json!({"status":"outcome_unknown","error":message,"cleanup_confirmed":cleanup,"automatic_retry":false,"task_completed":false}).to_string(),success:false,cancelled:false,requires_reconciliation:true,artifact_refs:refs}
    }
    fn parse(&self, call: &ToolCall) -> Result<Call> {
        if call.name != "web_session"
            || !identifier(&call.id)
            || call.arguments.len() > 128 * 1024
            || self.revoked.load(Ordering::Acquire)
        {
            return Err(Error::Invalid("unregistered web tool or revoked host"));
        }
        let mut value = melissa_model_gateway::strict_json(&call.arguments)
            .map_err(|e| Error::Arguments(format!("strict web arguments JSON: {e}")))?;
        if self.portable_wire {
            let fields = value.as_object_mut().ok_or_else(|| {
                Error::Arguments("portable web arguments must be an object".into())
            })?;
            let raw = fields
                .remove("operation_json")
                .and_then(|v| v.as_str().map(String::from))
                .ok_or_else(|| {
                    Error::Arguments(
                        "operation_json must be a JSON string containing only the command object"
                            .into(),
                    )
                })?;
            if fields.len() != 1 || !fields.contains_key("session_id") {
                return Err(Error::Arguments(
                    "portable outer fields must be exactly session_id and operation_json".into(),
                ));
            }
            fields.insert(
                "operation".into(),
                melissa_model_gateway::strict_json(&raw)
                    .map_err(|e| Error::Arguments(format!("invalid operation_json: {e}; encode the command object once, e.g. {{\"command\":\"start\"}}")))?,
            );
        }
        let mut parsed: Call = serde_json::from_value(value)
            .map_err(|e| Error::Arguments(format!("invalid typed web arguments: {e}; operation_json contains only the command object; session_id belongs in the outer arguments")))?;
        if self.portable_wire {
            match &mut parsed.operation {
                Command::Api { request } => canonicalize_portable_headers(request)?,
                Command::ApiConcurrent { requests } => {
                    for request in requests {
                        canonicalize_portable_headers(request)?;
                    }
                }
                _ => {}
            }
        }
        parsed.operation.validate()?;
        match &self.session {
            None if parsed.session_id.is_none()
                && matches!(parsed.operation, Command::Start {}) => {}
            Some(s)
                if parsed.session_id.as_deref() == Some(s.id())
                    && !matches!(parsed.operation, Command::Start {}) =>
            {
                s.validate(&parsed.operation)?
            }
            _ => {
                return Err(Error::Invalid(
                    "foreign, absent or closed web session handle",
                ));
            }
        }
        Ok(parsed)
    }
    fn execute(
        &mut self,
        call: ToolCall,
        ctx: ToolContext,
        cancel: Arc<AtomicBool>,
    ) -> Result<ToolExecution> {
        let parsed = self.parse(&call)?;
        if cancel.load(Ordering::Acquire) {
            return Err(Error::Invalid("web call cancelled before send"));
        }
        if matches!(parsed.operation, Command::Start {}) {
            let source = self.repo.snapshot(&cancel)?;
            let plan = WebPlan::prepare(
                &self.repo,
                &source,
                self.registration.clone(),
                self.limits.clone(),
                &cancel,
            )?
            .with_api_reconciliation(self.api_rules.clone())?;
            let session = WebSession::start(
                self.repo.clone(),
                self.store.connect()?,
                self.catalog.clone(),
                self.policy.clone(),
                plan,
                ctx,
                cancel,
                self.revoked.clone(),
            )?;
            let reply = session
                .last_reply()
                .cloned()
                .ok_or(Error::Invalid("missing web startup reply"))?;
            let id = session.id().to_string();
            let refs = session.artifact_refs().to_vec();
            self.session = Some(session);
            return Ok(ToolExecution {
                content: json!({"session_id":id,"reply":reply,"task_completed":false}).to_string(),
                success: reply.ok,
                cancelled: false,
                requires_reconciliation: false,
                artifact_refs: refs,
            });
        }
        let session = self
            .session
            .as_mut()
            .ok_or(Error::Invalid("web session absent"))?;
        if !session.context_matches(&ctx) {
            return Err(Error::Invalid(
                "web context is outside owning task/profile/run",
            ));
        }
        let reply = session.command(parsed.operation.clone())?;
        let id = session.id().to_string();
        // Resolve only from exact host-registered proof, inside THIS tool call.
        // A pending effect must not permit another paid model turn first.
        let reconciliation = if session.pending_api_effect() && !self.api_rules.is_empty() {
            Some(session.command(Command::ReconcileApi {
                sequence: reply.seq,
            })?)
        } else {
            None
        };
        if session.requires_reconciliation() {
            return Ok(self.uncertain(
                "Owned write outcome unresolved; no new model turn or effect replay is allowed",
            ));
        }
        if matches!(parsed.operation, Command::Close {}) {
            let result = self
                .session
                .take()
                .ok_or(Error::Invalid("web close lost session"))?
                .finish()?;
            let content = if self.portable_wire {
                let mut compact = serde_json::to_value(&reply)?;
                if let Some(body) = compact["body"].as_object_mut()
                    && body.remove("trace_hex").is_some()
                {
                    body.insert("trace_delivery".into(),json!({"kind":"exact_native_web_reply_artifact","artifact_refs":result.artifact_refs}));
                }
                json!({"session_id":id,"reply":compact,"outcome":{"success":result.success,"cleanup_confirmed":result.cleanup_confirmed,"source_digest":result.source_digest,"source_current":result.source_current,"requires_reconciliation":result.requires_reconciliation,"native_attempt_id":result.native_attempt_id,"artifact_refs":result.artifact_refs},"task_completed":false}).to_string()
            } else {
                json!({"session_id":id,"reply":reply,"outcome":result,"task_completed":false})
                    .to_string()
            };
            return Ok(ToolExecution {
                content,
                success: reply.ok && result.success,
                cancelled: false,
                requires_reconciliation: result.requires_reconciliation,
                artifact_refs: result.artifact_refs,
            });
        }
        let mut content = json!({"session_id":id,"reply":reply,"task_completed":false});
        if let Some(proof) = reconciliation {
            content["reconciliation"] = serde_json::to_value(proof)?;
        }
        Ok(ToolExecution {
            content: content.to_string(),
            success: reply.ok,
            cancelled: false,
            requires_reconciliation: false,
            artifact_refs: session.artifact_refs().to_vec(),
        })
    }
}
impl WebDispatcher {
    /// Settle this host's owned session when a model run stops. This never
    /// replays an application command and preserves genuine unknown effects.
    pub fn finish_owned(&self) -> Result<Option<crate::session::Outcome>> {
        let mut host = self
            .host
            .lock()
            .map_err(|_| Error::Invalid("web host lock"))?;
        host.session.take().map(WebSession::finish).transpose()
    }
    pub fn with_portable_model_wire(self) -> Result<Self> {
        self.host
            .lock()
            .map_err(|_| Error::Invalid("web host lock"))?
            .portable_wire = true;
        Ok(self)
    }
    pub fn with_api_reconciliation(
        self,
        rules: Vec<crate::plan::ApiReconciliationRule>,
    ) -> Result<Self> {
        self.host
            .lock()
            .map_err(|_| Error::Invalid("web host lock"))?
            .api_rules = rules;
        Ok(self)
    }
    pub fn new(
        repo: Repository,
        store: LocalStore,
        catalog: Catalog,
        policy: Policy,
        registration: Registration,
        limits: ContainerLimits,
    ) -> Result<Self> {
        if [
            &policy.task_id,
            &policy.profile_id,
            &policy.principal_id,
            &policy.resource_id,
            &policy.authorization_ref,
        ]
        .iter()
        .any(|x| !identifier(x))
            || registration.services.is_empty()
        {
            return Err(Error::Invalid("invalid host web registration"));
        }
        let revoked = Arc::new(AtomicBool::new(false));
        Ok(Self {
            host: Arc::new(Mutex::new(Host {
                repo: Arc::new(repo),
                store,
                catalog,
                policy,
                registration,
                limits,
                session: None,
                revoked: revoked.clone(),
                api_rules: vec![],
                portable_wire: false,
            })),
            revoked,
        })
    }
    pub fn revocation(&self) -> Revocation {
        Revocation {
            revoked: self.revoked.clone(),
        }
    }
}
fn object(properties: Value) -> Value {
    let required = properties
        .as_object()
        .expect("host schema object")
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    json!({"type":"object","properties":properties,"required":required,"additionalProperties":false})
}
fn variant(name: &str, mut props: Value) -> Value {
    props
        .as_object_mut()
        .expect("host operation properties")
        .insert("command".into(), json!({"type":"string","enum":[name]}));
    object(props)
}
pub fn tool_schema() -> Value {
    let string = json!({"type":"string"});
    let integer = json!({"type":"integer"});
    let values = json!({"type":"array","items":{"type":["string","number","boolean","null"]}});
    let api = object(
        json!({"session":string,"method":{"type":"string","enum":["get","post","put","patch","delete"]},"path":string,"headers":{"type":"object","additionalProperties":{"type":"string"}},"body":{"type":["object","array","string","number","boolean","null"]},"timeout_ms":integer}),
    );
    let operations = vec![
        variant("reconcile_api", json!({"sequence":integer})),
        variant(
            "cursor",
            json!({"connection":string,"sql":string,"parameters":values,"limit":integer}),
        ),
        variant("fetch", json!({"cursor":string,"limit":integer})),
        variant(
            "query_plan",
            json!({"connection":string,"sql":string,"parameters":values}),
        ),
        variant(
            "sql_parallel",
            json!({"scripts":{"type":"array","minItems":2,"maxItems":4,"items":object(json!({"connection":string,"mode":{"type":"string","enum":["read_only","immediate","serializable"]},"steps":{"type":"array","minItems":1,"maxItems":16,"items":{"oneOf":[object(json!({"kind":{"type":"string","enum":["query"]},"sql":string,"parameters":values})),object(json!({"kind":{"type":"string","enum":["execute"]},"statement":string,"parameters":values})),object(json!({"kind":{"type":"string","enum":["barrier","commit","rollback"]}}))]}}}))}}),
        ),
        variant("start", json!({})),
        variant("close", json!({})),
        variant("navigate", json!({"page":string,"path":string})),
        variant("inspect", json!({"page":string})),
        variant("elements", json!({"page":string,"selector":string})),
        variant("screenshot", json!({"page":string})),
        variant(
            "fill",
            json!({"page":string,"selector":string,"value":string}),
        ),
        variant("click", json!({"page":string,"selector":string})),
        variant(
            "press",
            json!({"page":string,"selector":string,"key":string}),
        ),
        variant(
            "wait",
            json!({"page":string,"selector":string,"state":{"type":"string","enum":["visible","hidden","attached","detached"]},"timeout_ms":integer}),
        ),
        variant(
            "viewport",
            json!({"page":string,"width":integer,"height":integer}),
        ),
        variant("api", json!({"request":api})),
        variant(
            "api_concurrent",
            json!({"requests":{"type":"array","minItems":1,"maxItems":16,"items":api}}),
        ),
        variant(
            "query",
            json!({"connection":string,"sql":string,"parameters":values}),
        ),
        variant(
            "execute",
            json!({"connection":string,"statement":string,"parameters":values}),
        ),
        variant("profile", json!({"connection":string})),
        variant("migration", json!({"name":string})),
        variant(
            "begin",
            json!({"connection":string,"mode":{"type":"string","enum":["read_only","immediate","serializable"]}}),
        ),
        variant("commit", json!({"connection":string})),
        variant("rollback", json!({"connection":string})),
    ];
    object(json!({"session_id":{"type":["string","null"]},"operation":{"oneOf":operations}}))
}
impl ToolDispatcher for WebDispatcher {
    fn definitions(&self) -> Vec<BoundTool> {
        let Ok(h) = self.host.lock() else {
            return vec![];
        };
        vec![BoundTool {
            canonical_id: canonical(&h.registration).into(),
            definition: ToolDefinition {
                name: "web_session".into(),
                description: format!(
                    "Host-registered isolated development session; use null session_id only for start, then the returned opaque ID. Services {:?}; SQL statement names {:?}; migration names {:?}. No external URL, production connection, caller executable or JavaScript evaluation. Replies are developer observations; task_completed=false. {}",
                    h.registration.services,
                    h.registration.statements.keys().collect::<Vec<_>>(),
                    h.registration.migrations.keys().collect::<Vec<_>>(),
                    if h.portable_wire {
                        format!(
                            "operation_json contains ONLY the operation command object, serialized once as a JSON string. Start example: {}. Keep session_id in the outer arguments; do not nest operation/session_id inside operation_json or manually double-escape it. {}COMPLETE schema: {}",
                            json!({"command":"start"}),
                            if h.registration.services.contains(&Service::Api) {
                                "API request.session is a COOKIE-JAR LABEL, e.g. alice or client1, NOT the outer opaque web session_id. It must start with an ASCII letter, use only letters/digits/_/-, and be at most48 bytes. Reuse that label for cookies; use different labels for isolated clients. API header names content-type, authorization, idempotency-key and x-test-tenant are ASCII case-insensitive; values are preserved exactly. Duplicate names after case folding, forbidden headers and control characters are denied before send. "
                            } else {
                                ""
                            },
                            tool_schema()
                        )
                    } else {
                        String::new()
                    }
                ),
                parameters: if h.portable_wire {
                    object(
                        json!({"session_id":{"type":["string","null"]},"operation_json":{"type":"string"}}),
                    )
                } else {
                    tool_schema()
                },
                strict: true,
            },
        }]
    }
    fn validate_call(&self, call: &ToolCall) -> melissa_model_gateway::Result<()> {
        self.host
            .lock()
            .map_err(|_| GatewayError::new(ErrorKind::Tool, "web host lock poisoned"))?
            .parse(call)
            .map(|_| ())
            .map_err(err)
    }
    fn execute<'a>(
        &'a mut self,
        call: ToolCall,
        context: ToolContext,
        cancelled: Arc<AtomicBool>,
    ) -> Pin<Box<dyn Future<Output = melissa_model_gateway::Result<ToolExecution>> + Send + 'a>>
    {
        let host = self.host.clone();
        let revoked = self.revoked.clone();
        Box::pin(async move {
            let working = host.clone();
            match tokio::task::spawn_blocking(move || {
                let mut h = working
                    .lock()
                    .map_err(|_| Error::Invalid("web host lock poisoned"))?;
                match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    h.execute(call, context, cancelled)
                })) {
                    Ok(Ok(r)) => Ok(r),
                    Ok(Err(e @ Error::AfterDispatch(_))) => Ok(h.uncertain(&e.to_string())),
                    Ok(Err(e)) => Err(e),
                    Err(_) => Ok(h.uncertain("web host call unwound; inspect native evidence")),
                }
            })
            .await
            {
                Ok(r) => r.map_err(err),
                Err(_) => {
                    revoked.store(true, Ordering::Release);
                    let mut h = host.lock().unwrap_or_else(|p| p.into_inner());
                    Ok(h.uncertain("web host call join failed; inspect native evidence"))
                }
            }
        })
    }
}
