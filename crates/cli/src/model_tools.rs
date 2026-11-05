//! Least-authority wire projection of an already scoped CLI dispatcher.
use melissa_model_gateway::{
    ErrorKind, GatewayError, Result, ToolCall,
    tools::{BoundTool, ToolContext, ToolDispatcher, ToolExecution},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, atomic::AtomicBool},
};

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolProfile {
    #[default]
    Precision,
    Terminal,
    Hybrid,
}

impl ToolProfile {
    pub fn is_precision(&self) -> bool {
        *self == Self::Precision
    }
}

// These names resolve to existing host-registered, no-network SDK procedures.
// An alias accepts an empty object. The model cannot supply argv, cwd or env.
fn command(name: &str) -> Option<&'static str> {
    Some(match name {
        "code_project_build" => "sandbox_build",
        "code_project_check" => "sandbox_check",
        "code_project_lint" => "sandbox_lint",
        "code_format_check" => "sandbox_format_check",
        "code_project_tests" => "sandbox_tests",
        "code_project_doctests" => "sandbox_doctests",
        "code_project_e2e" => "sandbox_e2e",
        _ => return None,
    })
}

fn fixed_arguments(schema: &serde_json::Value) -> Option<serde_json::Value> {
    if let Some(value) = schema.get("const") {
        return Some(value.clone());
    }
    let properties = schema.get("properties")?.as_object()?;
    let required = schema.get("required")?.as_array()?;
    if schema.get("additionalProperties")?.as_bool()? || properties.len() != required.len() {
        return None;
    }
    properties
        .iter()
        .map(|(key, child)| {
            if !required.iter().any(|name| name.as_str() == Some(key)) {
                return None;
            }
            Some((key.clone(), fixed_arguments(child)?))
        })
        .collect::<Option<serde_json::Map<_, _>>>()
        .map(serde_json::Value::Object)
}

pub(crate) struct ModelTools {
    pub inner: Box<dyn ToolDispatcher>,
    pub profile: ToolProfile,
    pub specialist: String,
    pub writes: bool,
    pub domain: bool,
}

const EVIDENCE_READERS: [&str; 2] = ["task_evidence_read", "code_result_page"];

pub(crate) fn evidence_page(reader: &str, reference: &str) -> Value {
    json!({"tool":reader,"arguments":{"artifact_id":reference,"offset":0,"max_bytes":16384}})
}

fn page_call(reader: &str, reference: &str) -> ToolCall {
    ToolCall {
        id: "host-evidence-page".into(),
        name: reader.into(),
        arguments: evidence_page(reader, reference)["arguments"].to_string(),
    }
}

pub(crate) fn authorized_evidence_page(
    dispatcher: &dyn ToolDispatcher,
    reader: &str,
    reference: &str,
) -> Option<Value> {
    if !dispatcher
        .definitions()
        .iter()
        .any(|tool| tool.definition.name == reader)
    {
        return None;
    }
    dispatcher
        .validate_call(&page_call(reader, reference))
        .ok()?;
    Some(evidence_page(reader, reference))
}

pub(crate) fn attach_evidence_page(
    dispatcher: &dyn ToolDispatcher,
    output: &mut ToolExecution,
    reader: &str,
    reference: &str,
) {
    if let Some(page) = authorized_evidence_page(dispatcher, reader, reference)
        && let Ok(mut content) = serde_json::from_str::<Value>(&output.content)
        && let Some(object) = content.as_object_mut()
    {
        object.insert("evidence_page".into(), page);
        output.content = content.to_string();
    }
}

/// Preserve private receipts in the operator store. Advertise only the
/// authenticated normalized result that this specialist can page.
pub(crate) fn project_evidence(dispatcher: &dyn ToolDispatcher, output: &mut ToolExecution) {
    let Ok(mut content) = serde_json::from_str::<Value>(&output.content) else {
        return;
    };
    let reference = output
        .artifact_refs
        .iter()
        .find(|id| id.ends_with(":semantic-service-result"))
        .cloned();
    if reference.is_none() && content.pointer("/after_edit/sdk").is_none() {
        return;
    }
    if reference.is_some() {
        strip_operator_references(&mut content);
    }
    if let Some(sdk) = content.pointer_mut("/after_edit/sdk") {
        strip_operator_references(sdk);
    }
    if let Some(reference) = reference
        && content.get("evidence_page").is_none()
        && let Some(page) = authorized_evidence_page(dispatcher, "code_result_page", &reference)
        && let Some(object) = content.as_object_mut()
    {
        object.insert("evidence_page".into(), page);
    }
    output.content = content.to_string();
}

