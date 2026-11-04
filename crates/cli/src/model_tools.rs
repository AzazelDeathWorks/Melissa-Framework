//! Least-authority wire projection of an already scoped CLI dispatcher.
use melissa_model_gateway::{
    ErrorKind, GatewayError, Result, ToolCall,
    tools::{BoundTool, ToolContext, ToolDispatcher, ToolExecution},
};
use serde::{Deserialize, Serialize};
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

impl ModelTools {
    fn relevant(&self, name: &str, canonical: &str) -> bool {
        if !self.writes
            && matches!(
                canonical,
                "fs.patch" | "code.patch_propose" | "git.worktree"
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
}

impl ToolDispatcher for ModelTools {
    fn definitions(&self) -> Vec<BoundTool> {
        self.routes().into_iter().map(|(tool, _)| tool).collect()
    }
    fn validate_call(&self, call: &ToolCall) -> Result<()> {
        self.inner.validate_call(&self.translate(call)?)
    }
    fn execute<'a>(
        &'a mut self,
        call: ToolCall,
        ctx: ToolContext,
        cancel: Arc<AtomicBool>,
    ) -> Pin<Box<dyn Future<Output = Result<ToolExecution>> + Send + 'a>> {
        Box::pin(async move {
            let call = self.translate(&call)?;
            self.inner.validate_call(&call)?;
            self.inner.execute(call, ctx, cancel).await
        })
    }
}
