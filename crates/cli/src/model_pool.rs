//! Approved model selection at a specialist boundary. Native continuation stays
//! pinned to that model and its protocol for the lifetime of the lease.
use crate::{
    Error, Result,
    app::{bytes, decode, put},
    settings::Settings,
};
use melissa_local_store::{LocalStore, StoreError, journal::ModelBudgetSnapshot};
use melissa_model_gateway::{
    output_capabilities::{OutputCapabilityRegistration, OutputCapabilityRegistry, OutputMode},
    routing::{ModelRegistration, RouteRequirements, select_routes_for_output_mode},
    transport::{ApiKey, ProviderEndpoint},
};
use melissa_workflow::{
    adaptive::{Complexity, Signals, complexity},
    scheduler::WorkPacket,
};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, sync::Arc};

impl Settings {
    pub(crate) fn registration(
        &self,
        id: &str,
        endpoint: ProviderEndpoint,
        wall_time_ms: u64,
    ) -> ModelRegistration {
        ModelRegistration {
            registration_id: id.into(),
            endpoint,
            model_id: self.model.clone(),
            capabilities: self.capabilities,
            categories: BTreeSet::from(["coding".into(), "review".into(), "reasoning".into()]),
            context_tokens: self.context_tokens,
            max_output_tokens: self.max_output_tokens,
            timeout_ms: self.request_timeout_ms.min(wall_time_ms),
            max_concurrency: 1,
            rate_card: Some(self.rate_card.clone()),
            measurements: self.measurements.clone(),
            slots: Arc::new(tokio::sync::Semaphore::new(1)),
        }
    }
}
pub(crate) fn output_capability(
    registration: &ModelRegistration,
    model: &Settings,
    evidence: &str,
) -> Result<OutputCapabilityRegistration> {
    OutputCapabilityRegistration::for_model(
        registration,
        model.json_object,
        model.native_json_schema,
        evidence.into(),
    )
    .map_err(|_| Error::Usage("Invalid model capabilities, measurements or rate card."))
}

fn authorized(
    task: &melissa_core_contracts::wire::TaskContractV1,
    model: &ModelRegistration,
) -> bool {
    task.allowed_resource_ids
        .contains(&model.endpoint.resource_id)
        && task
            .authorization_refs
            .contains(&model.endpoint.authorization_ref)
}

