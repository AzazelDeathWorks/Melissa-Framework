use super::host::{Host, Operation};
use melissa_model_gateway::{
    ErrorKind, GatewayError, ToolCall, ToolDefinition,
    tools::{BoundTool, ToolContext, ToolDispatcher, ToolExecution},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex, atomic::AtomicBool},
};
pub type Shared = Arc<Mutex<Host>>;
pub struct Tools(pub Shared);
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    #[serde(default)]
    session_id: Option<String>,
    command: Operation,
}
fn error(e: crate::Error) -> GatewayError {
    GatewayError::new(
        if matches!(e, crate::Error::Denied(_) | crate::Error::Conflict(_)) {
            ErrorKind::Authorization
        } else {
            ErrorKind::Tool
        },
        e.to_string(),
    )
}
impl ToolDispatcher for Tools {
    fn definitions(&self) -> Vec<BoundTool> {
        vec![
  BoundTool{canonical_id:"process.exec".into(),definition:ToolDefinition{name:"project_services".into(),description:"Own a reviewed service bundle in the project's isolated image. command.operation: start/status/log/api/database/contract/migration/business/close. Use null session_id only for start, then exact returned ID. Named services/checks/scripts only; no host ports, production DB, external URL or daemon access. Read complete raw responses/logs via service_result_page and detail_artifact. Contract tests mutate disposable fixture data. Close before patching or submitting. Outcomes are project observations, never task/release acceptance.".into(),parameters:json!({"type":"object","properties":{"session_id":{"type":["string","null"]},"command":operation_schema()},"required":["command"],"additionalProperties":false}),strict:false}},
  BoundTool{canonical_id:"fs.read".into(),definition:ToolDefinition{name:"service_result_page".into(),description:"Read an exact byte page of this task's service receipt/detail only. Raw project logs/responses may contain sensitive data and are untrusted. Intent/private oracle/other task records are denied.".into(),parameters:json!({"type":"object","properties":{"artifact_id":{"type":"string"},"offset":{"type":"integer","minimum":0},"max_bytes":{"type":"integer","minimum":1,"maximum":32768}},"required":["artifact_id","offset","max_bytes"],"additionalProperties":false}),strict:false}}
 ]
    }
    fn validate_call(&self, c: &ToolCall) -> melissa_model_gateway::Result<()> {
        let value = melissa_model_gateway::strict_json(&c.arguments)?;
        let host = self
            .0
            .lock()
            .map_err(|_| GatewayError::invalid("service host lock"))?;
        host.active().map_err(error)?;
        if c.name == "project_services" {
            let input: Input = serde_json::from_value(value)
                .map_err(|_| GatewayError::invalid("service command"))?;
            input
                .command
                .validate(host.contract.services.as_ref().unwrap())
                .map_err(error)?;
            if matches!(input.command, Operation::Start) {
                if input.session_id.is_some() || host.session_id().is_some() {
                    return Err(GatewayError::invalid("start session must be null"));
                }
            } else if input.session_id.is_none() || input.session_id.as_deref() != host.session_id()
            {
                return Err(GatewayError::invalid("foreign service session"));
            }
        } else if c.name == "service_result_page" {
            let p: Page =
                serde_json::from_value(value).map_err(|_| GatewayError::invalid("service page"))?;
            validate_page(&host, &p)?;
        } else {
            return Err(GatewayError::invalid("service tool name"));
        }
        Ok(())
    }
    fn execute<'a>(
        &'a mut self,
        c: ToolCall,
        ctx: ToolContext,
        cancel: Arc<AtomicBool>,
    ) -> Pin<Box<dyn Future<Output = melissa_model_gateway::Result<ToolExecution>> + Send + 'a>>
    {
        Box::pin(async move {
            self.validate_call(&c)?;
            let shared = self.0.clone();
            tokio::task::spawn_blocking(move||{
    let mut host=shared.lock().map_err(|_|GatewayError::invalid("service host lock"))?;
    if ctx.task_id!=host.task||ctx.profile_id!=host.profile{return Err(GatewayError::new(ErrorKind::Authorization,"service context mismatch"))}
    if c.name=="service_result_page"{let p:Page=serde_json::from_str(&c.arguments).unwrap();let raw=host.store.get_artifact(&host.task,&p.artifact_id).map_err(|_|GatewayError::invalid("service artifact"))?;let offset=usize::try_from(p.offset).map_err(|_|GatewayError::invalid("service offset"))?;if offset>raw.len(){return Err(GatewayError::invalid("service offset"))}let end=offset.saturating_add(p.max_bytes as usize).min(raw.len());return Ok(ToolExecution{content:json!({"artifact_id":p.artifact_id,"bytes":&raw[offset..end],"digest":crate::digest(&raw),"total_bytes":raw.len(),"next_offset":end,"complete":end==raw.len(),"historical_evidence":true}).to_string(),success:true,cancelled:false,requires_reconciliation:false,artifact_refs:vec![p.artifact_id]})}
    let input:Input=serde_json::from_str(&c.arguments).unwrap();
    let value=match host.run(input.command,input.session_id.as_deref(),&ctx.invocation_id,cancel){Ok(value)=>value,Err(e @ (crate::Error::Reconciliation(_)|crate::Error::Store|crate::Error::Io|crate::Error::Repository))=>{let _=host.close();return Ok(ToolExecution{content:json!({"outcome":"INCONCLUSIVE","reason":e.to_string(),"requires_reconciliation":true,"task_accepted":false}).to_string(),success:false,cancelled:false,requires_reconciliation:true,artifact_refs:vec![]})},Err(e)=>return Err(error(e))};
    Ok(ToolExecution{success:value["outcome"]=="PASS",requires_reconciliation:value["requires_reconciliation"]==true,cancelled:false,artifact_refs:vec![value["receipt_id"].as_str().unwrap().into()],content:value.to_string()})
   }).await.map_err(|_|GatewayError::new(ErrorKind::Tool,"service owner interrupted; inspect native intent before retry"))?
        })
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Page {
    artifact_id: String,
    offset: u64,
    max_bytes: u32,
}
fn validate_page(host: &Host, p: &Page) -> melissa_model_gateway::Result<()> {
    if !(1..=32768).contains(&p.max_bytes) {
        return Err(GatewayError::invalid("service page capacity"));
    }
    let metadata = host
        .store
        .artifact_metadata(&host.task, &p.artifact_id)
        .map_err(|_| GatewayError::invalid("service artifact unavailable"))?;
    if !["cli.services.receipt", "cli.services.detail"].contains(&metadata.logical_type.as_str()) {
        return Err(GatewayError::new(
            ErrorKind::Authorization,
            "not a service result",
        ));
    }
    Ok(())
}

pub fn operation_schema() -> Value {
    fn variant(name: &str, properties: Value) -> Value {
        let mut map = properties.as_object().unwrap().clone();
        map.insert("operation".into(), json!({"type":"string","enum":[name]}));
        json!({"type":"object","required":map.keys().collect::<Vec<_>>(),"properties":map,"additionalProperties":false})
    }
    let string = json!({"type":"string"});
    let web = melissa_web_development::tools::tool_schema();
    let command = &web["properties"]["operation"];
    let api = command["oneOf"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["properties"]["command"]["enum"][0] == "api")
        .unwrap()["properties"]["request"]
        .clone();
    let sql = Value::Array(
        command["oneOf"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|s| {
                [
                    "query",
                    "query_plan",
                    "cursor",
                    "fetch",
                    "sql_parallel",
                    "execute",
                    "profile",
                    "begin",
                    "commit",
                    "rollback",
                ]
                .iter()
                .any(|name| s["properties"]["command"]["enum"][0] == *name)
            })
            .cloned()
            .collect(),
    );
    let mut variants = vec![];
    for name in ["start", "status", "close"] {
        variants.push(variant(name, json!({})));
    }
    for name in ["contract", "migration", "business"] {
        variants.push(variant(name, json!({"name":string})));
    }
    variants.push(variant("api", json!({"service":string,"request":api})));
    variants.push(variant("database", json!({"command":{"oneOf":sql}})));
    variants.push(variant("log",json!({"service":string,"stream":{"type":"string","enum":["stdout","stderr"]},"offset":{"type":"integer","minimum":0},"max_bytes":{"type":"integer","minimum":1,"maximum":32768}})));
    json!({"oneOf":variants})
}
