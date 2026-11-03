//! A one-use verify.run binding. The host supplies opaque, independently
//! produced evidence; models cannot choose tests, expected answers or commands.
use crate::{
    Error, Result,
    protected::{ProtectedQa, QaReceipt, StaticEvidence},
    security::AuditEvidence,
    source::FrozenSource,
    supply_chain::SupplyChainEvidence,
};
use melissa_model_gateway::{
    ErrorKind, GatewayError, ToolCall, ToolDefinition, strict_json,
    tools::{BoundTool, ToolContext, ToolDispatcher, ToolExecution},
};
use melissa_repository::Repository;
use serde_json::json;
use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex, atomic::AtomicBool},
};
pub struct VerificationJob {
    pub qa: ProtectedQa,
    pub source: FrozenSource,
    pub source_repository: Repository,
    pub package_repository: Repository,
    pub receipt: QaReceipt,
    pub static_scan: StaticEvidence,
    pub audit: AuditEvidence,
    pub supply_chain: SupplyChainEvidence,
}
pub struct VerificationDispatcher {
    job: Arc<Mutex<Option<VerificationJob>>>,
}
impl VerificationDispatcher {
    pub fn new(job: VerificationJob) -> Result<Self> {
        job.qa.validate_receipt(&job.receipt)?;
        Ok(Self {
            job: Arc::new(Mutex::new(Some(job))),
        })
    }
}
fn parse(call: &ToolCall) -> melissa_model_gateway::Result<()> {
    if call.name != "verify_run" || call.id.is_empty() || call.id.len() > 256 {
        return Err(GatewayError::unsupported("closed verify.run route"));
    }
    let v = strict_json(&call.arguments)?;
    if !v.as_object().is_some_and(serde_json::Map::is_empty) {
        return Err(GatewayError::invalid(
            "verification arguments must be empty; the host selects the exact job",
        ));
    }
    Ok(())
}
impl ToolDispatcher for VerificationDispatcher {
    fn definitions(&self) -> Vec<BoundTool> {
        if !self
            .job
            .lock()
            .is_ok_and(|g| g.as_ref().is_some_and(|j| j.qa.authority().is_ok()))
        {
            return vec![];
        }
        vec![BoundTool{canonical_id:"verify.run".into(),definition:ToolDefinition{name:"verify_run".into(),description:"Validate the host-bound protected QA, source, actual package, current security/advisory and SBOM/license evidence. One use; no arbitrary command or test selection. A model cannot create acceptance by declaration.".into(),parameters:json!({"type":"object","properties":{},"required":[],"additionalProperties":false}),strict:false}}]
    }
    fn validate_call(&self, call: &ToolCall) -> melissa_model_gateway::Result<()> {
        parse(call)?;
        let guard = self.job.lock().map_err(|_| {
            GatewayError::new(ErrorKind::Authorization, "verification host unavailable")
        })?;
        guard
            .as_ref()
            .ok_or_else(|| {
                GatewayError::new(
                    ErrorKind::Authorization,
                    "verification job consumed; do not retry",
                )
            })?
            .qa
            .authority()
            .map_err(|_| {
                GatewayError::new(ErrorKind::Authorization, "verification authority withdrawn")
            })
    }
    fn execute<'a>(
        &'a mut self,
        call: ToolCall,
        ctx: ToolContext,
        cancel: Arc<AtomicBool>,
    ) -> Pin<Box<dyn Future<Output = melissa_model_gateway::Result<ToolExecution>> + Send + 'a>>
    {
        let job = self.job.clone();
        Box::pin(async move {
            parse(&call)?;
            let result = tokio::task::spawn_blocking(move || -> Result<_> {
                let mut guard = job
                    .lock()
                    .map_err(|_| Error::Denied("verification host unavailable"))?;
                let pending = guard
                    .as_ref()
                    .ok_or(Error::Denied("verification job already consumed"))?;
                let (task, run) = pending.qa.tool_identity();
                if ctx.task_id != task
                    || ctx.run_id != run
                    || ctx.profile_id != "melissa.core.independent-qa"
                    || ctx.generation == 0
                {
                    return Err(Error::Denied("verification task/profile/run identity"));
                }
                if cancel.load(std::sync::atomic::Ordering::Acquire) {
                    return Err(Error::Cancelled);
                }
                // Consume before final evidence append. Any uncertain result
                // requires inspection; it cannot cause an automatic re-run.
                let mut job = guard.take().unwrap();
                drop(guard);
                let acceptance = job.qa.accept(
                    &job.source,
                    &job.source_repository,
                    &job.package_repository,
                    &job.receipt,
                    &job.static_scan,
                    &job.audit,
                    &job.supply_chain,
                    &cancel,
                )?;
                Ok(ToolExecution {
                    content: serde_json::to_string(&acceptance)?,
                    success: true,
                    cancelled: false,
                    requires_reconciliation: false,
                    artifact_refs: vec![],
                })
            })
            .await;
            match result {
                Ok(Ok(value))=>Ok(value),
                Ok(Err(Error::AfterAppend))|Err(_)=>Ok(ToolExecution{content:json!({"status":"requires_inspection","protected_acceptance":false,"automatic_retry":false}).to_string(),success:false,cancelled:false,requires_reconciliation:true,artifact_refs:vec![]}),
                Ok(Err(e))=>Err(GatewayError::new(ErrorKind::Authorization,e.to_string()))
            }
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn model_cannot_supply_commands_or_oracles_to_verification() {
        for (name, args) in [
            ("process_exec", "{}"),
            ("verify_run", r#"{"command":"cargo test"}"#),
            ("verify_run", r#"{"expected":true}"#),
            ("verify_run", r#"{"suite":"attacker","suite":"other"}"#),
        ] {
            assert!(
                parse(&ToolCall {
                    id: "one".into(),
                    name: name.into(),
                    arguments: args.into()
                })
                .is_err()
            );
        }
        assert!(
            parse(&ToolCall {
                id: "one".into(),
                name: "verify_run".into(),
                arguments: "{}".into()
            })
            .is_ok()
        );
    }
}
