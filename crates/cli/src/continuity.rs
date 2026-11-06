//! Operator-only continuation review. Old model/provider effects never replay.
#[cfg(all(test, windows))]
mod tests;
pub mod undo;
use crate::{
    Error, Result,
    app::{App, bytes, decode, nonce, now, put},
    digest,
};
use melissa_local_store::LocalStore;
use melissa_repository::RepositorySnapshot;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, atomic::AtomicBool},
};

pub(crate) fn guard_pending_undo(store: &LocalStore, id: &str) -> Result {
    for artifact in store.list_artifacts(id)? {
        if artifact.logical_type == "cli.continuity.undo-intent" {
            let receipt = artifact
                .artifact_id
                .strip_suffix(":intent")
                .ok_or(Error::Store)?;
            let value: Value = decode(&store.get_artifact(id, receipt).map_err(|_| {
                Error::Reconciliation("Geri alma intent'i uzlaştırılmadan yürütme sürmez.")
            })?)?;
            if !matches!(value["outcome"].as_str(), Some("PASS" | "FAIL")) {
                return Err(Error::Reconciliation(
                    "Kısmi/belirsiz geri alma çözülmeden yürütme sürmez.",
                ));
            }
        }
    }
    Ok(())
}
pub(crate) fn optional_scheduler(
    app: &App,
    store: &LocalStore,
    id: &str,
) -> Result<Option<melissa_workflow::scheduler::Scheduler>> {
    if store
        .list_artifacts(id)?
        .iter()
        .any(|a| a.logical_type == "workflow.state")
    {
        scheduler(app, store, id).map(Some)
    } else {
        Ok(None)
    }
}
pub(crate) fn settled(app: &App, store: &LocalStore, id: &str) -> Result {
    guard_pending_undo(store, id)?;
    let task = store.journal().task(id)?;
    let b = store.journal().model_budget(id)?;
    if task.record.unknown_invocation_id.is_some()
        || b.active_requests != 0
        || b.unknown_requests != 0
        || b.unknown_cost_requests != 0
        || b.unknown_input_requests != 0
        || b.unknown_output_requests != 0
        || !crate::engine::native_stop_proven(store, id)?
    {
        return Err(Error::Reconciliation(
            "Önce etkin/belirsiz çağrı ve süreçleri uzlaştır; tekrar veya geri alma yapılmadı.",
        ));
    }
    if app.live(id).is_ok_and(|s| s.active()) {
        return Err(Error::Conflict("Önce görevi güvenli şekilde duraklat."));
    }
    for event in store.journal().events(id)? {
        if let Some(inv) = event.invocation {
            let current = store.journal().invocation(&inv.invocation_id)?;
            if current.active_attempt_id.is_some() {
                return Err(Error::Reconciliation(
                    "Etkin araç girişimi varken durum değişmez.",
                ));
            }
        }
    }
    Ok(())
}
pub(crate) fn scheduler(
    app: &App,
    store: &LocalStore,
    id: &str,
) -> Result<melissa_workflow::scheduler::Scheduler> {
    let task = store.journal().task(id)?;
    let execution = app
        .execution(store, id)?
        .ok_or(Error::Denied("Yürütülebilir görev gerekli."))?;
    let source = task.contract.input_snapshot_digest;
    let policy = execution.settings_digest;
    melissa_workflow::scheduler::Scheduler::open(
        store.connect()?,
        id.into(),
        "cli-workflow-v1".into(),
        Arc::new(move || {
            Ok(melissa_workflow::scheduler::Freshness {
                workspace_digest: source.clone(),
                policy_digest: policy.clone(),
            })
        }),
        Arc::new(AtomicBool::new(false)),
    )
    .map_err(|_| Error::Conflict("Workflow henüz başlamadı veya kaydı doğrulanamadı."))
}
pub(crate) fn paused_snapshot(
    app: &App,
    store: &LocalStore,
    id: &str,
    snapshot: &RepositorySnapshot,
) -> Result {
    let task = store.journal().task(id)?;
    if task.record.state != melissa_core_contracts::TaskState::Running {
        return Ok(());
    }
    let control = crate::control::Control::new(
        app,
        id,
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicBool::new(false)),
    )?;
    let mut shared = store.connect()?;
    put(
        &mut shared,
        id,
        &format!("cli:paused-source:{}", control.session),
        "paused.source",
        &bytes(&snapshot)?,
    )?;
    control.publish(
        "paused",
        json!({"task_id":id,"version":shared.journal().task(id)?.record.version,"state":"running"}),
        json!({"operation":"operator_source_review"}),
        serde_json::to_value(shared.journal().model_budget(id)?).map_err(|_| Error::Store)?,
        json!({}),
    )
}
impl App {
    pub fn continuity(&self, id: &str) -> Result<Value> {
        let store = self.store(id)?;
        let task = store.journal().task(id)?;
        let repo = self.source(&self.workspace(&store, id)?)?;
        let current = repo.snapshot(&AtomicBool::new(false))?;
        let mut checkpoints = vec![];
        for artifact in store.list_artifacts(id)? {
            if artifact.logical_type == "workflow.native-continuation" {
                let Some(cp) = artifact.artifact_id.strip_suffix(":artifact") else {
                    continue;
                };
                let proof = store.journal().checkpoint(cp, id)?;
                if proof.artifact_id != artifact.artifact_id {
                    return Err(Error::Store);
                }
                let boundary = &proof.data["host_boundary"];
                let snapshot = boundary["snapshot_ref"]
                    .as_str()
                    .map(|s| store.get_artifact(id, s))
                    .transpose()?;
                let source = if let Some(raw) = snapshot {
                    if boundary["snapshot_digest"] != digest(&raw) {
                        return Err(Error::Store);
                    }
                    decode::<RepositorySnapshot>(&raw)?.digest
                } else {
                    String::new()
                };
                checkpoints.push(json!({"id":cp,"phase":proof.data["phase"],"task_version":proof.task_version,"source_digest":source,"source_current":source==current.digest,"boundary":boundary,"resumption_grant":false}));
            } else if artifact.logical_type == "cli.continuity.manual" {
                let mut value: Value = decode(&store.get_artifact(id, &artifact.artifact_id)?)?;
                value["source_current"] = json!(value["source_digest"] == current.digest);
                value["id"] = json!(artifact.artifact_id);
                checkpoints.push(value);
            }
        }
        let workflow = optional_scheduler(self, &store, id)?
            .map(|s| s.state())
            .transpose()
            .map_err(|_| Error::Store)?;
        let remaining=workflow.as_ref().map(|s|s.plan.nodes.iter().map(|node|json!({"id":node.id,"profile":node.profile,"goal":node.goal,"state":s.slots[&node.id].phase,"current_source":s.adaptive.as_ref().map(|l|l.source_version==current.digest)})).collect::<Vec<_>>()).unwrap_or_default();
        let live = self.live(id).ok();
        let safe = settled(self, &store, id).is_ok();
        let mut result = json!({"command":"task continuity","task_id":id,"state":task.record.state,"source_digest":current.digest,"git_context":melissa_repository::git_context::observe(&repo),"model_budget":store.journal().model_budget(id)?,"checkpoints":checkpoints,"remaining_milestones":remaining.iter().filter(|m|m["state"]!="candidate").collect::<Vec<_>>(),"milestones":remaining,"settled":safe,"last_control_phase":live.as_ref().map(|s|&s.phase),"owner_alive":live.as_ref().map(|s|s.owner.alive()),"model_calls":0,"task_accepted":false,"resume_policy":"same source and settled original accounting: resume; changed source: explicit adopt-preview/adopt-source creates fresh runs, never replays pending effects"});
        if let Some(s) = workflow {
            result["plan_digest"] = json!(s.plan.digest().map_err(|_| Error::Store)?);
            result["handoff_source_current"] = json!(
                s.adaptive
                    .as_ref()
                    .map(|l| l.source_version == current.digest)
            );
            result["failed_approaches"] = json!(s.adaptive.as_ref().map(|l| &l.failed_approaches));
        }
        Ok(result)
    }
    pub fn checkpoint(&self, id: &str, label: &str) -> Result<Value> {
        if label.is_empty() || label.len() > 1024 || label.chars().any(char::is_control) {
            return Err(Error::Usage("Checkpoint etiketi geçersiz."));
        }
        let mut store = self.store(id)?;
        settled(self, &store, id)?;
        let repo = self.source(&self.workspace(&store, id)?)?;
        let source = repo.snapshot(&AtomicBool::new(false))?;
        let reference = format!("cli:checkpoint:{}", nonce()?);
        let value = json!({"schema_version":"melissa.operator-checkpoint/v1","task_id":id,"label":label,"created_at_ms":now()?,"source_digest":source.digest,"source_snapshot":source,"git_context":melissa_repository::git_context::observe(&repo),"budget":store.journal().model_budget(id)?,"plan_digest":optional_scheduler(self,&store,id)?.map(|s|s.state()).transpose().map_err(|_|Error::Store)?.map(|s|s.plan.digest()).transpose().map_err(|_|Error::Store)?,"resumption_grant":false,"contains_transcript":false});
        put(
            &mut store,
            id,
            &reference,
            "continuity.manual",
            &bytes(&value)?,
        )?;
        Ok(
            json!({"command":"task checkpoint","checkpoint":reference,"record":value,"model_calls":0}),
        )
    }
    pub fn adopt_preview(&self, id: &str) -> Result<Value> {
        let store = self.store(id)?;
        settled(self, &store, id)?;
        let task = store.journal().task(id)?;
        let live = self.live(id)?;
        if task.record.state != melissa_core_contracts::TaskState::Running || live.phase != "paused"
        {
            return Err(Error::Conflict(
                "Kaynak uzlaştırması güvenli paused running görev gerektirir.",
            ));
        }
        let e = self.execution(&store, id)?.ok_or(Error::Store)?;
        if now()? >= e.expires_at_ms
            || e.runtime_digest.as_deref() != Some(crate::app::runtime_digest()?.as_str())
            || e.settings_digest != self.settings()?.digest()?
        {
            return Err(Error::Conflict(
                "Özgün yürütme ayarı/sürümü/süresi geçerli değil.",
            ));
        }
        let repo = self.source(&self.workspace(&store, id)?)?;
        let source = repo.snapshot(&AtomicBool::new(false))?;
        let old: RepositorySnapshot =
            decode(&store.get_artifact(id, &format!("cli:paused-source:{}", live.session))?)?;
        let previous: BTreeMap<_, _> = old.files.iter().map(|f| (&f.path, &f.digest)).collect();
        let current: BTreeMap<_, _> = source.files.iter().map(|f| (&f.path, &f.digest)).collect();
        let changed = previous
            .keys()
            .chain(current.keys())
            .filter(|p| previous.get(*p) != current.get(*p))
            .map(|p| (*p).clone())
            .collect::<BTreeSet<_>>();
        if changed.iter().any(|p| {
            e.recipe.protected_paths.iter().any(|protected| {
                let path = p.to_uppercase();
                let prefix = protected.to_uppercase();
                path == prefix || path.starts_with(&(prefix + "/"))
            })
        }) {
            return Err(Error::Denied(
                "Korunan kabul dosyası değişti; aynı görevde kabul sözleşmesi değiştirilemez.",
            ));
        }
        let scheduler = optional_scheduler(self, &store, id)?;
        let previous_plan = scheduler
            .as_ref()
            .map(|s| s.state())
            .transpose()
            .map_err(|_| Error::Store)?;
        let (pending, _) = store
            .journal()
            .latest_checkpoint_in_series(id, "cli:pending-plan:")?;
        let discarded = if pending > 0
            && store
                .journal()
                .checkpoint(&format!("cli:applied-plan:{}", pending - 1), id)
                .is_err()
            && store
                .journal()
                .checkpoint(&format!("cli:discarded-plan:{}", pending - 1), id)
                .is_err()
        {
            Some(pending - 1)
        } else {
            None
        };
        let value = json!({"schema_version":"melissa.source-adoption/v1","task_id":id,"source_digest":source.digest,"source_snapshot":source,"git_context":melissa_repository::git_context::observe(&repo),"changed_paths":changed,"previous_plan_digest":previous_plan.as_ref().map(|s|s.plan.digest()).transpose().map_err(|_|Error::Store)?,"task_version":task.record.version,"budget":store.journal().model_budget(id)?,"discarded_pending_plan":discarded,"operator":"explicit_cli_review","effects":"invalidate current candidate outputs and leases; restart specialists with fresh generations; retain original evidence, budget, deadline, writes and acceptance"});
        Ok(
            json!({"command":"task adopt-preview","review_digest":digest(&bytes(&value)?),"review":value,"model_calls":0}),
        )
    }
    pub fn adopt_source(&self, id: &str, expected: &str) -> Result<Value> {
        let preview = self.adopt_preview(id)?;
        if preview["review_digest"] != expected {
            return Err(Error::Conflict(
                "Güncel kaynak/plan/bütçeyi kapsayan review_digest gerekli.",
            ));
        }
        let mut store = self.store(id)?;
        settled(self, &store, id)?;
        let repo = self.source(&self.workspace(&store, id)?)?;
        let snapshot: RepositorySnapshot = decode(&bytes(&preview["review"]["source_snapshot"])?)?;
        repo.validate_snapshot(&snapshot, &AtomicBool::new(false))?;
        let reference = format!("cli:source-adoption:{}", nonce()?);
        put(
            &mut store,
            id,
            &reference,
            "continuity.adoption",
            &bytes(&preview["review"])?,
        )?;
        if let Some(mut s) = optional_scheduler(self, &store, id)? {
            s.restart_observed_source(&snapshot.digest, &reference, &crate::resources::catalog()?)
                .map_err(|_| {
                    Error::Conflict("Kaynak yeniden bağlanamadı; önceki işlem geçmişi korundu.")
                })?;
        }
        if let Some(sequence) = preview["review"]["discarded_pending_plan"].as_u64() {
            let artifact = format!("cli:discarded-plan:{sequence}:evidence");
            put(
                &mut store,
                id,
                &artifact,
                "workflow.discarded-revision",
                &bytes(
                    &json!({"source_adoption":reference,"reason":"operator reviewed source change invalidates queued stale revision"}),
                )?,
            )?;
            let version = store.journal().task(id)?.record.version;
            store.append_checkpoint(
                &format!("cli:discarded-plan:{sequence}"),
                id,
                version,
                &artifact,
                json!({"source_adoption":reference}),
            )?;
        }
        paused_snapshot(self, &store, id, &snapshot)?;
        Ok(
            json!({"command":"task adopt-source","source_digest":snapshot.digest,"review":reference,"state":"paused","fresh_specialist_runs_required":true,"budget_reset":false,"model_calls":0,"task_accepted":false}),
        )
    }
}
