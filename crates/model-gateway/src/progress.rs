//! Redacted, best-effort telemetry. This is not a continuation or billing receipt.
use crate::{Result, agent_loop::RunResult};
use melissa_local_store::{
    LocalStore,
    journal::{ModelRequestRecord, ModelRequestState},
};
use serde::Serialize;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProgressPhase {
    Started,
    RequestDispatched,
    RequestSettled,
    ToolDispatched,
    ToolSettled,
    Finished,
}

#[derive(Debug, Clone, Serialize)]
pub struct RunProgress {
    pub schema_version: &'static str,
    pub phase: ProgressPhase,
    pub elapsed_ms: u64,
    pub remaining_ms: u64,
    pub requests: usize,
    pub tool_calls: u32,
    pub reported_input_tokens: u64,
    pub reported_output_tokens: u64,
    pub cached_input_tokens: u64,
    pub unreported_usage_requests: usize,
    pub pending_requests: usize,
    pub unknown_requests: usize,
    pub tool_in_flight: bool,
    pub requires_reconciliation: bool,
    pub final_status: Option<crate::agent_loop::RunStatus>,
}

/// Host-only, synchronous and bounded sinks. Sink I/O errors must not change
/// accounting, request settlement, execution, or authorization decisions.
pub trait ProgressObserver: Send {
    fn observe(&mut self, progress: &RunProgress);
}

pub(crate) fn snapshot(
    store: &LocalStore,
    result: &RunResult,
    phase: ProgressPhase,
    elapsed_ms: u64,
    wall_ms: u64,
    records: &mut BTreeMap<String, ModelRequestRecord>,
) -> Result<RunProgress> {
    let mut progress = RunProgress {
        schema_version: "melissa.run-progress/v1",
        phase,
        elapsed_ms,
        remaining_ms: wall_ms.saturating_sub(elapsed_ms),
        requests: result.request_ids.len(),
        tool_calls: result.tool_calls,
        reported_input_tokens: 0,
        reported_output_tokens: 0,
        cached_input_tokens: 0,
        unreported_usage_requests: 0,
        pending_requests: 0,
        unknown_requests: 0,
        tool_in_flight: phase == ProgressPhase::ToolDispatched,
        requires_reconciliation: result.requires_reconciliation,
        final_status: (phase == ProgressPhase::Finished).then_some(result.status),
    };
    for id in &result.request_ids {
        // Settled ledger rows cannot change during this run. Avoid rereading
        // its whole history from SQLite for every tool observation.
        if records.get(id).is_none_or(|r| {
            matches!(
                r.state,
                ModelRequestState::Reserved | ModelRequestState::Dispatched
            )
        }) {
            let record = store
                .journal()
                .model_request(id)
                .map_err(|e| crate::GatewayError::new(crate::ErrorKind::Store, format!("{e:?}")))?;
            records.insert(id.clone(), record);
        }
        let record = &records[id];
        match record.state {
            ModelRequestState::Reserved | ModelRequestState::Dispatched => {
                progress.pending_requests += 1;
            }
            ModelRequestState::OutcomeUnknown => progress.unknown_requests += 1,
            _ => {}
        }
        let usage = record.settlement.as_ref().map(|s| &s.usage);
        progress.reported_input_tokens += usage.and_then(|u| u.input_tokens).unwrap_or(0);
        progress.reported_output_tokens += usage.and_then(|u| u.output_tokens).unwrap_or(0);
        progress.cached_input_tokens += usage.and_then(|u| u.cached_input_tokens).unwrap_or(0);
        if usage.is_none_or(|u| u.input_tokens.is_none() || u.output_tokens.is_none()) {
            progress.unreported_usage_requests += 1;
        }
    }
    Ok(progress)
}
