use super::Spec;
use crate::{
    Error, Result,
    app::{App, bytes, decode, now, put},
    project::{Command, Contract, runtime::Runtime},
};
use melissa_local_store::LocalStore;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum Operation {
    Start,
    Status,
    Log {
        service: String,
        stream: String,
        offset: u64,
        max_bytes: u32,
    },
    Api {
        service: String,
        request: melissa_web_development::protocol::ApiRequest,
    },
    Database {
        command: melissa_web_development::protocol::Command,
    },
    Contract {
        name: String,
    },
    Migration {
        name: String,
    },
    Business {
        name: String,
    },
    Close,
}
impl Operation {
    pub fn historical(&self) -> bool {
        matches!(self, Self::Status | Self::Log { .. } | Self::Close)
    }
    pub fn validate(&self, spec: &Spec) -> Result {
        match self {
            Self::Log {
                service,
                stream,
                max_bytes,
                ..
            } if !(spec.services.contains_key(service)
                || (service == "database"
                    && stream == "stderr"
                    && spec.database.as_ref().is_some_and(|d| {
                        matches!(d.engine, melissa_web_development::plan::Database::Postgres)
                    })))
                || !["stdout", "stderr"].contains(&stream.as_str())
                || !(1..=32768).contains(max_bytes) =>
            {
                return Err(Error::Denied("Kayıtlı servis log sayfası gerekli."));
            }
            Self::Api { service, request } => {
                if !spec.services.contains_key(service) {
                    return Err(Error::Denied("API hedefi kayıtlı servis olmalı."));
                }
                request
                    .validate()
                    .map_err(|_| Error::Usage("Yerel API isteği geçersiz."))?;
            }
            Self::Database { command } => {
                command
                    .validate()
                    .map_err(|_| Error::Usage("SQL komutu geçersiz."))?;
                if spec.database.is_none()
                    || command.service()
                        != Some(melissa_web_development::protocol::Service::Database)
                    || matches!(
                        command,
                        melissa_web_development::protocol::Command::Migration { .. }
                    )
                {
                    return Err(Error::Denied(
                        "Yalnız kayıtlı özel DB komutu; migration için prova seç.",
                    ));
                }
                if matches!(command,melissa_web_development::protocol::Command::Execute{statement,..} if !spec.statements.contains_key(statement))
                    || matches!(command,melissa_web_development::protocol::Command::SqlParallel{scripts} if scripts.iter().flat_map(|s|&s.steps).any(|s|matches!(s,melissa_web_development::protocol::SqlStep::Execute{statement,..} if !spec.statements.contains_key(statement))))
                {
                    return Err(Error::Denied("SQL yazımı onaylı statement adı gerektirir."));
                }
            }
            Self::Contract { name } if !spec.api_checks.contains_key(name) => {
                return Err(Error::Denied("Bilinmeyen API sözleşme kontrolü."));
            }
            Self::Migration { name } if !spec.rehearsals.contains_key(name) => {
                return Err(Error::Denied("Bilinmeyen migration provası."));
            }
            Self::Business { name } if !spec.business_checks.contains(name) => {
                return Err(Error::Denied("Bilinmeyen iş kuralı kontrolü."));
            }
            _ => {}
        }
        Ok(())
    }
}
pub struct Host {
    pub home: PathBuf,
    pub task: String,
    pub profile: String,
    pub contract: Contract,
    pub expires: u64,
    pub store: LocalStore,
    pub(crate) final_check: bool,
    runtime: Runtime,
    session: Option<Session>,
}
struct Session {
    id: String,
    source: String,
    cursor: u64,
    sequence: u64,
    buffer: Vec<u8>,
}
impl Host {
    pub fn new(
        app: &App,
        store: &LocalStore,
        task: &str,
        profile: &str,
        contract: Contract,
        expires: u64,
    ) -> Result<Self> {
        let spec = contract
            .services
            .as_ref()
            .ok_or(Error::Denied("Görevde servis sözleşmesi yok."))?;
        spec.validate(&contract)?;
        Ok(Self {
            home: app.home.clone(),
            task: task.into(),
            profile: profile.into(),
            runtime: Runtime::new(app, task, profile, contract.clone(), expires)?
                .with_private_input()
                .with_creation_backend(spec.backend),
            contract,
            expires,
            store: store.connect()?,
            session: None,
            final_check: false,
        })
    }
    pub fn session_id(&self) -> Option<&str> {
        self.session.as_ref().map(|s| s.id.as_str())
    }
    pub fn active(&self) -> Result {
        let task = self.store.journal().task(&self.task)?;
        if !(task.record.state == melissa_core_contracts::TaskState::Running
            || (self.final_check
                && task.record.state == melissa_core_contracts::TaskState::Verifying))
            || task.record.unknown_invocation_id.is_some()
            || !task.contract.selected_profile_ids.contains(&self.profile)
            || task.contract.acceptance_contract_digest != crate::digest(&bytes(&self.contract)?)
            || now()? >= self.expires
        {
            return Err(Error::Denied(
                "Servis görev/profil/süre/sözleşme yetkisi etkin değil.",
            ));
        }
        Ok(())
    }
    pub fn start(&mut self, invocation: &str, cancel: Arc<AtomicBool>) -> Result<Value> {
        self.run(Operation::Start, None, invocation, cancel)
    }
    fn prepare(&mut self, app: &App, cancel: &Arc<AtomicBool>, invocation: &str) -> Result {
        if self.session.is_some() {
            return Err(Error::Conflict("Önce mevcut servis oturumunu kapat."));
        }
        let repo = app.source(&app.workspace(&self.store, &self.task)?)?;
        let source = repo.snapshot(cancel)?;
        let spec = self.contract.services.as_ref().unwrap();
        for path in spec.source_paths() {
            if !source.files.iter().any(|f| f.path == path) {
                return Err(Error::Denied(
                    "Kayıtlı servis/SQL/OpenAPI dosyası görünür kaynakta yok.",
                ));
            }
        }
        let snapshot = repo
            .worker_input_with_budget(&source, cancel, self.contract.environment.limits.disk_bytes)?
            .snapshot;
        let registration = json!({"app":null,"database":null,"seed_path":null,"migrations":{},"statements":{},"services":[]});
        let web_program = json!({"registration":registration,"source_digest":source.digest,"sources":melissa_container_executor::source_manifest::seal(&snapshot),"source_manifest_version":melissa_container_executor::source_manifest::VERSION});
        let options: BTreeMap<_, _> = self
            .contract
            .checks
            .keys()
            .map(|n| (n, crate::project_verification::check(&self.contract, n)))
            .collect();
        let files = BTreeMap::from([
            (
                ".melissa-extension/services/worker.py".into(),
                [
                    melissa_container_executor::source_manifest::VERIFIER,
                    include_str!("worker.py"),
                ]
                .join("\n")
                .into_bytes(),
            ),
            (
                ".melissa-extension/services/input.json".into(),
                bytes(
                    &json!({"spec":spec,"environment":self.contract.environment,"checks":self.contract.checks,"check_options":options,"source_digest":source.digest,"sources":melissa_container_executor::source_manifest::seal(&snapshot),"source_manifest_version":melissa_container_executor::source_manifest::VERSION,"max_bytes":self.contract.environment.limits.output_bytes}),
                )?,
            ),
            (
                ".melissa-extension/services/web.py".into(),
                [
                    melissa_container_executor::source_manifest::VERIFIER,
                    melissa_web_development::WORKER_LIBRARY,
                ]
                .join("\n")
                .into_bytes(),
            ),
            (
                ".melissa-extension/services/web_private/program.json".into(),
                bytes(&web_program)?,
            ),
            (
                ".melissa-extension/services/web_private/sql_runtime.py".into(),
                melissa_web_development::SQL_LIBRARY.as_bytes().to_vec(),
            ),
            (
                ".melissa-extension/verification/worker.py".into(),
                include_bytes!("../project_verification/worker.py").to_vec(),
            ),
        ]);
        let command = Command {
            executable: "/opt/melissa-python/bin/python".into(),
            argv: vec![
                "-I".into(),
                "-u".into(),
                ".melissa-extension/services/worker.py".into(),
            ],
            cwd: ".".into(),
        };
        let id = self.runtime.start_with_files(
            app,
            &self.store,
            &repo,
            command,
            invocation,
            cancel.clone(),
            false,
            false,
            files,
        )?;
        self.session = Some(Session {
            id,
            source: source.digest,
            cursor: 0,
            sequence: 0,
            buffer: vec![],
        });
        Ok(())
    }
    pub fn run(
        &mut self,
        operation: Operation,
        session: Option<&str>,
        invocation: &str,
        cancel: Arc<AtomicBool>,
    ) -> Result<Value> {
        self.active()?;
        operation.validate(self.contract.services.as_ref().unwrap())?;
        let app = App::open(&self.home)?;
        let repo = app.source(&app.workspace(&self.store, &self.task)?)?;
        let current_snapshot = repo.snapshot(&cancel)?;
        let current_source = &current_snapshot.digest;
        let input_digest = crate::digest(&bytes(
            &json!({"operation":operation,"session":session,"profile":self.profile}),
        )?);
        let receipt_id = format!("service:{}", blake3::hash(invocation.as_bytes()));
        if let Ok(raw) = self.store.get_artifact(&self.task, &receipt_id) {
            let result: Value = decode(&raw)?;
            if result["input_digest"] != input_digest
                || result["source_digest"] != current_source.as_str()
            {
                return Err(Error::Conflict(
                    "Servis invocation farklı girdiyle tekrar kullanıldı.",
                ));
            }
            if matches!(operation, Operation::Start)
                && self.session_id() != result["session_id"].as_str()
            {
                return Err(Error::Conflict(
                    "Eski start kaydı canlı servis oturumu yerine kullanılamaz.",
                ));
            }
            return Ok(result);
        }
        let intent = format!("{receipt_id}:intent");
        if self.store.artifact_metadata(&self.task, &intent).is_ok() {
            return Err(Error::Reconciliation(
                "Önceki servis komutunun sonucu eksik; otomatik tekrar yok.",
            ));
        }
        if matches!(operation, Operation::Start) {
            if session.is_some() || self.session.is_some() {
                return Err(Error::Conflict(
                    "Başlatmadan önce mevcut servis oturumunu kapat.",
                ));
            }
            for path in self.contract.services.as_ref().unwrap().source_paths() {
                if !current_snapshot.files.iter().any(|f| f.path == path) {
                    return Err(Error::Denied(
                        "Servis girdisi görünür kaynakta yok; süreç başlatılmadı.",
                    ));
                }
            }
        } else if session.is_none() || session != self.session_id() {
            return Err(Error::Denied("Yabancı/eksik servis oturumu."));
        }
        if !operation.historical()
            && self.session.as_ref().is_some_and(|s| {
                repo.snapshot(&cancel)
                    .map_or(true, |v| v.digest != s.source)
            })
        {
            return Err(Error::Conflict(
                "Servis kaynağı değişti; oturumu kapatıp güncel kaynakla aç.",
            ));
        }
        put(
            &mut self.store,
            &self.task,
            &intent,
            "services.intent",
            &bytes(&json!({"input_digest":input_digest,"operation":operation,"session":session}))?,
        )?;
        if matches!(operation, Operation::Start) {
            self.prepare(&app, &cancel, invocation)?;
        }
        let active = self
            .session
            .as_mut()
            .ok_or(Error::Conflict("Servis oturumu yok."))?;
        active.sequence += 1;
        let seq = active.sequence;
        let sid = active.id.clone();
        let source = active.source.clone();
        let mut raw = bytes(&json!({"seq":seq,"operation":operation}))?;
        raw.push(b'\n');
        for (part, data) in raw.chunks(30000).enumerate() {
            let hex: String = data.iter().map(|b| format!("{b:02x}")).collect();
            app.project_private_input(
                &self.task,
                &sid,
                &format!("service:{invocation}:{part}"),
                "stdin",
                &hex,
            )?;
        }
        let reply = loop {
            if cancel.load(Ordering::Acquire) || now()? >= self.expires {
                self.close()?;
                return Err(Error::Reconciliation(
                    "Servis komutu yanıtlanmadan iptal/süre sonu; sonuç uydurulmadı.",
                ));
            }
            let s = self.session.as_mut().unwrap();
            let page = app.project_poll(&self.task, &sid, s.cursor, 256 * 1024)?;
            if page["owner_lost"] == true {
                return Err(Error::Reconciliation("Servis oturum sahibi kayboldu."));
            }
            for event in page["events"].as_array().ok_or(Error::Store)? {
                let value = &event["value"];
                if value["event"] == "stdout" {
                    let hex = value["hex"].as_str().ok_or(Error::Store)?;
                    for i in (0..hex.len()).step_by(2) {
                        s.buffer.push(
                            u8::from_str_radix(&hex[i..i + 2], 16).map_err(|_| Error::Store)?,
                        );
                    }
                }
            }
            s.cursor = page["next_cursor"].as_u64().ok_or(Error::Store)?;
            if s.buffer.len() as u64 > self.contract.environment.limits.output_bytes {
                return Err(Error::Reconciliation(
                    "Servis yanıtı çıktı kapasitesini aştı.",
                ));
            }
            if let Some(end) = s.buffer.iter().position(|b| *b == b'\n') {
                let line: Vec<_> = s.buffer.drain(..=end).collect();
                let value: Value = decode(&line)?;
                if value["event"] != "service_reply"
                    || value["seq"] != seq
                    || value["source_digest"] != source
                {
                    return Err(Error::Reconciliation(
                        "Servis yanıt kimliği/sırası/kaynak bağı geçersiz.",
                    ));
                }
                break value;
            }
            if !matches!(
                page["state"]["phase"].as_str(),
                Some("preparing" | "running")
            ) {
                let native = &page["state"]["result"];
                if native["status"] == "BLOCKED"
                    && native["command_started"] == false
                    && native["tree_confirmed_stopped"] == true
                    && native["requires_reconciliation"] == false
                {
                    break json!({"outcome":"BLOCKED","reason":"selected_service_sdk_preflight_blocked","body":{"native_result":native,"command_started":false},"requires_reconciliation":false});
                }

                return Err(Error::Reconciliation(
                    "Servis yanıtı gelmeden native süreç bitti; kayıtları incele.",
                ));
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        let detail = format!("{receipt_id}:detail");
        put(
            &mut self.store,
            &self.task,
            &detail,
            "services.detail",
            &bytes(&reply)?,
        )?;
        let body = summary(reply["body"].clone());
        let mut result = json!({"receipt_id":receipt_id,"session_id":sid,"source_digest":source,"source_current":repo.snapshot(&AtomicBool::new(false))?.digest==source,"worker_source_current":reply["worker_source_current"],"environment_digest":self.contract.environment.fingerprint()?,"input_digest":input_digest,"outcome":reply["outcome"],"reason":reply["reason"],"body":body,"detail_artifact":detail,"requires_reconciliation":reply["requires_reconciliation"],"model_calls":0,"task_accepted":false,"evidence_class":"public_project_service_observation","host_ports_published":false});
        if matches!(operation, Operation::Close) {
            let native = Runtime::wait(&app, &self.task, &sid, &cancel)?;
            self.runtime.close()?;
            result["native_result"] = native.clone();
            if native["tree_confirmed_stopped"] != true || native["requires_reconciliation"] == true
            {
                result["outcome"] = json!("INCONCLUSIVE");
                result["requires_reconciliation"] = json!(true);
            }
            self.session = None;
        }
        if result["outcome"] == "BLOCKED" && result["body"]["command_started"] == false {
            self.runtime.close()?;
            self.session = None;
        }
        put(
            &mut self.store,
            &self.task,
            &receipt_id,
            "services.receipt",
            &bytes(&result)?,
        )?;
        Ok(result)
    }
    pub fn close(&mut self) -> Result {
        self.runtime.close()?;
        self.session = None;
        Ok(())
    }
}

fn summary(mut value: Value) -> Value {
    match &mut value {
        Value::Object(map) => {
            map.retain(|key, _| !key.ends_with("_hex"));
            for item in map.values_mut() {
                *item = summary(std::mem::take(item));
            }
        }
        Value::Array(items) => {
            for item in items {
                *item = summary(std::mem::take(item));
            }
        }
        _ => {}
    }
    value
}
