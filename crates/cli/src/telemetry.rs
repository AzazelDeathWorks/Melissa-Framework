//! Host-only benchmark measurements projected from task-owned evidence.
use crate::{Error, Result, app::App, digest};
use melissa_core_contracts::TaskState;
use melissa_local_store::{
    LocalStore, StoreError,
    journal::{AttemptState, JournalEvent},
};
use melissa_model_gateway::{Usage, context_compiler::ContextReport};
use melissa_workflow::comparison::{authenticated_request, verified_task_outcome};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Default, Serialize)]
struct Samples {
    reported_samples: usize,
    missing_samples: usize,
    known_total: Option<u64>,
}

impl Samples {
    fn add(&mut self, sample: Option<u64>) -> Result<()> {
        if let Some(sample) = sample {
            self.known_total = Some(
                self.known_total
                    .unwrap_or_default()
                    .checked_add(sample)
                    .ok_or(Error::Store)?,
            );
            self.reported_samples += 1;
        } else {
            self.missing_samples += 1;
        }
        Ok(())
    }
}

#[derive(Deserialize)]
struct RequestEvidence {
    context_report: Option<ContextReport>,
}

#[derive(Deserialize)]
struct TransportReceipt {
    request_id: String,
    request_digest: String,
    elapsed_ms: u64,
    usage: Option<Usage>,
    model_evidence_write_us: Option<u64>,
}

impl App {
    pub fn metrics(&self, id: &str) -> Result<Value> {
        let store = self.store(id)?;
        let mut value = report(&store, id)?;
        let mut measurements = store
            .list_artifacts(id)?
            .into_iter()
            .filter(|a| a.logical_type == "cli.host-measurement")
            .map(|a| {
                store
                    .get_artifact(id, &a.artifact_id)
                    .map_err(Error::from)
                    .and_then(|raw| {
                        crate::app::decode::<crate::process_measurement::Measurement>(&raw)
                    })
            })
            .collect::<Result<Vec<_>>>()?;
        if let Ok(live) = self.live(id)
            && let Some(sample) = live.host_measurement
        {
            measurements.push(sample);
        }
        value["metrics"]["host_process_measurements"] = json!(measurements);
        value["metrics"]["peak_host_rss_bytes"] = json!(
            measurements
                .iter()
                .filter_map(|m| m.latest.process_lifetime_peak_working_set_bytes)
                .max()
        );
        value["metrics"]["host_measurement_scope"] = json!(
            "CLI process lifetime peak, not task-only/worker RAM; transfer I/O is not physical disk I/O"
        );
        Ok(value)
    }
}

fn report(store: &LocalStore, id: &str) -> Result<Value> {
    let events = store.journal().events(id)?;
    let current = store.journal().task(id)?;
    let last = events.last().ok_or(Error::Store)?;
    let state = current.record.state;
    let accepted = verified_task_outcome(&current, last).map_err(|_| Error::Store)?;
    let run_wall_ms = events
        .iter()
        .find(|event| event.task.record.state == TaskState::Running)
        .filter(|_| state.is_terminal())
        .and_then(|event| last.created_at_ms.checked_sub(event.created_at_ms));

    let requests = store.journal().model_requests(id)?;
    let budget = store.journal().model_budget(id)?;
    let mut dispatches = 0;
    let mut model_latency = Samples::default();
    let mut model_evidence_write = Samples::default();
    let mut model_routes = BTreeMap::new();
    let mut input = Samples::default();
    let mut output = Samples::default();
    let mut cached = Samples::default();
    let mut reasoning = Samples::default();
    let mut cost = Samples::default();
    let mut compiler_reports = 0;
    let mut context_state_injections = 0;
    let mut context_checkpoints = BTreeSet::new();
    let mut peak_wire_bytes = None;
    let mut peak_exact_input_tokens = None;
    let mut exact_token_samples = 0;
    for request in &requests {
        if request.dispatched_at_unix_ms.is_none() {
            continue;
        }
        dispatches += 1;
        let reservation = &request.reservation;
        let route = digest(
            &serde_json::to_vec(&(&reservation.provider_id, &reservation.model_id))
                .map_err(|_| Error::Store)?,
        );
        *model_routes.entry(route).or_insert(0usize) += 1;
        let request_bytes = authenticated_request(store, id, request).map_err(|_| Error::Store)?;
        let envelope: RequestEvidence =
            serde_json::from_slice(&request_bytes).map_err(|_| Error::Store)?;
        if let Some(context) = envelope.context_report {
            compiler_reports += 1;
            peak_wire_bytes = peak_wire_bytes.max(Some(context.native_body_bytes));
            peak_exact_input_tokens = peak_exact_input_tokens.max(context.exact_token_count);
            exact_token_samples += usize::from(context.exact_token_count.is_some());
            if let Some(intelligence) = context.intelligence {
                context_checkpoints.extend(intelligence.checkpoint_refs);
                context_state_injections += usize::from(
                    intelligence
                        .stages
                        .get("turn_state")
                        .is_some_and(|v| v == "injected"),
                );
            }
        }

        let settlement = request.settlement.as_ref();
        let usage = settlement.map(|settled| &settled.usage);
        input.add(usage.and_then(|value| value.input_tokens))?;
        output.add(usage.and_then(|value| value.output_tokens))?;
        cached.add(usage.and_then(|value| value.cached_input_tokens))?;
        cost.add(usage.and_then(|value| value.cost_micro_usd))?;

        let mut receipt = None;
        if request.reconciliation_evidence_ref.is_none()
            && let Some(expected) = settlement.and_then(|value| value.receipt_digest.as_ref())
        {
            let artifact = format!("{}:receipt", reservation.request_id);
            match store.get_artifact(id, &artifact) {
                Ok(raw) => {
                    if digest(&raw) != *expected {
                        return Err(Error::Store);
                    }
                    let parsed: TransportReceipt =
                        serde_json::from_slice(&raw).map_err(|_| Error::Store)?;
                    if parsed.request_id != reservation.request_id
                        || parsed.request_digest != reservation.request_digest
                    {
                        return Err(Error::Store);
                    }
                    receipt = Some(parsed);
                }
                Err(StoreError::NotFound(_)) => (),
                Err(error) => return Err(error.into()),
            }
        }
        model_latency.add(receipt.as_ref().map(|value| value.elapsed_ms))?;
        model_evidence_write.add(
            receipt
                .as_ref()
                .and_then(|value| value.model_evidence_write_us),
        )?;
        reasoning.add(
            receipt
                .and_then(|value| value.usage)
                .and_then(|value| value.reasoning_output_tokens),
        )?;
    }

    let (tool_calls, tool_attempts, tool_latency, search_latency) = tool_metrics(&events)?;
    Ok(json!({
        "schema_version": "melissa.task-metrics/v1",
        "command": "task metrics",
        "task_id": id,
        "state": state,
        "metrics": {
            "verified_acceptance": accepted,
            "run_wall_ms": run_wall_ms,
            "run_wall_basis": "first_running_to_terminal_journal_event_including_pauses",
            "model_dispatch_attempts": dispatches,
            "model_route_dispatch_counts": model_routes,
            "model_ledger_records": requests.len(),
            "model_unknown_outcomes": budget.unknown_requests,
            "model_latency_ms": model_latency,
            "input_tokens": input,
            "output_tokens": output,
            "cached_input_tokens": cached,
            "reasoning_output_tokens": reasoning,
            "accounted_cost_micro_usd": cost,
            "conservative_charged_micro_usd": budget.charged_cost_micro_usd,
            "cost_basis": "host_rate_card_or_reservation",
            "tool_calls_dispatched": tool_calls,
            "tool_attempts_dispatched": tool_attempts,
            "tool_redispatches": tool_attempts - tool_calls,
            "tool_attempt_latency_ms": tool_latency,
            "context_compiler_reports": compiler_reports,
            "context_state_injections": context_state_injections,
            "context_compaction_checkpoints": (compiler_reports > 0).then_some(context_checkpoints.len()),
            "peak_reported_context_wire_bytes": peak_wire_bytes,
            "peak_reported_exact_context_tokens": peak_exact_input_tokens,
            "exact_context_token_samples": exact_token_samples,
            "peak_host_rss_bytes": null,
            "disk_read_bytes": null,
            "disk_written_bytes": null,
            "model_evidence_write_latency_us": model_evidence_write,
            "repository_search_latency_ms": search_latency
        }
    }))
}

