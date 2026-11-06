//! Allowlisted diagnostic projections: raw events remain in the encrypted store.
pub mod otlp;
#[cfg(all(test, windows))]
mod tests;
use crate::{
    Error, Result,
    app::{App, bytes, decode, now},
    digest,
};
use melissa_core_contracts::TaskState;
use serde_json::{Value, json};
use std::path::Path;
pub fn explain(error: &Error) -> Value {
    let (class, status, next) = match error {
        Error::Reconciliation(_) => (
            "uncertain",
            "INCONCLUSIVE",
            "Özgün istek/araç makbuzunu uzlaştır; işlemi otomatik tekrarlama.",
        ),
        Error::Denied(_) => (
            "policy",
            "BLOCKED",
            "İşlemin izin ve kapsamını incele; kapsam değişikliğini operatörle açık değerlendir.",
        ),
        Error::Unavailable(_) => (
            "environment",
            "BLOCKED",
            "Çalışma ortamının ve sahip olunan süreçlerin hazırlığını denetle.",
        ),
        Error::Verification(_) => (
            "acceptance",
            "FAIL",
            "Aynı kaynak sürümündeki başarısız kontrol kanıtını incele ve düzelt.",
        ),
        Error::Model(kind) => {
            use melissa_model_gateway::ErrorKind;
            match kind {
                ErrorKind::Authorization | ErrorKind::Budget => (
                    "policy",
                    "BLOCKED",
                    "Onaylanan model/araç kapsamını denetle.",
                ),
                ErrorKind::Invalid => (
                    "input",
                    "NOT_RUN",
                    "İsteğin yerel doğrulamasını düzelt; dispatch ve kullanım kaydını ayrıca incele.",
                ),
                ErrorKind::Unsupported
                | ErrorKind::ProviderUnavailable
                | ErrorKind::RateLimited => (
                    "environment",
                    "BLOCKED",
                    "Desteklenen model/protokol ve sağlayıcı hazırlığını denetle.",
                ),
                ErrorKind::Authentication => (
                    "model",
                    "BLOCKED",
                    "Kimlik doğrulama ayarını özel olarak denetle; anahtarı rapora koyma.",
                ),
                ErrorKind::Tool => (
                    "tool",
                    "FAIL",
                    "Typed araç makbuzunu ve özgün kaynağı incele.",
                ),
                ErrorKind::Store => (
                    "storage",
                    "INCONCLUSIVE",
                    "Özgün şifreli kanıtı koru ve kayıt bütünlüğünü incele.",
                ),
                ErrorKind::Cancelled => (
                    "state",
                    "BLOCKED",
                    "İptal ve durmuş süreç kanıtını incele; otomatik tekrar yapma.",
                ),
                ErrorKind::Transport | ErrorKind::Timeout => (
                    "uncertain",
                    "INCONCLUSIVE",
                    "Gönderim ve kullanım makbuzunu uzlaştır; otomatik tekrar yapma.",
                ),
                _ => (
                    "model",
                    "FAIL",
                    "İstek makbuzunu incele; model/protokol hatasını araç veya ortam hatasıyla karıştırma.",
                ),
            }
        }
        Error::Repository => (
            "tool",
            "BLOCKED",
            "Kaynak kimliği, sürüm ve dosya sınırını yeniden gözlemle.",
        ),
        Error::Store => (
            "storage",
            "INCONCLUSIVE",
            "Mevcut kayıtları koru; bütünlük ve yedek doğrulamasını incele.",
        ),
        Error::Io => (
            "environment",
            "FAIL",
            "Yerel dosya erişimi ve disk alanını denetle.",
        ),
        Error::Conflict(_) => (
            "state",
            "BLOCKED",
            "Güncel görev/kaynak sürümünü incele; eski onayı veya işlemi tekrar kullanma.",
        ),
        Error::Usage(_) => (
            "input",
            "NOT_RUN",
            "Eksik/geçersiz girdiyi düzelt; yürütme yapılmış sayılmaz.",
        ),
    };
    json!({"class":class,"status":status,"error_kind":error.kind(),"next_step":next,"automatic_retry":false})
}
fn classify(kind: &str) -> &'static str {
    match kind {
        "requires_reconciliation" => "uncertain",
        "denied" => "policy",
        "unavailable" | "io" => "environment",
        "verification" => "acceptance",
        "model_protocol" | "provider_unavailable" | "model_execution" => "model",
        "repository_boundary" => "tool",
        "store_integrity" => "storage",
        "conflict" => "state",
        _ => "unknown",
    }
}
fn reference(id: &str) -> String {
    digest(id.as_bytes())
}
impl App {
    pub fn diagnose(&self, id: &str) -> Result<Value> {
        let store = match self.store(id) {
            Ok(s) => s,
            Err(Error::Unavailable(_)) => {
                let live = self.live(id)?;
                return Ok(
                    json!({"command":"observe diagnose","task_ref":reference(id),"status":"INCONCLUSIVE","live_only":true,"active":live.active(),"phase":safe_phase(&live.phase),"pending_model_requests":live.model_budget["active_requests"],"unknown_model_requests":live.model_budget["unknown_requests"],"host_measurement":live.host_measurement,"reason":"exclusive task store owned by another session; full diagnosis not available","model_calls":0}),
                );
            }
            Err(e) => return Err(e),
        };
        let task = store.journal().task(id)?;
        let budget = store.journal().model_budget(id)?;
        let mut issues = vec![];
        let requests = store.journal().model_requests(id)?;
        let dispatched = requests
            .iter()
            .filter(|r| r.dispatched_at_unix_ms.is_some())
            .count();
        if task.record.unknown_invocation_id.is_some()
            || budget.unknown_requests > 0
            || budget.unknown_cost_requests > 0
            || budget.unknown_input_requests > 0
            || budget.unknown_output_requests > 0
        {
            issues.push(json!({"class":"uncertain","status":"INCONCLUSIVE","basis":"durable invocation/model ledger","retry":false}));
        }
        if self.restored(id)? {
            issues.push(json!({"class":"policy","status":"BLOCKED","basis":"restored evidence cannot resume; create a new reviewed task"}));
        }
        let mut checkpoint = None;
        let mut last_sequence = 0;
        for artifact in store.list_artifacts(id)? {
            if artifact.logical_type == "cli.run.error" {
                let value: Value = decode(&store.get_artifact(id, &artifact.artifact_id)?)?;
                let class = value["diagnosis"]["class"]
                    .as_str()
                    .filter(|c| {
                        [
                            "model",
                            "tool",
                            "environment",
                            "policy",
                            "acceptance",
                            "uncertain",
                            "storage",
                            "state",
                            "input",
                        ]
                        .contains(c)
                    })
                    .unwrap_or_else(|| classify(value["error_kind"].as_str().unwrap_or("")));
                let status = if value["requires_reconciliation"] == true || class == "uncertain" {
                    "INCONCLUSIVE"
                } else if class == "model" && dispatched == 0 {
                    "NOT_RUN"
                } else if ["environment", "policy", "state"].contains(&class) {
                    "BLOCKED"
                } else {
                    "FAIL"
                };
                issues.push(json!({"class":class,"status":status,"evidence_ref":reference(&artifact.artifact_id),"raw_error":"omitted"}));
            }
            if (artifact.logical_type == "workflow.native-continuation"
                || artifact.logical_type == "cli.continuity.manual")
                && artifact.created_at_ms >= last_sequence
            {
                last_sequence = artifact.created_at_ms;
                checkpoint = Some(
                    json!({"evidence_ref":reference(&artifact.artifact_id),"created_at_ms":artifact.created_at_ms,"kind":if artifact.logical_type=="workflow.native-continuation"{"native_continuation"}else{"operator_checkpoint"}}),
                );
            }
            if artifact.logical_type == "tool.receipt" {
                use melissa_model_gateway::{
                    ErrorKind,
                    adapter::{Status, ToolReceipt},
                };
                let receipt: ToolReceipt = decode(&store.get_artifact(id, &artifact.artifact_id)?)?;
                if receipt.context.task_id != id {
                    return Err(Error::Store);
                }
                if receipt.status != Status::Pass || receipt.requires_reconciliation {
                    let (class, status) = if receipt.requires_reconciliation
                        || receipt.status == Status::Inconclusive
                    {
                        ("uncertain", "INCONCLUSIVE")
                    } else if receipt.error_kind == Some(ErrorKind::Authorization) {
                        ("policy", "BLOCKED")
                    } else if receipt.status == Status::NotRun {
                        ("tool", "NOT_RUN")
                    } else if receipt.status == Status::Blocked {
                        ("tool", "BLOCKED")
                    } else {
                        ("tool", "FAIL")
                    };
                    issues.push(json!({"class":class,"status":status,"evidence_ref":reference(&artifact.artifact_id),"dispatch_started":receipt.dispatch_started,"historical":true,"raw_tool_data":"omitted"}));
                }
            }
        }

        let events = store.journal().events(id)?;
        let mut failed_tools = std::collections::BTreeSet::new();
        for event in &events {
            if let Some(invocation) = &event.invocation
                && invocation.state == melissa_core_contracts::InvocationState::Failed
            {
                failed_tools.insert(reference(&invocation.invocation_id));
            }
        }
        if !failed_tools.is_empty() {
            issues.push(json!({"class":"tool","status":"FAIL","failed_invocation_refs":failed_tools,"historical":true,"task_acceptance_implied":false}));
        }
        if task.record.state == TaskState::Failed && issues.is_empty() {
            issues.push(json!({"class":"unknown","status":"INCONCLUSIVE","basis":"terminal failure without classified host evidence; cause not guessed"}));
        }
        let settings=self.settings().ok().map(|s|json!({"schema":"melissa.cli-settings/v1","digest":s.digest().ok(),"provider_model_endpoint_credentials":"omitted"}));
        Ok(
            json!({"schema_version":"melissa.diagnosis/v1","command":"observe diagnose","task_ref":reference(id),"state":task.record.state,"task_version":task.record.version,"status":if issues.iter().any(|i|i["class"]=="uncertain"){"INCONCLUSIVE"}else{"PASS"},"meaning":"PASS means diagnostic projection available, not task/model success","model_dispatches":dispatched,"model_test_status":if dispatched==0{"NOT_RUN"}else{"OBSERVED_SEE_LEDGER"},"model_budget":budget,"issues":issues,"last_checkpoint":checkpoint,"runtime":{"cli_version":env!("CARGO_PKG_VERSION"),"binary_digest":crate::app::runtime_digest()?},"settings":settings,"source_digest":task.contract.input_snapshot_digest,"events":events.len(),"model_calls":0,"raw_prompts_arguments_bodies_paths":"omitted"}),
        )
    }
    pub fn support(&self, id: &str, out: &Path) -> Result<Value> {
        let diagnosis = self.diagnose(id)?;
        let disk = self.disk(id)?;
        let metrics = self.metrics(id)?;
        let (out, _parent) = crate::maintenance::destination(Some(self), Some(id), out)?;
        std::fs::create_dir(&out)?;
        let _pin = melissa_repository::Repository::open(&out, Default::default(), vec![])?;
        // Reconstruct scalar metrics; never copy arbitrary nested/raw task data.
        let m = &metrics["metrics"];
        let selected = [
            "verified_acceptance",
            "run_wall_ms",
            "model_dispatch_attempts",
            "model_unknown_outcomes",
            "tool_calls_dispatched",
            "tool_attempts_dispatched",
            "context_compiler_reports",
            "peak_reported_context_wire_bytes",
            "peak_reported_exact_context_tokens",
            "peak_host_rss_bytes",
        ];
        let safe_metrics = selected
            .into_iter()
            .map(|k| (k.to_owned(), m[k].clone()))
            .collect::<serde_json::Map<_, _>>();
        // Budget task_id is an opaque host ID but keep only numeric fields in support.
        let mut diagnosis = diagnosis;
        let mut budget = serde_json::Map::new();
        if let Some(values) = diagnosis["model_budget"].as_object() {
            for (key, value) in values {
                if value.is_number() || value.is_boolean() {
                    budget.insert(key.clone(), value.clone());
                }
            }
        }
        diagnosis["model_budget"] = json!(budget);
        let reproduction = json!({"mode":"manual_only","execution_performed":false,"guidance":"Use task diagnostics, exact private evidence and the reviewed reproduce-failure workflow. No raw command or effect is replayed from this package.","source_digest":diagnosis["source_digest"],"last_checkpoint":diagnosis["last_checkpoint"]});
        let payload = json!({"schema_version":"melissa.support/v1","created_at_ms":now()?,"diagnosis":diagnosis,"metrics":safe_metrics,"disk":disk["disk"],"reproduction":reproduction,"redaction_policy":"allowlisted host scalars, hashed identifiers; no source/goal/error text/prompt/command/endpoint/model/provider/key/env/path or raw artifact payload"});
        let raw = bytes(&payload)?;
        crate::maintenance::write_new(&out.join("support.json"), &raw)?;
        crate::maintenance::write_new(
            &out.join("manifest.json"),
            &bytes(
                &json!({"schema_version":"melissa.support-manifest/v1","files":[{"path":"support.json","digest":digest(&raw),"bytes":raw.len()}],"contains_credentials":false}),
            )?,
        )?;
        Ok(
            json!({"command":"observe support","status":"PASS","out":out,"source_records_modified":false,"files":2,"model_calls":0}),
        )
    }
}
fn safe_phase(phase: &str) -> &str {
    match phase {
        "preparing" | "agent" | "verifying" | "paused" | "completed" | "failed" | "blocked"
        | "cancelled" | "deadline" => phase,
        _ => "unknown",
    }
}
