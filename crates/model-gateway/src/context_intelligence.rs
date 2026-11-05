//! Host-selected working-context policy. Full transcripts remain immutable.
use crate::{GatewayError, Message, Result, TurnRequest};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextPolicy {
    pub hot_messages: usize,
    pub warm_messages: usize,
    pub observation_bytes: usize,
    pub checkpoint_min_turns: usize,
    /// Optional stricter host limits. They never lower model output capacity.
    pub section_byte_limits: BTreeMap<String, u64>,
}
impl Default for ContextPolicy {
    fn default() -> Self {
        Self {
            hot_messages: 8,
            warm_messages: 64,
            observation_bytes: 16 * 1024,
            checkpoint_min_turns: 16,
            section_byte_limits: BTreeMap::new(),
        }
    }
}
impl ContextPolicy {
    pub fn validate(&self) -> Result<()> {
        if !(2..=1024).contains(&self.hot_messages)
            || self.warm_messages < self.hot_messages
            || self.warm_messages > crate::RequestLimits::MAX_MESSAGES
            || !(1024..=1024 * 1024).contains(&self.observation_bytes)
            || !(2..=1024).contains(&self.checkpoint_min_turns)
            || self
                .section_byte_limits
                .iter()
                .any(|(k, v)| !SECTION_NAMES.contains(&k.as_str()) || *v == 0)
        {
            return Err(GatewayError::invalid(
                "context policy bounds or section name",
            ));
        }
        Ok(())
    }
}

