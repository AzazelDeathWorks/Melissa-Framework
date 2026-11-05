//! Per-run continuation over a SHARED native task ledger. The CODE-07 hook
//! deliberately keeps its single-run task contract unchanged. This adapter
//! authenticates own run IDs and settled monotonic known accounting while
//! allowing other admitted nodes to charge the same root task.
use crate::{
    Kind, Result, digest, error,
    gate::{SharedScheduler, lock},
    require,
    scheduler::WorkPacket,
};
use melissa_local_store::{ArtifactRequest, LocalStore, StoreError};
use melissa_model_gateway::{
    ErrorKind, GatewayError,
    continuation::{ContinuationStore, Phase, TurnCheckpoint, TurnCheckpointView},
    transcript::{Manifest, MessageLog},
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::BTreeSet;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    state: TurnCheckpoint,
    handoff_digest: String,
    known_cost: u64,
    known_input: u64,
    known_output: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    transcript: Option<Manifest>,
}
pub struct WorkflowContinuation {
    owner: SharedScheduler,
    packet: WorkPacket,
    sequence: u64,
    messages: MessageLog,
}
fn gateway(e: crate::Error) -> GatewayError {
    GatewayError::new(ErrorKind::Store, e.to_string())
}
impl WorkflowContinuation {
    pub fn new(owner: SharedScheduler, packet: WorkPacket) -> Self {
        Self {
            owner,
            packet,
            sequence: 0,
            messages: MessageLog::default(),
        }
    }
    fn cp(&self, n: u64) -> String {
        format!("workflow-turn:{}:{n}", self.packet.lease.run)
    }
    fn current(
        &self,
        store: &LocalStore,
        pending: Option<&str>,
    ) -> Result<(u64, u64, u64, BTreeSet<String>)> {
        lock(&self.owner)?.check(&self.packet.lease)?;
        let budget = store
            .journal()
            .model_budget(&self.packet.parent_task.task_id)?;
        require(
            budget.unknown_requests == 0 && !budget.budget_exceeded,
            Kind::Uncertain,
            "unknown root usage blocks continuation",
        )?;
        let prefix = format!("model:{}:", self.packet.lease.run);
        let own = store
            .journal()
            .model_requests(&self.packet.parent_task.task_id)?
            .into_iter()
            .filter(|r| r.reservation.request_id.starts_with(&prefix))
            .collect::<Vec<_>>();
        require(
            own.iter().all(|r| {
                (pending == Some(r.reservation.request_id.as_str())
                    && r.state == melissa_local_store::journal::ModelRequestState::Reserved
                    && r.dispatched_at_unix_ms.is_none())
                    || !matches!(
                        r.state,
                        melissa_local_store::journal::ModelRequestState::Reserved
                            | melissa_local_store::journal::ModelRequestState::Dispatched
                            | melissa_local_store::journal::ModelRequestState::OutcomeUnknown
                    )
            }),
            Kind::Uncertain,
            "own model request is unsettled",
        )?;
        require(
            pending.is_none_or(|id| {
                own.iter().any(|r| {
                    r.reservation.request_id == id
                        && r.state == melissa_local_store::journal::ModelRequestState::Reserved
                        && r.dispatched_at_unix_ms.is_none()
                        && r.reservation.profile_id == self.packet.node.profile
                })
            }),
            Kind::Uncertain,
            "pending checkpoint has no owned reservation",
        )?;
        let mut invocations = std::collections::BTreeMap::new();
        let tool_prefix = format!("model-tool:{}:", self.packet.lease.run);
        for event in store.journal().events(&self.packet.parent_task.task_id)? {
            if let Some(i) = event.invocation
                && i.invocation_id.starts_with(&tool_prefix)
            {
                invocations.insert(i.invocation_id.clone(), i);
            }
        }
        require(
            invocations.values().all(|i| i.active_attempt_id.is_none()),
            Kind::Uncertain,
            "own native tool effect is active",
        )?;
        Ok((
            budget.known_cost_micro_usd,
            budget.known_input_tokens,
            budget.known_output_tokens,
            own.into_iter()
                .map(|r| r.reservation.request_id)
                .filter(|id| pending != Some(id.as_str()))
                .collect(),
        ))
    }
}
impl ContinuationStore for WorkflowContinuation {
    fn begin(
        &mut self,
        store: &mut LocalStore,
        binding: &str,
        task: &str,
        run: &str,
    ) -> melissa_model_gateway::Result<Option<TurnCheckpoint>> {
        (|| -> Result<_> {
            require(
                task == self.packet.parent_task.task_id && run == self.packet.lease.run,
                Kind::Authority,
                "continuation task/run mismatch",
            )?;
            let (cost, input, output, ids) = self.current(store, None)?;
            match store.artifact_metadata(task, &format!("model-run:{run}:result")) {
                Ok(_) => return Err(error(Kind::Conflict, "terminal run cannot be replayed")),
                Err(StoreError::NotFound(_)) => {}
                Err(e) => return Err(e.into()),
            }
            let (next, checkpoint) = store
                .journal()
                .latest_checkpoint_in_series(task, &format!("workflow-turn:{run}:"))?;
            self.sequence = next;
            let mut last = None;
            if let Some(cp) = checkpoint {
                let bytes = store.get_artifact(task, &cp.artifact_id)?;
                require(
                    cp.data["digest"] == digest(&bytes),
                    Kind::Store,
                    "continuation index/content mismatch",
                )?;
                last = Some(melissa_model_gateway::checkpoint::decode::<Envelope>(
                    &bytes,
                )?);
            }
            if let Some(mut last) = last {
                if let Some(manifest) = &last.transcript {
                    require(
                        last.state.messages.is_empty(),
                        Kind::Store,
                        "ambiguous transcript checkpoint",
                    )?;
                    last.state.messages = MessageLog::restore(store, task, run, manifest)
                        .map_err(|e| error(Kind::Store, &e.to_string()))?;
                    self.messages.adopt(manifest);
                }
                let own_ids = last
                    .state
                    .result
                    .request_ids
                    .iter()
                    .cloned()
                    .collect::<BTreeSet<_>>();
                require(
                    last.state.binding_digest == binding
                        && last.state.phase == Phase::Safe
                        && last.handoff_digest == digest(&serde_json::to_vec(&self.packet)?)
                        && own_ids == ids
                        && cost >= last.known_cost
                        && input >= last.known_input
                        && output >= last.known_output,
                    Kind::Stale,
                    "pending/stale/incomplete run history or accounting rollback",
                )?;
                Ok(Some(last.state))
            } else {
                require(
                    ids.is_empty(),
                    Kind::Stale,
                    "missing checkpoint for an already used run",
                )?;
                Ok(None)
            }
        })()
        .map_err(gateway)
    }
    fn publish(
        &mut self,
        store: &mut LocalStore,
        state: &TurnCheckpoint,
    ) -> melissa_model_gateway::Result<()> {
        self.publish_view(store, &TurnCheckpointView::from(state))
    }
    fn publish_view(
        &mut self,
        store: &mut LocalStore,
        state: &TurnCheckpointView<'_>,
    ) -> melissa_model_gateway::Result<()> {
        self.publish_with_metadata(store, state, serde_json::Value::Null)
    }
}
impl WorkflowContinuation {
    /// The host source receipt and native safe turn are admitted by one journal append.
    pub fn publish_with_metadata(
        &mut self,
        store: &mut LocalStore,
        state: &TurnCheckpointView<'_>,
        metadata: serde_json::Value,
    ) -> melissa_model_gateway::Result<()> {
        (|| -> Result<_> {
            let pending = (state.phase == Phase::InferencePending).then(|| {
                format!(
                    "model:{}:{}",
                    self.packet.lease.run,
                    state.result.request_ids.len() + 1
                )
            });
            let (cost, input, output, ids) = self.current(store, pending.as_deref())?;
            require(
                state.result.task_id == self.packet.parent_task.task_id
                    && state.result.run_id == self.packet.lease.run
                    && state
                        .result
                        .request_ids
                        .iter()
                        .cloned()
                        .collect::<BTreeSet<_>>()
                        == ids,
                Kind::Store,
                "checkpoint does not cover own native request history",
            )?;
            let e = Envelope {
                state: state.metadata(),
                transcript: Some(
                    self.messages
                        .save(
                            store,
                            &state.result.task_id,
                            &state.result.run_id,
                            state.messages,
                        )
                        .map_err(|e| error(Kind::Store, &e.to_string()))?,
                ),
                handoff_digest: digest(&serde_json::to_vec(&self.packet)?),
                known_cost: cost,
                known_input: input,
                known_output: output,
            };
            let bytes = melissa_model_gateway::checkpoint::encode(&e)?;
            let id = self.cp(self.sequence);
            let artifact = format!("{id}:artifact");
            let task = &self.packet.parent_task.task_id;
            store.put_artifact(
                &ArtifactRequest {
                    artifact_id: artifact.clone(),
                    task_id: task.clone(),
                    content_type: "application/json".into(),
                    logical_type: "workflow.native-continuation".into(),
                    schema_version: "melissa.workflow-continuation/v1".into(),
                    retain_until_ms: 0,
                    legal_hold: false,
                },
                &bytes,
            )?;
            let version = store.journal().task(task)?.record.version;
            store.append_checkpoint(
                &id,
                task,
                version,
                &artifact,
                if metadata.is_null() {
                    json!({"digest":digest(&bytes),"phase":state.phase})
                } else {
                    json!({"digest":digest(&bytes),"phase":state.phase,"host_boundary":metadata})
                },
            )?;
            self.sequence += 1;
            Ok(())
        })()
        .map_err(gateway)
    }
}
