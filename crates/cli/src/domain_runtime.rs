use super::*;
use crate::{
    app::{bytes, nonce, now, put},
    recipe::Execution,
    resources,
};
use melissa_code_quality::source::FrozenSource;
use melissa_container_executor::ContainerLimits;
use melissa_core_contracts::{TaskState, wire::TaskContractV1};
use melissa_local_store::LocalStore;
use melissa_model_gateway::tools::ToolContext;
use melissa_model_gateway::{
    ToolCall, ToolDefinition,
    tools::{BoundTool, ToolExecution},
};
use melissa_web_development::{
    plan::WebPlan,
    session::{Outcome, Policy, WebSession},
};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

fn web_error(_: impl std::fmt::Display) -> Error {
    Error::Reconciliation("Domain yürütme kaydı incelenmeden işlem tekrarlanmaz.")
}
fn text_observations(values: &[Value]) -> Vec<Value> {
    values
        .iter()
        .map(|v| {
            let mut v = v.clone();
            if let Some(body) = v["body"].as_object_mut() {
                body.remove("png_hex");
                body.remove("trace_hex");
            }
            v
        })
        .collect()
}
fn limits(t: &TaskContractV1) -> ContainerLimits {
    ContainerLimits {
        wall_time_ms: t.limits.max_wall_time_ms.min(180_000),
        cpu_time_ms: 120_000,
        cpu_rate_basis_points: 10_000,
        memory_bytes: 2 * 1024 * 1024 * 1024,
        disk_bytes: 512 * 1024 * 1024,
        output_bytes: 32 * 1024 * 1024,
        export_bytes: 64 * 1024 * 1024,
        max_processes: 256,
    }
}
pub(crate) fn tool_definitions(d: &DomainRecipe) -> Vec<BoundTool> {
    vec![BoundTool{canonical_id:"code.tests".into(),definition:ToolDefinition{name:"code_tests".into(),description:"Run ONLY the approved PUBLIC development flows on current source in fresh native sessions, closed before returning. Final private tests/fixtures are never run or exposed by this tool. Use the returned source_digest for submit_candidate after tests pass.".into(),parameters:json!({"type":"object","properties":{},"required":[],"additionalProperties":false}),strict:false}},
    BoundTool{canonical_id:match d.kind {Kind::Frontend=>"browser.navigate",Kind::Backend=>"code.tests",Kind::Database=>"data.query"}.into(),definition:ToolDefinition{name:"web_flow".into(),description:format!("Observe a complete source-bound local flow in ONE call: operations_json is a serialized JSON ARRAY of native commands. Include setup/navigation and all related actions; host CLOSES before the next model response. No start/close/session_id, external targets, private fixtures or raw unregistered SQL writes. Registry statement/migration names only. Review final-source images for frontend. Native command schema: {}",melissa_web_development::tools::tool_schema()),parameters:json!({"type":"object","properties":{"operations_json":{"type":"string"}},"required":["operations_json"],"additionalProperties":false}),strict:false}}]
}
pub(crate) fn model_tool(
    store: &mut LocalStore,
    t: &TaskContractV1,
    e: &Execution,
    repo: &Repository,
    call: ToolCall,
    cancel: Arc<AtomicBool>,
) -> melissa_model_gateway::Result<ToolExecution> {
    let observed = tokio::task::block_in_place(|| -> Result<(Value, Vec<String>)> {
        if call.name == "code_tests" {
            return Ok((check(store, t, e, repo, cancel.clone())?, vec![]));
        }
        let args = melissa_model_gateway::strict_json(&call.arguments)
            .map_err(|_| Error::Usage("Akış JSON geçersiz."))?;
        let parsed = melissa_model_gateway::strict_json(
            args["operations_json"]
                .as_str()
                .ok_or(Error::Usage("operations_json gerekli."))?,
        )
        .map_err(|_| Error::Usage("Komut dizisi geçersiz."))?;
        let ops: Vec<Command> =
            serde_json::from_value(parsed).map_err(|_| Error::Usage("Komut şeması geçersiz."))?;
        if ops.is_empty() || ops.len() > 64 {
            return Err(Error::Usage("Akış 1..64 komut gerektirir."));
        }
        let domain = e.recipe.domain.as_ref().ok_or(Error::Store)?;
        for op in &ops {
            domain.validate_operation(op)?;
        }
        let source = FrozenSource::capture(repo, &cancel).map_err(|_| Error::Repository)?;
        let baseline = prepare_baseline(store, t, e, repo, &source, false, cancel.clone())?;
        let width = ops.iter().rev().find_map(|o| {
            if let Command::Viewport { width, .. } = o {
                Some(*width)
            } else {
                None
            }
        });
        let flow = Flow {
            id: "model-observation".into(),
            steps: ops
                .into_iter()
                .map(|operation| Step {
                    operation,
                    expect_error: false,
                    expect: vec![],
                })
                .collect(),
        };
        let result = run_flow(
            store,
            t,
            e,
            repo,
            &source,
            &flow,
            false,
            baseline.as_deref(),
            cancel.clone(),
            "model-observation",
        )?;
        let refs = if result.success {
            promote_image(store, &t.task_id, &result, width, "model-observation")?
        } else {
            vec![]
        };
        Ok((
            json!({"status":if result.success{"observed"}else{"failed"},"reply":result.observations.last(),"observations":text_observations(&result.observations),"source_digest":source.snapshot().digest,"native_evidence_child":result.child,"closed_before_model_review":true,"private_oracle_was_not_run":true,"task_accepted":false}),
            refs,
        ))
    });
    match observed {
        Ok((content,artifact_refs))=>Ok(ToolExecution{success:content["status"]!="failed",content:content.to_string(),cancelled:cancel.load(Ordering::Acquire),requires_reconciliation:false,artifact_refs}),
        Err(error)=>Ok(ToolExecution{content:json!({"status":"failed","diagnostic":error.to_string(),"private_oracle_was_not_run":true,"task_accepted":false}).to_string(),success:false,cancelled:cancel.load(Ordering::Acquire),requires_reconciliation:matches!(error,Error::Reconciliation(_)),artifact_refs:vec![]})
    }
}
pub(crate) struct FlowResult {
    pub success: bool,
    pub observations: Vec<Value>,
    pub outcome: Outcome,
    pub child: String,
}
/// Promote only the last successful native PNG observation. Private acceptance
/// flows never call this function. Original child bytes remain immutable.
fn promote_image(
    store: &mut LocalStore,
    parent: &str,
    result: &FlowResult,
    width: Option<u32>,
    origin: &str,
) -> Result<Vec<String>> {
    let Some(reply) = result.observations.last() else {
        return Ok(vec![]);
    };
    if reply["ok"] != true || reply["body"]["content_type"] != "image/png" {
        return Ok(vec![]);
    }
    let mut native_reply = None;
    let mut native_receipt = None;
    for id in &result.outcome.artifact_refs {
        let meta = store.artifact_metadata(&result.child, id)?;
        if meta.logical_type == "web.reply"
            && serde_json::from_slice::<Value>(&store.get_artifact(&result.child, id)?)
                .map_err(|_| Error::Store)?
                == *reply
        {
            native_reply = Some(id.clone());
        }
    }
    let native_reply = native_reply.ok_or(Error::Store)?;
    if let Some(hex) = reply["body"]["png_hex"].as_str() {
        if hex.len() > 8 * 1024 * 1024 || hex.len() % 2 != 0 {
            return Err(Error::Store);
        }
        let raw = hex
            .as_bytes()
            .as_chunks::<2>()
            .0
            .iter()
            .map(|p| {
                let d = |c: u8| match c {
                    b'0'..=b'9' => Ok(c - b'0'),
                    b'a'..=b'f' => Ok(c - b'a' + 10),
                    _ => Err(Error::Store),
                };
                Ok(d(p[0])? * 16 + d(p[1])?)
            })
            .collect::<Result<Vec<u8>>>()?;
        melissa_model_gateway::vision::InlinePng::new(raw.clone()).map_err(|_| Error::Store)?;
        if width.is_some_and(|w| reply["body"]["viewport"]["width"] != w) {
            return Err(Error::Store);
        }
        let prefix = format!("cli:domain-image:{}", nonce()?);
        let png_id = format!("{prefix}:png");
        let reply_id = format!("{prefix}:reply");
        for (id, kind, content_type, data) in [
            (&png_id, "web.png", "image/png", raw.clone()),
            (
                &reply_id,
                "web.reply",
                "application/json",
                store.get_artifact(&result.child, &native_reply)?,
            ),
        ] {
            store.put_artifact(
                &melissa_local_store::ArtifactRequest {
                    artifact_id: id.clone(),
                    task_id: parent.into(),
                    content_type: content_type.into(),
                    logical_type: kind.into(),
                    schema_version: "melissa.cli-native-image/v1".into(),
                    retain_until_ms: 0,
                    legal_hold: true,
                },
                &data,
            )?;
        }
        put(
            store,
            parent,
            &format!("{prefix}:link"),
            "domain.preview",
            &bytes(
                &json!({"source_digest":reply["source_digest"],"width":width,"origin":origin,"image":png_id,"native_child":result.child,"native_reply":native_reply,"native_png_encoding":"inline_native_reply","image_digest":digest(&raw)}),
            )?,
        )?;
        return Ok(vec![png_id, reply_id]);
    }
    for id in &result.outcome.artifact_refs {
        if store.artifact_metadata(&result.child, id)?.logical_type == "web.binary-receipt" {
            let receipt: Value = serde_json::from_slice(&store.get_artifact(&result.child, id)?)
                .map_err(|_| Error::Store)?;
            if receipt["web_reply_artifact_id"] == native_reply
                && receipt["descriptor"] == reply["body"]["binary_artifact"]
                && receipt["source_digest"] == reply["source_digest"]
                && receipt["seq"] == reply["seq"]
            {
                native_receipt = Some((id.clone(), receipt));
                break;
            }
        }
    }
    let (native_receipt_id, mut receipt) = native_receipt.ok_or(Error::Store)?;
    let native_png = receipt["artifact_id"]
        .as_str()
        .ok_or(Error::Store)?
        .to_owned();
    let raw = store.get_artifact(&result.child, &native_png)?;
    let meta = store.artifact_metadata(&result.child, &native_png)?;
    if meta.logical_type != "web.png"
        || meta.content_type != "image/png"
        || receipt["blake3"] != digest(&raw)
        || reply["body"]["binary_artifact"]["bytes"] != raw.len() as u64
    {
        return Err(Error::Store);
    }
    melissa_model_gateway::vision::InlinePng::new(raw.clone()).map_err(|_| Error::Store)?;
    if width.is_some_and(|w| reply["body"]["viewport"]["width"] != w) {
        return Err(Error::Store);
    }
    let prefix = format!("cli:domain-image:{}", nonce()?);
    let png_id = format!("{prefix}:png");
    let reply_id = format!("{prefix}:reply");
    let receipt_id = format!("{prefix}:receipt");
    receipt["artifact_id"] = json!(png_id);
    receipt["web_reply_artifact_id"] = json!(reply_id);
    for (id, kind, content_type, data) in [
        (&png_id, "web.png", "image/png", raw),
        (
            &reply_id,
            "web.reply",
            "application/json",
            store.get_artifact(&result.child, &native_reply)?,
        ),
        (
            &receipt_id,
            "web.binary-receipt",
            "application/json",
            bytes(&receipt)?,
        ),
    ] {
        store.put_artifact(
            &melissa_local_store::ArtifactRequest {
                artifact_id: id.clone(),
                task_id: parent.into(),
                content_type: content_type.into(),
                logical_type: kind.into(),
                schema_version: "melissa.cli-native-image/v1".into(),
                retain_until_ms: 0,
                legal_hold: true,
            },
            &data,
        )?;
    }
    put(
        store,
        parent,
        &format!("{prefix}:link"),
        "domain.preview",
        &bytes(
            &json!({"source_digest":reply["source_digest"],"width":width,"origin":origin,"image":png_id,"native_child":result.child,"native_png":native_png,"native_reply":native_reply,"native_receipt":native_receipt_id,"image_digest":receipt["blake3"]}),
        )?,
    )?;
    Ok(vec![png_id, reply_id, receipt_id])
}
fn reviewed_previews(
    store: &LocalStore,
    parent: &str,
    source: &str,
    preview: &Preview,
) -> Result<Vec<Value>> {
    let artifacts = store.list_artifacts(parent)?;
    let mut completed_requests = vec![];
    for a in artifacts
        .iter()
        .filter(|a| a.logical_type == "model.receipt")
    {
        let receipt: Value = serde_json::from_slice(&store.get_artifact(parent, &a.artifact_id)?)
            .map_err(|_| Error::Store)?;
        if receipt["accepted_complete"] == true {
            let id = format!(
                "{}:request",
                receipt["request_id"].as_str().ok_or(Error::Store)?
            );
            let raw = store.get_artifact(parent, &id)?;
            if digest(&raw) != receipt["request_digest"] {
                return Err(Error::Store);
            }
            completed_requests.push((id, raw));
        }
    }
    let mut reviewed = vec![];
    for a in artifacts
        .iter()
        .filter(|a| a.logical_type == "cli.domain.preview")
    {
        let link: Value = serde_json::from_slice(&store.get_artifact(parent, &a.artifact_id)?)
            .map_err(|_| Error::Store)?;
        if link["source_digest"] != source || link["origin"] != "model-observation" {
            continue;
        }
        let raw = store.get_artifact(parent, link["image"].as_str().ok_or(Error::Store)?)?;
        if digest(&raw) != link["image_digest"] {
            return Err(Error::Store);
        }
        let data = melissa_model_gateway::vision::InlinePng::new(raw)
            .map_err(|_| Error::Store)?
            .data_url();
        if let Some((request, _)) = completed_requests
            .iter()
            .find(|(_, raw)| raw.windows(data.len()).any(|s| s == data.as_bytes()))
        {
            reviewed.push(json!({"width":link["width"],"image":link["image"],"image_digest":link["image_digest"],"request":request,"source_digest":source}));
        }
    }
    if !preview
        .widths
        .iter()
        .all(|w| reviewed.iter().any(|r| r["width"] == *w))
    {
        return Err(Error::Denied(
            "Son kaynak için tarifteki tüm viewport PNG'leri tamamlanan gerçek model isteklerinde bulunmalı.",
        ));
    }
    Ok(reviewed)
}
fn child(
    store: &mut LocalStore,
    root: &TaskContractV1,
    e: &Execution,
    source: &str,
    tag: &str,
) -> Result<TaskContractV1> {
    if !root
        .allowed_effects
        .iter()
        .any(|s| s == "verification.run_protected")
        || root.acceptance_contract_digest != e.recipe.acceptance_digest()?
    {
        return Err(Error::Denied("Domain QA üst görev sözleşmesi eşleşmiyor."));
    }
    let domain = e.recipe.domain.as_ref().ok_or(Error::Store)?;
    let mut t = root.clone();
    t.task_id = format!("qa:{}:{}", root.task_id, nonce()?);
    t.input_snapshot_digest = source.into();
    t.goal = format!("Source-bound independent domain {tag}");
    t.selected_profile_ids = vec![
        "melissa.core.independent-qa".into(),
        domain.kind.profile().into(),
    ];
    t.allowed_effects = vec![
        "workspace.read".into(),
        "process.execute_scoped".into(),
        "evidence.append".into(),
        "verification.run_protected".into(),
        domain.kind.effect().into(),
    ];
    t.limits.max_cost_micro_usd = 0;
    store.journal_mut().create_task(&t)?;
    for state in [
        TaskState::Planned,
        TaskState::Queued,
        TaskState::Leased,
        TaskState::Running,
    ] {
        let version = store.journal().task(&t.task_id)?.record.version;
        store.transition_task(&t.task_id, version, state, None)?;
    }
    put(
        store,
        &root.task_id,
        &format!("cli:qa-child:{}", nonce()?),
        "qa.child",
        &bytes(
            &json!({"parent_task":root.task_id,"child_task":t.task_id,"source_digest":source,"domain":true,"phase":tag,"model_authority":false}),
        )?,
    )?;
    Ok(t)
}
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_flow(
    store: &mut LocalStore,
    root: &TaskContractV1,
    e: &Execution,
    repo: &Repository,
    source: &FrozenSource,
    flow: &Flow,
    private: bool,
    baseline: Option<&str>,
    cancel: Arc<AtomicBool>,
    tag: &str,
) -> Result<FlowResult> {
    let domain = e.recipe.domain.as_ref().ok_or(Error::Store)?;
    if cancel.load(Ordering::Acquire) {
        return Err(Error::Conflict("Domain kontrolü başlamadan iptal edildi."));
    }
    for step in &flow.steps {
        domain.validate_operation(&step.operation)?;
    }
    source
        .current(repo, &cancel)
        .map_err(|_| Error::Repository)?;
    let t = child(store, root, e, &source.snapshot().digest, tag)?;
    let id = format!("domain-{}", nonce()?);
    let ctx = ToolContext {
        task_id: t.task_id.clone(),
        profile_id: domain.kind.profile().into(),
        run_id: id.clone(),
        request_id: format!("host:{id}"),
        invocation_id: format!("host:{id}"),
        attempt_id: format!("host:{id}:attempt"),
        generation: 1,
    };
    let registration = domain.registration(repo, source.snapshot(), baseline)?;
    let mut plan = WebPlan::prepare(repo, source.snapshot(), registration, limits(&t), &cancel)
        .map_err(|_| Error::Denied("Native başlatma öncesi domain kaynağı/kaydı geçersiz."))?
        .with_local_project_adapter()
        .map_err(|_| Error::Denied("Yerel proje adaptörü hazırlanamadı."))?
        .with_base64_binary_transport()
        .map_err(|_| Error::Denied("Native domain programı hazırlanamadı."))?;
    if private && let Some(sql) = &domain.private_seed_sql {
        plan = plan
            .with_host_fixture_seed(sql.as_bytes())
            .map_err(|_| Error::Denied("Özel fixture hazırlanamadı."))?;
    }
    put(
        store,
        &t.task_id,
        &format!("{id}:intent"),
        "domain.intent",
        &bytes(
            &json!({"invocation":ctx.invocation_id,"source_digest":source.snapshot().digest,"private_acceptance":private,"image":melissa_web_development::plan::WEB_IMAGE}),
        )?,
    )?;
    let started = WebSession::start(
        Arc::new(repo.reopen()?),
        store.connect()?,
        resources::catalog()?,
        Policy {
            task_id: t.task_id.clone(),
            profile_id: domain.kind.profile().into(),
            principal_id: "cli:protected-domain-host".into(),
            resource_id: t.workspace_ref.clone(),
            authorization_ref: t.authorization_refs[0].clone(),
            expires_at_ms: e.expires_at_ms,
        },
        plan,
        ctx.clone(),
        cancel.clone(),
        Arc::new(AtomicBool::new(false)),
    );
    let mut session = match started {
        Ok(s) => s,
        Err(error) => {
            if matches!(
                store.journal().managed_execution(&ctx.invocation_id),
                Err(melissa_local_store::StoreError::NotFound(_))
            ) {
                put(
                    store,
                    &t.task_id,
                    &format!("{id}:no-launch"),
                    "domain.no_launch",
                    &bytes(
                        &json!({"invocation":ctx.invocation_id,"managed_admission_absent":true,"workload_started":false}),
                    )?,
                )?;
                return Err(Error::Denied(
                    "Domain kapsamı native yürütme öncesinde reddedildi.",
                ));
            }
            return Err(web_error(error));
        }
    };
    let mut observations =
        vec![serde_json::to_value(session.last_reply()).map_err(|_| Error::Store)?];
    let mut passed = session.last_reply().is_some_and(|r| r.ok);
    for step in &flow.steps {
        if !passed {
            break;
        }
        match session.command(step.operation.clone()) {
            Ok(reply) => {
                let mut value = serde_json::to_value(&reply).map_err(|_| Error::Store)?;
                if let Some(hex) = value["body"]["body_hex"]
                    .as_str()
                    .filter(|s| s.len() <= 2 * 1024 * 1024 && s.len() % 2 == 0)
                {
                    let decoded = hex
                        .as_bytes()
                        .as_chunks::<2>()
                        .0
                        .iter()
                        .map(|p| u8::from_str_radix(std::str::from_utf8(p).unwrap_or(""), 16))
                        .collect::<std::result::Result<Vec<_>, _>>();
                    if let Ok(raw) = decoded
                        && let Ok(text) = String::from_utf8(raw)
                    {
                        value["body"]["text"] = json!(text);
                        if let Ok(parsed) = melissa_model_gateway::strict_json(&text) {
                            value["body"]["json"] = parsed;
                        }
                    }
                }
                let matched = reply.ok != step.expect_error
                    && step.expect.iter().all(|rule| rule.matches(&value));
                observations.push(value);
                if !matched {
                    passed = false;
                    break;
                }
            }
            Err(_) => {
                passed = false;
                break;
            }
        }
    }
    let outcome = session.finish().map_err(web_error)?;
    let success = passed
        && outcome.success
        && outcome.cleanup_confirmed
        && outcome.source_current
        && !outcome.requires_reconciliation;
    put(
        store,
        &t.task_id,
        &format!("{id}:observations"),
        "domain.observations",
        &bytes(
            &json!({"flow":flow.id,"success":success,"native_artifact_refs":outcome.artifact_refs,"native_final_artifact":format!("{}:web-final",outcome.native_attempt_id),"private_acceptance":private,"source_digest":source.snapshot().digest,"model_task_accepted":false}),
        )?,
    )?;
    if outcome.requires_reconciliation {
        return Err(Error::Reconciliation(
            "Native domain etkisi belirsiz; kanıtsız tekrar yapılmaz.",
        ));
    }
    source
        .current(repo, &AtomicBool::new(false))
        .map_err(|_| Error::Repository)?;
    if cancel.load(Ordering::Acquire) {
        return Err(Error::Conflict(
            "Domain çalıştırma iptal edildi; kaynaktan bağımsız duruş kaydı korundu.",
        ));
    }
    Ok(FlowResult {
        success,
        observations,
        outcome,
        child: t.task_id,
    })
}
pub(crate) fn prepare_baseline(
    store: &mut LocalStore,
    t: &TaskContractV1,
    e: &Execution,
    repo: &Repository,
    source: &FrozenSource,
    private: bool,
    cancel: Arc<AtomicBool>,
) -> Result<Option<String>> {
    let domain = e.recipe.domain.as_ref().ok_or(Error::Store)?;
    if domain.kind != Kind::Database {
        return Ok(None);
    }
    let flow = Flow {
        id: "host-baseline".into(),
        steps: vec![Step {
            operation: Command::Profile {
                connection: "meta".into(),
            },
            expect_error: false,
            expect: vec![],
        }],
    };
    let result = run_flow(
        store, t, e, repo, source, &flow, private, None, cancel, "baseline",
    )?;
    if !result.success {
        return Err(Error::Conflict(
            "Başlangıç veritabanı profili doğrulanamadı.",
        ));
    }
    Ok(Some(
        result.observations[1]["body"]["schema_digest"]
            .as_str()
            .ok_or(Error::Store)?
            .into(),
    ))
}
pub(crate) fn check(
    store: &mut LocalStore,
    t: &TaskContractV1,
    e: &Execution,
    repo: &Repository,
    cancel: Arc<AtomicBool>,
) -> Result<Value> {
    let domain = e.recipe.domain.as_ref().ok_or(Error::Store)?;
    let source = FrozenSource::capture(repo, &cancel).map_err(|_| Error::Repository)?;
    let baseline = prepare_baseline(store, t, e, repo, &source, false, cancel.clone())?;
    let mut rows = vec![];
    for flow in &domain.public_checks {
        let result = run_flow(
            store,
            t,
            e,
            repo,
            &source,
            flow,
            false,
            baseline.as_deref(),
            cancel.clone(),
            "development",
        )?;
        rows.push(json!({"flow":flow.id,"passed":result.success,"evidence_child":result.child}));
        if !result.success {
            return Ok(
                json!({"status":"failed","source_digest":source.snapshot().digest,"checks":rows,"private_oracle_was_not_run":true,"task_accepted":false}),
            );
        }
    }
    Ok(
        json!({"status":if rows.is_empty(){"no_public_checks"}else{"passed"},"source_digest":source.snapshot().digest,"checks":rows,"private_oracle_was_not_run":true,"task_accepted":false}),
    )
}
pub(crate) struct Verified {
    pub package: BTreeMap<String, Vec<u8>>,
    pub package_digest: String,
    pub receipt: Value,
}
#[allow(clippy::too_many_arguments)]
pub(crate) fn final_verify(
    store: &mut LocalStore,
    t: &TaskContractV1,
    e: &Execution,
    repo: &Repository,
    source: &FrozenSource,
    negative_repo: &Repository,
    negative: &FrozenSource,
    cancel: Arc<AtomicBool>,
) -> Result<Verified> {
    let domain = e.recipe.domain.as_ref().ok_or(Error::Store)?;
    let reviewed = if let Some(p) = &domain.preview {
        reviewed_previews(store, &t.task_id, &source.snapshot().digest, p)?
    } else {
        vec![]
    };
    let baseline = prepare_baseline(store, t, e, repo, source, true, cancel.clone())?;
    let negative_baseline =
        prepare_baseline(store, t, e, negative_repo, negative, true, cancel.clone())?;
    let mut negative_rejected = false;
    let mut negative_children = vec![];
    for flow in &domain.private_checks {
        let result = run_flow(
            store,
            t,
            e,
            negative_repo,
            negative,
            flow,
            true,
            negative_baseline.as_deref(),
            cancel.clone(),
            "negative-final",
        )?;
        negative_children.push(result.child);
        if !result.success {
            negative_rejected = true;
            break;
        }
    }
    if !negative_rejected {
        return Err(Error::Denied(
            "Negatif aday özel testleri geçti; kabul oracle'ı ayrım yapmıyor.",
        ));
    }
    let mut children = vec![];
    for flow in &domain.private_checks {
        let result = run_flow(
            store,
            t,
            e,
            repo,
            source,
            flow,
            true,
            baseline.as_deref(),
            cancel.clone(),
            "protected-final",
        )?;
        children.push(result.child);
        if !result.success {
            return Err(Error::Denied(
                "Aday bağımsız domain kabul testinden geçmedi; gizli testler modele geri beslenmez.",
            ));
        }
    }
    if let Some(preview) = &domain.preview {
        for width in &preview.widths {
            let flow = Flow {
                id: format!("preview-{width}"),
                steps: vec![
                    Step {
                        operation: Command::Viewport {
                            page: "preview".into(),
                            width: *width,
                            height: 900,
                        },
                        expect_error: false,
                        expect: vec![],
                    },
                    Step {
                        operation: Command::Navigate {
                            page: "preview".into(),
                            path: preview.path.clone(),
                        },
                        expect_error: false,
                        expect: vec![],
                    },
                    Step {
                        operation: Command::Screenshot {
                            page: "preview".into(),
                        },
                        expect_error: false,
                        expect: vec![],
                    },
                ],
            };
            let result = run_flow(
                store,
                t,
                e,
                repo,
                source,
                &flow,
                false,
                None,
                cancel.clone(),
                "accepted-preview",
            )?;
            if !result.success {
                return Err(Error::Denied("Son kaynak önizlemesi üretilemedi."));
            }
            promote_image(
                store,
                &t.task_id,
                &result,
                Some(*width),
                "accepted-source-preview",
            )?;
            children.push(result.child);
        }
    }
    source
        .current(repo, &cancel)
        .map_err(|_| Error::Repository)?;
    let package = source
        .snapshot()
        .files
        .iter()
        .map(|f| {
            Ok((
                f.path.clone(),
                source
                    .bytes(&f.path)
                    .map_err(|_| Error::Repository)?
                    .to_vec(),
            ))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    let package_digest = digest(&bytes(&package)?);
    let mut key = zeroize::Zeroizing::new([0u8; 32]);
    getrandom::fill(&mut *key).map_err(|_| Error::Store)?;
    let mut receipt = json!({"schema_version":"melissa.protected-domain-receipt/v1","task_id":t.task_id,"source_digest":source.snapshot().digest,"package_digest":package_digest,"suite_digest":e.recipe.acceptance_digest()?,"environment_digest":digest(melissa_web_development::plan::WEB_IMAGE.as_bytes()),"verifier_digest":digest(include_bytes!("domain_runtime.rs")),"passed":domain.private_checks.len(),"total":domain.private_checks.len(),"reviewed_source_images":reviewed,"negative_control_rejected":true,"QA_children":children,"negative_QA_children":negative_children,"private_expected_answers_sent_to_model":false,"private_fixture_sent_to_model":false,"created_at_ms":now()?});
    let mac = blake3::keyed_hash(&key, &bytes(&receipt)?)
        .to_hex()
        .to_string();
    receipt["mac"] = json!(mac);
    // Validate before publishing. No model tool can construct this receipt or
    // access the per-verification host key.
    let mut unsigned = receipt.clone();
    unsigned.as_object_mut().ok_or(Error::Store)?.remove("mac");
    if blake3::keyed_hash(&key, &bytes(&unsigned)?)
        .to_hex()
        .as_str()
        != receipt["mac"].as_str().ok_or(Error::Store)?
    {
        return Err(Error::Store);
    }
    put(
        store,
        &t.task_id,
        &format!("cli:domain-receipt:{}", nonce()?),
        "domain.receipt",
        &bytes(&receipt)?,
    )?;
    Ok(Verified {
        package,
        package_digest,
        receipt,
    })
}