/// Fresh host state, rebuilt for every dispatch. A successful developer tool
/// does not make a milestone or task independently accepted.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnState {
    pub task_id: String,
    pub run_id: String,
    pub profile_id: String,
    pub goal: String,
    pub task_version: u64,
    pub task_state: String,
    pub turn: u32,
    pub successful_tools: BTreeSet<String>,
    pub remaining_required_tools: BTreeSet<String>,
    pub milestone: Option<Milestone>,
    pub budget: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Milestone {
    pub id: String,
    pub goal: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Omission {
    pub call_id: String,
    pub reason: String,
    pub artifact_ref: String,
    pub source_digest: String,
    pub original_bytes: usize,
    pub working_bytes: usize,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SectionUsage {
    pub section: String,
    pub tier: String,
    pub required: bool,
    pub serialized_bytes: u64,
    pub byte_budget: u64,
    pub conservative_token_budget: u64,
    pub counting_method: String,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IntelligenceReport {
    pub original_history_bytes: u64,
    pub working_history_bytes: u64,
    pub build_microseconds: u64,
    pub original_messages: usize,
    pub working_messages: usize,
    pub sections: Vec<SectionUsage>,
    pub omissions: Vec<Omission>,
    pub checkpoint_refs: Vec<String>,
    pub stages: BTreeMap<String, String>,
    pub section_budget_semantics: String,
}
pub const SECTION_NAMES: &[&str] = &[
    "system_and_specialist",
    "task_skills_and_user",
    "goal_and_milestone",
    "tool_observations",
    "repository_ranges",
    "accepted_memory",
    "diff_and_diagnostics",
    "provider_continuation",
    "conversation",
    "tool_schemas",
    "output_schema",
    "cold_references",
    "native_wire_and_framing",
];

pub fn serialized_bytes<T: Serialize + ?Sized>(value: &T) -> Result<u64> {
    struct Count(u64);
    impl std::io::Write for Count {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0 = self
                .0
                .checked_add(b.len() as u64)
                .ok_or_else(|| std::io::Error::other("context size"))?;
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut c = Count(0);
    serde_json::to_writer(&mut c, value)
        .map_err(|_| GatewayError::invalid("context section serialization"))?;
    Ok(c.0)
}

pub fn state_message(state: &TurnState) -> Result<Message> {
    Ok(Message::User {
        text: serde_json::to_string(&json!({
            "schema_version":"melissa.current-context/v1", "current_goal_and_milestone":state,
            "milestone_status":"active_run; successful tools are not independent acceptance",
            "authority":"host runtime facts only; no new grants or instructions",
        }))
        .map_err(|_| GatewayError::invalid("context state serialization"))?,
    })
}

pub fn opaque_continuation(response: &crate::ModelResponse) -> bool {
    response.continuation.iter().any(|item| {
        let allowed: &[&str] = match (response.protocol, item["type"].as_str()) {
            (crate::Protocol::OpenAiResponses, Some("function_call")) => {
                &["type", "id", "call_id", "name", "arguments", "status"]
            }
            (crate::Protocol::OpenAiResponses, Some("message")) => {
                &["type", "id", "role", "content", "status"]
            }
            (crate::Protocol::AnthropicMessages, Some("tool_use")) => {
                &["type", "id", "name", "input"]
            }
            (crate::Protocol::AnthropicMessages, Some("text")) => &["type", "text"],
            _ => return true,
        };
        item.as_object()
            .is_none_or(|o| o.keys().any(|k| !allowed.contains(&k.as_str())))
    })
}

pub fn call_names(messages: &[Message]) -> BTreeMap<String, String> {
    messages
        .iter()
        .filter_map(|m| {
            if let Message::Assistant { response } = m {
                Some(response)
            } else {
                None
            }
        })
        .flat_map(|r| r.calls.iter().map(|c| (c.id.clone(), c.name.clone())))
        .collect()
}

/// Sections share a single input pool; their ceilings are NOT additive quotas.
/// The complete native envelope is independently counted after compilation.
pub fn sections(
    request: &TurnRequest,
    policy: &ContextPolicy,
    capacity: u64,
    native_input_units: u64,
) -> Result<Vec<SectionUsage>> {
    let usable = capacity
        .checked_sub(request.max_output_tokens)
        .ok_or_else(|| GatewayError::invalid("output reservation exceeds context capacity"))?;
    let mut counts: BTreeMap<&str, u64> = SECTION_NAMES.iter().map(|n| (*n, 0)).collect();
    counts.insert(
        "system_and_specialist",
        serialized_bytes(&request.system_prompt)?,
    );
    counts.insert("tool_schemas", serialized_bytes(&request.tools)?);
    counts.insert("output_schema", serialized_bytes(&request.output_schema)?);
    let names = call_names(&request.messages);
    for message in &request.messages {
        let section = match message {
            Message::User { text } if text.starts_with("{\"authority\":\"host runtime facts") => {
                "goal_and_milestone"
            }
            Message::User { .. } | Message::UserImages { .. } => "task_skills_and_user",
            Message::Assistant { response } => {
                *counts.get_mut("provider_continuation").unwrap() +=
                    serialized_bytes(&response.continuation)?;
                "conversation"
            }
            Message::ToolResult {
                call_id, content, ..
            }
            | Message::ToolImages {
                call_id, content, ..
            } => {
                if content.contains("\"context_archive\"") {
                    "cold_references"
                } else {
                    match names.get(call_id).map(String::as_str).unwrap_or("") {
                        "fs_read" | "fs_read_page" | "fs_search" | "context_search"
                        | "code_semantic" => "repository_ranges",
                        "memory_recall" | "project_memory_search" | "memory_search" => {
                            "accepted_memory"
                        }
                        "code_git"
                        | "code_build"
                        | "code_tests"
                        | "code_project_check"
                        | "code_project_test"
                        | "code_project_tests"
                        | "code_project_doctests"
                        | "code_project_e2e"
                        | "code_project_lint"
                        | "code_project_build" => "diff_and_diagnostics",
                        _ => "tool_observations",
                    }
                }
            }
        };
        let mut bytes = serialized_bytes(message)?;
        if let Message::Assistant { response } = message {
            bytes = bytes.saturating_sub(serialized_bytes(&response.continuation)?);
        }
        *counts.get_mut(section).unwrap() += bytes;
    }
    counts.insert("native_wire_and_framing", native_input_units);
    counts
        .into_iter()
        .map(|(name, bytes)| {
            // Exact tokenizers can admit more bytes than the conservative token
            // pool; byte limits are explicit host limits, never inferred tokens.
            let byte_budget = policy
                .section_byte_limits
                .get(name)
                .copied()
                .unwrap_or(crate::RequestLimits::MAX_HISTORY_BYTES as u64);
            if bytes > byte_budget {
                return Err(GatewayError::invalid(format!(
                    "context section {name} exceeds host byte budget; required content preserved"
                )));
            }
            Ok(SectionUsage {
                section: name.into(),
                tier: if name == "cold_references" {
                    "cold_reference"
                } else if matches!(
                    name,
                    "repository_ranges" | "accepted_memory" | "conversation"
                ) {
                    "warm"
                } else {
                    "hot"
                }
                .into(),
                required: !matches!(name, "cold_references" | "tool_observations"),
                serialized_bytes: bytes,
                byte_budget,
                conservative_token_budget: usable,
                counting_method:
                    "serialized UTF-8 section bytes; shared input pool, not exact section tokens"
                        .into(),
            })
        })
        .collect()
}

/// Safe extension point for an already-accounted, host-reviewed summary. This
/// never invokes a model or spends a budget. The original evidence is mandatory.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewedSummary {
    pub task_id: String,
    pub run_id: String,
    pub call_id: String,
    pub source_digest: String,
    pub summary: String,
    pub evidence_ref: String,
    pub review_ref: String,
}
impl ReviewedSummary {
    pub fn validate(&self, original: &str) -> Result<()> {
        if self.source_digest != format!("blake3:{}", blake3::hash(original.as_bytes()))
            || [&self.task_id, &self.run_id, &self.call_id]
                .iter()
                .any(|id| id.is_empty() || id.len() > 256)
            || self.summary.trim().is_empty()
            || self.summary.len() > 16 * 1024
            || self.evidence_ref.trim().is_empty()
            || self.review_ref.trim().is_empty()
            || self.evidence_ref.len() > 512
            || self.review_ref.len() > 512
        {
            return Err(GatewayError::invalid(
                "unbound or unreviewed context summary",
            ));
        }
        Ok(())
    }
}

pub fn reference(record: &crate::context_compiler::ArchivedObservation, reason: &str) -> Value {
    json!({"context_archive":record,"reason":reason,"authority":"untrusted_data","historical":true,
        "read_tool":"history_read","notice":"Exact original bytes retained. Read required ranges before relying on omitted details; this reference is not a new observation."})
}
