use crate::{Error, Result, digest};
use melissa_code_quality::{source::FrozenSource, vault};
use melissa_core_contracts::{
    Module, TaskState,
    catalog_models::AgentIndex,
    json::parse_contract,
    wire::{TaskContractV1, TaskLimitsV1},
};
use melissa_local_store::{ArtifactRequest, LocalStore, StoreConfig, StoreKey};
use melissa_repository::{Limits, Repository, RepositorySnapshot};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
};
pub(crate) const WORKSPACE: &str = "cli:workspace:v1";
pub(crate) const APPROVAL: &str = "cli:approval-request:v1";
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct HomeManifest {
    schema_version: String,
    installation_id: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Workspace {
    pub(crate) schema_version: String,
    pub(crate) root: String,
    pub(crate) limits: Limits,
    pub(crate) explicit_excludes: Vec<String>,
    pub(crate) snapshot: RepositorySnapshot,
    pub(crate) files: BTreeMap<String, String>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Approval {
    schema_version: String,
    task_id: String,
    request_id: String,
    expected_task_version: u64,
    source_digest: String,
    contract_digest: String,
    effects: Vec<String>,
    meaning: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    execution_digest: Option<String>,
}
pub struct App {
    pub(crate) home: PathBuf,
    pub(crate) key: StoreKey,
    _pin: Repository,
}
fn task_store_config() -> StoreConfig {
    StoreConfig {
        max_blob_bytes: 512 * 1024 * 1024,
        max_cas_bytes: 24 * 1024 * 1024 * 1024,
        ..StoreConfig::default()
    }
}
pub(crate) fn now() -> Result<u64> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .map_err(|_| Error::Unavailable("Sistem saati doğrulanamadı."))
}
pub(crate) fn nonce() -> Result<String> {
    let mut b = [0; 16];
    getrandom::fill(&mut b)
        .map_err(|_| Error::Unavailable("Güvenli görev kimliği üretilemedi."))?;
    Ok(b.iter().map(|v| format!("{v:02x}")).collect())
}
pub(crate) fn runtime_digest() -> Result<String> {
    let mut h = blake3::Hasher::new();
    h.update_reader(fs::File::open(std::env::current_exe()?)?)?;
    Ok(format!("blake3:{}", h.finalize()))
}
pub(crate) fn bytes(v: &impl Serialize) -> Result<Vec<u8>> {
    serde_json::to_vec(v).map_err(|_| Error::Store)
}
pub(crate) fn decode<T: serde::de::DeserializeOwned>(b: &[u8]) -> Result<T> {
    let value = strict_value(b)?;
    serde_json::from_value(value).map_err(|_| Error::Store)
}
fn strict_value(b: &[u8]) -> Result<Value> {
    #[derive(Deserialize)]
    #[serde(transparent)]
    struct Document(Value);
    impl melissa_core_contracts::validation::Validate for Document {
        fn validate(&self) -> melissa_core_contracts::validation::ContractResult {
            Ok(())
        }
    }
    parse_contract::<Document>(b, 64 * 1024 * 1024)
        .map(|d| d.0)
        .map_err(|_| Error::Store)
}
pub fn profiles() -> Result<AgentIndex> {
    parse_contract(
        crate::resources::file("catalog/agent-index.json")?,
        1024 * 1024,
    )
    .map_err(|_| Error::Store)
}
pub fn default_home() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("MELISSA_HOME") {
        return absolute(Path::new(&path));
    }
    let base = std::env::var_os("LOCALAPPDATA")
        .ok_or(Error::Usage("--home PATH veya MELISSA_HOME gerekli."))?;
    absolute(&PathBuf::from(base).join("Melissa/cli-v1"))
}
pub fn absolute(path: &Path) -> Result<PathBuf> {
    if path
        .components()
        .any(|p| matches!(p, std::path::Component::ParentDir))
    {
        return Err(Error::Denied(
            "Üst klasöre geçiş içeren bir yol kabul edilmez.",
        ));
    }
    let path = std::path::absolute(path)?.components().collect::<PathBuf>();
    safe_path(&path)?;
    Ok(path)
}
pub(crate) fn safe_path(path: &Path) -> Result {
    use std::path::{Component, Prefix};
    let mut current = PathBuf::new();
    for part in path.components() {
        match part {
            Component::Prefix(p) if matches!(p.kind(), Prefix::Disk(_)) => {}
            Component::Prefix(_) | Component::ParentDir | Component::CurDir => {
                return Err(Error::Denied(
                    "Yerel, açık ve geçiş içermeyen bir yol gerekli.",
                ));
            }
            _ => {}
        }
        if let Component::Normal(name) = part {
            let name = name
                .to_str()
                .ok_or(Error::Denied("Unicode yerel dosya adı gerekli."))?;
            melissa_core_contracts::catalog::safe_relative_path(name)
                .map_err(|_| Error::Denied("ADS, aygıt veya belirsiz dosya adı kabul edilmez."))?;
        }
        current.push(part);
        match fs::symlink_metadata(&current) {
            Ok(m) => {
                #[cfg(windows)]
                {
                    use std::os::windows::fs::MetadataExt;
                    if m.file_attributes() & 0x400 != 0 {
                        return Err(Error::Denied(
                            "Bağlantı/reparse içeren veri yolu kabul edilmez.",
                        ));
                    }
                }
                if m.file_type().is_symlink() {
                    return Err(Error::Denied("Bağlantı içeren veri yolu kabul edilmez."));
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(Error::Io),
        }
    }
    Ok(())
}
pub(crate) fn put(
    store: &mut LocalStore,
    task: &str,
    id: &str,
    kind: &str,
    payload: &[u8],
) -> Result {
    store.put_artifact(
        &ArtifactRequest {
            artifact_id: id.into(),
            task_id: task.into(),
            content_type: if kind == "source.bytes" {
                "application/octet-stream"
            } else {
                "application/json"
            }
            .into(),
            logical_type: format!("cli.{kind}"),
            schema_version: "v1".into(),
            retain_until_ms: 0,
            legal_hold: true,
        },
        payload,
    )?;
    Ok(())
}
impl App {
    pub fn init(home: &Path) -> Result<Value> {
        let home = absolute(home)?;
        if home.join("home.json").exists() {
            Self::open(&home)?;
            return Ok(json!({"command":"init","status":"already_initialized","home":home}));
        }
        if home.exists() && fs::read_dir(&home)?.next().is_some() {
            return Err(Error::Conflict(
                "Veri klasörü boş değil veya kurulumu yarım; dosyalar korunuyor.",
            ));
        }
        fs::create_dir_all(&home)?;
        safe_path(&home)?;
        let _pin = Repository::open(&home, Limits::default(), vec![])?;
        let key = vault::create_key(&home.join("store-key.dpapi")).map_err(|_| {
            Error::Denied(
                "Windows kullanıcı anahtarı oluşturulamadı; mevcut anahtar değiştirilmedi.",
            )
        })?;
        drop(key);
        fs::create_dir(home.join("tasks"))?;
        let m = HomeManifest {
            schema_version: "melissa.cli-home/v1".into(),
            installation_id: nonce()?,
        };
        let mut file = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(home.join("home.json"))?;
        file.write_all(&bytes(&m)?)?;
        file.sync_all()?;
        Ok(
            json!({"command":"init","status":"initialized","home":home,"encryption":"DPAPI_user_key_and_authenticated_local_store","API_calls":0}),
        )
    }
    pub fn open(home: &Path) -> Result<Self> {
        let home = absolute(home)?;
        if !home.join("home.json").is_file() {
            return Err(Error::Usage(
                "CLI veri alanı hazır değil; önce melissa init çalıştırın.",
            ));
        }
        let pin = Repository::open(&home, Limits::default(), vec![])?;
        let m: HomeManifest = decode(&pin.read("home.json")?.1)?;
        if m.schema_version != "melissa.cli-home/v1" || !hex_id(&m.installation_id) {
            return Err(Error::Store);
        }
        safe_path(&home.join("tasks"))?;
        if !home.join("tasks").is_dir() {
            return Err(Error::Store);
        }
        safe_path(&home.join("store-key.dpapi"))?;
        let key = vault::open_key(&home.join("store-key.dpapi")).map_err(|_| {
            Error::Denied("Bu Windows kullanıcısı şifreli veri anahtarını açamadı.")
        })?;
        Ok(Self {
            home,
            key,
            _pin: pin,
        })
    }
    pub(crate) fn store(&self, id: &str) -> Result<LocalStore> {
        validate_id(id)?;
        let dir = self.home.join("tasks").join(id);
        safe_path(&dir)?;
        if !dir.is_dir() {
            return Err(Error::Usage("Görev bulunamadı."));
        }
        if !dir.join("store/store.sqlite3").is_file() {
            return Err(Error::Store);
        }
        LocalStore::open(&dir.join("store"), self.key.clone(), task_store_config())
            .and_then(LocalStore::with_pinned_cas_inventory)
            .map_err(|e| match e {
                melissa_local_store::StoreError::Busy => Error::Unavailable(
                    "Görev başka bir oturumda açık; canlı kontrol bağlantısı gereklidir.",
                ),
                _ => Error::Store,
            })
    }
    pub(crate) fn workspace(&self, s: &LocalStore, id: &str) -> Result<Workspace> {
        let w: Workspace = decode(&s.get_artifact(id, WORKSPACE)?)?;
        if w.schema_version != "melissa.cli-workspace/v1"
            || w.snapshot.digest != s.journal().task(id)?.contract.input_snapshot_digest
        {
            return Err(Error::Store);
        }
        Ok(w)
    }
    pub(crate) fn source(&self, w: &Workspace) -> Result<Repository> {
        let r = Repository::open(
            Path::new(&w.root),
            w.limits.clone(),
            w.explicit_excludes.clone(),
        )?;
        if r.root_identity() != w.snapshot.root_identity {
            return Err(Error::Conflict(
                "Çalışma klasörünün kimliği değişti; başka klasöre geçilmez.",
            ));
        }
        Ok(r)
    }
    pub fn create(
        &self,
        workspace: &Path,
        goal: &str,
        profile: &str,
        excludes: Vec<String>,
    ) -> Result<Value> {
        self.create_inner(workspace, goal, profile, excludes, None)
    }
    pub fn create_execution(
        &self,
        workspace: &Path,
        goal: &str,
        excludes: Vec<String>,
        recipe: crate::recipe::Recipe,
    ) -> Result<Value> {
        let recipe = recipe.with_serial_verification()?;
        let settings = self.settings()?;
        let source_root =
            Repository::open(&absolute(workspace)?, Limits::default(), excludes.clone())?;
        let reject_private = |path: &Path| -> Result {
            let path = absolute(path)?;
            let root = source_root
                .root_path()
                .to_string_lossy()
                .replace('/', "\\")
                .to_uppercase();
            let candidate = path.to_string_lossy().replace('/', "\\").to_uppercase();
            if candidate.starts_with(&(root.trim_end_matches('\\').to_owned() + "\\")) {
                return Err(Error::Denied(
                    "Özel kabul/anahtar dosyası modelin kaynak klasöründe olamaz.",
                ));
            }
            // Pin the relevant directory and its ancestors once. Opening every
            // ancestor as an independent repository also probes unrelated drive
            // roots and rejects valid external credential/recipe references.
            if let Some(parent) = path.parent().filter(|p| p.exists())
                && Repository::open(parent, Limits::default(), vec![])?.root_identity()
                    == source_root.root_identity()
            {
                return Err(Error::Denied(
                    "Özel kabul/anahtar dosyası modelin kaynak klasöründe olamaz.",
                ));
            }
            Ok(())
        };
        if let Some(path) = &recipe.source_ref {
            reject_private(path)?;
        }
        for (_, model) in settings.models() {
            if let crate::settings::Credential::File { path } = &model.credential {
                reject_private(path)?;
            }
        }
        if recipe
            .domain
            .as_ref()
            .is_some_and(|d| d.kind == crate::domain::Kind::Frontend)
            && !settings
                .models()
                .any(|(_, model)| model.capabilities.vision)
        {
            return Err(Error::Denied(
                "Frontend tarifi gerçek görsel girdi destekli model gerektirir.",
            ));
        }
        let negative = Repository::open(
            &absolute(&recipe.negative_workspace)?,
            Limits::default(),
            vec![],
        )?;
        if source_root.root_identity() == negative.root_identity()
            || negative.root_path().starts_with(source_root.root_path())
            || source_root.root_path().starts_with(negative.root_path())
        {
            return Err(Error::Denied(
                "Negatif kaynak ve asıl proje tamamen ayrı olmalı.",
            ));
        }
        let frozen = FrozenSource::capture(&negative, &AtomicBool::new(false))
            .map_err(|_| Error::Repository)?;
        let negative_files = frozen
            .snapshot()
            .files
            .iter()
            .map(|f| {
                Ok((
                    f.path.clone(),
                    frozen
                        .bytes(&f.path)
                        .map_err(|_| Error::Repository)?
                        .to_vec(),
                ))
            })
            .collect::<Result<_>>()?;
        let profile = recipe.agents[0].profile.clone();
        let expires_at_ms = now()?
            .checked_add(recipe.wall_time_ms)
            .ok_or(Error::Store)?;
        let execution = crate::recipe::Execution {
            recipe,
            settings_digest: settings.digest()?,
            expires_at_ms,
            negative_files,
            runtime_digest: Some(runtime_digest()?),
        };
        self.create_inner(workspace, goal, &profile, excludes, Some(execution))
    }
    pub(crate) fn require_separate_workspace(&self, repo: &Repository) -> Result {
        for p in self.home.ancestors() {
            if let Ok(r) = Repository::open(p, Limits::default(), vec![])
                && r.root_identity() == repo.root_identity()
            {
                return Err(Error::Denied(
                    "CLI veri klasörü çalışma projesinin dışında olmalı.",
                ));
            }
        }
        for p in repo.root_path().ancestors() {
            if let Ok(r) = Repository::open(p, Limits::default(), vec![])
                && r.root_identity() == self._pin.root_identity()
            {
                return Err(Error::Denied(
                    "Veri klasörünün içi kaynak projesi olarak kullanılamaz.",
                ));
            }
        }
        Ok(())
    }

    fn create_inner(
        &self,
        workspace: &Path,
        goal: &str,
        profile: &str,
        excludes: Vec<String>,
        execution: Option<crate::recipe::Execution>,
    ) -> Result<Value> {
        if goal.trim().is_empty() || goal.len() > 16384 || goal.contains('\0') {
            return Err(Error::Usage("Görev amacı boş olamaz ve 16 KiB'ı aşamaz."));
        }
        if !profiles()?
            .profiles
            .iter()
            .any(|p| p.id == profile && p.module == Module::Code)
        {
            return Err(Error::Denied(
                "Bu ilk CLI katmanında katalogdaki Code profillerinden biri seçilmeli.",
            ));
        }
        let root = absolute(workspace)?;
        let limits = Limits::default();
        let repo = Repository::open(&root, limits.clone(), excludes.clone())?;
        self.require_separate_workspace(&repo)?;
        let frozen =
            FrozenSource::capture(&repo, &AtomicBool::new(false)).map_err(|_| Error::Repository)?;
        let id = format!("task_{}", nonce()?);
        let resource = format!("workspace:{}", &id[5..]);
        let auth = format!("authorization:{}", &id[5..]);
        let acceptance = vec![
            "Final source and distribution are independently verified against the task contract"
                .to_owned(),
        ];
        let mut task = TaskContractV1 {
            schema_version: "melissa.task-contract/v1".into(),
            task_id: id.clone(),
            module: Module::Code,
            goal: goal.into(),
            workspace_ref: resource.clone(),
            input_snapshot_digest: frozen.snapshot().digest.clone(),
            acceptance_contract_digest: digest(&bytes(&acceptance)?),
            acceptance_criteria: acceptance,
            selected_profile_ids: vec![profile.into()],
            allowed_resource_ids: vec![resource],
            allowed_effects: vec!["workspace.read".into(), "evidence.append".into()],
            authorization_refs: vec![auth],
            limits: TaskLimitsV1 {
                max_wall_time_ms: 1800000,
                max_cost_micro_usd: 0,
                max_input_tokens: i64::MAX as u64,
                max_output_tokens: i64::MAX as u64,
                max_parallel_jobs: 1,
                max_disk_bytes: 512 * 1024 * 1024,
                max_output_bytes: 64 * 1024 * 1024,
            },
            authority_source: "user_task_and_rust_policy".into(),
        };
        if let Some(e) = &execution {
            if e.recipe.negative_workspace == root {
                return Err(Error::Denied("Negatif kontrol kaynaktan ayrı olmalı."));
            }
            task.selected_profile_ids = e
                .recipe
                .agents
                .iter()
                .map(|s| s.profile.clone())
                .chain(std::iter::once("melissa.core.independent-qa".into()))
                .chain(e.recipe.domain.as_ref().map(|d| d.kind.profile().into()))
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .collect();
            let s = self.settings()?;
            if s.digest()? != e.settings_digest {
                return Err(Error::Conflict(
                    "Model settings changed during task preparation.",
                ));
            }
            for provider in s
                .models()
                .map(|(_, m)| &m.provider_id)
                .collect::<std::collections::BTreeSet<_>>()
            {
                task.allowed_resource_ids
                    .push(format!("provider:{provider}"));
                task.authorization_refs
                    .push(format!("authorization:provider:{provider}"));
            }
            task.allowed_effects = vec![
                "workspace.read".into(),
                "workspace.propose_patch".into(),
                "workspace.apply_patch_scoped".into(),
                "process.execute_scoped".into(),
                "evidence.append".into(),
                "verification.run_protected".into(),
                "memory.propose".into(),
                "git.read".into(),
                "workspace.create_worktree_scoped".into(),
            ];
            task.limits.max_cost_micro_usd = e.recipe.max_cost_micro_usd;
            if let Some(domain) = &e.recipe.domain {
                task.allowed_effects.push(domain.kind.effect().into());
                task.limits.max_output_bytes = 128 * 1024 * 1024;
            }
            task.limits.max_wall_time_ms = e.recipe.wall_time_ms;
            task.acceptance_criteria=vec!["Independent source-bound protected suite with rejected negative control; no unknown effects; exact final artifact and recorded candidate".into()];
            task.acceptance_contract_digest = e.recipe.acceptance_digest()?;
        }
        let dir = self.home.join("tasks").join(&id);
        fs::create_dir(&dir)?;
        let _task_pin = Repository::open(&dir, Limits::default(), vec![])?;
        let mut s = LocalStore::open(&dir.join("store"), self.key.clone(), task_store_config())?
            .with_pinned_cas_inventory()?;
        s.journal_mut().create_task(&task)?;
        let mut files = BTreeMap::new();
        for f in &frozen.snapshot().files {
            let artifact = format!("cli:baseline:{}", blake3::hash(f.path.as_bytes()));
            put(
                &mut s,
                &id,
                &artifact,
                "source.bytes",
                frozen.bytes(&f.path).map_err(|_| Error::Repository)?,
            )?;
            files.insert(f.path.clone(), artifact);
        }
        let w = Workspace {
            schema_version: "melissa.cli-workspace/v1".into(),
            root: repo.root_path().to_str().ok_or(Error::Repository)?.into(),
            limits,
            explicit_excludes: excludes,
            snapshot: frozen.snapshot().clone(),
            files,
        };
        put(&mut s, &id, WORKSPACE, "workspace", &bytes(&w)?)?;
        if let Some(e) = &execution {
            put(
                &mut s,
                &id,
                crate::recipe::EXECUTION,
                "execution",
                &bytes(e)?,
            )?;
        }
        let t = s.transition_task(&id, 0, TaskState::Planned, None)?;
        let meaning = if execution.is_some() {
            "Authorize the displayed recipe, exact source, displayed model settings and approved providers, scoped writable paths, isolated build/protected checks and paid model use within the task budget; final acceptance still requires protected host evidence"
        } else {
            "Acknowledge this frozen source-review plan; no process, model, patch or completion permission is granted"
        };
        let request = Approval {
            schema_version: "melissa.cli-approval/v1".into(),
            task_id: id.clone(),
            request_id: format!("review_{}", nonce()?),
            expected_task_version: t.record.version + 1,
            source_digest: w.snapshot.digest.clone(),
            contract_digest: digest(&bytes(&task)?),
            effects: task.allowed_effects.clone(),
            meaning: meaning.into(),
            execution_digest: execution
                .as_ref()
                .map(|e| bytes(e).map(|b| digest(&b)))
                .transpose()?,
        };
        put(&mut s, &id, APPROVAL, "approval.request", &bytes(&request)?)?;
        frozen
            .current(&repo, &AtomicBool::new(false))
            .map_err(|_| {
                Error::Conflict("Kaynak görev hazırlanırken değişti; kayıt korundu, onaylanmadı.")
            })?;
        s.transition_task(&id, t.record.version, TaskState::AwaitingApproval, None)?;
        self.show_store(&s, &id)
    }
    pub(crate) fn show_store(&self, s: &LocalStore, id: &str) -> Result<Value> {
        let t = s.journal().task(id)?;
        let w = self.workspace(s, id)?;
        let request: Approval = decode(&s.get_artifact(id, APPROVAL)?)?;
        let mut value = json!({"command":"task show","task_id":id,"state":t.record.state,"version":t.record.version,"goal":t.contract.goal,"profile":t.contract.selected_profile_ids,"workspace":w.root,"source_digest":t.contract.input_snapshot_digest,"source_files":w.snapshot.files.len(),"excluded_prefixes":w.snapshot.excluded_prefixes,"approval":{"request_id":request.request_id,"digest":digest(&bytes(&request)?),"meaning":request.meaning,"pending":t.record.state==TaskState::AwaitingApproval},"model_budget":s.journal().model_budget(id)?,"task_accepted":t.record.state==TaskState::Completed,"execution_backend":"not_connected_in_first_CLI_slice"});
        if let Some(e) = self.execution(s, id)? {
            value["execution_backend"] =
                json!("Gateway_workflow_scoped_repository_CODE03_protected_QA");
            value["execution_plan"] = e.recipe.public_plan();
            value["execution_plan"]["verification"] =
                crate::verification::Policy::for_recipe(&e.recipe).plan(&e.recipe);
            value["execution_plan"]["timing"] = json!({"task_wall_time_ms":e.recipe.wall_time_ms,
                "task_deadline_ms":e.expires_at_ms,"default_sdk_tool_wall_time_ms":crate::development::limits().wall_time_ms});
            value["settings_digest"] = json!(e.settings_digest);
            if let Ok(settings) = self.settings()
                && settings.digest()? == e.settings_digest
            {
                value["execution_plan"]["approved_models"] = settings.model_summary();
            }
            value["expires_at_ms"] = json!(e.expires_at_ms);
            value["runtime_digest"] = json!(e.runtime_digest);
            if let Ok(control) = self.live(id) {
                value["phase"] = json!(control.phase);
            }
        }
        Ok(value)
    }
    pub fn show(&self, id: &str) -> Result<Value> {
        match self.store(id) {
            Ok(store) => self.show_store(&store, id),
            Err(Error::Unavailable(_)) => {
                let s = self.live(id)?;
                if !s.active() {
                    return Err(Error::Unavailable("Çalıştırıcı durumu doğrulanamadı."));
                }
                Ok(
                    json!({"command":"task show","task_id":id,"state":s.task["state"],"goal":s.task["goal"],"live":true,"phase":s.phase,"updated_at_ms":s.updated_at_ms,"model_budget":s.model_budget,"workflow":s.workflow,"cancel_requested":s.cancel_requested,"pause_requested":s.pause_requested,"task_accepted":false}),
                )
            }
            Err(e) => Err(e),
        }
    }
    pub fn list(&self) -> Result<Value> {
        let mut ids = vec![];
        for entry in fs::read_dir(self.home.join("tasks"))? {
            let entry = entry?;
            let id = entry.file_name().into_string().map_err(|_| Error::Store)?;
            validate_id(&id)?;
            safe_path(&entry.path())?;
            if !entry.file_type()?.is_dir() {
                return Err(Error::Store);
            }
            ids.push(id);
            if ids.len() > 10000 {
                return Err(Error::Unavailable(
                    "Görev listesi güvenli kayıt sınırını aşıyor.",
                ));
            }
        }
        ids.sort();
        let mut tasks = vec![];
        for id in ids {
            match self.store(&id) {
                Ok(s) => {
                    let t = s.journal().task(&id)?;
                    tasks.push(json!({"task_id":id,"state":t.record.state,"version":t.record.version,"goal":t.contract.goal,"task_accepted":t.record.state==TaskState::Completed}));
                }
                Err(Error::Unavailable(_)) => {
                    tasks.push(json!({"task_id":id,"state":"store_busy","task_accepted":false}))
                }
                Err(e) => return Err(e),
            }
        }
        Ok(json!({"command":"task list","tasks":tasks}))
    }
    pub fn diff(&self, id: &str) -> Result<Value> {
        let s = self.store(id)?;
        let w = self.workspace(&s, id)?;
        let r = self.source(&w)?;
        let current = r.snapshot(&AtomicBool::new(false))?;
        let before: BTreeMap<_, _> = w.snapshot.files.iter().map(|f| (&f.path, f)).collect();
        let after: BTreeMap<_, _> = current.files.iter().map(|f| (&f.path, f)).collect();
        let mut paths = before
            .keys()
            .chain(after.keys())
            .copied()
            .collect::<Vec<_>>();
        paths.sort();
        paths.dedup();
        let changes=paths.into_iter().filter_map(|p|{let b=before.get(p);let a=after.get(p);let kind=match(b,a){(None,Some(_))=>"added",(Some(_),None)=>"deleted",(Some(b),Some(a)) if b.digest!=a.digest=>"modified",(Some(b),Some(a)) if b.identity!=a.identity=>"replaced_identity",_=>return None};Some(json!({"path":p,"kind":kind,"before_digest":b.map(|f|&f.digest),"after_digest":a.map(|f|&f.digest)}))}).collect::<Vec<_>>();
        Ok(
            json!({"command":"task diff","task_id":id,"baseline_digest":w.snapshot.digest,"current_digest":current.digest,"atomic_tree_snapshot":false,"changes":changes,"raw_source_printed":false}),
        )
    }
    pub fn approve(&self, id: &str, request_id: &str, provided_digest: &str) -> Result<Value> {
        let mut s = self.store(id)?;
        let t = s.journal().task(id)?;
        let w = self.workspace(&s, id)?;
        let request: Approval = decode(&s.get_artifact(id, APPROVAL)?)?;
        if t.record.state != TaskState::AwaitingApproval
            || t.record.version != request.expected_task_version
            || request.task_id != id
            || request.schema_version != "melissa.cli-approval/v1"
            || request.request_id != request_id
            || digest(&bytes(&request)?) != provided_digest
            || request.contract_digest != digest(&bytes(&t.contract)?)
            || request.source_digest != t.contract.input_snapshot_digest
            || request.effects != t.contract.allowed_effects
        {
            return Err(Error::Conflict(
                "Onay isteği, kapsamı veya görev sürümü eşleşmiyor.",
            ));
        }
        let r = self.source(&w)?;
        r.validate_snapshot(&w.snapshot, &AtomicBool::new(false))
            .map_err(|_| Error::Conflict("Onaydan önce kaynak değişti; eski onay kullanılamaz."))?;
        let execution = self.execution(&s, id)?;
        if request.execution_digest
            != execution
                .as_ref()
                .map(|e| bytes(e).map(|b| digest(&b)))
                .transpose()?
        {
            return Err(Error::Conflict("Onaylı yürütme tarifi değişti."));
        }
        if let Some(e) = &execution
            && (self.settings()?.digest()? != e.settings_digest || now()? >= e.expires_at_ms)
        {
            return Err(Error::Conflict("Yürütme ayarı veya tarif süresi değişti."));
        }
        let scope = if execution.is_some() {
            "execute_recipe"
        } else {
            "prepare_source_review_only"
        };
        let decision = json!({"schema_version":"melissa.cli-approval-decision/v1","request_digest":provided_digest,"request_id":request_id,"task_id":id,"source_digest":request.source_digest,"approved_at_ms":now()?,"actor":"local_operator","scope":scope,"process_or_model_permit_created":false});
        put(
            &mut s,
            id,
            &format!("cli:decision:{request_id}"),
            "approval.decision",
            &bytes(&decision)?,
        )?;
        let queued = s.transition_task(id, t.record.version, TaskState::Queued, None)?;
        Ok(
            json!({"command":"task approve","task_id":id,"state":queued.record.state,"scope":scope,"worker_started":false,"API_calls":0,"task_accepted":false}),
        )
    }
    pub fn cancel(&self, id: &str) -> Result<Value> {
        let mut s = match self.store(id) {
            Ok(s) => s,
            Err(Error::Unavailable(_)) => {
                if self.live(id).is_ok_and(|s| s.active()) {
                    return self.request_control(id, "cancel");
                }
                return Err(Error::Unavailable(
                    "Görev başka oturumda açık; doğrulanmış canlı kontrol yok.",
                ));
            }
            Err(e) => return Err(e),
        };
        let t = s.journal().task(id)?;
        let budget = s.journal().model_budget(id)?;
        if t.record.state == TaskState::Cancelled {
            return Ok(
                json!({"command":"task cancel","task_id":id,"state":"cancelled","already_cancelled":true,"task_accepted":false}),
            );
        }
        let executable = self.execution(&s, id)?.is_some();
        if executable && !crate::engine::native_stop_proven(&s, id)? {
            return Err(Error::Reconciliation(
                "Süreç temizliği doğrulanmadı; iptal sonucu uydurulamaz.",
            ));
        }
        let paused = executable
            && self
                .live(id)
                .is_ok_and(|s| s.phase == "paused" && !s.active());
        if !(matches!(
            t.record.state,
            TaskState::Created
                | TaskState::Planned
                | TaskState::AwaitingApproval
                | TaskState::Queued
                | TaskState::Blocked
        ) || (paused && t.record.state == TaskState::Running))
            || t.record.unknown_invocation_id.is_some()
            || budget.active_requests != 0
            || budget.unknown_requests != 0
        {
            return Err(Error::Conflict(
                "Canlı veya belirsiz iş durdurulmuş gibi gösterilemez; yürütücü kontrolü/uzlaştırma gerekli.",
            ));
        }
        let t = s.transition_task(id, t.record.version, TaskState::Cancelled, None)?;
        Ok(
            json!({"command":"task cancel","task_id":id,"state":t.record.state,"worker_was_started":if executable {Value::Null}else{json!(false)},"owned_workers_confirmed_stopped":true,"task_accepted":false}),
        )
    }
    pub fn events(&self, id: &str) -> Result<Value> {
        let s = self.store(id)?;
        Ok(json!({"command":"task events","task_id":id,"events":s.journal().events(id)?}))
    }
    pub fn evidence(&self, id: &str, artifact: Option<&str>, out: Option<&Path>) -> Result<Value> {
        if artifact.is_some() != out.is_some() {
            return Err(Error::Usage("--artifact ve --out birlikte verilmeli."));
        }
        let s = self.store(id)?;
        let inventory = self.evidence_inventory(&s, id)?;
        if let (Some(artifact), Some(out)) = (artifact, out) {
            let owner = inventory
                .iter()
                .find(|a| a.artifact_id == artifact)
                .ok_or(Error::Store)?
                .task_id
                .clone();
            let data = s.get_artifact(&owner, artifact)?;
            let target = absolute(out)?;
            let parent = target.parent().ok_or(Error::Io)?;
            if !parent.is_dir() {
                return Err(Error::Usage("Çıktı klasörü önceden var olmalı."));
            }
            let _pin = Repository::open(parent, Limits::default(), vec![])?;
            let mut file = fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&target)
                .map_err(|_| {
                    Error::Conflict("Çıktı dosyası var veya yazılamıyor; üzerine yazılmadı.")
                })?;
            file.write_all(&data)?;
            file.sync_all()?;
            Ok(
                json!({"command":"task evidence export","task_id":id,"artifact_id":artifact,"output":target,"bytes":data.len(),"content_digest":digest(&data),"exported_content_is_untrusted_data":true}),
            )
        } else {
            Ok(json!({"command":"task evidence","task_id":id,"artifacts":inventory}))
        }
    }
}
pub(crate) fn validate_id(id: &str) -> Result {
    if id.len() != 37 || !id.starts_with("task_") || !hex_id(&id[5..]) {
        return Err(Error::Usage(
            "Geçerli task_<32 küçük hex karakter> kimliği gerekli.",
        ));
    }
    Ok(())
}
fn hex_id(value: &str) -> bool {
    value.len() == 32
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
