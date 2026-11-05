use super::{Contract, Environment};
use crate::{
    Error, Result,
    app::{App, bytes},
    conversation::Mode,
    recipe::{Recipe, Step},
};
use melissa_repository::Repository;
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};
impl App {
    fn environment_path(&self, workspace: &Path) -> Result<(PathBuf, String)> {
        let repo = Repository::open(
            &crate::app::absolute(workspace)?,
            Default::default(),
            vec![],
        )?;
        self.require_separate_workspace(&repo)?;
        let identity = repo.root_identity().to_owned();
        let dir = self.home.join("project-environments");
        crate::app::safe_path(&dir)?;
        Ok((
            dir.join(format!("{}.enc", blake3::hash(identity.as_bytes()))),
            identity,
        ))
    }
    pub fn project_environment(&self, workspace: &Path) -> Result<Environment> {
        let (path, identity) = self.environment_path(workspace)?;
        crate::app::safe_path(&path)?;
        if !path.exists() {
            return Ok(Environment::default());
        }
        let repo = Repository::open(path.parent().ok_or(Error::Io)?, Default::default(), vec![])?;
        let cipher = repo
            .read(path.file_name().and_then(|s| s.to_str()).ok_or(Error::Io)?)?
            .1;
        let plain = zeroize::Zeroizing::new(self.key.open(
            &cipher,
            format!("melissa.project-environment/v1:{identity}").as_bytes(),
        )?);
        let environment: Environment = crate::app::decode(&plain)?;
        environment.validate()?;
        Ok(environment)
    }
    pub fn save_environment(&self, workspace: &Path, environment: Environment) -> Result<Value> {
        use std::io::Write;
        environment.validate()?;
        let (path, identity) = self.environment_path(workspace)?;
        std::fs::create_dir_all(path.parent().ok_or(Error::Io)?)?;
        crate::app::safe_path(&path)?;
        let cipher = self.key.seal(
            &bytes(&environment)?,
            format!("melissa.project-environment/v1:{identity}").as_bytes(),
        )?;
        let temp = path.with_extension(format!("{}.tmp", crate::app::nonce()?));
        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temp)?;
        file.write_all(&cipher)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(temp, path)?;
        Ok(
            json!({"command":"project environment","environment":environment,"project_only":true,"global_defaults_changed":false,"runtime_preflight":"NOT_RUN"}),
        )
    }
    #[allow(clippy::too_many_arguments)] // Reviewed task preparation and explicit operator spending/runtime choices.
    pub fn create_project(
        &self,
        workspace: &Path,
        goal: &str,
        mode: Mode,
        plan: Value,
        mut contract: Contract,
        budget: u64,
        wall_time_ms: u64,
    ) -> Result<Value> {
        contract.validate()?;
        if mode == Mode::Plan {
            return self.create_conversation(workspace, goal, mode, plan);
        }
        let repo = Repository::open(workspace, Default::default(), vec![])?;
        if contract.extensions.is_none() {
            contract.extensions = self.freeze_extensions(&repo, goal)?;
        }
        contract.validate()?;
        let source = repo.snapshot(&std::sync::atomic::AtomicBool::new(false))?;
        if plan["source_digest"].as_str() != Some(source.digest.as_str()) {
            return Err(Error::Conflict(
                "Plan kaynağı değişti; amacı yeniden hazırla.",
            ));
        }
        let writes = if mode == Mode::Develop {
            plan["scope"]["proposed_writes"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        } else {
            BTreeSet::new()
        };
        let mut steps = plan["milestones"]
            .as_array()
            .ok_or(Error::Usage("Görev planı eksik."))?
            .iter()
            .map(|v| {
                Ok(Step {
                    profile: v["profile"].as_str().ok_or(Error::Store)?.into(),
                    goal: v["goal"].as_str().ok_or(Error::Store)?.into(),
                    write: mode == Mode::Develop && v["write"] == true,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        if steps.is_empty() {
            steps.push(Step {
                profile: "melissa.code.implementer".into(),
                goal: goal.into(),
                write: !writes.is_empty(),
            });
        }
        // A new repository can have no initial file paths. Keep a writer role
        // capable of later operator-approved scope; an empty repository write
        // allow-list itself grants no file access and never means wildcard.
        if mode == Mode::Develop
            && plan["write_intent"] == true
            && !steps.iter().any(|s| s.write)
            && let Some(step) = steps.iter_mut().find(|s| {
                !matches!(
                    s.profile.as_str(),
                    "melissa.code.repository-analyst"
                        | "melissa.code.architect"
                        | "melissa.code.reviewer"
                        | "melissa.code.security-reviewer"
                )
            })
        {
            step.write = true;
        }
        let recipe = Recipe {
            schema_version: "melissa.cli-recipe/v3".into(),
            agents: steps,
            writable_paths: writes,
            protected_paths: source
                .files
                .iter()
                .filter(|f| {
                    f.path
                        .rsplit('/')
                        .next()
                        .is_some_and(|p| p.eq_ignore_ascii_case("AGENTS.md"))
                })
                .map(|f| f.path.clone())
                .collect(),
            language: melissa_code_quality::protected::Language::Python,
            entry: String::new(),
            negative_workspace: PathBuf::new(),
            checks: vec![],
            development_checks: vec![],
            domain: None,
            project: Some(contract),
            candidate_schema: None,
            report_checks: vec![],
            source_ref: None,
            max_cost_micro_usd: budget,
            wall_time_ms,
        };
        let value = self.create_execution(workspace, goal, vec![], recipe)?;
        let id = value["task_id"].as_str().ok_or(Error::Store)?;
        self.attach_conversation(id, mode, plan)?;
        self.show(id)
    }
    pub fn project_contract(&self, workspace: &Path, environment: Environment) -> Result<Contract> {
        let repo = Repository::open(workspace, Default::default(), vec![])?;
        super::discovery::discover(&repo)?.contract(environment)
    }
}
impl App {
    /// Operator-native checks use the same immutable approved task. They end in
    /// a safe pause, not task completion; model execution can subsequently resume.
    pub async fn run_project_operation(
        &self,
        id: &str,
        operation: &str,
        command: Option<super::Command>,
    ) -> Result<Value> {
        let result = self
            .run_project_operation_inner(id, operation, command)
            .await;
        if result.is_err()
            && let Ok(mut store) = self.store(id)
        {
            let t = store.journal().task(id)?;
            let b = store.journal().model_budget(id)?;
            if t.record.state == melissa_core_contracts::TaskState::Running
                && t.record.unknown_invocation_id.is_none()
                && b.active_requests == 0
                && b.unknown_requests == 0
                && crate::engine::native_stop_proven(&store, id)?
            {
                store.transition_task(
                    id,
                    t.record.version,
                    melissa_core_contracts::TaskState::Blocked,
                    None,
                )?;
            }
        }
        result
    }
    async fn run_project_operation_inner(
        &self,
        id: &str,
        operation: &str,
        command: Option<super::Command>,
    ) -> Result<Value> {
        use melissa_core_contracts::TaskState;
        use std::sync::{Arc, atomic::AtomicBool};
        let mut store = self.store(id)?;
        let task = store.journal().task(id)?;
        let e = self.execution(&store, id)?.ok_or(Error::Denied(
            "Önce ortam/bütçe ile yürütülebilir proje görevi oluştur.",
        ))?;
        let project = e
            .recipe
            .project
            .as_ref()
            .ok_or(Error::Denied("Bu işlem genel proje sözleşmesi gerektirir."))?;
        if !["exec", "sync", "preflight"].contains(&operation) {
            return Err(Error::Usage("project exec|sync|preflight gerekli."));
        }
        if task.record.state != TaskState::Queued || crate::app::now()? >= e.expires_at_ms {
            return Err(Error::Conflict(
                "Operatör komutu henüz başlamamış, onaylı proje görevi gerektirir.",
            ));
        }
        let approval: Value = crate::app::decode(&store.get_artifact(id, crate::app::APPROVAL)?)?;
        let decision: Value = crate::app::decode(&store.get_artifact(
            id,
            &format!(
                "cli:decision:{}",
                approval["request_id"].as_str().ok_or(Error::Store)?
            ),
        )?)?;
        if decision["scope"] != "execute_recipe" {
            return Err(Error::Denied("Proje yürütmesi onaylı değil."));
        }
        let workspace = self.workspace(&store, id)?;
        let repo = self.source(&workspace)?;
        repo.validate_snapshot(&workspace.snapshot, &AtomicBool::new(false))?;
        let command = command.unwrap_or(super::Command {
            executable: "/bin/true".into(),
            argv: vec![],
            cwd: ".".into(),
        });
        command.validate()?;
        let profile = e
            .recipe
            .agents
            .iter()
            .find(|s| s.write)
            .or_else(|| {
                e.recipe
                    .agents
                    .iter()
                    .find(|s| s.profile == "melissa.code.test-engineer")
            })
            .ok_or(Error::Store)?
            .profile
            .clone();
        if operation == "exec"
            && !e
                .recipe
                .agents
                .iter()
                .any(|s| s.write && s.profile == profile)
        {
            return Err(Error::Denied(
                "İnceleme görevinde genel komut için yazıcı adımı yok; onaylı kontrolleri kullan.",
            ));
        }
        if !crate::views::image_present(&project.environment.image_id) {
            return Err(Error::Unavailable(
                "Seçilen proje image'i yerelde yok; süreç/API çağrısı başlatılmadı.",
            ));
        }
        let leased = store.transition_task(id, task.record.version, TaskState::Leased, None)?;
        store.transition_task(id, leased.record.version, TaskState::Running, None)?;
        let cancel = Arc::new(AtomicBool::new(false));
        let pause = Arc::new(AtomicBool::new(false));
        let control = crate::control::Control::new(self, id, cancel.clone(), pause.clone())?;
        let mut runtime =
            super::runtime::Runtime::new(self, id, &profile, project.clone(), e.expires_at_ms)?;
        let sid = runtime.start(
            self,
            &store,
            &repo,
            command,
            "",
            cancel.clone(),
            operation == "preflight",
            operation == "sync",
        )?;
        let result = loop {
            control.poll()?;
            if pause.load(std::sync::atomic::Ordering::Acquire) {
                let _ = self.project_input(id, &sid, "operator-pause", "cancel", "");
            }
            let state = store.journal().task(id)?;
            control.publish("agent",json!({"state":state.record.state,"version":state.record.version,"goal":state.contract.goal,"task_id":id}),json!({"project_session":sid}),serde_json::to_value(store.journal().model_budget(id)?).map_err(|_|Error::Store)?,json!({"total":store.journal().events(id)?.len()}))?;
            let poll = self.project_poll(id, &sid, 0, 1)?;
            if !matches!(
                poll["state"]["phase"].as_str(),
                Some("preparing" | "running")
            ) {
                break poll["state"]["result"].clone();
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        };
        runtime.close()?;
        if !crate::engine::native_stop_proven(&store, id)? {
            return Err(Error::Reconciliation(
                "Proje sürecinin duruşu kanıtlanmadı; eski kayıt korundu.",
            ));
        }
        let snapshot = repo.snapshot(&AtomicBool::new(false))?;
        crate::app::put(
            &mut store,
            id,
            &format!("cli:paused-source:{}", control.session),
            "paused.source",
            &bytes(&snapshot)?,
        )?;
        let t = store.journal().task(id)?;
        if cancel.load(std::sync::atomic::Ordering::Acquire) {
            let cancelled =
                store.transition_task(id, t.record.version, TaskState::Cancelled, None)?;
            control.publish("cancelled",json!({"task_id":id,"state":cancelled.record.state,"version":cancelled.record.version,"goal":t.contract.goal}),json!({"project_session":sid}),serde_json::to_value(store.journal().model_budget(id)?).map_err(|_|Error::Store)?,json!({"total":store.journal().events(id)?.len()}))?;
            return Ok(
                json!({"command":format!("project {operation}"),"task_id":id,"session_id":sid,"result":result,"state":"cancelled","model_calls":0,"task_accepted":false}),
            );
        }
        control.publish("paused",json!({"task_id":id,"state":t.record.state,"version":t.record.version,"goal":t.contract.goal}),json!({"project_session":sid}),serde_json::to_value(store.journal().model_budget(id)?).map_err(|_|Error::Store)?,json!({"total":store.journal().events(id)?.len()}))?;
        Ok(
            json!({"command":format!("project {operation}"),"task_id":id,"session_id":sid,"result":result,"state":"paused","resume":"task resume","model_calls":0,"task_accepted":false}),
        )
    }
}
pub fn effective_writes(app: &App, task: &str, recipe: &Recipe) -> Result<BTreeSet<String>> {
    let mut paths = recipe.writable_paths.clone();
    if recipe.project.is_some()
        && let Some(brief) = app.conversation(task)?
        && brief.runtime_scope_amendments
    {
        for path in brief.scope.writes {
            if recipe.protected_paths.iter().any(|p| {
                path.eq_ignore_ascii_case(p)
                    || path
                        .to_ascii_lowercase()
                        .starts_with(&(p.to_ascii_lowercase() + "/"))
            }) {
                return Err(Error::Denied("Ek kapsam korunan dosyayı kapsıyor."));
            }
            paths.insert(path);
        }
    }
    Ok(paths)
}
pub fn manifest(
    source: &melissa_code_quality::source::FrozenSource,
) -> Result<(std::collections::BTreeMap<String, Vec<u8>>, String)> {
    let raw = bytes(
        &json!({"schema_version":"melissa.project-source-manifest/v1","source":source.snapshot(),"kind":"source_manifest_not_installer","private_oracle":false}),
    )?;
    let files = std::collections::BTreeMap::from([("project-source-manifest.json".into(), raw)]);
    let hash = crate::digest(&bytes(&files)?);
    Ok((files, hash))
}
#[allow(clippy::too_many_arguments)]
pub fn verify(
    app: &App,
    store: &mut melissa_local_store::LocalStore,
    task: &melissa_core_contracts::wire::TaskContractV1,
    source: &melissa_code_quality::source::FrozenSource,
    repo: &Repository,
    contract: &Contract,
    expires: u64,
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> Result<Value> {
    let mut runtime = super::runtime::Runtime::new(
        app,
        &task.task_id,
        "melissa.code.test-engineer",
        contract.clone(),
        expires,
    )?;
    let mut results = vec![];
    for (name, command) in &contract.checks {
        source
            .current(repo, &cancel)
            .map_err(|_| Error::Conflict("Son proje kontrolünden önce kaynak değişti."))?;
        let sid = runtime.start(
            app,
            store,
            repo,
            command.clone(),
            "",
            cancel.clone(),
            false,
            false,
        )?;
        let result = super::runtime::Runtime::wait(app, &task.task_id, &sid, &cancel)?;
        results.push(json!({"name":name,"session_id":sid,"result":result}));
        if result["status"] != "PASS" {
            runtime.close()?;
            return Err(if result["status"] == "BLOCKED" {
                Error::Unavailable(
                    "Proje ortamı/kontrolü engellendi; gerçek AI başarısızlığı olarak gösterilmedi.",
                )
            } else {
                Error::Verification("Son proje komutu geçmedi; yerel süreç kayıtları korunuyor.")
            });
        }
        if result["source_digest"] != source.snapshot().digest
            || result["environment_digest"] != contract.environment.fingerprint()?
        {
            return Err(Error::Verification(
                "Proje kontrolünün kaynak/ortam bağı eşleşmedi.",
            ));
        }
    }
    runtime.close()?;
    source
        .current(repo, &cancel)
        .map_err(|_| Error::Conflict("Proje kontrolü sürerken kaynak değişti."))?;
    let receipt = json!({"schema_version":"melissa.project-verification/v1","acceptance_kind":"host_source_bound_project_checks","source_digest":source.snapshot().digest,"environment_digest":contract.environment.fingerprint()?,"checks":results,"private_oracle":false,"feature_coverage":"not_proven_by_command_exit_alone","release_acceptance":false});
    crate::app::put(
        store,
        &task.task_id,
        &format!("cli:project-verification:{}", crate::app::nonce()?),
        "project.verification",
        &bytes(&receipt)?,
    )?;
    Ok(receipt)
}
