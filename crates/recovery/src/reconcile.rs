//! Read actual files against the authentic admitted patch/preimages. A full
//! preimage or postimage can be resolved; mixed/foreign/partial effects remain
//! unknown. Non-idempotent operations without such proof remain blocked.
use crate::{Result, checked, digest, session::put, store_error};
use melissa_local_store::LocalStore;
use melissa_model_gateway::ErrorKind;
use melissa_repository::{PatchProposal, Repository};
use serde_json::{Value, json};

/// Resolve only an interrupted disposable web startup with NO subsequent
/// application commands. Authenticated native report, admission, input and the
/// complete stdout must agree; an API/SQL/browser action excludes this path.
/// Cleanup alone never clears an interrupted write or admits a retry.
pub fn inspect_start_only_web_effect(
    store: &mut LocalStore,
    task: &str,
    invocation: &str,
) -> Result<Value> {
    use melissa_container_executor::{ContainerReport, Snapshot, TerminationReason};
    use melissa_core_contracts::{InvocationState, ReconciliationEvidence, TaskState};
    let owned = store
        .journal()
        .invocation(invocation)
        .map_err(store_error)?;
    let admission = store
        .journal()
        .managed_execution(invocation)
        .map_err(store_error)?;
    checked(
        owned.task_id == task
            && admission.task_id == task
            && admission.invocation_id == invocation
            && admission.tool_id == owned.tool_id
            && admission.generation == owned.generation
            && matches!(
                owned.state,
                InvocationState::OutcomeUnknown | InvocationState::NeedsReconciliation
            ),
        ErrorKind::Authorization,
        "requires exact owned unknown web startup",
    )?;
    let budget = store.journal().model_budget(task).map_err(store_error)?;
    checked(
        budget.active_requests == 0 && budget.unknown_requests == 0,
        ErrorKind::Store,
        "model usage must already be settled",
    )?;
    let proposal_id = owned.arguments["proposal_artifact_id"]
        .as_str()
        .ok_or_else(|| store_error("web proposal"))?;
    let proposal_bytes = store.get_artifact(task, proposal_id).map_err(store_error)?;
    let proposal: Value = serde_json::from_slice(&proposal_bytes).map_err(store_error)?;
    checked(
        store
            .artifact_metadata(task, proposal_id)
            .map_err(store_error)?
            .logical_type
            == "web.proposal"
            && proposal_id == format!("{invocation}:web-proposal")
            && owned.arguments["proposal_digest"] == digest(&proposal_bytes)
            && digest(&proposal_bytes) == admission.permit_digest
            && proposal["schema_version"] == "melissa.web-dispatch/v1"
            && proposal["policy"]["task_id"] == task
            && proposal["policy"]["profile_id"] == admission.profile_id
            && proposal["policy"]["principal_id"] == admission.principal_id
            && proposal["policy"]["resource_id"] == admission.resource_id
            && proposal["permit_id"] == admission.permit_id,
        ErrorKind::Authorization,
        "startup proposal/admission mismatch",
    )?;
    let input_id = proposal["input_artifact_id"]
        .as_str()
        .ok_or_else(|| store_error("web input"))?;
    let input: Snapshot =
        serde_json::from_slice(&store.get_artifact(task, input_id).map_err(store_error)?)
            .map_err(store_error)?;
    let input_digest = input.digest().map_err(store_error)?;
    checked(
        proposal["snapshot_digest"] == input_digest,
        ErrorKind::Store,
        "startup snapshot mismatch",
    )?;
    let attempt = &admission.attempt_id;
    let request_ids = store
        .list_artifacts(task)
        .map_err(store_error)?
        .into_iter()
        .filter(|m| {
            m.artifact_id
                .starts_with(&format!("{attempt}:web-request:"))
        })
        .map(|m| m.artifact_id)
        .collect::<Vec<_>>();
    checked(
        request_ids == [format!("{attempt}:web-request:1")],
        ErrorKind::Authorization,
        "application command exists; start-only reconciliation denied",
    )?;
    let request: Value = serde_json::from_slice(
        &store
            .get_artifact(task, &request_ids[0])
            .map_err(store_error)?,
    )
    .map_err(store_error)?;
    checked(
        request == json!({"seq":1,"operation":{"command":"start"}}),
        ErrorKind::Authorization,
        "only exact startup is reconcilable",
    )?;
    let final_id = format!("{attempt}:web-final");
    checked(
        store
            .artifact_metadata(task, &final_id)
            .map_err(store_error)?
            .logical_type
            == "web.final",
        ErrorKind::Store,
        "native final type",
    )?;
    let final_bytes = store.get_artifact(task, &final_id).map_err(store_error)?;
    let outcome: Value = serde_json::from_slice(&final_bytes).map_err(store_error)?;
    let worker: ContainerReport =
        serde_json::from_value(outcome["worker"].clone()).map_err(store_error)?;
    checked(
        outcome["native_attempt_id"] == *attempt
            && outcome["source_digest"] == proposal["source_digest"]
            && outcome["session_id"] == proposal["session_id"]
            && outcome["application_outcome_unknown"] == false
            && outcome["executor_error"].is_null()
            && worker.image_id == proposal["request"]["image_id"]
            && worker.input_digest == input_digest
            && worker.execution_contract == melissa_container_executor::EXECUTION_CONTRACT
            && worker.cleanup_confirmed
            && worker.tree_confirmed_stopped
            && worker.policy_verified
            && !worker.output_truncated
            && !worker.export_truncated
            && worker.monitor_error.is_none()
            && worker.orphan_process_ids.is_empty()
            && worker.termination == TerminationReason::Cancelled
            && worker.exit_code == Some(0),
        ErrorKind::Store,
        "native cancelled startup/cleanup not proven",
    )?;
    let identity: Value = serde_json::from_slice(
        &store
            .get_artifact(task, &format!("{attempt}:web-container-identity"))
            .map_err(store_error)?,
    )
    .map_err(store_error)?;
    checked(
        identity["task_id"] == task
            && identity["invocation_id"] == invocation
            && identity["attempt_id"] == *attempt
            && identity["container_id"] == worker.container_id
            && identity["image"] == worker.image_id
            && identity["permit_digest"] == admission.permit_digest,
        ErrorKind::Authorization,
        "startup native identity differs",
    )?;
    let mut actual = vec![];
    let mut parts = 0;
    let mut total = 0;
    for (index, line) in std::str::from_utf8(&worker.stdout)
        .map_err(store_error)?
        .lines()
        .enumerate()
    {
        let p: Value = serde_json::from_str(line).map_err(store_error)?;
        if index == 0 {
            parts = p["parts"].as_u64().unwrap_or(0);
            total = p["total_bytes"].as_u64().unwrap_or(0);
        }
        checked(
            p["schema_version"] == "melissa.web-fragment/v1"
                && p["seq"] == 1
                && p["index"] == index
                && p["parts"] == parts
                && p["total_bytes"] == total
                && parts > 0
                && parts <= 16
                && total <= 1024 * 1024,
            ErrorKind::Store,
            "startup stdout has additional or invalid frames",
        )?;
        let h = p["data_hex"]
            .as_str()
            .ok_or_else(|| store_error("startup hex"))?;
        checked(
            h.is_ascii() && h.bytes().all(|b| b.is_ascii_hexdigit()) && h.len().is_multiple_of(2),
            ErrorKind::Store,
            "startup hex length",
        )?;
        for i in (0..h.len()).step_by(2) {
            actual.push(u8::from_str_radix(&h[i..i + 2], 16).map_err(store_error)?);
        }
    }
    checked(
        worker.stdout.iter().filter(|&&b| b == b'\n').count() as u64 == parts
            && actual.len() as u64 == total
            && actual
                == store
                    .get_artifact(task, &format!("{attempt}:web-reply:1"))
                    .map_err(store_error)?,
        ErrorKind::Store,
        "startup stdout/CAS incomplete or different",
    )?;
    let reply: Value = serde_json::from_slice(&actual).map_err(store_error)?;
    checked(
        reply["seq"] == 1
            && reply["ok"] == true
            && reply["closed"] == false
            && reply["source_digest"] == proposal["source_digest"],
        ErrorKind::Store,
        "startup reply differs",
    )?;
    let report = json!({"schema_version":"melissa.start-only-web-reconciliation/v1",
        "task_id":task,"invocation_id":invocation,"native_final_digest":digest(&final_bytes),
        "proposal_digest":digest(&proposal_bytes),"effect_disposition":"cancelled_startup_no_application_command",
        "cleanup_confirmed":true,"application_commands_dispatched":0,"automatic_replay_allowed":false,"task_accepted":false});
    let bytes = serde_json::to_vec(&report).map_err(store_error)?;
    let id = format!("startup-reconciliation:{invocation}:{}", digest(&bytes));
    put(
        store,
        task,
        &id,
        "recovery.web_startup_reconciliation",
        &bytes,
    )?;
    let mut current = store.journal().task(task).map_err(store_error)?;
    if current.record.state == TaskState::OutcomeUnknown {
        current = store
            .transition_task(
                task,
                current.record.version,
                TaskState::NeedsReconciliation,
                None,
            )
            .map_err(store_error)?;
    }
    store
        .reconcile_invocation(
            current.record.version,
            owned.version,
            &ReconciliationEvidence {
                task_id: task.into(),
                invocation_id: invocation.into(),
                resolution_artifact_ref: id.clone(),
                external_effect_resolved: true,
            },
        )
        .map_err(store_error)?;
    Ok(json!({"artifact_id":id,"proof":report,"automatic_replay_allowed":false}))
}

