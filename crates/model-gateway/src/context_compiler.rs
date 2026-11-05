//! Per-dispatch native envelope preflight and reversible observation archiving.
//! UTF-8 wire units are conservative host accounting, not measured model tokens.
use crate::{
    ErrorKind, GatewayError, Message, Result, TurnRequest,
    transport::{HttpTransport, ProviderEndpoint},
};
use melissa_local_store::{ArtifactRequest, LocalStore, StoreError};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Default)]
pub struct ContextCompiler {
    archived: std::collections::BTreeMap<(String, String, String), ArchivedObservation>,
    counter: Option<Box<dyn NativeTokenCounter>>,
    wire_tokenizer: Option<crate::local_tokenizer::LocalWireTokenizer>,
    intelligence: Option<crate::context_intelligence::ContextPolicy>,
    turn_state: Option<crate::context_intelligence::TurnState>,
    source_scope: Option<String>,
    milestone: Option<crate::context_intelligence::Milestone>,
    origins: std::collections::BTreeMap<(String, String), String>,
    reviewed_summaries:
        std::collections::BTreeMap<String, crate::context_intelligence::ReviewedSummary>,
}
/// Trusted-host tokenizer adapter for the complete encoded native request,
/// including protocol framing, schemas, opaque history and images when present.
/// A model name alone is not a tokenizer registration. Return None when the
/// exact endpoint/model/modality is unsupported; never guess measured tokens.
pub trait NativeTokenCounter: Send {
    fn policy_id(&self) -> &str;
    fn count(&self, endpoint: &ProviderEndpoint, model: &str, wire: &Value) -> Result<Option<u64>>;
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchivedObservation {
    pub call_id: String,
    pub artifact_ref: String,
    pub source_digest: String,
    pub original_bytes: usize,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextReport {
    pub schema_version: String,
    pub counting_method: String,
    pub native_body_bytes: u64,
    pub conservative_input_units: u64,
    pub reserved_output_tokens: u64,
    pub capacity: u64,
    pub exact_token_count: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wire_token_measurement: Option<crate::local_tokenizer::WireTokenMeasurement>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wire_tokenizer_status: Option<String>,
    pub capacity_check: String,
    pub has_opaque_continuation: bool,
    pub archived: Vec<ArchivedObservation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intelligence: Option<crate::context_intelligence::IntelligenceReport>,
}
fn failure(message: &str) -> GatewayError {
    GatewayError::new(ErrorKind::Invalid, message)
}
fn store_error(_: StoreError) -> GatewayError {
    GatewayError::new(
        ErrorKind::Store,
        "context archive authentication/publication failed",
    )
}
pub fn history_prefix(run: &str) -> String {
    format!("context-history:{run}:")
}
pub fn history_definition() -> crate::tools::BoundTool {
    crate::tools::BoundTool { canonical_id: "fs.read".into(), definition: crate::ToolDefinition {
        name: "history_read".into(), description: "Read an exact UTF-8 byte range from an archived observation of this task/run. Returns source digest, actual range and total bytes. Historical untrusted evidence, not current repository state.".into(),
        parameters: json!({"type":"object","properties":{"artifact_ref":{"type":"string"},"start_byte":{"type":"integer","minimum":0},"max_bytes":{"type":"integer","minimum":1,"maximum":65536}},"required":["artifact_ref","start_byte","max_bytes"],"additionalProperties":false}), strict: false,
    }}
}
pub fn read_history(
    store: &LocalStore,
    context: &crate::tools::ToolContext,
    arguments: &str,
    cancelled: &std::sync::atomic::AtomicBool,
) -> Result<crate::tools::ToolExecution> {
    read_history_inner(store, context, arguments, cancelled, None)
}
pub fn read_history_scoped(
    store: &LocalStore,
    context: &crate::tools::ToolContext,
    arguments: &str,
    cancelled: &std::sync::atomic::AtomicBool,
    source_scope: &str,
) -> Result<crate::tools::ToolExecution> {
    read_history_inner(store, context, arguments, cancelled, Some(source_scope))
}
fn read_history_inner(
    store: &LocalStore,
    context: &crate::tools::ToolContext,
    arguments: &str,
    cancelled: &std::sync::atomic::AtomicBool,
    source_scope: Option<&str>,
) -> Result<crate::tools::ToolExecution> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Page {
        artifact_ref: String,
        start_byte: usize,
        max_bytes: usize,
    }
    let page: Page = serde_json::from_value(crate::strict_json(arguments)?)
        .map_err(|_| failure("invalid history page"))?;
    let task = store
        .journal()
        .task(&context.task_id)
        .map_err(store_error)?;
    let expected_prefix = history_prefix(&context.run_id);
    let scoped_digest = page
        .artifact_ref
        .strip_prefix(&expected_prefix)
        .filter(|hash| {
            hash.len() == 64
                && hash
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        });
    if cancelled.load(std::sync::atomic::Ordering::Acquire)
        || !task
            .contract
            .selected_profile_ids
            .contains(&context.profile_id)
        || !task
            .contract
            .allowed_effects
            .iter()
            .any(|s| s == "workspace.read")
        || !matches!(
            task.record.state,
            melissa_core_contracts::TaskState::Running
                | melissa_core_contracts::TaskState::Repairing
                | melissa_core_contracts::TaskState::Verifying
        )
        || scoped_digest.is_none()
    {
        return Err(GatewayError::new(
            ErrorKind::Authorization,
            "history scope is inactive or not granted",
        ));
    }
    if !(1..=65536).contains(&page.max_bytes) {
        return Err(failure("history page limit"));
    }
    let scope_id = format!("context-history-scope:{}", context.run_id);
    match store.get_artifact(&context.task_id, &scope_id) {
        Ok(bytes) if source_scope.is_some_and(|s| s.as_bytes() == bytes) => {}
        Err(StoreError::NotFound(_)) if source_scope.is_none() => {}
        _ => {
            return Err(GatewayError::new(
                ErrorKind::Authorization,
                "history source visibility changed or was not bound; re-observe through current scoped tools",
            ));
        }
    }
    let meta = store
        .artifact_metadata(&context.task_id, &page.artifact_ref)
        .map_err(store_error)?;
    if meta.logical_type != "context.archived-observation"
        || meta.digest.strip_prefix("blake3:") != scoped_digest
        || source_scope.is_some_and(|scope| meta.schema_version != archive_schema(Some(scope)))
    {
        return Err(failure("not an archived observation"));
    }
    let raw = store
        .get_artifact(&context.task_id, &page.artifact_ref)
        .map_err(store_error)?;
    let text = std::str::from_utf8(&raw).map_err(|_| failure("history UTF-8"))?;
    if page.start_byte > text.len() || !text.is_char_boundary(page.start_byte) {
        return Err(failure("history byte boundary"));
    }
    let mut end = page
        .start_byte
        .saturating_add(page.max_bytes)
        .min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    if end == page.start_byte && end < text.len() {
        return Err(failure("history page is too small for one UTF-8 character"));
    }
    Ok(crate::tools::ToolExecution { content: json!({"artifact_ref":page.artifact_ref,"source_digest":meta.digest,"start_byte":page.start_byte,"end_byte":end,"total_bytes":text.len(),"content":&text[page.start_byte..end],"authority":"untrusted_data","historical":true}).to_string(), success:true, cancelled:false, requires_reconciliation:false, artifact_refs:vec![page.artifact_ref] })
}
fn archive_schema(scope: Option<&str>) -> String {
    scope.map_or_else(
        || "melissa.context-observation/v1".into(),
        |s| {
            format!(
                "melissa.context-observation/v2:{}",
                blake3::hash(s.as_bytes())
            )
        },
    )
}
fn archive(
    store: &mut LocalStore,
    task: &str,
    run: &str,
    call: &str,
    content: &str,
    source_scope: Option<&str>,
) -> Result<ArchivedObservation> {
    let hash = blake3::hash(content.as_bytes()).to_hex().to_string();
    let id = format!("{}{hash}", history_prefix(run));
    match store.artifact_metadata(task, &id) {
        Ok(meta) => {
            if meta.logical_type != "context.archived-observation"
                || meta.schema_version != archive_schema(source_scope)
                || store.get_artifact(task, &id).map_err(store_error)? != content.as_bytes()
            {
                return Err(failure("context archive collision"));
            }
        }
        Err(StoreError::NotFound(_)) => {
            store
                .put_artifact(
                    &ArtifactRequest {
                        artifact_id: id.clone(),
                        task_id: task.into(),
                        content_type: "text/plain; charset=utf-8".into(),
                        logical_type: "context.archived-observation".into(),
                        schema_version: archive_schema(source_scope),
                        retain_until_ms: 0,
                        legal_hold: false,
                    },
                    content.as_bytes(),
                )
                .map_err(store_error)?;
        }
        Err(e) => return Err(store_error(e)),
    }
    Ok(ArchivedObservation {
        call_id: call.into(),
        artifact_ref: id,
        source_digest: format!("blake3:{hash}"),
        original_bytes: content.len(),
    })
}
impl ContextCompiler {
    fn prepare(
        &mut self,
        request: &mut TurnRequest,
        endpoint: &ProviderEndpoint,
        store: &mut LocalStore,
        task: &str,
        run: &str,
        archived: &mut Vec<ArchivedObservation>,
    ) -> Result<Option<crate::context_intelligence::IntelligenceReport>> {
        use crate::context_intelligence::{
            IntelligenceReport, Omission, call_names, reference, serialized_bytes,
        };
        let Some(policy) = self.intelligence.clone() else {
            return Ok(None);
        };
        let mut seen = std::collections::BTreeMap::new();
        for tool in &request.tools {
            if let Some(previous) = seen.insert(tool.name.clone(), tool.clone())
                && previous != *tool
            {
                return Err(failure("conflicting duplicate tool schemas"));
            }
        }
        let mut names = std::collections::BTreeSet::new();
        request.tools.retain(|t| names.insert(t.name.clone()));
        request.validate_structure()?;
        for message in &request.messages {
            if let Message::Assistant { response } = message {
                if response.continuation.is_empty()
                    || !matches!(
                        response.stop,
                        crate::StopReason::EndTurn
                            | crate::StopReason::ToolCalls
                            | crate::StopReason::Refusal
                    )
                {
                    return Err(failure("incomplete native history before compaction"));
                }
                match response.protocol {
                    crate::Protocol::OpenAiResponses => {
                        crate::openai::validate_history_response(response)?
                    }
                    crate::Protocol::AnthropicMessages => {
                        crate::anthropic::validate_continuation(response)?
                    }
                }
            }
        }
        let opaque = request
            .messages
            .iter()
            .any(|m| matches!(m,Message::Assistant{response} if crate::context_intelligence::opaque_continuation(response)));
        if request.messages.iter().any(|m|matches!(m,Message::Assistant{response}
            if response.protocol!=endpoint.protocol || (!response.continuation.is_empty() && response.requested_model!=request.model))) {
            return Err(GatewayError::protocol("continuation cannot cross protocol/model identity"));
        }
        if request
            .messages
            .iter()
            .any(|m| matches!(m, Message::Assistant { .. }))
        {
            let origin = format!(
                "{}|{}|{:?}|{}",
                endpoint.provider_id, endpoint.url, endpoint.protocol, request.model
            );
            let key = (task.into(), run.into());
            if self.origins.get(&key).is_some_and(|old| old != &origin) {
                return Err(GatewayError::protocol(
                    "opaque continuation cannot cross provider origin",
                ));
            }
            self.origins.insert(key, origin);
        }
        if let Some(scope) = &self.source_scope {
            let id = format!("context-history-scope:{run}");
            match store.get_artifact(task, &id) {
                Ok(bytes) if bytes == scope.as_bytes() => {}
                Err(StoreError::NotFound(_)) => {
                    store
                        .put_artifact(
                            &ArtifactRequest {
                                artifact_id: id,
                                task_id: task.into(),
                                content_type: "text/plain".into(),
                                logical_type: "context.source-scope".into(),
                                schema_version: "melissa.context-source-scope/v1".into(),
                                retain_until_ms: 0,
                                legal_hold: false,
                            },
                            scope.as_bytes(),
                        )
                        .map_err(store_error)?;
                }
                _ => {
                    return Err(GatewayError::new(
                        ErrorKind::Authorization,
                        "context source visibility changed on resume",
                    ));
                }
            }
        }
        let mut report=IntelligenceReport { original_history_bytes:serialized_bytes(&request.messages)?,
            original_messages:request.messages.len(), section_budget_semantics:"section ceilings share one input pool; complete native wire plus output reservation is separately checked".into(),
            ..Default::default() };
        report.stages = std::collections::BTreeMap::from([
            (
                "deterministic_filter".into(),
                "enabled; old observations only".into(),
            ),
            (
                "duplicate_observations".into(),
                "enabled; exact digest only".into(),
            ),
            (
                "typed_normalization".into(),
                "enabled; known dispatcher formats only".into(),
            ),
            (
                "milestone_checkpoint".into(),
                if opaque {
                    "preserve_native_continuation"
                } else {
                    "no_eligible_completed_prefix"
                }
                .into(),
            ),
            (
                "model_assisted_summary".into(),
                "not_configured; no inference dispatched".into(),
            ),
            (
                "native_provider_compaction".into(),
                "not_registered; opaque bytes preserved verbatim".into(),
            ),
        ]);
        if !request.tools.iter().any(|t| t.name == "history_read") {
            report.stages.insert(
                "archiving".into(),
                "disabled; scoped history reader unavailable".into(),
            );
        } else {
            let call_names = call_names(&request.messages);
            let hot = request.messages.len().saturating_sub(policy.hot_messages);
            let cold = request.messages.len().saturating_sub(policy.warm_messages);
            let mut latest = std::collections::BTreeMap::new();
            for (i, m) in request.messages.iter().enumerate() {
                if let Message::ToolResult { content, .. } = m {
                    latest.insert(*blake3::hash(content.as_bytes()).as_bytes(), i);
                }
            }
            for (i, message) in request.messages.iter_mut().enumerate() {
                let Message::ToolResult {
                    call_id, content, ..
                } = message
                else {
                    continue;
                };
                if call_names.get(call_id).is_some_and(|n| n == "history_read") && i >= hot {
                    continue;
                }
                let hash = *blake3::hash(content.as_bytes()).as_bytes();
                let duplicate = i < hot && latest.get(&hash).is_some_and(|last| *last > i);
                let normalized = crate::observations::normalize(
                    call_names.get(call_id).map(String::as_str).unwrap_or(""),
                    content,
                );
                let reason = if duplicate {
                    "duplicate_observation"
                } else if normalized.is_some() {
                    "typed_observation"
                } else if i < cold {
                    "cold_observation"
                } else if i < hot && content.len() > policy.observation_bytes {
                    "warm_observation_budget"
                } else {
                    continue;
                };
                // Small observations cost less verbatim than their references.
                if content.len() < 2048 {
                    continue;
                }
                let record = archive(
                    store,
                    task,
                    run,
                    call_id,
                    content,
                    self.source_scope.as_deref(),
                )?;
                let mut replacement = reference(&record, reason);
                if !duplicate && let Some(view) = normalized {
                    replacement["observation"] = view;
                    replacement["full_log_ref"] = json!(record.artifact_ref);
                    replacement["diff_ref"] = json!(record.artifact_ref);
                }
                if i < hot
                    && let Some(summary) = self.reviewed_summaries.get(&record.source_digest)
                    && summary.task_id == task
                    && summary.run_id == run
                    && summary.call_id == *call_id
                {
                    summary.validate(content)?;
                    let evidence = store
                        .get_artifact(task, &summary.evidence_ref)
                        .map_err(store_error)?;
                    let review = store
                        .get_artifact(task, &summary.review_ref)
                        .map_err(store_error)?;
                    let review: Value = crate::checkpoint::decode(&review)?;
                    if evidence != content.as_bytes()
                        || review["task_id"] != task
                        || review["run_id"] != run
                        || review["call_id"] != *call_id
                        || review["approved"] != true
                        || review["source_digest"] != summary.source_digest
                        || review["summary_digest"]
                            != format!("blake3:{}", blake3::hash(summary.summary.as_bytes()))
                    {
                        return Err(failure("summary evidence/review mismatch"));
                    }
                    replacement["reviewed_summary"] = json!({"text":summary.summary,"review_ref":summary.review_ref,"authority":"untrusted_data"});
                    report.stages.insert(
                        "model_assisted_summary".into(),
                        "reviewed_external_summary; no inference dispatched by compiler".into(),
                    );
                }
                let replacement = replacement.to_string();
                if replacement.len() >= content.len()
                    || replacement.len() > policy.observation_bytes
                {
                    continue;
                }
                report.omissions.push(Omission {
                    call_id: call_id.clone(),
                    reason: reason.into(),
                    artifact_ref: record.artifact_ref.clone(),
                    source_digest: record.source_digest.clone(),
                    original_bytes: content.len(),
                    working_bytes: replacement.len(),
                });
                *content = replacement;
                archived.push(record);
            }
            self.checkpoint_completed(request, &policy, store, task, run, &mut report, archived)?;
        }
        if let Some(state) = &self.turn_state {
            if state.task_id != task || state.run_id != run {
                return Err(failure("current context task/run mismatch"));
            }
            request
                .messages
                .push(crate::context_intelligence::state_message(state)?);
        }
        Ok(Some(report))
    }

