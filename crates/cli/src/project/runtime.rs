//! Owned long-running snapshot processes; native broker receipts stay authoritative.
use super::{
    Command, Contract,
    disk::{Disk, State},
};
use crate::{
    Error, Result,
    app::{App, bytes, nonce, now},
    control::ProcessIdentity,
};
use melissa_container_executor::{ContainerRequest, Conversation, Snapshot, TerminationReason};
use melissa_execution_broker::{
    InvocationIdentity,
    container_broker::ContainerBroker,
    container_policy::container_execution_digest,
    policy::{
        AuthorityGrant, CpuLimitMode, DiskLimitMode, PermitRequest, ProcessLimits,
        authenticated_digest,
    },
};
use melissa_local_store::LocalStore;
use melissa_repository::Repository;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};
pub struct Runtime {
    home: PathBuf,
    task: String,
    profile: String,
    contract: Contract,
    expires: u64,
    private_input: bool,
    creation_backend: melissa_container_executor::CreationBackend,
    workers: Vec<(String, Arc<AtomicBool>, thread::JoinHandle<()>)>,
}
impl Runtime {
    pub fn new(
        app: &App,
        task: &str,
        profile: &str,
        contract: Contract,
        expires: u64,
    ) -> Result<Self> {
        contract.validate()?;
        Ok(Self {
            home: app.home.clone(),
            task: task.into(),
            profile: profile.into(),
            contract,
            expires,
            private_input: false,
            creation_backend: Default::default(),
            workers: vec![],
        })
    }
    pub(crate) fn with_creation_backend(
        mut self,
        backend: melissa_container_executor::CreationBackend,
    ) -> Self {
        self.creation_backend = backend;
        self
    }
    pub(crate) fn with_private_input(mut self) -> Self {
        self.private_input = true;
        self
    }
    #[allow(clippy::too_many_arguments)] // One source/task/permit/IO binding at the trusted host boundary.
    pub fn start(
        &mut self,
        app: &App,
        store: &LocalStore,
        repo: &Repository,
        command: Command,
        ctx_invocation: &str,
        cancel: Arc<AtomicBool>,
        preflight_only: bool,
        sync_only: bool,
    ) -> Result<String> {
        self.start_with_files(
            app,
            store,
            repo,
            command,
            ctx_invocation,
            cancel,
            preflight_only,
            sync_only,
            BTreeMap::new(),
        )
    }
    #[allow(clippy::too_many_arguments)] // Exact approved snapshot supplement, never a host mount.
    pub fn start_with_files(
        &mut self,
        app: &App,
        store: &LocalStore,
        repo: &Repository,
        command: Command,
        ctx_invocation: &str,
        cancel: Arc<AtomicBool>,
        preflight_only: bool,
        sync_only: bool,
        extra: BTreeMap<String, Vec<u8>>,
    ) -> Result<String> {
        command.validate()?;
        self.contract.validate()?;
        if now()? >= self.expires || cancel.load(Ordering::Acquire) {
            return Err(Error::Conflict(
                "Görev süresi doldu veya iptal istendi; süreç başlatılmadı.",
            ));
        }
        let source = repo.snapshot(&AtomicBool::new(false))?;
        if !extra.is_empty()
            && (source.files.iter().any(|f| {
                f.path == ".melissa-extension" || f.path.starts_with(".melissa-extension/")
            }) || extra
                .keys()
                .any(|p| !p.starts_with(".melissa-extension/") || !melissa_workflow::plan::path(p)))
        {
            return Err(Error::Denied(
                "Eklenti snapshot alanı çakışıyor veya geçersiz.",
            ));
        }
        if source
            .files
            .iter()
            .any(|f| f.path.starts_with(".melissa-project/") || f.path == ".melissa-project")
        {
            return Err(Error::Denied(
                ".melissa-project alanı host çalışma protokolü için ayrılmıştır.",
            ));
        }
        let discovery = super::discovery::discover(repo)?;
        let program = json!({"environment":self.contract.environment,"command":command,"required_commands":self.contract.checks.values().chain(self.contract.services.iter().flat_map(|s|s.services.values().map(|s|&s.command))).collect::<Vec<_>>(),"constraints":discovery.constraints,"preflight_only":preflight_only,"sync":self.contract.sync});
        let program = if let Some(services) = &self.contract.services {
            let mut program = program;
            program["service_requirements"] = json!({"browser":services.browser.is_some(),"axe":services.browser.is_some(),"visual":services.browser.as_ref().is_some_and(|b|b.checks.values().any(|c|!c.visual.is_empty())),"devtools":services.browser.as_ref().is_some_and(|b|b.devtools),"contract":!services.api_checks.is_empty(),"postgres":services.database.as_ref().is_some_and(|d|matches!(d.engine,melissa_web_development::plan::Database::Postgres))||services.rehearsals.values().any(|r|matches!(r.engine,melissa_web_development::plan::Database::Postgres))});
            program
        } else {
            program
        };
        // environment-sync uses the same session and returns after its last sync.
        let program = if sync_only {
            let mut p = program;
            p["command"] = json!({"executable":"/bin/true","argv":[],"cwd":"."});
            p
        } else {
            program
        };
        let encoded = bytes(&program)?;
        Snapshot::preflight(
            source
                .files
                .iter()
                .map(|f| (f.path.as_str(), f.bytes))
                .chain(extra.iter().map(|(p, v)| (p.as_str(), v.len() as u64)))
                .chain([
                    (".melissa-project/program.json", encoded.len() as u64),
                    (
                        ".melissa-project/worker.py",
                        include_bytes!("worker.py").len() as u64,
                    ),
                ]),
            self.contract.environment.limits.disk_bytes,
        )
        .map_err(|_| {
            Error::Unavailable(
                "Proje aktarımı worker dosya/byte/disk kapasitesini aşıyor; süreç başlatılmadı.",
            )
        })?;
        let input = repo.worker_input_with_budget(
            &source,
            &cancel,
            self.contract.environment.limits.disk_bytes,
        )?;
        let materialization = input.materialization;
        let mut files = input.snapshot.files;
        files.extend(extra);
        files.insert(".melissa-project/program.json".into(), encoded);
        files.insert(
            ".melissa-project/worker.py".into(),
            include_bytes!("worker.py").to_vec(),
        );
        let snapshot = Snapshot { files };
        let mut limits = self.contract.environment.limits.clone();
        limits.wall_time_ms = limits.wall_time_ms.min(self.expires.saturating_sub(now()?));
        limits.cpu_time_ms = limits.cpu_time_ms.min(limits.wall_time_ms);
        limits.export_bytes = 0;
        let request = ContainerRequest {
            backend: self.creation_backend,
            schema_version: "melissa.container-exec/v1".into(),
            image_id: self.contract.environment.image_id.clone(),
            executable: "/usr/bin/python3".into(),
            argv: vec!["-u".into(), ".melissa-project/worker.py".into()],
            cwd: ".".into(),
            env: BTreeMap::new(),
            limits,
        };
        request
            .validate()
            .map_err(|_| Error::Usage("Proje worker sınırları geçersiz."))?;
        snapshot.validate(request.limits.disk_bytes).map_err(|_| {
            Error::Unavailable("Proje aktarımı dosya/byte veya worker disk kapasitesini aşıyor.")
        })?;
        repo.validate_snapshot(&source, &AtomicBool::new(false))?;
        let task = store.journal().task(&self.task)?.contract;
        let profile = &self.profile;
        let p = crate::resources::catalog()?
            .profiles
            .get(profile)
            .cloned()
            .ok_or(Error::Store)?;
        if !p.tool_ids.iter().any(|s| s == "process.exec")
            || !task.selected_profile_ids.contains(profile)
        {
            return Err(Error::Denied("Uzmanın süreç başlatma yetkisi yok."));
        }
        let issued = now()?;
        if issued >= self.expires {
            return Err(Error::Conflict(
                "Proje hazırlığı sırasında görev süresi doldu; süreç başlatılmadı.",
            ));
        }
        let expiry = issued
            .checked_add(request.limits.wall_time_ms)
            .ok_or(Error::Store)?
            .min(self.expires);
        let mut policy = crate::development::policy();
        policy.revision = "project-snapshot-process-v1".into();
        policy.max_permit_lifetime_ms = expiry - issued;
        policy.ceiling = ProcessLimits {
            wall_time_ms: request.limits.wall_time_ms,
            cpu_time_ms: request.limits.cpu_time_ms,
            cpu_rate_basis_points: request.limits.cpu_rate_basis_points as u16,
            cpu_mode: CpuLimitMode::MonitoredTotal,
            memory_bytes: request.limits.memory_bytes,
            disk_bytes: request.limits.disk_bytes,
            disk_mode: DiskLimitMode::Strict,
            output_bytes: request.limits.output_bytes,
            max_processes: request.limits.max_processes,
            network: melissa_execution_broker::policy::NetworkPolicy::Deny,
        };
        let auth = task.authorization_refs.first().ok_or(Error::Store)?.clone();
        policy.grants.insert(
            auth.clone(),
            AuthorityGrant {
                authorization_ref: auth.clone(),
                task_id: self.task.clone(),
                task_digest: authenticated_digest(&task).map_err(|_| Error::Store)?,
                principal_id: "cli:operator".into(),
                profile_id: profile.clone(),
                resource_id: task.workspace_ref.clone(),
                process_digest: container_execution_digest(
                    &request,
                    &snapshot.digest().map_err(|_| Error::Store)?,
                )
                .map_err(|_| Error::Store)?,
                allow_monitored_disk: false,
                allow_monitored_cpu: true,
                allowed_effects: BTreeSet::from(["process.execute_scoped".into()]),
                not_before_unix_ms: issued,
                expires_at_unix_ms: expiry,
            },
        );
        let mut broker = ContainerBroker::new(
            store.connect()?,
            crate::resources::catalog()?,
            task.workspace_ref.clone(),
            policy,
        )
        .map_err(|_| Error::Denied("Proje komutu broker yetki denetiminden geçmedi."))?;
        let sid = nonce()?;
        let signed = broker
            .issue_permit_mode(
                &self.task,
                request.clone(),
                &snapshot,
                PermitRequest {
                    permit_id: format!("project:{sid}"),
                    nonce: nonce()?,
                    principal_id: "cli:operator".into(),
                    profile_id: profile.clone(),
                    resource_id: task.workspace_ref,
                    authorization_ref: auth,
                    not_before_unix_ms: issued,
                    expires_at_unix_ms: expiry,
                },
                true,
            )
            .map_err(|_| Error::Denied("Proje komut izni verilmedi; süreç çalıştırılmadı."))?;
        let invocation = if ctx_invocation.is_empty() {
            format!("project:{sid}")
        } else {
            ctx_invocation.into()
        };
        let identity = InvocationIdentity {
            invocation_id: invocation.clone(),
            attempt_id: format!("project-attempt:{sid}"),
            generation: 1,
            idempotency_key: None,
        };
        let state = State {
            private_input: self.private_input,
            expires_at_ms: self.expires,
            task_id: self.task.clone(),
            session_id: sid.clone(),
            invocation_id: invocation,
            source_digest: source.digest,
            environment_digest: self.contract.environment.fingerprint()?,
            owner: ProcessIdentity::current()?,
            phase: "preparing".into(),
            events: 0,
            chain: String::new(),
            controls: 0,
            command_started: false,
            stdout_bytes: 0,
            stderr_bytes: 0,
            result: None,
        };
        let disk = Disk::open(app, &self.task, &sid, true)?;
        disk.write("state.enc", &state, false)?;
        disk.event(json!({"event":"prepared","command":command,"environment":self.contract.environment,"source_digest":state.source_digest,"snapshot_digest":snapshot.digest().map_err(|_|Error::Store)?,"authority":"snapshot_only_no_host_writeback","materialization":materialization}))?;
        drop(disk);
        let local_cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = local_cancel.clone();
        let home = self.home.clone();
        let tid = self.task.clone();
        let session = sid.clone();
        let owner=thread::Builder::new().name(format!("project-{}",&sid[..8])).spawn(move||{
            let app = match App::open(&home) {
                Ok(v) => v,
                Err(_) => return,
            };
            let (input_tx, input_rx) = mpsc::sync_channel::<Vec<u8>>(128);
            let (events_tx, events_rx) = mpsc::sync_channel(
                (request.limits.output_bytes as usize / 8192).max(32) + 32,
            );
            let (done_tx, done_rx) = mpsc::channel();
            let native_cancel = worker_cancel.clone();
            let task = tid.clone();
            let native = thread::spawn(move || {
                let result = broker.execute_conversation(
                    &task, &signed, &snapshot, &identity, &native_cancel,
                    Conversation { input: input_rx, output: events_tx },
                );
                let _ = done_tx.send(result);
            });
            let mut control_cursor = 0;
            let mut collector_error = false;
            let mut blocked = None;
            let result = loop {
                if cancel.load(Ordering::Acquire) {
                    worker_cancel.store(true, Ordering::Release);
                }
                while let Ok(raw) = events_rx.try_recv() {
                    let event: Value = match crate::app::decode(&raw) {
                        Ok(v) => v,
                        Err(_) => {
                            collector_error = true;
                            worker_cancel.store(true, Ordering::Release);
                            continue;
                        }
                    };
                    if event["event"] == "blocked" { blocked = Some(event.clone()); }
                    let recorded = Disk::open(&app, &tid, &session, true).and_then(|d| d.event(event));
                    if recorded.is_err() {
                        collector_error = true;
                        worker_cancel.store(true, Ordering::Release);
                    }
                }
                let controls = (|| -> Result {
                    let d = Disk::open(&app, &tid, &session, false)?;
                    let s = d.state()?;
                    while control_cursor < s.controls {
                        let c: Value = d.read(&format!("control-{control_cursor:020}.enc"))?;
                        if c["operation"] == "cancel" {
                            worker_cancel.store(true, Ordering::Release);
                        } else {
                            let mut raw = serde_json::to_vec(&json!({
                                "operation": c["operation"], "hex": c["hex"], "request_id": c["request_id"]
                            })).map_err(|_| Error::Store)?;
                            raw.push(b'\n');
                            input_tx.try_send(raw).map_err(|_| Error::Unavailable(
                                "Süreç stdin kuyruğu doldu/kapandı; giriş teslimi doğrulanmadı."
                            ))?;
                        }
                        control_cursor += 1;
                    }
                    Ok(())
                })();
                if controls.is_err() {
                    collector_error = true;
                    worker_cancel.store(true, Ordering::Release);
                }
                match done_rx.try_recv() {
                    Ok(v) => break v,
                    Err(mpsc::TryRecvError::Disconnected) => break Err(
                        melissa_execution_broker::BrokerError::Denied("native session owner stopped without outcome".into())
                    ),
                    Err(_) => thread::sleep(Duration::from_millis(10)),
                }
            };
            let _ = native.join();
            // Drain every final frame before publishing native completion.
            while let Ok(raw)=events_rx.try_recv() {
                match crate::app::decode::<Value>(&raw) {
                    Ok(v)=> {
                        if v["event"]=="blocked" {blocked=Some(v.clone());}
                        if Disk::open(&app,&tid,&session,true).and_then(|d|d.event(v)).is_err(){collector_error=true;}
                    },
                    Err(_)=>collector_error=true
                }
            }
            let finish=(||->Result {
                let d = Disk::open(&app, &tid, &session, true)?;
                let mut state = d.state()?;
                let value=match result {
                    Ok(native)=> {
                        let report = native.report.as_ref();
                        let not_started = report.is_none()
                            && native.observation.effect_status == melissa_core_contracts::wire::EffectStatus::NotStarted
                            && !native.requires_reconciliation;
                        let stopped = not_started || report.is_some_and(|r|
                            r.cleanup_confirmed && r.tree_confirmed_stopped && r.policy_verified
                        );
                        let success = !collector_error
                            && native.observation.status == melissa_core_contracts::wire::ObservationStatus::Success
                            && report.is_some_and(|r| r.exit_code == Some(0) && r.termination == TerminationReason::Exited)
                            && stopped;
                        let status = if native.requires_reconciliation || !stopped || collector_error {
                            "INCONCLUSIVE"
                        } else if success {
                            "PASS"
                        } else if (blocked.is_some() && !state.command_started) || not_started
                            || report.is_some_and(|r| r.termination == TerminationReason::Cancelled) {
                            "BLOCKED"
                        } else {
                            "FAIL"
                        };
                        json!({"status":status,"observation":native.observation,"result_artifact_id":native.result_artifact_id,"requires_reconciliation":native.requires_reconciliation || !stopped || collector_error,"container_id":report.map(|r|&r.container_id),"exit_code":report.and_then(|r|r.exit_code),"termination":report.map(|r|r.termination),"tree_confirmed_stopped":stopped,"blocked":blocked,"source_digest":state.source_digest,"environment_digest":state.environment_digest,"command_started":state.command_started,"private_oracle":false,"task_accepted":false})},
                    Err(error)=>json!({"status":"INCONCLUSIVE","requires_reconciliation":true,"diagnostic":error.to_string(),"task_accepted":false})
                };
                state.phase = match value["status"].as_str() {
                    Some("PASS") => "completed", Some("BLOCKED") => "blocked",
                    Some("FAIL") => "failed", _ => "unknown",
                }.into();
                state.result = Some(value);
                d.write("state.enc", &state, true)
            })();
            if finish.is_err() { worker_cancel.store(true, Ordering::Release); }
        })?;
        self.workers.push((sid.clone(), local_cancel, owner));
        Ok(sid)
    }
    pub fn close(&mut self) -> Result {
        for (_, cancel, _) in &self.workers {
            cancel.store(true, Ordering::Release);
        }
        let mut failed = false;
        for (_, _, worker) in self.workers.drain(..) {
            failed |= worker.join().is_err();
        }
        if failed {
            Err(Error::Reconciliation(
                "Süreç sahibi güvenli kapanış sonucu bırakmadı.",
            ))
        } else {
            Ok(())
        }
    }
    pub fn wait(app: &App, task: &str, session: &str, cancel: &AtomicBool) -> Result<Value> {
        loop {
            let poll = app.project_poll(task, session, 0, 1)?;
            if poll["owner_lost"] == true {
                return Err(Error::Reconciliation(
                    "Süreç sahibi kayboldu; sonuç uydurulmadı.",
                ));
            }
            if !matches!(
                poll["state"]["phase"].as_str(),
                Some("preparing" | "running")
            ) {
                return Ok(poll["state"]["result"].clone());
            }
            if cancel.load(Ordering::Acquire) {
                let _ = app.project_input(task, session, "host-wait-cancel", "cancel", "");
            }
            thread::sleep(Duration::from_millis(20));
        }
    }
}
impl Drop for Runtime {
    fn drop(&mut self) {
        let _ = self.close();
    }
}