fn strip_operator_references(value: &mut Value) {
    if let Some(object) = value.as_object_mut() {
        for key in [
            "raw_artifact_ids",
            "raw_worker_artifact_id",
            "report_artifact_id",
            "raw_report_artifact",
            "artifact_refs",
        ] {
            object.remove(key);
        }
    }
}

impl ModelTools {
    fn relevant(&self, name: &str, canonical: &str) -> bool {
        if !self.writes
            && matches!(
                canonical,
                "fs.patch"
                    | "code.patch_propose"
                    | "git.worktree"
                    | "code.build"
                    | "code.lint"
                    | "code.diagnostics"
                    | "process.exec"
            )
        {
            return false;
        }
        if name.starts_with("code_debug_") && self.specialist != "melissa.code.debugger" {
            return false;
        }
        if name == "memory_propose" && !self.writes {
            return false;
        }
        if !self.domain && (canonical.starts_with("browser.") || canonical.starts_with("data.")) {
            return false;
        }
        if matches!(
            self.specialist.as_str(),
            "melissa.code.architect" | "melissa.code.repository-analyst"
        ) && (command(name).is_some() || matches!(name, "code_tests" | "code_build"))
        {
            return false;
        }
        true
    }

    fn routes(&self) -> Vec<(BoundTool, Option<(String, String)>)> {
        let mut routes = Vec::new();
        for tool in self.inner.definitions() {
            if !self.relevant(&tool.definition.name, &tool.canonical_id) {
                continue;
            }
            if self.profile != ToolProfile::Precision
                && let Some(alias) = command(&tool.definition.name)
                && (self.profile == ToolProfile::Terminal || alias != "sandbox_tests")
                && let Some(arguments) = fixed_arguments(&tool.definition.parameters)
            {
                let mut projected = tool.clone();
                projected.definition.name = alias.into();
                projected.definition.description = format!(
                    "Run the approved isolated {} command against current source with the task's fixed SDK and limits. Supply an empty object.",
                    alias.trim_start_matches("sandbox_")
                );
                projected.definition.parameters = serde_json::json!({
                    "type":"object","properties":{},"additionalProperties":false
                });
                routes.push((
                    projected,
                    Some((tool.definition.name.clone(), arguments.to_string())),
                ));
                continue;
            }
            if self.profile == ToolProfile::Terminal && tool.definition.name == "code_semantic" {
                continue;
            }
            routes.push((tool, None));
        }
        routes
    }

    fn translate(&self, call: &ToolCall) -> Result<ToolCall> {
        let (tool, alias) = self
            .routes()
            .into_iter()
            .find(|(tool, _)| tool.definition.name == call.name)
            .ok_or_else(|| {
                GatewayError::new(
                    ErrorKind::Authorization,
                    "Tool is outside the current specialist projection.",
                )
            })?;
        melissa_model_gateway::schema::validate_output(
            &tool.definition.parameters,
            &call.arguments,
        )?;
        let mut routed = call.clone();
        if let Some((name, arguments)) = alias {
            routed.name = name;
            routed.arguments = arguments;
        }
        Ok(routed)
    }

    fn validated_route(&self, call: &ToolCall) -> Result<ToolCall> {
        let routed = self.translate(call)?;
        let error = match self.inner.validate_call(&routed) {
            Ok(()) => return Ok(routed),
            Err(error)
                if EVIDENCE_READERS.contains(&call.name.as_str())
                    && error.kind == ErrorKind::Authorization =>
            {
                error
            }
            Err(error) => return Err(error),
        };

        // Both routes remain bound to fs.read. The alternate must be advertised
        // and independently accept the exact same reference and page bounds.
        let other = EVIDENCE_READERS
            .iter()
            .find(|name| **name != call.name)
            .unwrap();
        let mut alternate = call.clone();
        alternate.name = (*other).into();
        if let Ok(alternate) = self.translate(&alternate)
            && self.inner.validate_call(&alternate).is_ok()
        {
            return Ok(alternate);
        }
        let arguments = melissa_model_gateway::strict_json(&call.arguments)?;
        let reference = arguments["artifact_id"].as_str().unwrap_or_default();
        if error.message == "artifact is not a public task-ledger reference"
            || reference.starts_with("recovery-failure:")
            || reference.starts_with("workflow-")
            || (reference.starts_with("model-tool:")
                && !reference.ends_with(":semantic-service-result"))
        {
            return Err(GatewayError::invalid(
                "No authorized reader for that reference. Use the exact evidence_page from a current result or rely on the inline diagnostic.",
            ));
        }
        Err(error)
    }
}