    #[allow(clippy::too_many_arguments)]
    fn checkpoint_completed(
        &self,
        request: &mut TurnRequest,
        policy: &crate::context_intelligence::ContextPolicy,
        store: &mut LocalStore,
        task: &str,
        run: &str,
        report: &mut crate::context_intelligence::IntelligenceReport,
        archived: &mut Vec<ArchivedObservation>,
    ) -> Result<()> {
        // Native continuation may contain signatures, reasoning or provider
        // function items. Never splice, translate or summarize those bytes.
        if request
            .messages
            .iter()
            .any(|m| matches!(m,Message::Assistant{response} if crate::context_intelligence::opaque_continuation(response)))
        {
            return Ok(());
        }
        let cutoff = request.messages.len().saturating_sub(policy.warm_messages);
        let latest_tools: std::collections::BTreeMap<_, _> = request
            .messages
            .iter()
            .enumerate()
            .filter_map(|(i, m)| {
                if let Message::Assistant { response } = m {
                    Some((i, response))
                } else {
                    None
                }
            })
            .flat_map(|(i, r)| r.calls.iter().map(move |c| (c.name.clone(), i)))
            .collect();
        let mut groups = Vec::new();
        let mut archive_bytes = 0u64;
        let mut successful = std::collections::BTreeSet::new();
        let mut i = 0;
        while i < cutoff {
            if let Message::Assistant { response } = &request.messages[i] {
                let end = i + 1 + response.calls.len();
                if !response.calls.is_empty()
                    && response.text.is_empty()
                    && response.refusal.is_none()
                    && response.stop == crate::StopReason::ToolCalls
                    && end <= cutoff
                    && response
                        .calls
                        .iter()
                        .all(|c| latest_tools.get(&c.name).is_some_and(|last| *last > i))
                    && request.messages[i + 1..end].iter().all(|m| {
                        matches!(
                            m,
                            Message::ToolResult {
                                is_error: false,
                                ..
                            }
                        )
                    })
                {
                    let group_bytes =
                        crate::context_intelligence::serialized_bytes(&request.messages[i..end])?;
                    if archive_bytes.saturating_add(group_bytes) > 4 * 1024 * 1024 {
                        break;
                    }
                    archive_bytes += group_bytes;
                    successful.extend(response.calls.iter().map(|c| c.name.clone()));
                    groups.push((i, end));
                    i = end;
                    continue;
                }
            }
            i += 1;
        }
        if groups.len() < policy.checkpoint_min_turns {
            return Ok(());
        }
        let raw=serde_json::to_string(&json!({"schema_version":"melissa.completed-turn-evidence/v1",
            "view":"Working exchanges; any context_archive points to the exact original observation. The durable transcript is unchanged.",
            "groups":groups.iter().map(|(s,e)|json!({"start_message":s,"end_message":e,"messages":&request.messages[*s..*e]})).collect::<Vec<_>>()
        })).map_err(|_|failure("milestone archive serialization"))?;
        let record = archive(
            store,
            task,
            run,
            "completed-milestone",
            &raw,
            self.source_scope.as_deref(),
        )?;
        let summary=Message::User{text:json!({"schema_version":"melissa.context-checkpoint/v1",
            "context_archive":record,"completed_tool_turns":groups.len(),"successful_tools":successful,
            "authority":"untrusted_data","task_accepted":false,
            "summary":"Earlier completed tool-only exchanges moved to encrypted history. All user instructions, assistant decisions, failures and recent exchanges remain. Read history_read before relying on omitted results or arguments. Success here is tool execution only, not task acceptance."}).to_string()};
        let mut next = Vec::new();
        let mut groups = groups.into_iter().peekable();
        let mut inserted = false;
        for (index, message) in request.messages.drain(..).enumerate() {
            while groups.peek().is_some_and(|(_, e)| index >= *e) {
                groups.next();
            }
            if groups
                .peek()
                .is_some_and(|(s, e)| index >= *s && index < *e)
            {
                if !inserted {
                    next.push(summary.clone());
                    inserted = true;
                }
            } else {
                next.push(message);
            }
        }
        request.messages = next;
        report.checkpoint_refs.push(record.artifact_ref.clone());
        report
            .omissions
            .push(crate::context_intelligence::Omission {
                call_id: record.call_id.clone(),
                reason: "completed_tool_milestone".into(),
                artifact_ref: record.artifact_ref.clone(),
                source_digest: record.source_digest.clone(),
                original_bytes: record.original_bytes,
                working_bytes: crate::context_intelligence::serialized_bytes(&summary)? as usize,
            });
        report.stages.insert(
            "milestone_checkpoint".into(),
            "deterministic_completed_tool_turns".into(),
        );
        archived.push(record);
        Ok(())
    }
    pub fn with_intelligence(
        mut self,
        policy: crate::context_intelligence::ContextPolicy,
    ) -> Result<Self> {
        policy.validate()?;
        self.intelligence = Some(policy);
        Ok(self)
    }
    pub fn intelligence_enabled(&self) -> bool {
        self.intelligence.is_some()
    }
    pub fn milestone(&self) -> Option<crate::context_intelligence::Milestone> {
        self.milestone.clone()
    }
    pub fn set_milestone(&mut self, id: String, goal: String) -> Result<()> {
        if self.intelligence.is_none()
            || id.trim().is_empty()
            || id.len() > 256
            || goal.trim().is_empty()
            || goal.len() > 64 * 1024
        {
            return Err(failure("context milestone bounds"));
        }
        self.milestone = Some(crate::context_intelligence::Milestone { id, goal });
        Ok(())
    }
    pub fn set_turn_state(&mut self, state: crate::context_intelligence::TurnState) {
        self.turn_state = Some(state);
    }
    pub fn set_source_scope(&mut self, scope: String) -> Result<()> {
        if self.intelligence.is_none() || scope.trim().is_empty() || scope.len() > 512 {
            return Err(failure("source visibility identity"));
        }
        self.source_scope = Some(scope);
        Ok(())
    }
    pub fn with_reviewed_summary(
        mut self,
        summary: crate::context_intelligence::ReviewedSummary,
    ) -> Self {
        self.reviewed_summaries
            .insert(summary.source_digest.clone(), summary);
        self
    }
    pub fn with_wire_tokenizer(
        mut self,
        tokenizer: crate::local_tokenizer::LocalWireTokenizer,
    ) -> Self {
        self.wire_tokenizer = Some(tokenizer);
        self
    }
    pub fn with_counter(mut self, counter: Box<dyn NativeTokenCounter>) -> Result<Self> {
        if counter.policy_id().trim().is_empty() || counter.policy_id().len() > 512 {
            return Err(failure("token counter policy identity"));
        }
        self.counter = Some(counter);
        Ok(self)
    }
    pub fn policy_id(&self) -> String {
        let mut base = format!(
            "native-context/v1:{}",
            self.counter
                .as_ref()
                .map_or("utf8-framing", |c| c.policy_id())
        );
        if let Some(policy) = &self.intelligence {
            let bytes = serde_json::to_vec(&(
                policy,
                &self.source_scope,
                &self.reviewed_summaries,
                &self.milestone,
            ))
            .expect("serializable context policy");
            base = format!("{base}:intelligence/v2:{}", blake3::hash(&bytes));
        }
        match &self.wire_tokenizer {
            Some(t) => format!("{base}:{}", t.policy_id()),
            None => base,
        }
    }
    pub fn working_messages(
        &self,
        messages: &[Message],
        task: &str,
        run: &str,
    ) -> (Vec<Message>, Vec<ArchivedObservation>) {
        if self.intelligence.is_some() {
            return (messages.to_vec(), vec![]);
        }
        let mut records = Vec::new();
        let working = messages
            .iter()
            .enumerate()
            .map(|(i, message)| {
                if let Message::ToolResult {
                    call_id,
                    content,
                    is_error,
                } = message
                    && i < messages.len().saturating_sub(8)
                    && let Some(record) =
                        self.archived
                            .get(&(task.into(), run.into(), call_id.clone()))
                    && record.source_digest
                        == format!("blake3:{}", blake3::hash(content.as_bytes()))
                {
                    records.push(record.clone());
                    Message::ToolResult {
                        call_id: call_id.clone(),
                        content: placeholder(record),
                        is_error: *is_error,
                    }
                } else {
                    message.clone()
                }
            })
            .collect();
        (working, records)
    }
    /// Required contracts, schemas, user messages and provider-owned continuation
    /// bytes are never shortened. Only old tool observations can be archived;
    /// their paired calls/results remain in place and a scoped page tool can
    /// recover the exact bytes. The full transcript is owned by Gateway.
    #[allow(clippy::too_many_arguments)] // Explicit host scope is separate from model request data.
    pub fn compile(
        &mut self,
        request: &mut TurnRequest,
        transport: &HttpTransport,
        endpoint: &ProviderEndpoint,
        capacity: u64,
        store: &mut LocalStore,
        task: &str,
        run: &str,
        mut archived: Vec<ArchivedObservation>,
    ) -> Result<(Value, ContextReport)> {
        let started = std::time::Instant::now();
        let mut intelligence = self.prepare(request, endpoint, store, task, run, &mut archived)?;
        request.validate_structure()?;
        let has_images = request
            .messages
            .iter()
            .any(|m| matches!(m, Message::UserImages { .. } | Message::ToolImages { .. }));
        let can_archive = request.tools.iter().any(|t| t.name == "history_read");
        let protected_start =
            request
                .messages
                .len()
                .saturating_sub(self.intelligence.as_ref().map_or(8, |p| {
                    p.hot_messages + usize::from(self.turn_state.is_some())
                }));
        let has_opaque = request.messages.iter().any(
            |m| matches!(m, Message::Assistant { response } if !response.continuation.is_empty()),
        );
        let mut scanned = 0;
        // Resume can begin with an uncached transcript larger than the wire
        // envelope. Count without a second full buffer, then archive before
        // calling the protocol encoder's unchanged request-byte validator.
        let mut size = ByteCount(0);
        serde_json::to_writer(&mut size, request).map_err(|_| failure("history byte count"))?;
        let mut excess = size
            .0
            .saturating_sub(transport.request_limits().max_history_bytes as u64);
        while excess > 0 {
            let candidate = if can_archive {
                (scanned..protected_start).find(|&i| matches!(&request.messages[i], Message::ToolResult {content,..} if content.len()>2048))
            } else {
                None
            };
            let Some(i) = candidate else {
                // The conservative per-replacement saving can understate the
                // actual saving. Recount before rejecting a now-fitting view.
                let mut remaining = ByteCount(0);
                serde_json::to_writer(&mut remaining, &request)
                    .map_err(|_| failure("history byte recount"))?;
                if remaining.0 <= transport.request_limits().max_history_bytes as u64 {
                    break;
                }
                return Err(failure(
                    "required history exceeds wire byte envelope; no request dispatched",
                ));
            };
            scanned = i + 1;
            if let Message::ToolResult {
                call_id, content, ..
            } = &mut request.messages[i]
            {
                let record = archive(
                    store,
                    task,
                    run,
                    call_id,
                    content,
                    self.source_scope.as_deref(),
                )?;
                let replacement = placeholder(&record);
                excess = excess.saturating_sub(
                    content
                        .len()
                        .saturating_sub(replacement.len().saturating_mul(2).saturating_add(256))
                        as u64,
                );
                *content = replacement;
                self.archived
                    .insert((task.into(), run.into(), call_id.clone()), record.clone());
                archived.push(record);
            }
        }
        loop {
            let wire = transport.request_body(endpoint, request)?;
            let native_body_bytes = serde_json::to_vec(&wire)
                .map_err(|_| failure("native context serialization"))?
                .len() as u64;
            // Covers framing beyond the serialized native request. No guessed
            // chars/4 conversion and no assertion of exact provider token counts.
            let conservative_input_units = native_body_bytes
                .saturating_add(1024)
                .saturating_add(request.messages.len() as u64 * 64);
            let exact = self
                .counter
                .as_ref()
                .map(|c| c.count(endpoint, &request.model, &wire))
                .transpose()?
                .flatten();
            let input = exact.unwrap_or(conservative_input_units);
            // Preserve existing vision behavior when no matching image tokenizer
            // is installed. Do not mistake compressed PNG/base64 bytes for image
            // tokens, shorten images, or report a verified capacity check.
            let unknown_images = has_images && exact.is_none();
            if unknown_images
                || input
                    .checked_add(request.max_output_tokens)
                    .is_some_and(|n| n <= capacity)
            {
                if let Some(report) = intelligence.as_mut() {
                    report.sections = crate::context_intelligence::sections(
                        request,
                        self.intelligence.as_ref().unwrap(),
                        capacity,
                        conservative_input_units,
                    )?;
                    report.working_history_bytes =
                        crate::context_intelligence::serialized_bytes(&request.messages)?;
                    report.working_messages = request.messages.len();
                    report.build_microseconds =
                        started.elapsed().as_micros().min(u64::MAX as u128) as u64;
                    for record in &archived {
                        if !report.omissions.iter().any(|o| {
                            o.call_id == record.call_id && o.artifact_ref == record.artifact_ref
                        }) {
                            report
                                .omissions
                                .push(crate::context_intelligence::Omission {
                                    call_id: record.call_id.clone(),
                                    reason: "capacity_pressure".into(),
                                    artifact_ref: record.artifact_ref.clone(),
                                    source_digest: record.source_digest.clone(),
                                    original_bytes: record.original_bytes,
                                    working_bytes: placeholder(record).len(),
                                });
                        }
                    }
                }
                let mut wire_tokenizer_status = None;
                let wire_token_measurement = if let Some(tokenizer) = &self.wire_tokenizer {
                    if has_images || has_opaque {
                        wire_tokenizer_status =
                            Some("not_measured_images_or_provider_continuation".into());
                        None
                    } else {
                        let measured = tokenizer.measure(endpoint, &request.model, &wire)?;
                        wire_tokenizer_status = Some(
                            if measured.is_some() {
                                "measured_wire_only; capacity_and_budget_unchanged"
                            } else {
                                "not_measured_binding_or_size"
                            }
                            .into(),
                        );
                        measured
                    }
                } else {
                    None
                };
                return Ok((
                    wire,
                    ContextReport {
                        schema_version: "melissa.native-context-report/v1".into(),
                        counting_method: if exact.is_some() {
                            self.policy_id()
                        } else {
                            "native-utf8-bytes-plus-framing-v1; not a provider tokenizer".into()
                        },
                        native_body_bytes,
                        conservative_input_units,
                        reserved_output_tokens: request.max_output_tokens,
                        capacity,
                        exact_token_count: exact,
                        wire_token_measurement,
                        wire_tokenizer_status,
                        capacity_check: if unknown_images {
                            "not_measured_multimodal"
                        } else if exact.is_some() {
                            "host_tokenizer"
                        } else {
                            "conservative_text_units"
                        }
                        .into(),
                        has_opaque_continuation: has_opaque,
                        archived,
                        intelligence,
                    },
                ));
            }
            let mut debt = input
                .saturating_add(request.max_output_tokens)
                .saturating_sub(capacity);
            let batch_start = scanned;
            while debt > 0 {
                let candidate = if can_archive {
                    (scanned..protected_start).find(|&i| matches!(&request.messages[i], Message::ToolResult { content, .. } if content.len() > 2048))
                } else {
                    None
                };
                let Some(i) = candidate else {
                    if scanned > batch_start {
                        break;
                    }
                    return Err(failure(
                        "required/recent native context exceeds capacity; history preserved and no request dispatched",
                    ));
                };
                scanned = i + 1;
                if let Message::ToolResult {
                    call_id, content, ..
                } = &mut request.messages[i]
                {
                    let record = archive(
                        store,
                        task,
                        run,
                        call_id,
                        content,
                        self.source_scope.as_deref(),
                    )?;
                    let replacement = placeholder(&record);
                    debt = debt.saturating_sub(
                        content
                            .len()
                            .saturating_sub(replacement.len().saturating_mul(2).saturating_add(256))
                            as u64,
                    );
                    *content = replacement;
                    self.archived
                        .insert((task.into(), run.into(), call_id.clone()), record.clone());
                    archived.push(record);
                    // A registered tokenizer need not scale with UTF-8 bytes.
                    // Recount after each archive when exact accounting is used.
                    if exact.is_some() {
                        break;
                    }
                }
            }
        }
    }
}
fn placeholder(record: &ArchivedObservation) -> String {
    json!({"context_archived":true,"authority":"untrusted_data","artifact_ref":record.artifact_ref,"source_digest":record.source_digest,"original_bytes":record.original_bytes,"read_tool":"history_read","note":"Earlier observation retained verbatim in encrypted history; read the required range before relying on its contents."}).to_string()
}
struct ByteCount(u64);
impl std::io::Write for ByteCount {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0 = self
            .0
            .checked_add(bytes.len() as u64)
            .ok_or_else(|| std::io::Error::other("history size overflow"))?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
