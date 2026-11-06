//! Authenticated, task/session-bound foreground control. The worker never
//! mounts this directory or receives the host StoreKey. Files are requests,
//! not proof that a process stopped. Old-session requests cannot control a resume.
use crate::{
    Error, Result,
    app::{App, absolute, bytes, decode, nonce, now, safe_path, validate_id},
};
use melissa_local_store::StoreKey;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU8, Ordering},
    },
};
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProcessIdentity {
    pub pid: u32,
    pub created: u64,
}
#[cfg(windows)]
fn identity(pid: u32) -> Result<ProcessIdentity> {
    use windows_sys::Win32::{
        Foundation::{CloseHandle, FILETIME, WAIT_TIMEOUT},
        System::Threading::{
            GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
            WaitForSingleObject,
        },
    };
    unsafe {
        let h = OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
            0,
            pid,
        );
        if h.is_null() {
            return Err(Error::Unavailable("Çalıştırıcı süreci bulunamadı."));
        }
        let mut c: FILETIME = std::mem::zeroed();
        let mut e = c;
        let mut k = c;
        let mut u = c;
        let ok = GetProcessTimes(h, &mut c, &mut e, &mut k, &mut u) != 0
            && WaitForSingleObject(h, 0) == WAIT_TIMEOUT;
        CloseHandle(h);
        if !ok {
            return Err(Error::Unavailable("Çalıştırıcı süreci sonlanmış."));
        }
        Ok(ProcessIdentity {
            pid,
            created: ((c.dwHighDateTime as u64) << 32) | c.dwLowDateTime as u64,
        })
    }
}
#[cfg(not(windows))]
fn identity(_: u32) -> Result<ProcessIdentity> {
    Err(Error::Unavailable("Bu yürütücü Windows gerektirir."))
}
impl ProcessIdentity {
    pub fn current() -> Result<Self> {
        identity(std::process::id())
    }
    pub fn alive(&self) -> bool {
        identity(self.pid).is_ok_and(|v| v == *self)
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    pub schema_version: String,
    pub task_id: String,
    pub session: String,
    pub owner: ProcessIdentity,
    pub updated_at_ms: u64,
    pub phase: String,
    pub task: Value,
    pub workflow: Value,
    pub model_budget: Value,
    pub events: Value,
    pub cancel_requested: bool,
    pub pause_requested: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_measurement: Option<crate::process_measurement::Measurement>,
}
impl Snapshot {
    pub fn active(&self) -> bool {
        matches!(self.phase.as_str(), "preparing" | "agent" | "verifying") && self.owner.alive()
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    schema_version: String,
    task_id: String,
    session: String,
    nonce: String,
    action: String,
}
#[derive(Clone)]
pub(crate) struct Control {
    root: PathBuf,
    key: StoreKey,
    task: String,
    pub session: String,
    pub owner: ProcessIdentity,
    pub cancel: Arc<AtomicBool>,
    pub pause: Arc<AtomicBool>,
    pub phase: Arc<AtomicU8>,
    baseline: crate::process_measurement::Sample,
}
fn aad(task: &str) -> String {
    format!("melissa.cli.control/v1:{task}")
}
fn write(path: &Path, key: &StoreKey, task: &str, value: &impl Serialize, replace: bool) -> Result {
    safe_path(path)?;
    let encrypted = key.seal(&bytes(value)?, aad(task).as_bytes())?;
    // Readers enumerate request-*.enc. Never expose that name until the entire
    // authenticated record is durable and the writer handle has closed.
    let target = path.with_extension(format!("{}.tmp", nonce()?));
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&target)?;
    f.write_all(&encrypted)?;
    f.sync_all()?;
    drop(f);
    if replace {
        fs::rename(target, path)?;
    } else {
        publish_new(&target, path)?;
    }
    Ok(())
}
#[cfg(windows)]
fn publish_new(source: &Path, destination: &Path) -> Result {
    use std::os::windows::ffi::OsStrExt;
    let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let destination: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    // No REPLACE_EXISTING flag: a colliding request is preserved, never replaced.
    if unsafe {
        windows_sys::Win32::Storage::FileSystem::MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            0,
        )
    } == 0
    {
        return Err(Error::Io);
    }
    Ok(())
}
#[cfg(not(windows))]
fn publish_new(source: &Path, destination: &Path) -> Result {
    fs::hard_link(source, destination)?;
    fs::remove_file(source)?;
    Ok(())
}
fn read<T: serde::de::DeserializeOwned>(
    root: &Path,
    path: &str,
    key: &StoreKey,
    task: &str,
) -> Result<T> {
    let repo = melissa_repository::Repository::open(root, Default::default(), vec![])?;
    let (_, cipher) = repo.read(path)?;
    let plain = zeroize::Zeroizing::new(key.open(&cipher, aad(task).as_bytes())?);
    decode(&plain)
}
impl Control {
    pub fn new(
        app: &App,
        id: &str,
        cancel: Arc<AtomicBool>,
        pause: Arc<AtomicBool>,
    ) -> Result<Self> {
        validate_id(id)?;
        let root = app.home.join("tasks").join(id).join("control");
        safe_path(&root)?;
        fs::create_dir_all(&root)?;
        Ok(Self {
            root,
            key: app.key.clone(),
            task: id.into(),
            session: nonce()?,
            owner: ProcessIdentity::current()?,
            cancel,
            pause,
            phase: Arc::new(AtomicU8::new(0)),
            baseline: crate::process_measurement::sample(),
        })
    }
    pub fn poll(&self) -> Result {
        for e in fs::read_dir(&self.root)? {
            let e = e?;
            let name = e.file_name().into_string().map_err(|_| Error::Store)?;
            if !name.starts_with("request-") || !name.ends_with(".enc") {
                continue;
            }
            let r: Request = read(&self.root, &name, &self.key, &self.task)?;
            if r.schema_version != "melissa.cli-control-request/v1" || r.task_id != self.task {
                return Err(Error::Store);
            }
            if r.session != self.session {
                continue;
            }
            match r.action.as_str() {
                "cancel" => self.cancel.store(true, Ordering::Release),
                "pause" => self.pause.store(true, Ordering::Release),
                _ => return Err(Error::Store),
            }
        }
        Ok(())
    }
    pub(crate) fn measurement(&self) -> crate::process_measurement::Measurement {
        crate::process_measurement::Measurement::observe(&self.baseline)
    }
    pub fn publish(
        &self,
        phase: &str,
        task: Value,
        workflow: Value,
        budget: Value,
        events: Value,
    ) -> Result {
        let s = Snapshot {
            schema_version: "melissa.cli-control-snapshot/v1".into(),
            task_id: self.task.clone(),
            session: self.session.clone(),
            owner: self.owner.clone(),
            updated_at_ms: now()?,
            phase: phase.into(),
            task,
            workflow,
            model_budget: budget,
            events,
            cancel_requested: self.cancel.load(Ordering::Acquire),
            pause_requested: self.pause.load(Ordering::Acquire),
            host_measurement: Some(self.measurement()),
        };
        write(
            &self.root.join("snapshot.enc"),
            &self.key,
            &self.task,
            &s,
            true,
        )
    }
}
impl App {
    pub fn live(&self, id: &str) -> Result<Snapshot> {
        validate_id(id)?;
        let root = self.home.join("tasks").join(id).join("control");
        let s: Snapshot = read(&root, "snapshot.enc", &self.key, id)?;
        if s.schema_version != "melissa.cli-control-snapshot/v1"
            || s.task_id != id
            || s.session.len() != 32
        {
            return Err(Error::Store);
        }
        Ok(s)
    }
    pub fn request_control(&self, id: &str, action: &str) -> Result<Value> {
        if !matches!(action, "cancel" | "pause") {
            return Err(Error::Usage("Kontrol işlemi bilinmiyor."));
        }
        let s = self.live(id)?;
        if !s.active() {
            return Err(Error::Conflict(
                "Canlı oturum yok; kalıcı durum ve resume kaydını inceleyin.",
            ));
        }
        let r = Request {
            schema_version: "melissa.cli-control-request/v1".into(),
            task_id: id.into(),
            session: s.session,
            nonce: nonce()?,
            action: action.into(),
        };
        let path = absolute(
            &self
                .home
                .join("tasks")
                .join(id)
                .join("control")
                .join(format!("request-{}.enc", r.nonce)),
        )?;
        write(&path, &self.key, id, &r, false)?;
        Ok(
            json!({"command":format!("task {action}"),"task_id":id,"state":format!("{action}_requested"),"request_id":r.nonce,"process_stopped":false,"task_accepted":false,"note":"Request recorded; wait/progress shows acknowledged result. Pause waits for a safe settled boundary."}),
        )
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    #[test]
    fn control_requests_publish_complete_records_and_never_replace_a_collision() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        App::init(&home).unwrap();
        let app = App::open(&home).unwrap();
        let task = "task_00000000000000000000000000000003";
        fs::create_dir(app.home.join("tasks").join(task)).unwrap();
        let control = Control::new(
            &app,
            task,
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
        control
            .publish("agent", Value::Null, Value::Null, Value::Null, Value::Null)
            .unwrap();
        let unfinished = control.root.join("request-unpublished.tmp");
        fs::write(&unfinished, b"partial unauthenticated bytes").unwrap();
        control.poll().unwrap();
        assert!(!control.cancel.load(Ordering::Acquire));
        let request = Request {
            schema_version: "melissa.cli-control-request/v1".into(),
            task_id: task.into(),
            session: control.session.clone(),
            nonce: nonce().unwrap(),
            action: "cancel".into(),
        };
        let path = control.root.join(format!("request-{}.enc", request.nonce));
        write(&path, &app.key, task, &request, false).unwrap();
        let original = fs::read(&path).unwrap();
        assert!(
            write(
                &path,
                &app.key,
                task,
                &json!({"forged":"replacement"}),
                false
            )
            .is_err()
        );
        assert_eq!(fs::read(&path).unwrap(), original);
        control.poll().unwrap();
        assert!(control.cancel.load(Ordering::Acquire));
    }
    #[test]
    fn authenticated_requests_cannot_cross_tasks_sessions_or_forge_a_stopped_process() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        App::init(&home).unwrap();
        let app = App::open(&home).unwrap();
        let task = "task_00000000000000000000000000000001";
        fs::create_dir(app.home.join("tasks").join(task)).unwrap();
        let control = Control::new(
            &app,
            task,
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
        control
            .publish(
                "agent",
                json!({"state":"running"}),
                Value::Null,
                Value::Null,
                Value::Null,
            )
            .unwrap();
        assert!(app.live(task).unwrap().active());
        let request = app.request_control(task, "cancel").unwrap();
        assert_eq!(request["process_stopped"], false);
        control.poll().unwrap();
        assert!(control.cancel.load(Ordering::Acquire));
        let next = Control::new(
            &app,
            task,
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
        next.poll().unwrap();
        assert!(!next.cancel.load(Ordering::Acquire));
        let other = "task_00000000000000000000000000000002";
        fs::create_dir(app.home.join("tasks").join(other)).unwrap();
        let second = Control::new(
            &app,
            other,
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
        fs::copy(
            control.root.join("snapshot.enc"),
            second.root.join("snapshot.enc"),
        )
        .unwrap();
        assert!(app.live(other).is_err());
        let p = control.root.join("snapshot.enc");
        let mut bad = fs::read(&p).unwrap();
        bad[20] ^= 1;
        fs::write(&p, bad).unwrap();
        assert!(app.live(task).is_err());
        let fake = ProcessIdentity {
            pid: std::process::id(),
            created: 0,
        };
        assert!(!fake.alive());
    }
}