fn admit(
    model: &ModelRegistration,
    settings: &Settings,
    packet: &WorkPacket,
    requirements: &RouteRequirements,
    budget: &ModelBudgetSnapshot,
    input_units: u64,
) -> Result<Option<u64>> {
    if !authorized(&packet.parent_task, model) {
        return Ok(None);
    }
    let limits = &packet.parent_task.limits;
    let context = model.context_tokens;
    let output = model.max_output_tokens;
    let cost = settings
        .rate_card
        .upper_bound(context, output)
        .map_err(|_| Error::Usage("Model cost reservation overflow."))?;
    if context.saturating_sub(output) < input_units
        || context
            > limits
                .max_input_tokens
                .saturating_sub(budget.charged_input_tokens)
        || output
            > limits
                .max_output_tokens
                .saturating_sub(budget.charged_output_tokens)
        || cost
            > limits
                .max_cost_micro_usd
                .saturating_sub(budget.charged_cost_micro_usd)
    {
        return Ok(None);
    }
    let mode = if settings.json_object {
        OutputMode::JsonObject
    } else {
        OutputMode::NativeJsonSchema
    };
    let registry = OutputCapabilityRegistry::new(vec![output_capability(
        model,
        settings,
        "approved-cli-pool",
    )?])
    .map_err(|_| Error::Store)?;
    let admitted = select_routes_for_output_mode(
        std::slice::from_ref(model),
        requirements,
        &BTreeSet::from([model.registration_id.clone()]),
        mode,
        &registry,
    )
    .is_ok();
    Ok(admitted.then_some(cost))
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Selection {
    settings_digest: String,
    plan_digest: String,
    registration: String,
    complexity: Complexity,
    ranking: String,
    tool_profile: crate::model_tools::ToolProfile,
    conservative_cost_micro_usd: u64,
    remaining_cost_micro_usd: u64,
    minimum_context_units: u64,
    measured_success_ppm: Option<u32>,
    measured_p95_ms: Option<u64>,
}

pub(crate) fn select<'a>(
    settings: &'a Settings,
    store: &mut LocalStore,
    packet: &WorkPacket,
    requirements: &RouteRequirements,
    repository_files: usize,
    input_units: u64,
) -> Result<(&'a str, &'a Settings)> {
    // Leave the original one-model admission and reservation path intact.
    if settings.model_pool.is_empty() {
        return Ok(("cli-selected", settings));
    }
    let task = &packet.parent_task;
    let artifact = format!("cli:model-route:{}", packet.lease.run);
    let settings_digest = settings.digest()?;
    let budget = store.journal().model_budget(&task.task_id)?;
    if budget.active_requests != 0
        || budget.unknown_requests != 0
        || budget.unknown_input_requests != 0
        || budget.unknown_output_requests != 0
        || budget.unknown_cost_requests != 0
        || budget.budget_exceeded
    {
        return Err(Error::Reconciliation(
            "Model routing requires settled usage.",
        ));
    }
    match store.get_artifact(&task.task_id, &artifact) {
        Ok(raw) => {
            let selection: Selection = decode(&raw)?;
            if selection.settings_digest != settings_digest
                || selection.plan_digest != packet.lease.plan_digest
            {
                return Err(Error::Conflict(
                    "Saved model route differs from the approved lease.",
                ));
            }
            return settings
                .models()
                .find(|(id, _)| *id == selection.registration)
                .ok_or(Error::Store);
        }
        Err(StoreError::NotFound(_)) => {}
        Err(error) => return Err(error.into()),
    }
    let affected = packet.node.literal_input["writable_paths"].as_array();
    let changed = packet
        .specialist_handoff
        .as_ref()
        .map_or(0, |h| h.changed_files.len());
    let class = complexity(&Signals {
        repository_files,
        affected_files: affected.map_or(0, Vec::len).max(changed),
        affected_modules: affected
            .into_iter()
            .flatten()
            .filter_map(|p| p.as_str()?.rsplit_once('/').map(|(dir, _)| dir))
            .collect::<BTreeSet<_>>()
            .len(),
        cross_module_dependencies: 0,
        test_files: 0,
        high_risk: task
            .selected_profile_ids
            .iter()
            .any(|p| p == "melissa.code.security-reviewer"),
        architecture_change: task
            .selected_profile_ids
            .iter()
            .any(|p| p == "melissa.code.architect"),
        compiler_failure: packet.node.profile == "melissa.code.debugger",
        estimated_cost_micro_usd: None,
        authorized_cost_micro_usd: Some(task.limits.max_cost_micro_usd),
    });
    let quality_first = matches!(
        class,
        Complexity::Complex | Complexity::LongHorizon | Complexity::HighRisk
    ) || matches!(requirements.category.as_str(), "review" | "reasoning");
    let mut candidates = Vec::new();
    for (id, m) in settings.models() {
        let placeholder = ApiKey::new("metadata-only".into()).map_err(|_| Error::Store)?;
        let registration = m.registration(
            id,
            m.endpoint_with_key(placeholder)?,
            task.limits.max_wall_time_ms,
        );
        if let Some(cost) = admit(&registration, m, packet, requirements, &budget, input_units)? {
            candidates.push((id, m, cost));
        }
    }
    candidates.sort_by_key(|&(id, m, cost)| {
        let quality = u64::from(1_000_001 - m.measurements.task_success_ppm.map_or(0, |v| v + 1));
        let latency = m.measurements.p95_latency_ms.unwrap_or(u64::MAX);
        if quality_first {
            (
                quality,
                u64::from(id != "cli-selected" && m.measurements.task_success_ppm.is_none()),
                cost,
                latency,
                id,
            )
        } else {
            (cost, latency, quality, 0, id)
        }
    });
    let (id, model, cost) = candidates.into_iter().next().ok_or(Error::Denied(
        "No approved model fits this specialist's capabilities, context and remaining budget.",
    ))?;
    let selection = Selection {
        settings_digest,
        plan_digest: packet.lease.plan_digest.clone(),
        registration: id.into(),
        complexity: class,
        ranking: if quality_first {
            "quality_cost_latency"
        } else {
            "cost_latency_quality"
        }
        .into(),
        tool_profile: model.tool_profile,
        conservative_cost_micro_usd: cost,
        remaining_cost_micro_usd: task
            .limits
            .max_cost_micro_usd
            .saturating_sub(budget.charged_cost_micro_usd),
        minimum_context_units: input_units,
        measured_success_ppm: model.measurements.task_success_ppm,
        measured_p95_ms: model.measurements.p95_latency_ms,
    };
    put(
        store,
        &task.task_id,
        &artifact,
        "model-route",
        &bytes(&selection)?,
    )?;
    Ok((id, model))
}
