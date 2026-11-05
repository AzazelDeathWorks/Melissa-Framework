//! Durable, encrypted, task-owned CAS/checkpoint admission. Unique checkpoint
//! sequence keys fence concurrent schedulers before any lease is returned.
//! Abandoned publication artifacts do not consume a checkpoint identity.
use crate::{
    Kind, Result, digest, error, now,
    plan::{Node, Plan, conflicts, identifier},
    require,
};
use melissa_core_contracts::{TaskState, catalog::Catalog, wire::TaskContractV1};
use melissa_local_store::{
    ArtifactRequest, LocalStore, StoreError,
    journal::{ModelBudgetSnapshot, ModelRequestState},
};
use melissa_model_gateway::{
    agent_loop::{RunResult, RunStatus},
    schema,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Freshness {
    pub workspace_digest: String,
    pub policy_digest: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Pending,
    Leased,
    Candidate,
    Failed,
    NeedsReconciliation,
    Cancelled,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Lease {
    pub task: String,
    pub plan_digest: String,
    pub revision: u64,
    pub node: String,
    pub generation: u64,
    pub worker: String,
    pub run: String,
    pub nonce: String,
    pub issued_at_ms: u64,
    pub expires_at_ms: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Slot {
    pub phase: Phase,
    pub generation: u64,
    pub lease: Option<Lease>,
    pub output: Option<Value>,
    pub native_run_ref: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct State {
    pub schema_version: String,
    pub sequence: u64,
    pub previous_digest: Option<String>,
    pub plan: Plan,
    pub slots: BTreeMap<String, Slot>,
    pub cancel_requested: bool,
    pub cancellation_settled: bool,
    pub no_progress: u32,
    pub last_clock_ms: u64,
    pub started_at_ms: u64,
    pub deadline_ms: u64,
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adaptive: Option<crate::adaptive::Ledger>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_handoff: Option<WorkPacket>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkPacket {
    pub schema_version: String,
    pub parent_task: TaskContractV1,
    pub node: Node,
    pub lease: Lease,
    pub bound_inputs: BTreeMap<String, Value>,
    pub dependency_run_refs: BTreeMap<String, String>,
    pub shared_budget: ModelBudgetSnapshot,
    pub input_trust: String,
    pub completion_authority: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub specialist_handoff: Option<crate::adaptive::Handoff>,
}
pub type Refresh = Arc<dyn Fn() -> Result<Freshness> + Send + Sync>;
pub struct Scheduler {
    store: LocalStore,
    task: String,
    workflow: String,
    refresh: Refresh,
    cancelled: Arc<AtomicBool>,
}
fn parse<T: for<'de> Deserialize<'de>>(bytes: &[u8]) -> Result<T> {
    Ok(melissa_model_gateway::checkpoint::decode(bytes)?)
}
fn cp(task: &str, workflow: &str, n: u64) -> String {
    format!("workflow:{task}:{workflow}:checkpoint:{n}")
}
fn idle(store: &LocalStore, task: &str) -> Result<()> {
    let b = store.journal().model_budget(task)?;
    require(
        b.active_requests == 0
            && b.unknown_requests == 0
            && b.unknown_input_requests == 0
            && b.unknown_output_requests == 0
            && b.unknown_cost_requests == 0,
        Kind::Uncertain,
        "active/unknown provider effects retain workflow ownership",
    )?;
    let mut invocations = BTreeMap::new();
    for event in store.journal().events(task)? {
        if let Some(i) = event.invocation {
            invocations.insert(i.invocation_id.clone(), i);
        }
    }
    require(
        invocations.values().all(|i| i.active_attempt_id.is_none())
            && store
                .journal()
                .task(task)?
                .record
                .unknown_invocation_id
                .is_none(),
        Kind::Uncertain,
        "active/unknown worker effects retain workflow ownership",
    )
}
impl Scheduler {
    pub fn create(
        store: LocalStore,
        plan: Plan,
        catalog: &Catalog,
        refresh: Refresh,
        cancelled: Arc<AtomicBool>,
    ) -> Result<Self> {
        Self::create_until(store, plan, catalog, refresh, cancelled, u64::MAX)
    }

    pub fn create_until(
        mut store: LocalStore,
        plan: Plan,
        catalog: &Catalog,
        refresh: Refresh,
        cancelled: Arc<AtomicBool>,
        expires_at_ms: u64,
    ) -> Result<Self> {
        let parent = store.journal().task(&plan.task)?;
        plan.validate(&parent.contract, catalog)?;
        require(
            plan.revision == 1 && plan.workspace_digest == parent.contract.input_snapshot_digest,
            Kind::Stale,
            "initial plan must bind exact parent input snapshot",
        )?;
        let started = now()?;
        let deadline = started
            .checked_add(parent.contract.limits.max_wall_time_ms)
            .ok_or_else(|| error(Kind::Invalid, "root deadline overflow"))?
            .min(expires_at_ms);
        require(
            deadline > started,
            Kind::Cancelled,
            "approved root deadline elapsed",
        )?;
        let initial = State {
            schema_version: "melissa.workflow-state/v1".into(),
            sequence: 0,
            previous_digest: None,
            slots: plan
                .nodes
                .iter()
                .map(|n| {
                    (
                        n.id.clone(),
                        Slot {
                            phase: Phase::Pending,
                            generation: 0,
                            lease: None,
                            output: None,
                            native_run_ref: None,
                        },
                    )
                })
                .collect(),
            plan: plan.clone(),
            cancel_requested: false,
            cancellation_settled: false,
            no_progress: 0,
            last_clock_ms: started,
            started_at_ms: started,
            deadline_ms: deadline,
            reason: "host-validated initial plan".into(),
            active_handoff: None,
            adaptive: (plan.mode == crate::plan::Mode::AdaptiveSerial)
                .then(|| crate::adaptive::Ledger::new(&parent.contract, &plan, deadline)),
        };
        require(
            matches!(
                store
                    .journal()
                    .checkpoint(&cp(&plan.task, &plan.id, 0), &plan.task),
                Err(StoreError::NotFound(_))
            ),
            Kind::Conflict,
            "workflow identity already consumed or unavailable",
        )?;
        let owner = Self {
            store: store.connect()?,
            task: plan.task.clone(),
            workflow: plan.id.clone(),
            refresh,
            cancelled,
        };
        let version = owner.fresh(&initial)?;
        Self::write(
            &mut store,
            &initial,
            version,
            Some((&owner.cancelled, initial.deadline_ms)),
        )?;
        Ok(Self { store, ..owner })
    }
    pub fn open(
        store: LocalStore,
        task: String,
        workflow: String,
        refresh: Refresh,
        cancelled: Arc<AtomicBool>,
    ) -> Result<Self> {
        require(
            identifier(&workflow),
            Kind::Invalid,
            "invalid workflow identity",
        )?;
        let scheduler = Self {
            store,
            task,
            workflow,
            refresh,
            cancelled,
        };
        let state = scheduler.state()?;
        if state.cancel_requested
            || state
                .slots
                .values()
                .any(|v| v.phase == Phase::NeedsReconciliation)
        {
            scheduler.cancelled.store(true, Ordering::Release);
        }
        Ok(scheduler)
    }
    pub fn store(&self) -> &LocalStore {
        &self.store
    }
    pub fn flag(&self) -> Arc<AtomicBool> {
        self.cancelled.clone()
    }
    pub fn task_id(&self) -> &str {
        &self.task
    }
    pub fn state(&self) -> Result<State> {
        let mut latest = None;
        let mut previous = None;
        for n in 0..1_000_000u64 {
            let checkpoint = match self
                .store
                .journal()
                .checkpoint(&cp(&self.task, &self.workflow, n), &self.task)
            {
                Ok(v) => v,
                Err(StoreError::NotFound(_)) => break,
                Err(e) => return Err(e.into()),
            };
            let bytes = self
                .store
                .get_artifact(&self.task, &checkpoint.artifact_id)?;
            let s: State = parse(&bytes)?;
            require(
                s.schema_version == "melissa.workflow-state/v1"
                    && s.sequence == n
                    && s.previous_digest == previous
                    && s.plan.task == self.task
                    && s.plan.id == self.workflow,
                Kind::Store,
                "workflow checkpoint chain mismatch",
            )?;
            require(
                checkpoint.data == json!({"workflow_digest":digest(&bytes)}),
                Kind::Store,
                "workflow checkpoint artifact binding mismatch",
            )?;
            require(
                (s.plan.mode == crate::plan::Mode::AdaptiveSerial) == s.adaptive.is_some()
                    && s.adaptive
                        .as_ref()
                        .is_none_or(|l| l.schema_version == "melissa.adaptive-ledger/v1"),
                Kind::Store,
                "adaptive ledger version/mode mismatch",
            )?;
            previous = Some(digest(&bytes));
            latest = Some(s);
        }
        latest.ok_or_else(|| error(Kind::Store, "workflow checkpoint missing"))
    }
    fn write(
        store: &mut LocalStore,
        state: &State,
        expected_task_version: u64,
        active: Option<(&AtomicBool, u64)>,
    ) -> Result<()> {
        let bytes = melissa_model_gateway::checkpoint::encode(state)?;
        let h = digest(&bytes);
        let artifact = format!("workflow:{}:{}:state:{}", state.plan.task, state.plan.id, h);
        match store.artifact_metadata(&state.plan.task, &artifact) {
            Ok(_) => require(
                store.get_artifact(&state.plan.task, &artifact)? == bytes,
                Kind::Store,
                "published workflow artifact mismatch",
            )?,
            Err(StoreError::NotFound(_)) => {
                store.put_artifact(
                    &ArtifactRequest {
                        artifact_id: artifact.clone(),
                        task_id: state.plan.task.clone(),
                        content_type: "application/json".into(),
                        logical_type: "workflow.state".into(),
                        schema_version: "melissa.workflow-state/v1".into(),
                        retain_until_ms: 0,
                        legal_hold: false,
                    },
                    &bytes,
                )?;
            }
            Err(e) => return Err(e.into()),
        }
        if let Some((cancel, deadline)) = active {
            require(
                !cancel.load(Ordering::Acquire) && now()? < deadline,
                Kind::Cancelled,
                "workflow cancelled or lease expired before checkpoint admission",
            )?;
        }
        store
            .journal_mut()
            .append_checkpoint(
                &cp(&state.plan.task, &state.plan.id, state.sequence),
                &state.plan.task,
                expected_task_version,
                &artifact,
                json!({"workflow_digest":h}),
            )
            .map_err(|_| {
                error(
                    Kind::Conflict,
                    "workflow changed before durable checkpoint admission",
                )
            })?;
        Ok(())
    }
    fn commit(&mut self, old: &State, next: State, reason: &str) -> Result<State> {
        let version = self.store.journal().task(&self.task)?.record.version;
        self.commit_version(old, next, reason, version, false)
    }
    fn commit_version(
        &mut self,
        old: &State,
        mut next: State,
        reason: &str,
        task_version: u64,
        active: bool,
    ) -> Result<State> {
        require(
            digest(&serde_json::to_vec(&self.state()?)?) == digest(&serde_json::to_vec(old)?),
            Kind::Conflict,
            "scheduler snapshot is stale",
        )?;
        let clock = now()?;
        require(
            clock >= old.last_clock_ms,
            Kind::Stale,
            "host clock rollback",
        )?;
        next.sequence = old
            .sequence
            .checked_add(1)
            .ok_or_else(|| error(Kind::Invalid, "workflow sequence overflow"))?;
        next.previous_digest = Some(digest(&serde_json::to_vec(old)?));
        next.last_clock_ms = clock;
        next.reason = reason.into();
        let deadline = old
            .slots
            .iter()
            .filter_map(|(id, slot)| {
                (slot.phase == Phase::Leased
                    && next.slots.get(id).is_some_and(|n| n.phase != Phase::Leased))
                .then(|| slot.lease.as_ref().map(|l| l.expires_at_ms))
                .flatten()
            })
            .fold(next.deadline_ms, u64::min);
        Self::write(
            &mut self.store,
            &next,
            task_version,
            active.then_some((self.cancelled.as_ref(), deadline)),
        )?;
        Ok(next)
    }
    fn fresh(&self, state: &State) -> Result<u64> {
        require(
            !self.cancelled.load(Ordering::Acquire) && !state.cancel_requested,
            Kind::Cancelled,
            "workflow cancellation requested",
        )?;
        let task = self.store.journal().task(&self.task)?;
        require(
            matches!(
                task.record.state,
                TaskState::Running | TaskState::Repairing | TaskState::Verifying
            ),
            Kind::Cancelled,
            "parent task is not active",
        )?;
        require(
            task.record.unknown_invocation_id.is_none(),
            Kind::Uncertain,
            "parent worker effect requires reconciliation",
        )?;
        let budget = self.store.journal().model_budget(&self.task)?;
        require(
            budget.unknown_requests == 0 && !budget.budget_exceeded,
            Kind::Uncertain,
            "unknown usage or budget excess blocks new work",
        )?;
        require(
            !state
                .slots
                .values()
                .any(|s| s.phase == Phase::NeedsReconciliation),
            Kind::Uncertain,
            "uncertain node ownership blocks new work",
        )?;
        let current = (self.refresh)()?;
        require(
            current.workspace_digest == state.plan.workspace_digest
                && current.policy_digest == state.plan.policy_digest,
            Kind::Stale,
            "source or host authority policy changed",
        )?;
        require(
            now()? >= state.last_clock_ms,
            Kind::Stale,
            "host clock rollback",
        )?;
        require(
            now()? < state.deadline_ms,
            Kind::Cancelled,
            "shared root workflow deadline elapsed",
        )?;
        require(
            state.no_progress < state.plan.max_no_progress,
            Kind::Conflict,
            "workflow no-progress guard reached",
        )?;
        Ok(task.record.version)
    }
    pub fn check(&self, lease: &Lease) -> Result<()> {
        let s = self.state()?;
        self.check_state(&s, lease).map(|_| ())
    }
    /// Validates `lease` against one snapshot so a caller can commit against
    /// that same snapshot instead of re-reading state after the check.
    fn check_state(&self, s: &State, lease: &Lease) -> Result<u64> {
        let version = self.fresh(s)?;
        let slot = s
            .slots
            .get(&lease.node)
            .ok_or_else(|| error(Kind::Stale, "node missing"))?;
        require(
            lease.task == self.task
                && lease.revision == s.plan.revision
                && lease.plan_digest == s.plan.digest()?
                && slot.phase == Phase::Leased
                && slot.generation == lease.generation
                && slot.lease.as_ref() == Some(lease),
            Kind::Stale,
            "lease/generation/run identity is stale or forged",
        )?;
        require(
            now()? < lease.expires_at_ms,
            Kind::Stale,
            "node lease expired; do not replay effects",
        )?;
        Ok(version)
    }
    pub fn resume_packet(&self, node: &str) -> Result<WorkPacket> {
        let state = self.state()?;
        let lease = state
            .slots
            .get(node)
            .and_then(|s| s.lease.as_ref())
            .ok_or_else(|| error(Kind::Stale, "node has no resumable lease"))?;
        self.check(lease)?;
        let packet: WorkPacket = if let Some(packet) = &state.active_handoff {
            packet.clone()
        } else {
            parse(
                &self
                    .store
                    .get_artifact(&self.task, &format!("workflow-handoff:{}", lease.run))?,
            )?
        };
        require(
            packet.lease == *lease
                && state.plan.nodes.iter().find(|n| n.id == node) == Some(&packet.node)
                && packet.parent_task == self.store.journal().task(&self.task)?.contract,
            Kind::Stale,
            "persisted handoff no longer matches exact node/task",
        )?;
        Ok(packet)
    }
    pub fn acquire(&mut self, id: &str, worker: &str, lifetime_ms: u64) -> Result<WorkPacket> {
        let s = self.state()?;
        let version = self.fresh(&s)?;
        require(
            identifier(worker) && lifetime_ms > 0,
            Kind::Invalid,
            "invalid worker/lease lifetime",
        )?;
        let task = self.store.journal().task(&self.task)?.contract;
        require(
            lifetime_ms <= task.limits.max_wall_time_ms,
            Kind::Invalid,
            "lease exceeds parent wall allowance",
        )?;
        let node = s
            .plan
            .nodes
            .iter()
            .find(|n| n.id == id)
            .ok_or_else(|| error(Kind::Invalid, "unknown plan node"))?
            .clone();
        require(
            s.slots[id].phase == Phase::Pending,
            Kind::Conflict,
            "node is not pending",
        )?;
        require(
            node.after
                .iter()
                .all(|d| s.slots[d].phase == Phase::Candidate),
            Kind::Conflict,
            "dependencies not complete",
        )?;
        let active = s
            .slots
            .values()
            .filter(|v| matches!(v.phase, Phase::Leased | Phase::NeedsReconciliation))
            .count();
        require(
            active < usize::from(s.plan.max_parallel),
            Kind::Conflict,
            "workflow concurrency capacity exhausted",
        )?;
        for other in &s.plan.nodes {
            if matches!(
                s.slots[&other.id].phase,
                Phase::Leased | Phase::NeedsReconciliation
            ) {
                require(
                    !node
                        .claims
                        .iter()
                        .any(|a| other.claims.iter().any(|b| conflicts(a, b))),
                    Kind::Conflict,
                    "active resource ownership conflict",
                )?;
            }
        }
        if let Some(ledger) = &s.adaptive {
            require(
                !ledger
                    .repeated_failures
                    .values()
                    .any(|n| *n >= s.plan.max_no_progress),
                Kind::Conflict,
                "repeated host check failures require intervention",
            )?;
            idle(&self.store, &self.task)?;
        }
        let mut bound_inputs = BTreeMap::new();
        let mut refs = BTreeMap::new();
        for binding in &node.bindings {
            let slot = &s.slots[&binding.from];
            let output = slot
                .output
                .as_ref()
                .ok_or_else(|| error(Kind::Store, "dependency output absent"))?;
            let value = output
                .pointer(&binding.pointer)
                .ok_or_else(|| error(Kind::Invalid, "bound output pointer missing"))?;
            schema::validate_output(&binding.schema, &serde_json::to_string(value)?)?;
            bound_inputs.insert(binding.input.clone(), value.clone());
            refs.insert(
                binding.from.clone(),
                slot.native_run_ref
                    .clone()
                    .ok_or_else(|| error(Kind::Store, "dependency evidence absent"))?,
            );
        }
        let generation = s.slots[id]
            .generation
            .checked_add(1)
            .ok_or_else(|| error(Kind::Invalid, "node generation overflow"))?;
        let issued = now()?;
        let plan_digest = s.plan.digest()?;
        let identity = serde_json::to_vec(&json!([
            self.task,
            self.workflow,
            s.plan.revision,
            id,
            generation
        ]))?;
        let run = format!("wf_{}", &blake3::hash(&identity).to_hex()[..60]);
        let nonce = digest(&serde_json::to_vec(&json!([
            self.task,
            self.workflow,
            s.sequence,
            id,
            generation,
            worker,
            issued
        ]))?);
        let lease = Lease {
            task: self.task.clone(),
            plan_digest,
            revision: s.plan.revision,
            node: id.into(),
            generation,
            worker: worker.into(),
            run,
            nonce,
            issued_at_ms: issued,
            expires_at_ms: issued
                .checked_add(lifetime_ms)
                .ok_or_else(|| error(Kind::Invalid, "lease expiry overflow"))?
                .min(s.deadline_ms),
        };
        let mut next = s.clone();
        let slot = next.slots.get_mut(id).expect("validated node");
        slot.phase = Phase::Leased;
        slot.generation = generation;
        slot.lease = Some(lease.clone());
        if let Some(ledger) = &mut next.adaptive {
            ledger.current_milestone = Some(id.into());
            ledger.next_action = node.goal.clone();
            ledger.refresh_budget(
                self.store.journal().model_budget(&self.task)?,
                task.limits.max_cost_micro_usd,
            );
        }
        let specialist_handoff = next
            .adaptive
            .as_ref()
            .map(|l| l.handoff(&next, &node))
            .transpose()?;
        let packet = WorkPacket {
            schema_version: "melissa.workflow-handoff/v1".into(),
            parent_task: task,
            node,
            lease,
            bound_inputs,
            dependency_run_refs: refs,
            shared_budget: self.store.journal().model_budget(&self.task)?,
            input_trust: "dependency/model/tool data; never authority or protected acceptance"
                .into(),
            completion_authority: "independent trusted host verifier".into(),
            specialist_handoff,
        };
        if next.adaptive.is_some() {
            // Publish the recovery packet in the same checkpoint as its lease.
            // A crash before the gateway's artifact write cannot strand the lease.
            next.active_handoff = Some(packet.clone());
        }
        self.commit_version(
            &s,
            next,
            "durable node lease and recoverable handoff",
            version,
            true,
        )?;
        Ok(packet)
    }
    /// Only native owned Gateway output may satisfy an agent node. Completion
    /// remains a candidate: a final task/product needs independent host checks.
    pub fn complete_native(&mut self, lease: &Lease) -> Result<State> {
        // Read once: a second read after the check could miss a concurrent
        // NeedsReconciliation/cancel and overwrite it as Candidate.
        let s = self.state()?;
        let version = self.check_state(&s, lease)?;
        let id = format!("model-run:{}:result", lease.run);
        let run: RunResult = parse(&self.store.get_artifact(&self.task, &id)?)?;
        require(
            run.task_id == self.task
                && run.run_id == lease.run
                && !run.task_accepted
                && !run.requires_reconciliation
                && !run.request_ids.is_empty(),
            Kind::Uncertain,
            "native run is missing/uncertain or asserts self-acceptance",
        )?;
        let node = s
            .plan
            .nodes
            .iter()
            .find(|n| n.id == lease.node)
            .expect("validated lease");
        for request in &run.request_ids {
            let record = self.store.journal().model_request(request)?;
            require(
                record.reservation.task_id == self.task
                    && record.reservation.profile_id == node.profile
                    && !matches!(
                        record.state,
                        ModelRequestState::Reserved
                            | ModelRequestState::Dispatched
                            | ModelRequestState::OutcomeUnknown
                    ),
                Kind::Uncertain,
                "node model accounting is not authenticated and settled",
            )?;
        }
        let mut next = s.clone();
        let slot = next.slots.get_mut(&lease.node).expect("validated lease");
        slot.native_run_ref = Some(id.clone());
        if run.status == RunStatus::FinalCandidate {
            let output = schema::validate_output_with_limit(
                &node.output_schema,
                run.answer
                    .as_deref()
                    .ok_or_else(|| error(Kind::Invalid, "native final answer absent"))?,
                8 * 1024 * 1024,
            )?;
            slot.output = Some(output);
            slot.phase = Phase::Candidate;
        } else {
            slot.phase = Phase::Failed;
        }
        if let Some(ledger) = &mut next.adaptive {
            next.active_handoff = None;
            ledger.current_milestone = None;
            ledger.refresh_budget(
                self.store.journal().model_budget(&self.task)?,
                self.store
                    .journal()
                    .task(&self.task)?
                    .contract
                    .limits
                    .max_cost_micro_usd,
            );
            if run.status == RunStatus::FinalCandidate {
                ledger.completed_milestones.push(node.id.clone());
                ledger.candidate_artifacts.insert(node.id.clone(), id);
                ledger.next_action =
                    "Inspect the next authorized milestone or run protected QA".into();
            } else {
                ledger.failed_attempts.insert(node.id.clone(), id);
                ledger.current_diagnostics =
                    vec![format!("Native specialist ended with {:?}", run.status)];
                ledger.open_questions =
                    vec!["Inspect recorded evidence and propose a bounded recovery plan".into()];
                ledger.next_action = "Host review of failed attempt required".into();
                next.no_progress = next.no_progress.saturating_add(1);
            }
        } else {
            next.no_progress = 0;
        }
        self.commit_version(
            &s,
            next,
            "authenticated native node result; candidate only",
            version,
            true,
        )
    }
    pub fn outputs(&self) -> Result<BTreeMap<String, Value>> {
        let s = self.state()?;
        self.fresh(&s)?;
        require(
            s.slots.values().all(|slot| slot.phase == Phase::Candidate),
            Kind::Conflict,
            "workflow has unfinished/failed nodes",
        )?;
        s.plan
            .outputs
            .iter()
            .map(|id| {
                Ok((
                    id.clone(),
                    s.slots[id]
                        .output
                        .clone()
                        .ok_or_else(|| error(Kind::Store, "final output missing"))?,
                ))
            })
            .collect()
    }
    pub fn revise(&mut self, plan: Plan, catalog: &Catalog) -> Result<State> {
        self.revise_inner(plan, catalog, None)
    }

    fn revise_inner(
        &mut self,
        plan: Plan,
        catalog: &Catalog,
        repair: Option<crate::adaptive::Repair>,
    ) -> Result<State> {
        let s = self.state()?;
        require(
            !s.cancel_requested && !self.cancelled.load(Ordering::Acquire),
            Kind::Cancelled,
            "cancelled workflow cannot be revised",
        )?;
        idle(&self.store, &self.task)?;
        require(
            !s.slots
                .values()
                .any(|n| matches!(n.phase, Phase::Leased | Phase::NeedsReconciliation)),
            Kind::Conflict,
            "active/uncertain lease cannot be replaced",
        )?;
        let parent = self.store.journal().task(&self.task)?;
        require(
            matches!(
                parent.record.state,
                TaskState::Running | TaskState::Repairing | TaskState::Verifying
            ),
            Kind::Cancelled,
            "parent task is not active",
        )?;
        plan.validate(&parent.contract, catalog)?;
        require(
            (plan.mode == crate::plan::Mode::AdaptiveSerial) == s.adaptive.is_some(),
            Kind::Authority,
            "cannot change adaptive ledger mode after execution starts",
        )?;
        if let Some(ledger) = &s.adaptive {
            require(
                plan.mode == crate::plan::Mode::AdaptiveSerial
                    && plan.max_no_progress == s.plan.max_no_progress
                    && plan.workspace_digest == s.plan.workspace_digest
                    && plan.policy_digest == s.plan.policy_digest
                    && ledger.revision_count < s.plan.max_no_progress,
                Kind::Authority,
                "adaptive mode/revision limit",
            )?;
            for node in &plan.nodes {
                require(
                    ledger.templates.values().any(|template| {
                        template.profile == node.profile
                            && template.tools == node.tools
                            && template.effects == node.effects
                            && template.resources == node.resources
                            && template.authorizations == node.authorizations
                            && template.claims == node.claims
                            && template.skills == node.skills
                            && template.output_schema == node.output_schema
                            && template.literal_input == node.literal_input
                    }),
                    Kind::Authority,
                    "revision exceeds initially approved specialist template",
                )?;
            }
            require(
                s.plan
                    .nodes
                    .iter()
                    .filter(|n| s.slots[&n.id].phase == Phase::Candidate)
                    .all(|n| plan.nodes.iter().any(|new| new == n)),
                Kind::Stale,
                "completed adaptive milestone must be retained",
            )?;
        }
        require(
            plan.task == s.plan.task
                && plan.id == s.plan.id
                && plan.revision == s.plan.revision + 1
                && plan.parent_digest == Some(s.plan.digest()?),
            Kind::Stale,
            "revision must bind exact prior plan",
        )?;
        let fresh = (self.refresh)()?;
        require(
            fresh.workspace_digest == plan.workspace_digest
                && fresh.policy_digest == plan.policy_digest,
            Kind::Stale,
            "revised plan is not current",
        )?;
        let source_changed = plan.workspace_digest != s.plan.workspace_digest
            || plan.policy_digest != s.plan.policy_digest;
        let mut slots = BTreeMap::new();
        for node in &plan.nodes {
            let old = s.slots.get(&node.id);
            let unchanged = s.plan.nodes.iter().find(|n| n.id == node.id) == Some(node);
            if !source_changed && old.is_some_and(|o| o.phase == Phase::Candidate) {
                require(
                    unchanged,
                    Kind::Stale,
                    "completed node contract cannot be edited and reused",
                )?;
                slots.insert(node.id.clone(), old.unwrap().clone());
            } else {
                slots.insert(
                    node.id.clone(),
                    Slot {
                        phase: Phase::Pending,
                        generation: old.map_or(0, |o| o.generation),
                        lease: None,
                        output: None,
                        native_run_ref: None,
                    },
                );
            }
        }
        // A reused candidate cannot depend on a recomputed/replaced result.
        for node in &plan.nodes {
            if slots[&node.id].phase == Phase::Candidate {
                require(
                    node.after
                        .iter()
                        .all(|d| slots[d].phase == Phase::Candidate),
                    Kind::Stale,
                    "completed descendant cannot retain stale dependency output",
                )?;
            }
        }
        let mut next = s.clone();
        next.plan = plan;
        next.slots = slots;
        next.active_handoff = None;
        if let Some(ledger) = &mut next.adaptive {
            ledger.revision_count += 1;
            if let Some(repair) = repair {
                ledger.repairs.push(repair);
            }
            if let Some(proposal) = ledger.pending_proposal.take() {
                ledger
                    .applied_proposals
                    .insert(crate::digest(&serde_json::to_vec(
                        &proposal.remaining_steps,
                    )?));
            }
            ledger.next_action = "Run the next revised serial milestone".into();
        } else {
            next.no_progress = 0;
        }
        self.commit_version(
            &s,
            next,
            "host-validated plan revision; no budget reset",
            parent.record.version,
            true,
        )
    }
    /// Host-observed source state. The caller reads through the task's pinned
    /// repository capability; model tools cannot populate this receipt.
    pub fn observe_source(
        &mut self,
        source: String,
        changed: std::collections::BTreeSet<String>,
    ) -> Result<State> {
        let state = self.state()?;
        require(
            crate::plan::hash(&source)
                && changed.len() <= 10000
                && changed.iter().all(|p| crate::plan::path(p) && p != "/"),
            Kind::Invalid,
            "source receipt format",
        )?;
        if state.adaptive.is_none() {
            return Ok(state);
        }
        require(
            !state
                .slots
                .values()
                .any(|s| matches!(s.phase, Phase::Leased | Phase::NeedsReconciliation)),
            Kind::Conflict,
            "source handoff requires a settled specialist",
        )?;
        self.fresh(&state)?;
        idle(&self.store, &self.task)?;
        let mut next = state.clone();
        let ledger = next.adaptive.as_mut().expect("checked");
        if ledger.source_version == source && ledger.changed_files == changed {
            return Ok(state);
        }
        ledger
            .verified_facts
            .retain(|key, value| !key.starts_with("successful_check:") || value == &source);
        ledger.diagnostics.retain(|_, d| d.source_digest == source);
        if ledger.source_version != source {
            let current = ledger
                .observation_refs
                .keys()
                .map(|tool| digest(format!("{tool}:{source}").as_bytes()))
                .collect::<std::collections::BTreeSet<_>>();
            ledger
                .repeated_failures
                .retain(|key, _| current.contains(key));
        }
        ledger.source_version = source.clone();
        ledger.changed_files = changed;
        ledger
            .verified_facts
            .insert("host_observed_source_digest".into(), source);
        self.commit(
            &state,
            next,
            "host-observed repository source for specialist handoff",
        )
    }

    /// Trusted adapter observation; this is not a model-facing fact writer.
    pub fn observe_check(
        &mut self,
        lease: &Lease,
        tool: &str,
        source: &str,
        success: bool,
        content: &str,
    ) -> Result<Option<String>> {
        require(
            identifier(tool) && crate::plan::hash(source) && content.len() <= 4 * 1024 * 1024,
            Kind::Invalid,
            "check receipt bounds",
        )?;
        let state = self.state()?;
        let version = self.check_state(&state, lease)?;
        if state.adaptive.is_none() {
            return Ok(None);
        }
        let artifact = format!("workflow-check:{}:{}", lease.run, state.sequence);
        let previous = state.adaptive.as_ref().expect("checked");
        let patch = if tool == "after_edit" {
            Some(artifact.clone())
        } else {
            previous.last_patch.clone()
        };
        let diagnostic = (!success)
            .then(|| crate::diagnostics::classify(tool, source, &artifact, patch.clone(), content));
        let receipt = serde_json::to_vec(
            &json!({"tool":tool,"run":lease.run,"source_digest":source,"success":success,"diagnostic":diagnostic,"content":content,"task_accepted":false}),
        )?;
        self.store.put_artifact(
            &ArtifactRequest {
                artifact_id: artifact.clone(),
                task_id: self.task.clone(),
                content_type: "application/json".into(),
                logical_type: "workflow.host-check".into(),
                schema_version: "v1".into(),
                retain_until_ms: 0,
                legal_hold: false,
            },
            &receipt,
        )?;
        let mut next = state.clone();
        let ledger = next.adaptive.as_mut().expect("checked");
        require(
            ledger.observation_refs.len() < 256 || ledger.observation_refs.contains_key(tool),
            Kind::Invalid,
            "check tool capacity",
        )?;
        ledger
            .observation_refs
            .insert(tool.into(), artifact.clone());
        ledger.current_diagnostics = vec![format!(
            "{tool}: {}; source={source}; evidence={artifact}",
            if success { "PASS" } else { "FAIL" }
        )];
        ledger.last_patch = patch;
        let signature = digest(format!("{tool}:{source}").as_bytes());
        if success {
            ledger.diagnostics.remove(tool);
            if tool == "code_tests" {
                ledger.diagnostics.retain(|name, diagnostic| {
                    !name.starts_with("code_targeted_tests:") || diagnostic.source_digest != source
                });
                ledger.open_questions.retain(|question| {
                    !question.starts_with("Resolve failed check: code_targeted_tests:")
                });
            }
            ledger.repeated_failures.remove(&signature);
            ledger
                .verified_facts
                .insert(format!("successful_check:{tool}"), source.into());
            ledger
                .open_questions
                .retain(|q| q != &format!("Resolve failed check: {tool}"));
        } else {
            ledger
                .verified_facts
                .remove(&format!("successful_check:{tool}"));
            if let Some(diagnostic) = diagnostic {
                ledger.diagnostics.insert(tool.into(), diagnostic);
            }
            require(
                ledger.repeated_failures.len() < 256
                    || ledger.repeated_failures.contains_key(&signature),
                Kind::Invalid,
                "failure signature capacity",
            )?;
            let count = ledger.repeated_failures.entry(signature).or_default();
            *count = count.saturating_add(1);
            let question = format!("Resolve failed check: {tool}");
            if !ledger.open_questions.contains(&question) {
                ledger.open_questions.push(question);
            }
        }
        self.commit_version(
            &state,
            next,
            "source-bound host check observation",
            version,
            true,
        )?;
        Ok(Some(artifact))
    }

    /// Reuse only explicit successful native evidence at the exact source.
    /// The caller is responsible for checking the operation identity/arguments.
    pub fn check_evidence(
        &self,
        lease: &Lease,
        tool: &str,
        source: &str,
    ) -> Result<Option<String>> {
        let state = self.state()?;
        self.check_state(&state, lease)?;
        let Some(ledger) = &state.adaptive else {
            return Ok(None);
        };
        let signature = digest(format!("{tool}:{source}").as_bytes());
        require(
            ledger
                .repeated_failures
                .get(&signature)
                .copied()
                .unwrap_or(0)
                < state.plan.max_no_progress,
            Kind::Conflict,
            "unchanged check already exhausted the retry allowance",
        )?;
        Ok(ledger
            .verified_facts
            .get(&format!("successful_check:{tool}"))
            .filter(|observed| *observed == source)
            .and_then(|_| ledger.observation_refs.get(tool))
            .cloned())
    }

    /// Publish a recovery point after a settled milestone and its source
    /// observation. This receipt lives in the authenticated workflow state.
    pub fn checkpoint_milestone(&mut self) -> Result<()> {
        let state = self.state()?;
        let version = self.fresh(&state)?;
        idle(&self.store, &self.task)?;
        require(
            state.active_handoff.is_none()
                && state.slots.values().all(|s| s.phase != Phase::Leased),
            Kind::Conflict,
            "milestone checkpoint requires settled leases",
        )?;
        let Some(ledger) = &state.adaptive else {
            return Ok(());
        };
        if ledger.completed_milestones.is_empty() {
            return Ok(());
        }
        let plan = state.plan.digest()?;
        let budget = self.store.journal().model_budget(&self.task)?;
        if ledger.milestone_checkpoint.as_ref().is_some_and(|m| {
            m.source_digest == ledger.source_version
                && m.plan_digest == plan
                && m.task_version == version
                && m.budget == budget
                && m.completed == ledger.completed_milestones
        }) {
            return Ok(());
        }
        let mut next = state.clone();
        let ledger = next.adaptive.as_mut().expect("checked");
        ledger.milestone_checkpoint = Some(crate::adaptive::MilestoneCheckpoint {
            source_digest: ledger.source_version.clone(),
            plan_digest: plan,
            task_version: version,
            budget,
            completed: ledger.completed_milestones.clone(),
        });
        self.commit_version(
            &state,
            next,
            "authenticated settled milestone",
            version,
            true,
        )?;
        Ok(())
    }

    pub fn validate_milestone(&self, source: &str) -> Result<()> {
        let state = self.state()?;
        let version = self.fresh(&state)?;
        idle(&self.store, &self.task)?;
        let ledger = state
            .adaptive
            .as_ref()
            .ok_or_else(|| error(Kind::Stale, "missing adaptive milestone"))?;
        let checkpoint = ledger
            .milestone_checkpoint
            .as_ref()
            .ok_or_else(|| error(Kind::Stale, "missing settled milestone"))?;
        let budget = self.store.journal().model_budget(&self.task)?;
        require(
            state.active_handoff.is_none()
                && state.slots.values().all(|s| s.phase != Phase::Leased)
                && checkpoint.source_digest == source
                && ledger.source_version == source
                && checkpoint.task_version == version
                && checkpoint.plan_digest == state.plan.digest()?
                && checkpoint.budget == budget
                && checkpoint.completed == ledger.completed_milestones,
            Kind::Stale,
            "source, task version, budget or milestone changed after checkpoint",
        )
    }

    /// Schedule a scoped repair only before final review, or after a
    /// diagnostically explained specialist failure. Prior results stay durable.
    pub fn repair_from_diagnostics(&mut self, catalog: &Catalog) -> Result<bool> {
        let state = self.state()?;
        let Some(ledger) = &state.adaptive else {
            return Ok(false);
        };
        let next = state
            .plan
            .nodes
            .iter()
            .find(|n| state.slots[&n.id].phase != Phase::Candidate);
        if next.is_some_and(|n| {
            n.profile != "melissa.code.reviewer" && state.slots[&n.id].phase != Phase::Failed
        }) {
            return Ok(false);
        }
        self.fresh(&state)?;
        idle(&self.store, &self.task)?;
        require(
            state.active_handoff.is_none()
                && state.slots.values().all(|s| s.phase != Phase::Leased),
            Kind::Conflict,
            "repair requires a settled specialist boundary",
        )?;
        let candidate = ledger
            .diagnostics
            .values()
            .find(|d| d.source_digest == ledger.source_version && d.repairable);
        let Some(diagnostic) = candidate else {
            for last in &ledger.repairs {
                let key = format!("successful_check:{}", last.diagnostic.tool);
                let grammar_fix = last.diagnostic.tool == "after_edit"
                    && ledger
                        .verified_facts
                        .get("successful_check:code_project_check")
                        == Some(&ledger.source_version);
                let full_public_suite = last.diagnostic.tool.starts_with("code_targeted_tests:")
                    && ledger.verified_facts.get("successful_check:code_tests")
                        == Some(&ledger.source_version);
                require(
                    grammar_fix
                        || full_public_suite
                        || ledger.verified_facts.get(&key) == Some(&ledger.source_version),
                    Kind::RepairEvidence,
                    "repair needs an actual passing re-test on current source",
                )?;
            }
            return Ok(false);
        };
        require(
            !ledger
                .repairs
                .iter()
                .any(|r| r.diagnostic.fingerprint == diagnostic.fingerprint),
            Kind::RepairEvidence,
            "unchanged diagnostic persists after a bounded repair",
        )?;
        // Use the same initial writer and final verification templates as the
        // approved plan. Read-only tasks never obtain a writer here.
        let writer = ledger.templates.iter().find(|(_, n)| {
            n.effects.contains("workspace.apply_patch_scoped")
                && n.claims
                    .iter()
                    .any(|claim| claim.access == crate::plan::Access::Write)
        });
        let tester = ledger.templates.iter().rev().find(|(_, n)| {
            n.profile == "melissa.code.test-engineer"
                && !n.effects.contains("workspace.apply_patch_scoped")
        });
        let reviewer = ledger.templates.iter().rev().find(|(_, n)| {
            n.profile == "melissa.code.reviewer"
                && !n.effects.contains("workspace.apply_patch_scoped")
        });
        let (Some((writer, _)), Some((tester, test)), Some((reviewer, review))) =
            (writer, tester, reviewer)
        else {
            return Err(error(
                Kind::Authority,
                "repair requires already approved writer and independent verifier templates",
            ));
        };
        let tool = diagnostic
            .tool
            .split(':')
            .next()
            .unwrap_or(&diagnostic.tool);
        let cases = if diagnostic.targeted_case_ids.is_empty() {
            String::new()
        } else {
            format!(
                " Exact approved case IDs: {}.",
                serde_json::to_string(&diagnostic.targeted_case_ids)?
            )
        };
        let proposal = crate::adaptive::Proposal {
            expected_plan_digest: state.plan.digest()?,
            source_version: ledger.source_version.clone(),
            reason: "Host observed a source-bound development failure".into(),
            remaining_steps: vec![
                crate::adaptive::StepProposal {
                    template: writer.clone(),
                    goal: format!(
                        "Repair the cause documented by {} within the existing file scope. Use the structured diagnostic in the handoff. Repeat {tool} on current source and retain the result.{cases} For an after-edit failure, verify syntax/type. Refresh any outstanding earlier repair checks on this source.",
                        diagnostic.evidence_ref
                    ),
                },
                crate::adaptive::StepProposal {
                    template: tester.clone(),
                    goal: format!(
                        "{} Confirm the repair with current-source tests and retain evidence.",
                        test.goal
                    ),
                },
                crate::adaptive::StepProposal {
                    template: reviewer.clone(),
                    goal: review.goal.clone(),
                },
            ],
        };
        let revised = crate::adaptive::revise(&state, &proposal)?;
        let repair = crate::adaptive::Repair {
            diagnostic: diagnostic.clone(),
            revision: revised.revision,
        };
        self.revise_inner(revised, catalog, Some(repair))?;
        Ok(true)
    }

    pub fn propose(&mut self, lease: &Lease, proposal: crate::adaptive::Proposal) -> Result<State> {
        let state = self.state()?;
        let version = self.check_state(&state, lease)?;
        require(
            state
                .adaptive
                .as_ref()
                .is_some_and(|l| l.pending_proposal.is_none()),
            Kind::Conflict,
            "one pending planner proposal per boundary",
        )?;
        crate::adaptive::revise(&state, &proposal)?;
        let mut next = state.clone();
        next.adaptive.as_mut().expect("checked").pending_proposal = Some(proposal);
        self.commit_version(
            &state,
            next,
            "untrusted bounded plan proposal awaiting host boundary",
            version,
            true,
        )
    }

    pub fn apply_proposal(&mut self, source: &str, catalog: &Catalog) -> Result<bool> {
        let state = self.state()?;
        let Some(proposal) = state
            .adaptive
            .as_ref()
            .and_then(|l| l.pending_proposal.as_ref())
        else {
            return Ok(false);
        };
        require(
            !state
                .slots
                .values()
                .any(|s| matches!(s.phase, Phase::Leased | Phase::NeedsReconciliation)),
            Kind::Conflict,
            "planner activation requires a settled boundary",
        )?;
        self.fresh(&state)?;
        idle(&self.store, &self.task)?;
        require(
            state
                .adaptive
                .as_ref()
                .is_some_and(|l| l.source_version == source),
            Kind::Stale,
            "activation requires the host-observed current source",
        )?;
        if proposal.source_version != source {
            let mut next = state.clone();
            let ledger = next.adaptive.as_mut().expect("checked");
            ledger.pending_proposal = None;
            ledger.current_diagnostics =
                vec!["Plan proposal rejected because its source changed".into()];
            next.no_progress = next.no_progress.saturating_add(1);
            self.commit(&state, next, "stale planner proposal rejected")?;
            return Ok(false);
        }
        let plan = crate::adaptive::revise(&state, proposal)?;
        self.revise(plan, catalog)?;
        Ok(true)
    }

    /// Only the authenticated task completion record can promote a delivery.
    /// Calling twice after a recovered final publication is harmless.
    pub fn record_acceptance(&mut self, artifact: &str) -> Result<()> {
        let state = self.state()?;
        if state.adaptive.is_none() {
            return Ok(());
        }
        let task = self.store.journal().task(&self.task)?;
        let meta = self.store.artifact_metadata(&self.task, artifact)?;
        require(
            task.record.state == TaskState::Completed
                && task.record.final_artifact_digest.as_ref() == Some(&meta.digest),
            Kind::Authority,
            "only host-completed final artifact is accepted",
        )?;
        let mut next = state.clone();
        let ledger = next.adaptive.as_mut().expect("checked");
        if ledger.accepted_artifacts.iter().any(|id| id == artifact) {
            return Ok(());
        }
        ledger.accepted_artifacts.push(artifact.into());
        ledger
            .verified_facts
            .insert("host_accepted_final_digest".into(), meta.digest);
        ledger.next_action = "Task completed by protected host acceptance".into();
        ledger.open_questions.clear();
        self.commit(&state, next, "authenticated host acceptance")?;
        Ok(())
    }

    pub fn supervisor(&self) -> Result<Value> {
        let state = self.state()?;
        let budget = self.store.journal().model_budget(&self.task)?;
        let parent = self.store.journal().task(&self.task)?;
        let decision = if parent.record.state == TaskState::Completed {
            "completed"
        } else if state.cancel_requested || self.cancelled.load(Ordering::Acquire) {
            "cancelled"
        } else if state
            .slots
            .values()
            .any(|s| s.phase == Phase::NeedsReconciliation)
            || budget.unknown_requests > 0
        {
            "reconciliation_required"
        } else if budget.budget_exceeded || now()? >= state.deadline_ms {
            "budget_or_deadline_exhausted"
        } else if state.no_progress >= state.plan.max_no_progress
            || state.adaptive.as_ref().is_some_and(|l| {
                l.repeated_failures
                    .values()
                    .any(|n| *n >= state.plan.max_no_progress)
            })
        {
            "no_progress_stop"
        } else if state.slots.values().any(|s| s.phase == Phase::Leased)
            || budget.active_requests > 0
        {
            "specialist_active"
        } else if state
            .adaptive
            .as_ref()
            .is_some_and(|l| l.pending_proposal.is_some())
        {
            "validate_pending_plan"
        } else if state.slots.values().any(|s| s.phase == Phase::Failed) {
            "replan_required"
        } else if state.slots.values().all(|s| s.phase == Phase::Candidate) {
            "protected_qa_ready"
        } else {
            "next_specialist_ready"
        };
        let recommended = if decision == "replan_required" {
            state
                .adaptive
                .as_ref()
                .map(|ledger| {
                    let mut templates = Vec::new();
                    for role in [
                        "melissa.code.debugger",
                        "melissa.code.implementer",
                        "melissa.code.test-engineer",
                        "melissa.code.reviewer",
                    ] {
                        if let Some((id, _)) =
                            ledger.templates.iter().find(|(_, n)| n.profile == role)
                        {
                            templates.push(id.clone());
                        }
                    }
                    templates
                })
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        Ok(
            json!({"decision":decision,"recommended_remaining_templates":recommended,"active_specialists":state.slots.values().filter(|s| s.phase == Phase::Leased).count(),
            "revision":state.plan.revision,"no_progress":state.no_progress,"shared_budget":budget,
            "next_milestone":state.plan.nodes.iter().find(|n| state.slots[&n.id].phase == Phase::Pending).map(|n| &n.id)}),
        )
    }

    pub fn no_progress(&mut self, reason: &str) -> Result<State> {
        require(
            !reason.trim().is_empty(),
            Kind::Invalid,
            "progress reason required",
        )?;
        let s = self.state()?;
        let version = self.fresh(&s)?;
        let mut next = s.clone();
        next.no_progress = next
            .no_progress
            .checked_add(1)
            .ok_or_else(|| error(Kind::Invalid, "progress overflow"))?;
        self.commit_version(&s, next, reason, version, true)
    }
    pub fn mark_uncertain(&mut self, lease: &Lease, reason: &str) -> Result<State> {
        let s = self.state()?;
        require(
            s.slots
                .get(&lease.node)
                .is_some_and(|v| v.phase == Phase::Leased && v.lease.as_ref() == Some(lease)),
            Kind::Stale,
            "cannot alter stale lease",
        )?;
        let mut next = s.clone();
        next.slots
            .get_mut(&lease.node)
            .expect("checked lease")
            .phase = Phase::NeedsReconciliation;
        let result = self.commit(&s, next, reason)?;
        self.cancelled.store(true, Ordering::Release);
        Ok(result)
    }
    pub fn cancel(&mut self) -> Result<State> {
        let s = self.state()?;
        if !s.cancel_requested {
            let mut next = s.clone();
            next.cancel_requested = true;
            for slot in next.slots.values_mut() {
                if slot.phase == Phase::Pending {
                    slot.phase = Phase::Cancelled;
                }
            }
            self.commit(
                &s,
                next,
                "durable root cancellation; outstanding effects must settle",
            )?;
        }
        self.cancelled.store(true, Ordering::Release);
        let parent = self.store.journal().task(&self.task)?;
        if !parent.record.state.is_terminal() && idle(&self.store, &self.task).is_ok() {
            self.store.transition_task(
                &self.task,
                parent.record.version,
                TaskState::Cancelled,
                None,
            )?;
        }
        self.state()
    }
    pub fn settle_cancellation(&mut self) -> Result<State> {
        let s = self.state()?;
        require(
            s.cancel_requested,
            Kind::Invalid,
            "cancellation not requested",
        )?;
        idle(&self.store, &self.task)?;
        let parent = self.store.journal().task(&self.task)?;
        if !parent.record.state.is_terminal() {
            self.store.transition_task(
                &self.task,
                parent.record.version,
                TaskState::Cancelled,
                None,
            )?;
        }
        let mut next = s.clone();
        next.cancellation_settled = true;
        for slot in next.slots.values_mut() {
            if slot.phase == Phase::Leased {
                slot.phase = Phase::Cancelled;
            }
        }
        self.commit(
            &s,
            next,
            "owned model/worker effects known settled after cancellation",
        )
    }
}