pub fn inspect_patch_effect(
    store: &mut LocalStore,
    repo: &Repository,
    task: &str,
    invocation: &str,
) -> Result<Value> {
    let owned = store
        .journal()
        .invocation(invocation)
        .map_err(store_error)?;
    checked(
        owned.task_id == task
            && owned.tool_id == "fs.patch"
            && owned.arguments["schema_version"] == "melissa.repository-dispatch/v1",
        ErrorKind::Authorization,
        "reconciliation requires authentic owned patch dispatch",
    )?;
    let artifact = owned.arguments["proposal_artifact_id"]
        .as_str()
        .ok_or_else(|| store_error("proposal reference"))?;
    let bytes = store.get_artifact(task, artifact).map_err(store_error)?;
    checked(
        owned.arguments["proposal_digest"] == digest(&bytes),
        ErrorKind::Store,
        "admitted patch proposal digest mismatch",
    )?;
    let proposal = PatchProposal::parse(&bytes).map_err(store_error)?;
    checked(
        proposal.root_identity == repo.root_identity()
            && owned.arguments["root_identity"] == repo.root_identity(),
        ErrorKind::Authorization,
        "reconciliation repository identity changed",
    )?;
    let mut all_before = true;
    let mut all_after = true;
    let mut files = vec![];
    for (i, file) in proposal.files.iter().enumerate() {
        let before = store
            .get_artifact(task, &format!("{invocation}:preimage:{i}"))
            .map_err(store_error)?;
        checked(
            digest(&before) == file.before.digest && before.len() as u64 == file.before.bytes,
            ErrorKind::Store,
            "reconciliation preimage changed",
        )?;
        let mut after = before.clone();
        for edit in file.edits.iter().rev() {
            let start = usize::try_from(edit.start).map_err(store_error)?;
            let end = usize::try_from(edit.end).map_err(store_error)?;
            checked(
                end <= after.len() && start <= end && after[start..end] == edit.expected,
                ErrorKind::Store,
                "admitted patch preimage does not match",
            )?;
            after.splice(start..end, edit.replacement.clone());
        }
        let (actual, content) = repo.read(&file.before.path).map_err(store_error)?;
        checked(
            actual.identity == file.before.identity,
            ErrorKind::Authorization,
            "reconciliation file identity replaced",
        )?;
        all_before &= content == before;
        all_after &= content == after;
        files.push(json!({"path":file.before.path,"actual_version":actual,"before_digest":digest(&before),"after_digest":digest(&after)}));
    }
    checked(
        all_before || all_after,
        ErrorKind::Store,
        "mixed/partial/foreign file effects remain unresolved",
    )?;
    let disposition = if all_after {
        "postimage_present"
    } else {
        "preimage_present"
    };
    let proof = json!({"schema_version":"melissa.patch-reconciliation/v1","task_id":task,"invocation_id":invocation,"proposal_digest":digest(&bytes),"effect_disposition":disposition,"files":files,"task_accepted":false});
    let bytes = serde_json::to_vec(&proof).map_err(store_error)?;
    let id = format!("patch-reconciliation:{invocation}:{}", digest(&bytes));
    put(store, task, &id, "recovery.patch_reconciliation", &bytes)?;
    let current = store
        .journal()
        .invocation(invocation)
        .map_err(store_error)?;
    if matches!(
        current.state,
        melissa_core_contracts::InvocationState::OutcomeUnknown
            | melissa_core_contracts::InvocationState::NeedsReconciliation
    ) {
        let mut t = store.journal().task(task).map_err(store_error)?;
        if t.record.state == melissa_core_contracts::TaskState::OutcomeUnknown {
            t = store
                .transition_task(
                    task,
                    t.record.version,
                    melissa_core_contracts::TaskState::NeedsReconciliation,
                    None,
                )
                .map_err(store_error)?;
        }
        store
            .journal_mut()
            .reconcile_invocation(
                t.record.version,
                current.version,
                &melissa_core_contracts::ReconciliationEvidence {
                    task_id: task.into(),
                    invocation_id: invocation.into(),
                    resolution_artifact_ref: id.clone(),
                    external_effect_resolved: true,
                },
            )
            .map_err(store_error)?;
    }
    Ok(json!({"artifact_id":id,"proof":proof,"automatic_replay_allowed":false}))
}
