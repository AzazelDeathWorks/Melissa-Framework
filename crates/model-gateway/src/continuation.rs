//! Opt-in durable turn boundary. Default Gateway::run has no persistence hook.
//! Native provider messages remain lossless; pending inference/tools never replay.
use crate::{Message, Result, agent_loop::RunResult};
use melissa_local_store::LocalStore;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Safe,
    InferencePending,
    ToolsPending,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TurnCheckpoint {
    pub schema_version: String,
    pub binding_digest: String,
    pub phase: Phase,
    pub next_turn: u32,
    pub messages: Vec<Message>,
    pub seen_calls: BTreeSet<String>,
    pub successful: BTreeSet<String>,
    pub selected_model: Option<usize>,
    pub result: RunResult,
}
#[derive(Serialize)]
pub struct TurnCheckpointView<'a> {
    pub schema_version: &'a str,
    pub binding_digest: &'a str,
    pub phase: Phase,
    pub next_turn: u32,
    pub messages: &'a [Message],
    pub seen_calls: &'a BTreeSet<String>,
    pub successful: &'a BTreeSet<String>,
    pub selected_model: Option<usize>,
    pub result: &'a RunResult,
}
impl<'a> From<&'a TurnCheckpoint> for TurnCheckpointView<'a> {
    fn from(s: &'a TurnCheckpoint) -> Self {
        Self {
            schema_version: &s.schema_version,
            binding_digest: &s.binding_digest,
            phase: s.phase,
            next_turn: s.next_turn,
            messages: &s.messages,
            seen_calls: &s.seen_calls,
            successful: &s.successful,
            selected_model: s.selected_model,
            result: &s.result,
        }
    }
}
impl TurnCheckpointView<'_> {
    pub fn metadata(&self) -> TurnCheckpoint {
        TurnCheckpoint {
            schema_version: self.schema_version.into(),
            binding_digest: self.binding_digest.into(),
            phase: self.phase,
            next_turn: self.next_turn,
            messages: vec![],
            seen_calls: self.seen_calls.clone(),
            successful: self.successful.clone(),
            selected_model: self.selected_model,
            result: self.result.clone(),
        }
    }
}
/// The implementing trusted host owns source/policy freshness, encrypted
/// artifact/checkpoint publication and current budget/Unknown/cancel checks.
pub trait ContinuationStore: Send {
    fn begin(
        &mut self,
        store: &mut LocalStore,
        binding: &str,
        task: &str,
        run: &str,
    ) -> Result<Option<TurnCheckpoint>>;
    fn publish(&mut self, store: &mut LocalStore, state: &TurnCheckpoint) -> Result<()>;
    fn publish_view(
        &mut self,
        store: &mut LocalStore,
        state: &TurnCheckpointView<'_>,
    ) -> Result<()> {
        let mut owned = state.metadata();
        owned.messages = state.messages.to_vec();
        self.publish(store, &owned)
    }
}
