//! Exact inverse of known task-owned native file receipts. No reset/checkout,
//! fuzzy patching, external-effect replay or silent overwrite of user edits.
use crate::{
    Error, Result,
    app::{App, bytes, decode, nonce, now, put},
    digest,
};
use melissa_local_store::LocalStore;
use melissa_repository::{
    ByteEdit, FileMutation, FilePatch, FileVersion, MutationProposal, PatchBroker, PatchPolicy,
    PatchProposal, Repository,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::atomic::AtomicBool,
};
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pick {
    pub receipt_id: String,
    #[serde(default)]
    pub hunks: BTreeMap<String, BTreeSet<usize>>,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    #[serde(default)]
    pub turn_request: Option<String>,
    #[serde(default)]
    pub receipts: Vec<Pick>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Effect {
    Patch { proposal: PatchProposal },
    Mutation { proposal: MutationProposal },
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Plan {
    task_id: String,
    root_identity: String,
    source_digest: String,
    task_version: u64,
    request: Request,
    effects: Vec<Effect>,
    expected_files: BTreeMap<String, Option<String>>,
    directory_expectations: Vec<Value>,
    forward_receipts: Vec<String>,
    restored_hunks: BTreeMap<String, BTreeMap<String, BTreeSet<usize>>>,
}
struct VirtualFile {
    original: Option<FileVersion>,
    current: Option<Vec<u8>>,
}
fn file<'a>(
    files: &'a mut BTreeMap<String, VirtualFile>,
    repo: &Repository,
    path: &str,
) -> Result<&'a mut VirtualFile> {
    if !files.contains_key(path) {
        let value = match repo.read(path) {
            Ok((version, raw)) => VirtualFile {
                original: Some(version),
                current: Some(raw),
            },
            Err(melissa_repository::Error::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {
                repo.absent_path(path)?;
                VirtualFile {
                    original: None,
                    current: None,
                }
            }
            Err(error) => return Err(error.into()),
        };
        files.insert(path.into(), value);
    }
    Ok(files.get_mut(path).expect("inserted"))
}
fn read_receipt(store: &LocalStore, task: &str, id: &str) -> Result<Value> {
    let metadata = store.artifact_metadata(task, id)?;
    if !["repository_patch_receipt", "repository_mutation_receipt"]
        .contains(&metadata.logical_type.as_str())
    {
        return Err(Error::Denied(
            "Yalnız aynı görevin native dosya makbuzu geri alınabilir.",
        ));
    }
    let receipt: Value = decode(&store.get_artifact(task, id)?)?;
    let invocation = receipt["invocation_id"].as_str().ok_or(Error::Store)?;
    let record = store.journal().invocation(invocation)?;
    if record.task_id != task
        || record.state != melissa_core_contracts::InvocationState::Succeeded
        || record.active_attempt_id.is_some()
        || record.result_artifact_id.as_deref() != Some(id)
    {
        return Err(Error::Reconciliation(
            "Başarısı ve sahipliği kanıtlanmayan işlem geri alınmaz.",
        ));
    }
    Ok(receipt)
}
pub fn changes(app: &App, task: &str) -> Result<Value> {
    let store = app.store(task)?;
    let mut changes = vec![];
    for metadata in store.list_artifacts(task)? {
        if ["repository_patch_receipt", "repository_mutation_receipt"]
            .contains(&metadata.logical_type.as_str())
        {
            let receipt = read_receipt(&store, task, &metadata.artifact_id)?;
            let invocation = receipt["invocation_id"].as_str().ok_or(Error::Store)?;
            let binding = store
                .get_artifact(task, &format!("{invocation}:tool-binding"))
                .ok()
                .map(|v| decode::<Value>(&v))
                .transpose()?;
            let proposal: Value =
                decode(&store.get_artifact(task, &format!("{invocation}:proposal"))?)?;
            changes.push(json!({"receipt_id":metadata.artifact_id,"turn_request":binding.as_ref().map(|b|&b["request"]),"run":binding.as_ref().map(|b|&b["run"]),"receipt":receipt,"proposal":proposal,"provenance":"known_native_filesystem_effect; not API/DB/Git reset","selection":"receipt_id; optional hunks maps path to original native edit indices"}));
        }
    }
    Ok(
        json!({"command":"task changes","task_id":task,"changes":changes,"model_calls":0,"historical_evidence":true}),
    )
}
fn plan(app: &App, store: &LocalStore, task: &str, request: &Request) -> Result<Plan> {
    super::settled(app, store, task)?;
    let parent = store.journal().task(task)?;
    if !parent.record.state.is_terminal()
        && parent.record.state != melissa_core_contracts::TaskState::Running
    {
        return Err(Error::Conflict(
            "Geri alma için duraklatılmış veya bitmiş görev gerekli.",
        ));
    }
    if parent.record.state == melissa_core_contracts::TaskState::Running
        && !app.live(task).is_ok_and(|v| v.phase == "paused")
    {
        return Err(Error::Conflict("Önce güvenli pause gerekli."));
    }
    let execution = app
        .execution(store, task)?
        .ok_or(Error::Denied("Kaynak yazma kapsamı olan görev gerekli."))?;
    let repo = app.source(&app.workspace(store, task)?)?;
    let snapshot = repo.snapshot(&AtomicBool::new(false))?;
    let writes = crate::project::task::effective_writes(app, task, &execution.recipe)?;
    if request.turn_request.is_some() == !request.receipts.is_empty() {
        return Err(Error::Usage(
            "Tek turn_request veya receipts seç; ikisini birlikte seçme.",
        ));
    }
    let mut picks = request.receipts.clone();
    if let Some(turn) = &request.turn_request {
        for metadata in store.list_artifacts(task)? {
            if metadata.logical_type == "repository_tool_binding" {
                let b: Value = decode(&store.get_artifact(task, &metadata.artifact_id)?)?;
                if b["request"] == *turn && b["task"] == task {
                    let id = format!("{}:receipt", b["invocation"].as_str().ok_or(Error::Store)?);
                    if store.artifact_metadata(task, &id).is_ok() {
                        picks.push(Pick {
                            receipt_id: id,
                            hunks: BTreeMap::new(),
                        });
                    }
                }
            }
        }
    }
    if picks.is_empty() {
        return Err(Error::Usage("Geri alınacak native makbuz yok."));
    }
    let mut order = BTreeMap::new();
    for event in store.journal().events(task)? {
        if let Some(inv) = event.invocation {
            order.insert(inv.invocation_id, event.sequence);
        }
    }
    let mut receipts = vec![];
    let mut seen = BTreeSet::new();
    for pick in picks {
        if !seen.insert(pick.receipt_id.clone()) {
            return Err(Error::Usage("Aynı makbuz iki kez seçildi."));
        }
        let r = read_receipt(store, task, &pick.receipt_id)?;
        if r["root_identity"] != repo.root_identity() {
            return Err(Error::Denied("Makbuz farklı çalışma köküne ait."));
        }
        let sequence = *order
            .get(r["invocation_id"].as_str().ok_or(Error::Store)?)
            .ok_or(Error::Store)?;
        receipts.push((sequence, pick, r));
    }
    receipts.sort_by_key(|v| std::cmp::Reverse(v.0));
    let mut previous_hunks: BTreeMap<String, BTreeMap<String, BTreeSet<usize>>> = BTreeMap::new();
    for artifact in store.list_artifacts(task)? {
        if artifact.logical_type == "cli.continuity.undo-receipt" {
            let value: Value = decode(&store.get_artifact(task, &artifact.artifact_id)?)?;
            if value["outcome"] == "PASS" && value["restored_hunks"].is_object() {
                let completed: BTreeMap<String, BTreeMap<String, BTreeSet<usize>>> =
                    serde_json::from_value(value["restored_hunks"].clone())
                        .map_err(|_| Error::Store)?;
                for (receipt, paths) in completed {
                    for (path, indices) in paths {
                        previous_hunks
                            .entry(receipt.clone())
                            .or_default()
                            .entry(path)
                            .or_default()
                            .extend(indices);
                    }
                }
            }
        }
    }
    let mut restored_hunks: BTreeMap<String, BTreeMap<String, BTreeSet<usize>>> = BTreeMap::new();
    let mut files = BTreeMap::new();
    let mut touched = BTreeSet::new();
    let mut namespace = vec![];
    let mut directories = vec![];
    let mut used = vec![];
    for (_, pick, receipt) in &receipts {
        used.push(pick.receipt_id.clone());
        if receipt["files"].is_array() {
            let proposal = PatchProposal::parse(&store.get_artifact(
                task,
                &format!(
                    "{}:proposal",
                    receipt["invocation_id"].as_str().ok_or(Error::Store)?
                ),
            )?)?;
            if proposal.digest()? != receipt["proposal_digest"] {
                return Err(Error::Store);
            }
            if pick
                .hunks
                .keys()
                .any(|p| !proposal.files.iter().any(|f| &f.before.path == p))
            {
                return Err(Error::Usage("Seçili parça makbuzda yok."));
            }
            for patch in &proposal.files {
                let selection = if pick.hunks.is_empty() {
                    (0..patch.edits.len()).collect::<BTreeSet<_>>()
                } else {
                    pick.hunks
                        .get(&patch.before.path)
                        .cloned()
                        .unwrap_or_default()
                };
                let previous = previous_hunks
                    .get(&pick.receipt_id)
                    .and_then(|p| p.get(&patch.before.path))
                    .cloned()
                    .unwrap_or_default();
                let selection = selection
                    .difference(&previous)
                    .copied()
                    .collect::<BTreeSet<_>>();
                if selection.is_empty() {
                    continue;
                }
                if selection.iter().any(|i| *i >= patch.edits.len()) {
                    return Err(Error::Usage("Parça indeksi geçersiz."));
                }
                let applied = receipt["files"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|v| v["before"]["path"] == patch.before.path)
                    .ok_or(Error::Store)?;
                let raw = store.get_artifact(
                    task,
                    applied["preimage_artifact_id"]
                        .as_str()
                        .ok_or(Error::Store)?,
                )?;
                if digest(&raw) != patch.before.digest {
                    return Err(Error::Store);
                }
                let mut post = raw.clone();
                for (index, edit) in patch.edits.iter().enumerate().rev() {
                    if previous.contains(&index) {
                        continue;
                    }
                    if raw.get(edit.start as usize..edit.end as usize)
                        != Some(edit.expected.as_slice())
                    {
                        return Err(Error::Store);
                    }
                    post.splice(
                        edit.start as usize..edit.end as usize,
                        edit.replacement.clone(),
                    );
                }
                let first = !files.contains_key(&patch.before.path);
                let current = file(&mut files, &repo, &patch.before.path)?;
                if current.current.as_deref() != Some(post.as_slice())
                    || (first
                        && current.original.as_ref().map(|v| &v.identity)
                            != applied["after"]["identity"]
                                .as_str()
                                .map(|s| s.to_string())
                                .as_ref())
                {
                    return Err(Error::Conflict(
                        "Kullanıcı veya sonraki tur bu dosyayı değiştirdi; hiçbir dosya yazılmadı.",
                    ));
                }
                let mut inverse = vec![];
                let mut delta = 0i128;
                for (index, edit) in patch.edits.iter().enumerate() {
                    if previous.contains(&index) {
                        continue;
                    }
                    let start = (edit.start as i128 + delta) as usize;
                    if selection.contains(&index) {
                        inverse.push((
                            start,
                            start + edit.replacement.len(),
                            edit.expected.clone(),
                        ));
                    }
                    delta += edit.replacement.len() as i128 - edit.expected.len() as i128;
                }
                for (start, end, before) in inverse.into_iter().rev() {
                    post.splice(start..end, before);
                }
                current.current = Some(post);
                touched.insert(patch.before.path.clone());
                restored_hunks
                    .entry(pick.receipt_id.clone())
                    .or_default()
                    .insert(patch.before.path.clone(), selection);
            }
        } else {
            if !pick.hunks.is_empty() {
                return Err(Error::Usage("Namespace işlemi parçalara ayrılamaz."));
            }
            let before: Option<FileVersion> =
                serde_json::from_value(receipt["before"].clone()).map_err(|_| Error::Store)?;
            let after: Option<FileVersion> =
                serde_json::from_value(receipt["after"].clone()).map_err(|_| Error::Store)?;
            if receipt["namespace_kind"] == "empty_directory" {
                let mutation = match receipt["tool_id"].as_str() {
                    Some("fs.mkdir") => {
                        let v = after.ok_or(Error::Store)?;
                        if repo.empty_directory_version(&v.path)? != v {
                            return Err(Error::Conflict("Dizin değişti."));
                        }
                        directories.push(json!({"path":v.path,"exists":false}));
                        FileMutation::DeleteDirectory { before: v }
                    }
                    Some("fs.rmdir") => {
                        let v = before.ok_or(Error::Store)?;
                        let destination = repo.absent_path(&v.path)?;
                        directories.push(json!({"path":v.path,"exists":true}));
                        FileMutation::CreateDirectory { destination }
                    }
                    Some("fs.movedir") => {
                        let b = before.ok_or(Error::Store)?;
                        let a = after.ok_or(Error::Store)?;
                        if repo.empty_directory_version(&a.path)? != a {
                            return Err(Error::Conflict("Dizin değişti."));
                        }
                        let destination = repo.absent_path(&b.path)?;
                        directories.push(json!({"path":a.path,"exists":false}));
                        directories.push(json!({"path":b.path,"exists":true}));
                        FileMutation::MoveDirectory {
                            before: a,
                            destination,
                        }
                    }
                    _ => return Err(Error::Store),
                };
                namespace.push(Effect::Mutation {
                    proposal: MutationProposal {
                        schema_version: "melissa.repository-mutation/v2".into(),
                        root_identity: repo.root_identity().into(),
                        mutation,
                    },
                });
                continue;
            }
            let original = if let Some(reference) = receipt["preimage_artifact_id"].as_str() {
                Some(store.get_artifact(task, reference)?)
            } else {
                None
            };
            match receipt["tool_id"].as_str() {
                Some("fs.create") => {
                    let a = after.ok_or(Error::Store)?;
                    let first = !files.contains_key(&a.path);
                    let f = file(&mut files, &repo, &a.path)?;
                    if f.current.as_ref().is_none_or(|v| digest(v) != a.digest)
                        || (first && f.original.as_ref() != Some(&a))
                    {
                        return Err(Error::Conflict("Oluşturulan dosya değişmiş; silinmedi."));
                    }
                    f.current = None;
                    touched.insert(a.path);
                }
                Some("fs.delete") => {
                    let b = before.ok_or(Error::Store)?;
                    let raw = original.ok_or(Error::Store)?;
                    if digest(&raw) != b.digest {
                        return Err(Error::Store);
                    }
                    let f = file(&mut files, &repo, &b.path)?;
                    if f.current.is_some() {
                        return Err(Error::Conflict(
                            "Silinen dosyanın yolu yeniden kullanılıyor; üzerine yazılmadı.",
                        ));
                    }
                    f.current = Some(raw);
                    touched.insert(b.path);
                }
                Some("fs.move") => {
                    let b = before.ok_or(Error::Store)?;
                    let a = after.ok_or(Error::Store)?;
                    let first = !files.contains_key(&a.path);
                    let f = file(&mut files, &repo, &a.path)?;
                    if f.current.as_ref().is_none_or(|v| digest(v) != a.digest)
                        || (first && f.original.as_ref() != Some(&a))
                    {
                        return Err(Error::Conflict("Taşınan dosya değişmiş."));
                    }
                    let raw = f.current.take().ok_or(Error::Store)?;
                    let target = file(&mut files, &repo, &b.path)?;
                    if target.current.is_some() {
                        return Err(Error::Conflict("Eski taşıma yolu yeniden kullanılıyor."));
                    }
                    target.current = Some(raw);
                    touched.extend([a.path, b.path]);
                }
                _ => {
                    return Err(Error::Denied(
                        "Bu etki otomatik dosya geri alması değildir.",
                    ));
                }
            }
        }
    }
    if touched.is_empty() && namespace.is_empty() {
        return Err(Error::Usage("Seçili geri alma değişiklik içermiyor."));
    }
    if !namespace.is_empty() && (!touched.is_empty() || namespace.len() > 1) {
        return Err(Error::Denied(
            "Boş dizin geri almasını tek native işlem olarak incele; karma/recursive geri alma yapılmadı.",
        ));
    }
    if touched.iter().any(|p| {
        !writes
            .iter()
            .any(|w| p == w || p.starts_with(&(w.clone() + "/")))
            || execution
                .recipe
                .protected_paths
                .iter()
                .any(|w| p == w || p.starts_with(&(w.clone() + "/")))
    }) {
        return Err(Error::Denied(
            "Geri alma özgün yazma kapsamını veya korunan dosyayı aşamaz.",
        ));
    }
    let mut effects = vec![];
    let mut patches = vec![];
    let mut expected_files = BTreeMap::new();
    for (path, value) in files {
        if !touched.contains(&path) {
            continue;
        }
        expected_files.insert(path.clone(), value.current.as_ref().map(|v| digest(v)));
        match (value.original, value.current) {
            (Some(version), Some(raw)) => {
                let original = repo.read(&path)?.1;
                if original != raw {
                    patches.push(FilePatch {
                        before: version,
                        edits: vec![ByteEdit {
                            start: 0,
                            end: original.len() as u64,
                            expected: original,
                            replacement: raw,
                        }],
                    });
                }
            }
            (Some(before), None) => effects.push(Effect::Mutation {
                proposal: MutationProposal {
                    schema_version: "melissa.repository-mutation/v1".into(),
                    root_identity: repo.root_identity().into(),
                    mutation: FileMutation::Delete { before },
                },
            }),
            (None, Some(raw)) => effects.push(Effect::Mutation {
                proposal: MutationProposal {
                    schema_version: "melissa.repository-mutation/v1".into(),
                    root_identity: repo.root_identity().into(),
                    mutation: FileMutation::Create {
                        destination: repo.absent_path(&path)?,
                        bytes: raw,
                    },
                },
            }),
            _ => {}
        }
    }
    if !patches.is_empty() {
        effects.insert(
            0,
            Effect::Patch {
                proposal: PatchProposal {
                    schema_version: "melissa.repository-patch/v1".into(),
                    root_identity: repo.root_identity().into(),
                    files: patches,
                },
            },
        )
    }
    effects.extend(namespace);
    // Directory paths receive the same original scope check, before admission.
    for effect in &effects {
        if let Effect::Mutation { proposal } = effect {
            let paths = match &proposal.mutation {
                FileMutation::Create { destination, .. }
                | FileMutation::CreateDirectory { destination } => vec![&destination.path],
                FileMutation::Delete { before } | FileMutation::DeleteDirectory { before } => {
                    vec![&before.path]
                }
                FileMutation::Move {
                    before,
                    destination,
                }
                | FileMutation::MoveDirectory {
                    before,
                    destination,
                } => vec![&before.path, &destination.path],
            };
            if paths.iter().any(|p| {
                !writes
                    .iter()
                    .any(|w| *p == w || p.starts_with(&(w.clone() + "/")))
                    || execution
                        .recipe
                        .protected_paths
                        .iter()
                        .any(|w| *p == w || p.starts_with(&(w.clone() + "/")))
            }) {
                return Err(Error::Denied("Namespace geri alması özgün kapsam dışında."));
            }
        }
    }
    repo.validate_snapshot(&snapshot, &AtomicBool::new(false))?;
    Ok(Plan {
        task_id: task.into(),
        root_identity: repo.root_identity().into(),
        source_digest: snapshot.digest,
        task_version: parent.record.version,
        request: request.clone(),
        effects,
        expected_files,
        directory_expectations: directories,
        forward_receipts: used,
        restored_hunks,
    })
}
impl App {
    pub fn undo_preview(&self, task: &str, request: Request) -> Result<Value> {
        let store = self.store(task)?;
        let plan = plan(self, &store, task, &request)?;
        Ok(
            json!({"command":"task undo-preview","review_digest":digest(&bytes(&plan)?),"plan":plan,"multi_operation_atomic":false,"conflict_policy":"exact postimage; user edits including disjoint edits in the same touched file block undo","model_calls":0,"task_accepted":false}),
        )
    }
    pub fn undo(&self, task: &str, request: Request, expected: &str) -> Result<Value> {
        let mut store = self.store(task)?;
        let record = format!("cli:undo:{}", digest(expected.as_bytes()).replace(':', "-"));
        if let Ok(raw) = store.get_artifact(task, &record) {
            let mut value: Value = decode(&raw)?;
            if value["request_digest"] != digest(&bytes(&request)?) {
                return Err(Error::Conflict("Geri alma kimliği farklı seçime ait."));
            }
            value["historical_evidence"] = json!(true);
            value["reused"] = json!(true);
            return Ok(value);
        }
        if store
            .artifact_metadata(task, &format!("{record}:intent"))
            .is_ok()
        {
            return Err(Error::Reconciliation(
                "Önceki geri alma intent'i sonuçlanmamış; otomatik tekrar yok.",
            ));
        }
        let plan = plan(self, &store, task, &request)?;
        if digest(&bytes(&plan)?) != expected {
            return Err(Error::Conflict(
                "Güncel seçimi, kaynak ve planı kapsayan review_digest gerekli.",
            ));
        }
        if plan.effects.is_empty() {
            return Err(Error::Usage("Geri alma etkisi yok."));
        }
        let parent = store.journal().task(task)?;
        let execution = self.execution(&store, task)?.ok_or(Error::Store)?;
        let repo = self.source(&self.workspace(&store, task)?)?;
        let child = format!("undo:{}", nonce()?);
        let authorization = format!("undo-review:{}", blake3::hash(expected.as_bytes()));
        put(
            &mut store,
            task,
            &format!("{record}:intent"),
            "continuity.undo-intent",
            &bytes(
                &json!({"child_task":child,"review_digest":expected,"request_digest":digest(&bytes(&request)?),"plan":plan,"automatic_retry":false}),
            )?,
        )?;
        let mut contract = parent.contract.clone();
        contract.task_id = child.clone();
        contract.goal =
            "Operator-reviewed exact inverse file effects; not application acceptance".into();
        contract.input_snapshot_digest = plan.source_digest.clone();
        contract.acceptance_contract_digest = digest(&bytes(&plan)?);
        contract.authorization_refs = vec![authorization.clone()];
        contract.allowed_effects = vec![
            "workspace.read".into(),
            "workspace.apply_patch_scoped".into(),
            "evidence.append".into(),
        ];
        contract.limits.max_cost_micro_usd = 0;
        store.journal_mut().create_task(&contract)?;
        for state in [
            melissa_core_contracts::TaskState::Planned,
            melissa_core_contracts::TaskState::AwaitingApproval,
            melissa_core_contracts::TaskState::Queued,
            melissa_core_contracts::TaskState::Leased,
            melissa_core_contracts::TaskState::Running,
        ] {
            let version = store.journal().task(&child)?.record.version;
            store.transition_task(&child, version, state, None)?;
        }
        let profile = execution
            .recipe
            .agents
            .iter()
            .find(|s| s.write)
            .ok_or(Error::Denied("Özgün yazma yetkili uzman yok."))?
            .profile
            .clone();
        let policy = PatchPolicy {
            task_id: child.clone(),
            principal_id: "cli:operator".into(),
            profile_id: profile,
            resource_id: contract.workspace_ref.clone(),
            authorization_ref: authorization,
            expires_at_ms: now()?.saturating_add(60000),
            epoch: 1,
            writable_paths: crate::project::task::effective_writes(self, task, &execution.recipe)?,
            protected_paths: execution.recipe.protected_paths.clone(),
        };
        let mut receipts = vec![];
        let applied = (|| -> Result {
            if repo.snapshot(&AtomicBool::new(false))?.digest != plan.source_digest {
                return Err(Error::Conflict("Geri alma uygulanmadan kaynak değişti."));
            }
            for (index, effect) in plan.effects.iter().enumerate() {
                let invocation = format!("undo:{}:{index}", nonce()?);
                let attempt = format!("attempt:{invocation}");
                let mut broker = PatchBroker::new(&repo, &mut store, policy.clone())?;
                let value = match effect {
                    Effect::Patch { proposal } => {
                        let permit = broker.authorize(&invocation, &attempt, proposal.clone())?;
                        serde_json::to_value(broker.execute(permit, &AtomicBool::new(false))?)
                            .map_err(|_| Error::Store)?
                    }
                    Effect::Mutation { proposal } => {
                        let permit =
                            broker.authorize_mutation(&invocation, &attempt, proposal.clone())?;
                        serde_json::to_value(
                            broker.execute_mutation(permit, &AtomicBool::new(false))?,
                        )
                        .map_err(|_| Error::Store)?
                    }
                };
                receipts.push(value);
            }
            for (path, expected) in &plan.expected_files {
                match expected {
                    Some(hash) => {
                        if repo.read(path)?.0.digest != *hash {
                            return Err(Error::Reconciliation(
                                "Geri alma sonrası dosya özeti eşleşmedi.",
                            ));
                        }
                    }
                    None => {
                        repo.absent_path(path)?;
                    }
                }
            }
            for expected in &plan.directory_expectations {
                let path = expected["path"].as_str().ok_or(Error::Store)?;
                if expected["exists"] == true {
                    repo.empty_directory_version(path)?;
                } else {
                    repo.absent_path(path)?;
                }
            }
            Ok(())
        })();
        let outcome = if applied.is_ok() {
            "PASS"
        } else if receipts.is_empty()
            && store
                .journal()
                .task(&child)?
                .record
                .unknown_invocation_id
                .is_none()
        {
            "FAIL"
        } else {
            "INCONCLUSIVE"
        };
        let result = json!({"command":"task undo","parent_task":task,"child_task":child,"outcome":outcome,"request_digest":digest(&bytes(&request)?),"review_digest":expected,"source_digest":repo.snapshot(&AtomicBool::new(false))?.digest,"native_receipts":receipts,"restored_hunks":plan.restored_hunks,"reason":applied.err().map(|e|e.to_string()),"multi_operation_atomic":false,"parent_task_accepted":false,"budget_reset":false,"model_calls":0});
        put(
            &mut store,
            task,
            &record,
            "continuity.undo-receipt",
            &bytes(&result)?,
        )?;
        if outcome == "PASS" {
            let final_id = format!("{child}:exact-inverse-proof");
            put(
                &mut store,
                &child,
                &final_id,
                "continuity.inverse-proof",
                &bytes(&result)?,
            )?;
            let version = store.journal().task(&child)?.record.version;
            let selected = store
                .journal_mut()
                .set_final_artifact(&child, version, &final_id)?;
            let awaiting = store.transition_task(
                &child,
                selected.record.version,
                melissa_core_contracts::TaskState::Verifying,
                None,
            )?;
            let hash = store.artifact_metadata(&child, &final_id)?.digest;
            store.transition_task(
                &child,
                awaiting.record.version,
                melissa_core_contracts::TaskState::Completed,
                Some(&melissa_core_contracts::CompletionEvidence {
                    task_id: child.clone(),
                    acceptance_digest: contract.acceptance_contract_digest,
                    final_digest: hash.clone(),
                    verified_digest: hash,
                    verifier_run_id: format!("exact-inverse:{}", nonce()?),
                    protected_checks_passed: true,
                    actual_tool_run: true,
                    raw_artifact_refs: vec![final_id],
                }),
            )?;
        }
        Ok(result)
    }
}