fn tool_metrics(events: &[JournalEvent]) -> Result<(usize, usize, Samples, Samples)> {
    let mut attempts = BTreeMap::new();
    let mut invocations = BTreeSet::new();
    for event in events {
        let Some(attempt) = &event.attempt else {
            continue;
        };
        match attempt.state {
            AttemptState::Dispatched => {
                let invocation = event.invocation.as_ref().ok_or(Error::Store)?;
                let is_search = invocation.tool_id == "fs.search";
                attempts.insert(&attempt.attempt_id, (event.created_at_ms, None, is_search));
                invocations.insert(&attempt.invocation_id);
            }
            AttemptState::Succeeded | AttemptState::Failed | AttemptState::Cancelled => {
                if let Some((start, elapsed, _)) = attempts.get_mut(&attempt.attempt_id) {
                    *elapsed = event.created_at_ms.checked_sub(*start);
                }
            }
            _ => (),
        }
    }
    let mut latency = Samples::default();
    let mut search_latency = Samples::default();
    for (_, elapsed, is_search) in attempts.values() {
        latency.add(*elapsed)?;
        if *is_search {
            search_latency.add(*elapsed)?;
        }
    }
    Ok((invocations.len(), attempts.len(), latency, search_latency))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_measurement_is_separate_from_zero_and_partial_totals() {
        let mut samples = Samples::default();
        samples.add(None).unwrap();
        assert_eq!(samples.known_total, None);
        samples.add(Some(0)).unwrap();
        assert_eq!(samples.known_total, Some(0));
        samples.add(Some(31)).unwrap();
        assert_eq!(samples.known_total, Some(31));
        assert_eq!(samples.reported_samples, 2);
        assert_eq!(samples.missing_samples, 1);
    }

    #[cfg(windows)]
    #[test]
    fn prepared_task_metrics_preserve_the_journal_and_leave_acceptance_unknown() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let source = temp.path().join("source");
        std::fs::create_dir(&source).unwrap();
        std::fs::write(source.join("main.py"), b"print(1)\n").unwrap();
        App::init(&home).unwrap();
        let app = App::open(&home).unwrap();
        let task = app
            .create(&source, "Review main.py", "melissa.code.reviewer", vec![])
            .unwrap();
        let id = task["task_id"].as_str().unwrap();
        let before = app.store(id).unwrap().journal().events(id).unwrap();
        let report = app.metrics(id).unwrap();
        assert_eq!(report["state"], "awaiting_approval");
        assert!(report["metrics"]["verified_acceptance"].is_null());
        assert!(report["metrics"]["input_tokens"]["known_total"].is_null());
        assert_eq!(report["metrics"]["model_dispatch_attempts"], 0);
        assert_eq!(report["metrics"]["tool_attempts_dispatched"], 0);
        assert_eq!(app.store(id).unwrap().journal().events(id).unwrap(), before);
    }
}
