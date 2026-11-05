//! Host-authored adapter records. A tool receipt is not task acceptance.
use crate::{
    ErrorKind, ToolCall,
    tools::{BoundTool, ToolContext, ToolExecution},
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Status {
    Pass,
    Fail,
    Blocked,
    NotRun,
    Inconclusive,
}

/// Unknown provenance stays absent, rather than being inferred from model text.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdapterMetadata {
    pub adapter_id: Option<String>,
    pub adapter_version: Option<String>,
    pub environment_fingerprint: Option<String>,
    /// Host snapshot before dispatch; native mutation receipts describe the result.
    pub source_before: Option<String>,
    pub visibility_digest: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolPreflight {
    pub schema_version: String,
    pub wire_name: String,
    pub canonical_id: Option<String>,
    pub schema_digest: Option<String>,
    pub arguments_digest: String,
    pub elapsed_ms: u64,
    pub status: Status,
    pub dispatch_started: bool,
    pub task_accepted: bool,
    pub adapter: AdapterMetadata,
}
impl ToolPreflight {
    pub fn new(
        call: &ToolCall,
        bindings: &[BoundTool],
        status: Status,
        adapter: AdapterMetadata,
    ) -> Self {
        let binding = bindings.iter().find(|b| b.definition.name == call.name);
        Self {
            schema_version: "melissa.tool-preflight/v1".into(),
            wire_name: call.name.clone(),
            canonical_id: binding.map(|b| b.canonical_id.clone()),
            schema_digest: binding.map(|b| hash(b.definition.parameters.to_string().as_bytes())),
            arguments_digest: hash(call.arguments.as_bytes()),
            elapsed_ms: 0,
            status,
            dispatch_started: false,
            task_accepted: false,
            adapter,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolReceipt {
    pub schema_version: String,
    pub context: ReceiptContext,
    pub preflight: ToolPreflight,
    pub status: Status,
    pub dispatch_started: bool,
    pub elapsed_ms: u64,
    pub result_digest: Option<String>,
    pub artifact_refs: Vec<String>,
    pub requires_reconciliation: bool,
    pub error_kind: Option<ErrorKind>,
    pub task_accepted: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReceiptContext {
    pub task_id: String,
    pub profile_id: String,
    pub run_id: String,
    pub request_id: String,
    pub invocation_id: String,
    pub attempt_id: String,
    pub generation: u64,
}
impl From<&ToolContext> for ReceiptContext {
    fn from(c: &ToolContext) -> Self {
        Self {
            task_id: c.task_id.clone(),
            profile_id: c.profile_id.clone(),
            run_id: c.run_id.clone(),
            request_id: c.request_id.clone(),
            invocation_id: c.invocation_id.clone(),
            attempt_id: c.attempt_id.clone(),
            generation: c.generation,
        }
    }
}
impl ToolReceipt {
    pub fn not_run(
        context: &ToolContext,
        mut preflight: ToolPreflight,
        kind: Option<ErrorKind>,
    ) -> Self {
        preflight.status = if preflight.status == Status::Blocked {
            Status::Blocked
        } else {
            Status::NotRun
        };
        Self {
            schema_version: "melissa.tool-receipt/v1".into(),
            context: context.into(),
            status: preflight.status,
            preflight,
            dispatch_started: false,
            elapsed_ms: 0,
            result_digest: None,
            artifact_refs: vec![],
            requires_reconciliation: false,
            error_kind: kind,
            task_accepted: false,
        }
    }
    pub fn completed(
        context: &ToolContext,
        preflight: ToolPreflight,
        output: &ToolExecution,
        elapsed_ms: u64,
    ) -> Self {
        let status = if output.requires_reconciliation || output.cancelled {
            Status::Inconclusive
        } else if output.success {
            Status::Pass
        } else {
            Status::Fail
        };
        Self {
            schema_version: "melissa.tool-receipt/v1".into(),
            context: context.into(),
            preflight,
            status,
            dispatch_started: true,
            elapsed_ms,
            result_digest: Some(hash(output.content.as_bytes())),
            artifact_refs: output.artifact_refs.clone(),
            requires_reconciliation: output.requires_reconciliation,
            error_kind: None,
            task_accepted: false,
        }
    }
    pub fn dispatch_error(
        context: &ToolContext,
        preflight: ToolPreflight,
        kind: ErrorKind,
        elapsed_ms: u64,
    ) -> Self {
        let mut receipt = Self::not_run(context, preflight, Some(kind));
        receipt.dispatch_started = true;
        receipt.elapsed_ms = elapsed_ms;
        receipt.status = if kind == ErrorKind::Authorization {
            Status::Blocked
        } else {
            Status::Inconclusive
        };
        receipt.requires_reconciliation = kind != ErrorKind::Authorization;
        receipt
    }
}
fn hash(bytes: &[u8]) -> String {
    format!("blake3:{}", blake3::hash(bytes).to_hex())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ambiguous_and_cancelled_outputs_never_become_pass_receipts() {
        let call = ToolCall {
            id: "c".into(),
            name: "tool".into(),
            arguments: "{\"secret\":\"private\"}".into(),
        };
        let context = ToolContext {
            task_id: "t".into(),
            profile_id: "p".into(),
            run_id: "r".into(),
            request_id: "q".into(),
            invocation_id: "i".into(),
            attempt_id: "a".into(),
            generation: 1,
        };
        let preflight = ToolPreflight::new(&call, &[], Status::NotRun, AdapterMetadata::default());
        let mut output = ToolExecution {
            content: "private output".into(),
            success: true,
            cancelled: false,
            requires_reconciliation: true,
            artifact_refs: vec!["evidence".into()],
        };
        let record = ToolReceipt::completed(&context, preflight.clone(), &output, 5);
        assert_eq!(record.status, Status::Inconclusive);
        let encoded = serde_json::to_string(&record).unwrap();
        assert!(!encoded.contains("private"));
        assert!(!record.task_accepted);
        output.requires_reconciliation = false;
        output.cancelled = true;
        assert_eq!(
            ToolReceipt::completed(&context, preflight.clone(), &output, 5).status,
            Status::Inconclusive
        );
        let record = ToolReceipt::not_run(&context, preflight, None);
        assert_eq!(record.status, Status::NotRun);
        assert!(!record.dispatch_started);
        let preflight = ToolPreflight::new(&call, &[], Status::Pass, AdapterMetadata::default());
        assert_eq!(
            ToolReceipt::not_run(&context, preflight, None).status,
            Status::NotRun
        );
    }
}