impl ToolDispatcher for ModelTools {
    fn definitions(&self) -> Vec<BoundTool> {
        self.routes().into_iter().map(|(tool, _)| tool).collect()
    }
    fn validate_call(&self, call: &ToolCall) -> Result<()> {
        self.validated_route(call).map(|_| ())
    }
    fn execute<'a>(
        &'a mut self,
        call: ToolCall,
        ctx: ToolContext,
        cancel: Arc<AtomicBool>,
    ) -> Pin<Box<dyn Future<Output = Result<ToolExecution>> + Send + 'a>> {
        Box::pin(async move {
            let call = self.validated_route(&call)?;
            let mut output = self.inner.execute(call, ctx, cancel).await?;
            project_evidence(&*self.inner, &mut output);
            if let Ok(mut content) = serde_json::from_str::<Value>(&output.content)
                && let Some(sdk) = content.pointer_mut("/after_edit/sdk")
                && let Some(summary) = melissa_model_gateway::observations::normalize(
                    "code_project_check",
                    &sdk.to_string(),
                )
            {
                // The full SDK check already lives in the authenticated
                // operator receipt. Keep actionable diagnostics in the turn.
                *sdk = summary;
                output.content = content.to_string();
            }
            Ok(output)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct ScopedPages;
    impl ToolDispatcher for ScopedPages {
        fn definitions(&self) -> Vec<BoundTool> {
            EVIDENCE_READERS.iter().map(|reader| BoundTool {
                canonical_id: "fs.read".into(),
                definition: melissa_model_gateway::ToolDefinition {
                    name: (*reader).into(),
                    description: "Fixture evidence reader".into(),
                    parameters: json!({"type":"object","properties":{
                        "artifact_id":{"type":"string"},"offset":{"type":"integer"},"max_bytes":{"type":"integer"}
                    },"required":["artifact_id","offset","max_bytes"],"additionalProperties":false}),
                    strict: false,
                },
            }).collect()
        }

        fn validate_call(&self, call: &ToolCall) -> Result<()> {
            let arguments = melissa_model_gateway::strict_json(&call.arguments)?;
            match (call.name.as_str(), arguments["artifact_id"].as_str()) {
                ("task_evidence_read", Some("workflow-check:public"))
                | ("code_result_page", Some("inv:semantic-service-result")) => Ok(()),
                ("task_evidence_read", _) => Err(GatewayError::new(
                    ErrorKind::Authorization,
                    "artifact is not a public task-ledger reference",
                )),
                _ => Err(GatewayError::new(
                    ErrorKind::Authorization,
                    "fixture scope denied",
                )),
            }
        }

        fn execute<'a>(
            &'a mut self,
            _: ToolCall,
            _: ToolContext,
            _: Arc<AtomicBool>,
        ) -> Pin<Box<dyn Future<Output = Result<ToolExecution>> + Send + 'a>> {
            unreachable!("preflight test")
        }
    }

    #[test]
    fn exact_alternate_reader_recovers_public_evidence_and_keeps_private_bytes_closed() {
        let tools = ModelTools {
            inner: Box::new(ScopedPages),
            profile: ToolProfile::Precision,
            specialist: "melissa.code.test-engineer".into(),
            writes: false,
            domain: false,
        };
        for (wrong, reference, right) in [
            (
                "code_result_page",
                "workflow-check:public",
                "task_evidence_read",
            ),
            (
                "task_evidence_read",
                "inv:semantic-service-result",
                "code_result_page",
            ),
        ] {
            let call = page_call(wrong, reference);
            let routed = tools.validated_route(&call).unwrap();
            assert_eq!(routed.name, right);
            assert_eq!(routed.arguments, call.arguments);
        }
        for reader in EVIDENCE_READERS {
            for reference in ["recovery-failure:private", "model-tool:private:raw-log"] {
                let error = tools
                    .validated_route(&page_call(reader, reference))
                    .unwrap_err();
                assert_eq!(error.kind, ErrorKind::Invalid);
                assert!(error.message.contains("No authorized reader"));
            }
        }
        let mut observation = ToolExecution {
            content: json!({"status":"passed","raw_artifact_ids":["private-log"],
                "raw_worker_artifact_id":"private-worker","report_artifact_id":"private-sdk"})
            .to_string(),
            success: true,
            cancelled: false,
            requires_reconciliation: false,
            artifact_refs: vec!["inv:semantic-service-result".into(), "private-log".into()],
        };
        project_evidence(&*tools.inner, &mut observation);
        let output: Value = serde_json::from_str(&observation.content).unwrap();
        assert_eq!(output["evidence_page"]["tool"], "code_result_page");
        assert!(!observation.content.contains("private-"));
        assert!(observation.artifact_refs.contains(&"private-log".into()));
    }
}
