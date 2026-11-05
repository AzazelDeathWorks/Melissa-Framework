//! Authenticated process mailbox. Reads work while the task CAS is owned.
use crate::{
    Error, Result,
    app::{App, bytes, decode, nonce, safe_path, validate_id},
    control::ProcessIdentity,
    digest,
};
use melissa_local_store::StoreKey;
use melissa_repository::Repository;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{fs, io::Write, path::PathBuf};
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct State {
    #[serde(default = "no_deadline")]
    pub expires_at_ms: u64,
    pub task_id: String,
    pub session_id: String,
    pub invocation_id: String,
    pub source_digest: String,
    pub environment_digest: String,
    pub owner: ProcessIdentity,
    pub phase: String,
    pub events: u64,
    pub chain: String,
    pub controls: u64,
    pub command_started: bool,
    pub stdout_bytes: u64,
    pub stderr_bytes: u64,
    pub result: Option<Value>,
}
fn no_deadline() -> u64 {
    u64::MAX
}
pub(crate) struct Disk {
    root: PathBuf,
    key: StoreKey,
    task: String,
    session: String,
    _pin: Repository,
    _lock: fs::File,
}
impl Disk {
    pub fn open(app: &App, task: &str, session: &str, writer: bool) -> Result<Self> {
        validate_id(task)?;
        if session.len() != 32 || !session.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(Error::Usage("Süreç oturumu kimliği geçersiz."));
        }
        let task_root = app.home.join("tasks").join(task);
        safe_path(&task_root)?;
        if !task_root.join("store/store.sqlite3").is_file() {
            return Err(Error::Usage("Görev bulunamadı."));
        }
        let root = task_root.join("project-sessions").join(session);
        safe_path(&root)?;
        if writer {
            fs::create_dir_all(&root)?;
        }
        let pin = Repository::open(&root, Default::default(), vec![])?;
        let lockpath = root.join("writer.lock");
        safe_path(&lockpath)?;
        let lock = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(writer)
            .truncate(false)
            .open(lockpath)?;
        if writer {
            lock.lock()?;
        } else {
            lock.lock_shared()?;
        }
        Ok(Self {
            root,
            key: app.key.clone(),
            task: task.into(),
            session: session.into(),
            _pin: pin,
            _lock: lock,
        })
    }
    fn aad(&self, name: &str) -> String {
        format!(
            "melissa.project-session/v1:{}:{}:{name}",
            self.task, self.session
        )
    }
    pub fn read<T: serde::de::DeserializeOwned>(&self, name: &str) -> Result<T> {
        let cipher = self._pin.read(name)?.1;
        let plain = zeroize::Zeroizing::new(self.key.open(&cipher, self.aad(name).as_bytes())?);
        decode(&plain)
    }
    pub fn write(&self, name: &str, value: &impl Serialize, replace: bool) -> Result {
        let plain = zeroize::Zeroizing::new(bytes(value)?);
        let _: Value = decode(&plain)?;
        let cipher = self.key.seal(&plain, self.aad(name).as_bytes())?;
        if cipher.len() as u64 > melissa_repository::Limits::default().max_file_bytes {
            return Err(Error::Unavailable(
                "Süreç kaydı host dosya zarfına sığmadı.",
            ));
        }
        let path = self.root.join(name);
        safe_path(&path)?;
        let temporary = if replace {
            path.with_extension(format!("{}.tmp", nonce()?))
        } else {
            path.clone()
        };
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(&cipher)?;
        file.sync_all()?;
        drop(file);
        if replace {
            fs::rename(temporary, path)?;
        }
        Ok(())
    }
    pub fn state(&self) -> Result<State> {
        let state: State = self.read("state.enc")?;
        if state.task_id != self.task || state.session_id != self.session {
            return Err(Error::Store);
        }
        Ok(state)
    }
    pub fn event(&self, value: Value) -> Result<State> {
        let mut s = self.state()?;
        let record = json!({"sequence":s.events,"previous":s.chain,"value":value});
        self.write(&format!("event-{:020}.enc", s.events), &record, false)?;
        s.events += 1;
        s.chain = digest(&bytes(&record)?);
        if value["event"] == "started" && value["stage"] == "command" {
            s.command_started = true;
            s.phase = "running".into();
        }
        if let Some(hex) = value["hex"].as_str() {
            match value["event"].as_str() {
                Some("stdout") => s.stdout_bytes += hex.len() as u64 / 2,
                Some("stderr") => s.stderr_bytes += hex.len() as u64 / 2,
                _ => {}
            }
        }
        self.write("state.enc", &s, true)?;
        Ok(s)
    }
}
impl App {
    pub fn project_poll(
        &self,
        task: &str,
        session: &str,
        after: u64,
        max_bytes: usize,
    ) -> Result<Value> {
        if !(1..=256 * 1024).contains(&max_bytes) {
            return Err(Error::Usage(
                "Çıktı sayfası 1..262144 byte olmalı; kalan çıktı sonraki sayfada kalır.",
            ));
        }
        let d = Disk::open(self, task, session, false)?;
        let s = d.state()?;
        if after > s.events {
            return Err(Error::Usage("Çıktı imleci oturumun ilerisinde."));
        }
        let mut events = vec![];
        let mut cursor = after;
        let mut size = 0;
        let mut previous = if after == 0 {
            String::new()
        } else {
            let e: Value = d.read(&format!("event-{:020}.enc", after - 1))?;
            digest(&bytes(&e)?)
        };
        while cursor < s.events {
            let event: Value = d.read(&format!("event-{cursor:020}.enc"))?;
            if event["sequence"] != cursor || event["previous"] != previous {
                return Err(Error::Store);
            }
            let len = bytes(&event)?.len();
            if size + len > max_bytes && !events.is_empty() {
                break;
            }
            previous = digest(&bytes(&event)?);
            size += len;
            events.push(event);
            cursor += 1;
        }
        if cursor == s.events && previous != s.chain {
            return Err(Error::Store);
        }
        let active = matches!(s.phase.as_str(), "preparing" | "running");
        let lost = active && (!s.owner.alive() || crate::app::now()? >= s.expires_at_ms);
        Ok(
            json!({"command":"project poll","task_id":task,"session_id":session,"state":s,"events":events,"next_cursor":cursor,"has_more":cursor<s.events,"owner_lost":lost,"requires_reconciliation":lost,"execution_status":if lost || (active && s.command_started){"INCONCLUSIVE"}else if active{"NOT_RUN"}else{s.result.as_ref().and_then(|r|r["status"].as_str()).unwrap_or("INCONCLUSIVE")},"task_accepted":false}),
        )
    }
    pub fn project_input(
        &self,
        task: &str,
        session: &str,
        request_id: &str,
        operation: &str,
        hex: &str,
    ) -> Result<Value> {
        if !matches!(operation, "stdin" | "eof" | "cancel")
            || hex.len() > 60000
            || !hex.len().is_multiple_of(2)
            || !hex.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err(Error::Usage(
                "Süreç girişi geçersiz; stdin bir çerçevede en fazla 30000 byte.",
            ));
        }
        if request_id.is_empty() || request_id.len() > 256 {
            return Err(Error::Usage("Giriş isteği kimliği gerekli."));
        }
        let d = Disk::open(self, task, session, true)?;
        let mut s = d.state()?;
        if operation == "cancel"
            && !matches!(s.phase.as_str(), "preparing" | "running")
            && s.result
                .as_ref()
                .is_some_and(|r| r["tree_confirmed_stopped"] == true)
        {
            return Ok(
                json!({"request_id":request_id,"already_stopped":true,"process_stopped":true,"new_control_recorded":false}),
            );
        }
        if !matches!(s.phase.as_str(), "preparing" | "running") || !s.owner.alive() {
            return Err(Error::Conflict(
                "Canlı sahip doğrulanmadı; giriş/iptal gönderilmedi. Eski sonucu veya uzlaştırma kaydını incele.",
            ));
        }
        let once = format!("request-{}.enc", blake3::hash(request_id.as_bytes()));
        let value =
            json!({"request_id":request_id,"operation":operation,"hex":hex,"sequence":s.controls});
        if d.root.join(&once).exists() {
            let previous: Value = d.read(&once)?;
            if previous["operation"] != operation || previous["hex"] != hex {
                return Err(Error::Conflict("Giriş kimliği farklı içerikle kullanıldı."));
            }
            return Ok(
                json!({"request_id":request_id,"duplicate":true,"delivered":false,"note":"Already recorded; not enqueued twice."}),
            );
        }
        d.write(&format!("control-{:020}.enc", s.controls), &value, false)?;
        d.write(&once, &value, false)?;
        s.controls += 1;
        d.write("state.enc", &s, true)?;
        Ok(
            json!({"request_id":request_id,"recorded":true,"delivered":false,"process_stopped":false}),
        )
    }
}
