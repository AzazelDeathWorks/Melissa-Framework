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
    pub capacity_check: String,
    pub has_opaque_continuation: bool,
    pub archived: Vec<ArchivedObservation>,
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
        || !page
            .artifact_ref
            .starts_with(&history_prefix(&context.run_id))
    {
        return Err(GatewayError::new(
            ErrorKind::Authorization,
            "history scope is inactive or not granted",
        ));
    }
    if !(1..=65536).contains(&page.max_bytes) {
        return Err(failure("history page limit"));
    }
    let meta = store
        .artifact_metadata(&context.task_id, &page.artifact_ref)
        .map_err(store_error)?;
    if meta.logical_type != "context.archived-observation" {
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
fn archive(
    store: &mut LocalStore,
    task: &str,
    run: &str,
    call: &str,
    content: &str,
) -> Result<ArchivedObservation> {
    let hash = blake3::hash(content.as_bytes()).to_hex().to_string();
    let id = format!("{}{hash}", history_prefix(run));
    match store.artifact_metadata(task, &id) {
        Ok(meta) => {
            if meta.logical_type != "context.archived-observation"
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
                        schema_version: "melissa.context-observation/v1".into(),
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
    pub fn with_counter(mut self, counter: Box<dyn NativeTokenCounter>) -> Result<Self> {
        if counter.policy_id().trim().is_empty() || counter.policy_id().len() > 512 {
            return Err(failure("token counter policy identity"));
        }
        self.counter = Some(counter);
        Ok(self)
    }
    pub fn policy_id(&self) -> String {
        format!(
            "native-context/v1:{}",
            self.counter
                .as_ref()
                .map_or("utf8-framing", |c| c.policy_id())
        )
    }
    pub fn working_messages(
        &self,
        messages: &[Message],
        task: &str,
        run: &str,
    ) -> (Vec<Message>, Vec<ArchivedObservation>) {
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
        request.validate_structure()?;
        let has_images = request
            .messages
            .iter()
            .any(|m| matches!(m, Message::UserImages { .. } | Message::ToolImages { .. }));
        let can_archive = request.tools.iter().any(|t| t.name == "history_read");
        let protected_start = request.messages.len().saturating_sub(8);
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
                return Err(failure(
                    "required history exceeds wire byte envelope; no request dispatched",
                ));
            };
            scanned = i + 1;
            if let Message::ToolResult {
                call_id, content, ..
            } = &mut request.messages[i]
            {
                let record = archive(store, task, run, call_id, content)?;
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
                    },
                ));
            }
            let mut debt = input
                .saturating_add(request.max_output_tokens)
                .saturating_sub(capacity);
            while debt > 0 {
                let candidate = if can_archive {
                    (scanned..protected_start).find(|&i| matches!(&request.messages[i], Message::ToolResult { content, .. } if content.len() > 2048))
                } else {
                    None
                };
                let Some(i) = candidate else {
                    return Err(failure(
                        "required/recent native context exceeds capacity; history preserved and no request dispatched",
                    ));
                };
                scanned = i + 1;
                if let Message::ToolResult {
                    call_id, content, ..
                } = &mut request.messages[i]
                {
                    let record = archive(store, task, run, call_id, content)?;
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
