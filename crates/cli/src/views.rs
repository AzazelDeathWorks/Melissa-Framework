use crate::{
    Error, Result,
    app::{App, absolute, decode},
    digest,
};
use melissa_repository::{Limits, Repository};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap, fs, io::Write, path::Path, sync::atomic::AtomicBool, time::Duration,
};
impl App {
    pub(crate) fn evidence_inventory(
        &self,
        store: &melissa_local_store::LocalStore,
        id: &str,
    ) -> Result<Vec<melissa_local_store::ArtifactMetadata>> {
        let mut inventory = store.list_artifacts(id)?;
        for child in crate::engine::qa_children(store, id)? {
            inventory.extend(store.list_artifacts(&child)?.into_iter().filter(|a| {
                a.logical_type.starts_with("quality.protected.")
                    || a.logical_type.starts_with("web.")
                    || a.logical_type.starts_with("cli.domain.")
            }));
        }
        inventory.sort_by(|a, b| a.artifact_id.cmp(&b.artifact_id));
        Ok(inventory)
    }
    pub fn progress(&self, id: &str) -> Result<Value> {
        match self.store(id) {
            Ok(s) => {
                let mut v = self.show_store(&s, id)?;
                if let Some(e) = self.execution(&s, id)? {
                    let task = s.journal().task(id)?.contract;
                    let p = super::engine::plan(&task, &e)?;
                    let source = task.input_snapshot_digest.clone();
                    let policy = e.settings_digest;
                    let scheduler = melissa_workflow::scheduler::Scheduler::open(
                        s.connect()?,
                        id.into(),
                        p.id.clone(),
                        std::sync::Arc::new(move || {
                            Ok(melissa_workflow::scheduler::Freshness {
                                workspace_digest: source.clone(),
                                policy_digest: policy.clone(),
                            })
                        }),
                        std::sync::Arc::new(AtomicBool::new(false)),
                    );
                    v["workflow"] = if let Ok(s) = scheduler {
                        serde_json::to_value(s.state().map_err(|_| Error::Store)?)
                            .map_err(|_| Error::Store)?
                    } else {
                        json!({"plan":p,"execution_started":false})
                    };
                    if let Ok(live) = self.live(id) {
                        v["last_control_phase"] = json!(live.phase);
                        v["owner_alive"] = json!(live.owner.alive());
                    }
                }
                v["command"] = json!("task progress");
                v["native_workers"] = json!(crate::engine::native_workers(&s, id)?);
                v["owned_workers_confirmed_stopped"] =
                    json!(crate::engine::native_stop_proven(&s, id)?);
                Ok(v)
            }
            Err(Error::Unavailable(_)) => self.show(id),
            Err(e) => Err(e),
        }
    }
    pub fn logs(&self, id: &str) -> Result<Value> {
        match self.store(id) {
            Ok(s) => Ok(
                json!({"command":"task logs","task_id":id,"events":s.journal().events(id)?,"artifact_inventory":s.list_artifacts(id)?,"raw_content":"Export an exact selected artifact with task evidence; provider bodies and credentials are not printed automatically"}),
            ),
            Err(Error::Unavailable(_)) => {
                let s = self.live(id)?;
                if !s.active() {
                    return Err(Error::Unavailable("Canlı log oturumu doğrulanamadı."));
                }
                Ok(
                    json!({"command":"task logs","task_id":id,"events":s.events,"model_budget":s.model_budget,"live":true,"updated_at_ms":s.updated_at_ms}),
                )
            }
            Err(e) => Err(e),
        }
    }
    pub fn diff_content(&self, id: &str) -> Result<Value> {
        let mut view = self.diff(id)?;
        let s = self.store(id)?;
        let w = self.workspace(&s, id)?;
        let r = self.source(&w)?;
        let current = r.snapshot(&AtomicBool::new(false))?;
        if view["current_digest"] != current.digest {
            return Err(Error::Conflict("Diff okunurken kaynak değişti."));
        }
        let changes = view["changes"].as_array_mut().ok_or(Error::Store)?;
        for change in changes {
            let path = change["path"].as_str().ok_or(Error::Store)?.to_owned();
            change["before_bytes"] = if let Some(a) = w.files.get(&path) {
                json!(s.get_artifact(id, a)?)
            } else {
                Value::Null
            };
            change["after_bytes"] = if current.files.iter().any(|f| f.path == path) {
                json!(r.read(&path)?.1)
            } else {
                Value::Null
            };
        }
        r.validate_snapshot(&current, &AtomicBool::new(false))?;
        view["encoding"] = json!("exact_byte_arrays_preserve_CRLF_and_binary");
        view["raw_source_printed"] = json!(true);
        Ok(view)
    }
    pub fn delivery(&self, id: &str, out: Option<&Path>) -> Result<Value> {
        let s = self.store(id)?;
        let task = s.journal().task(id)?;
        if task.record.state != melissa_core_contracts::TaskState::Completed {
            return Err(Error::Conflict(
                "Doğrulanmış son teslim yok; görev tamamlanmadı.",
            ));
        }
        let a = s
            .list_artifacts(id)?
            .into_iter()
            .find(|a| a.logical_type == "cli.verified.task.delivery")
            .ok_or(Error::Store)?;
        if task.record.final_artifact_digest.as_deref() != Some(&a.digest) {
            return Err(Error::Store);
        }
        let data = s.get_artifact(id, &a.artifact_id)?;
        let value: Value = decode(&data)?;
        let package: BTreeMap<String, Vec<u8>> =
            serde_json::from_value(value["package"].clone()).map_err(|_| Error::Store)?;
        if digest(&serde_json::to_vec(&package).map_err(|_| Error::Store)?)
            != value["package_digest"]
        {
            return Err(Error::Store);
        }
        if let Some(out) = out {
            let out = absolute(out)?;
            let parent = out.parent().ok_or(Error::Io)?;
            let _pin = Repository::open(parent, Limits::default(), vec![])?;
            fs::create_dir(&out).map_err(|_| {
                Error::Conflict("Teslim klasörü var/yazılamıyor; üzerine yazılmaz.")
            })?;
            let _out_pin = Repository::open(&out, Limits::default(), vec![])?;
            for (path, bytes) in &package {
                if path == "/" || !melissa_workflow::plan::path(path) {
                    return Err(Error::Store);
                }
                let target = out.join(path);
                if let Some(p) = target.parent() {
                    fs::create_dir_all(p)?;
                }
                let mut f = fs::OpenOptions::new()
                    .create_new(true)
                    .write(true)
                    .open(&target)?;
                f.write_all(bytes)?;
                f.sync_all()?;
                if fs::read(target)? != *bytes {
                    return Err(Error::Store);
                }
            }
            let manifest = out.join("melissa-delivery.json");
            let mut f = fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(manifest)?;
            f.write_all(&serde_json::to_vec_pretty(&json!({"task_id":id,"source_digest":value["source_digest"],"package_digest":value["package_digest"],"receipt":value["receipt"],"files":package.iter().map(|(p,b)|(p,digest(b))).collect::<BTreeMap<_,_>>(),"release_security_license_acceptance":false})).map_err(|_|Error::Store)?)?;
            f.sync_all()?;
        }
        Ok(
            json!({"command":"task delivery","task_id":id,"artifact_id":a.artifact_id,"source_digest":value["source_digest"],"package_digest":value["package_digest"],"receipt":value["receipt"],"files":package.keys().collect::<Vec<_>>(),"task_accepted":true,"release_security_license_acceptance":false,"exported_to":out,"preview_command":"task preview ID [--out NEW_DIRECTORY]"}),
        )
    }
    /// Export only source-bound PNGs produced after protected acceptance, never
    /// private fixture data or an unauthenticated model screenshot claim.
    pub fn preview(&self, id: &str, out: Option<&Path>) -> Result<Value> {
        let delivery = self.delivery(id, None)?;
        let store = self.store(id)?;
        let mut previews = vec![];
        for a in store
            .list_artifacts(id)?
            .into_iter()
            .filter(|a| a.logical_type == "cli.domain.preview")
        {
            let link: Value = decode(&store.get_artifact(id, &a.artifact_id)?)?;
            if link["source_digest"] != delivery["source_digest"]
                || link["origin"] != "accepted-source-preview"
            {
                continue;
            }
            let image = link["image"].as_str().ok_or(Error::Store)?;
            let meta = store.artifact_metadata(id, image)?;
            let raw = store.get_artifact(id, image)?;
            if meta.logical_type != "web.png"
                || meta.content_type != "image/png"
                || digest(&raw) != link["image_digest"]
            {
                return Err(Error::Store);
            }
            melissa_model_gateway::vision::InlinePng::new(raw).map_err(|_| Error::Store)?;
            previews.push(link);
        }
        if previews.is_empty() {
            return Err(Error::Unavailable(
                "Bu tamamlanan görevde tarife bağlı frontend önizlemesi yok; API/DB kanıtları task evidence ile okunur.",
            ));
        }
        previews.sort_by_key(|p| p["width"].as_u64().unwrap_or(0));
        let manifest = json!({"command":"task preview","task_id":id,"source_digest":delivery["source_digest"],"previews":previews,"task_accepted":true,"exported_to":out,"live_server":false});
        if let Some(out) = out {
            let out = absolute(out)?;
            let _parent =
                Repository::open(out.parent().ok_or(Error::Io)?, Limits::default(), vec![])?;
            fs::create_dir(&out).map_err(|_| {
                Error::Conflict("Önizleme klasörü var/yazılamıyor; üzerine yazılmaz.")
            })?;
            let _pin = Repository::open(&out, Limits::default(), vec![])?;
            for (index, p) in previews.iter().enumerate() {
                let raw = store.get_artifact(id, p["image"].as_str().ok_or(Error::Store)?)?;
                let mut file =
                    fs::OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(out.join(format!(
                            "{index}-{}px.png",
                            p["width"].as_u64().ok_or(Error::Store)?
                        )))?;
                file.write_all(&raw)?;
                file.sync_all()?;
            }
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(out.join("melissa-preview.json"))?;
            file.write_all(&serde_json::to_vec_pretty(&manifest).map_err(|_| Error::Store)?)?;
            file.sync_all()?;
        }
        Ok(manifest)
    }
    pub async fn wait_task(&self, id: &str, timeout_ms: u64) -> Result<Value> {
        if timeout_ms == 0 || timeout_ms > 30000 {
            return Err(Error::Usage("Bekleme 1..30000 ms olmalı."));
        }
        let end = std::time::Instant::now() + Duration::from_millis(timeout_ms);
        loop {
            let v = self.progress(id)?;
            if v["live"] != true || std::time::Instant::now() >= end {
                return Ok(v);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
}
pub(crate) fn sdk_image_present() -> bool {
    image_present(melissa_code_quality::protected::SDK_IMAGE)
}
pub(crate) fn image_present(expected: &str) -> bool {
    #[cfg(windows)]
    let client = r"C:\Program Files\Docker\Docker\resources\bin\docker.exe";
    #[cfg(not(windows))]
    let client = "docker";
    let Ok(config) = tempfile::tempdir() else {
        return false;
    };
    let mut command = std::process::Command::new(client);
    command.env_clear();
    if let Some(root) = std::env::var_os("SystemRoot") {
        command.env("SystemRoot", root);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let image = command
        .arg("--config")
        .arg(config.path())
        .args([
            "--host",
            "npipe:////./pipe/dockerDesktopLinuxEngine",
            "image",
            "inspect",
            expected,
            "--format",
            "{{.Id}}",
        ])
        .output()
        .ok()
        .filter(|r| r.status.success())
        .map(|r| String::from_utf8_lossy(&r.stdout).trim().to_owned());
    image.as_deref() == Some(expected)
}
pub fn doctor() -> Result<Value> {
    let executable = std::env::current_exe()?;
    let image = sdk_image_present();
    let web_image = image_present(melissa_web_development::plan::WEB_IMAGE);
    let c = crate::resources::catalog()?;
    Ok(
        json!({"command":"doctor","host":std::env::consts::OS,"architecture":std::env::consts::ARCH,"binary_digest":digest(&fs::read(executable)?),"profiles":c.profiles.len(),"workers":c.workers.len(),"runtime_definitions":"bundled_and_validated_no_historical_evidence_claim","DPAPI":cfg!(windows),"isolated_SDK_image_present":image,"isolated_web_API_DB_image_present":web_image,"model_probe":"NOT_RUN","credential_read":false,"Tauri":false}),
    )
}
