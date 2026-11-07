//! Presentation-independent session adapter. The existing session remains the
//! sole implementation of preparation, exact approval, scope and undo rules.
use super::*;
use std::sync::{Arc, Mutex};

pub(crate) struct FrontendSession {
    session: Session,
    captured: Arc<Mutex<Vec<Value>>>,
    pub revision: u64,
}
impl FrontendSession {
    pub fn new(home: &Path, workspace: &Path) -> Result<Self> {
        let captured = Arc::new(Mutex::new(vec![]));
        Ok(Self {
            session: Session::new(home, workspace, Ui::captured(captured.clone()))?,
            captured,
            revision: 0,
        })
    }
    pub fn task(&self) -> Option<String> {
        self.session.task.as_ref().map(|t| t.id.clone())
    }
    pub fn home(&self) -> &Path {
        &self.session.app.home
    }
    pub fn snapshot(&self, busy: bool) -> Value {
        let s = &self.session;
        let progress = s.task.as_ref().map(|t| s.app.progress(&t.id));
        let (progress, error) = match progress {
            Some(Ok(v)) => (v, Value::Null),
            Some(Err(e)) => (
                Value::Null,
                serde_json::json!({"kind":e.kind(),"message":e.to_string()}),
            ),
            None => (Value::Null, Value::Null),
        };
        serde_json::json!({"schema_version":"melissa.frontend-state/v1",
            "revision":self.revision,"workspace":s.workspace,"mode":s.mode,"busy":busy,
            "task_id":self.task(),"configured":s.app.configuration().is_ok(),
            "context":s.context(),"budget_micro_usd":s.budget,"duration_ms":s.duration,
            "draft":s.draft.as_ref().map(|d|serde_json::json!({"goal":d.goal,"plan":d.plan,"ready":d.ready,"path":d.path})),
            "approval":s.task.as_ref().and_then(|t|t.approval.as_ref()).map(|(request,digest)|serde_json::json!({"request":request,"digest":digest})),
            "executable":s.task.as_ref().is_some_and(|t|t.executable),
            "brief":s.task.as_ref().and_then(|t|s.app.conversation(&t.id).ok().flatten()),"progress":progress,"read_error":error,"commands":crate::session_ui::COMMANDS})
    }
    pub fn check_revision(&self, expected: u64) -> Result {
        if expected != self.revision {
            return Err(Error::Conflict(
                "Görünüm değişti; güncel durumu inceleyip yeniden gönder.",
            ));
        }
        Ok(())
    }
    pub fn start_run(&mut self, resume: bool) -> Result<(PathBuf, String, bool)> {
        let t = self.session.task()?;
        if !t.executable || t.approval.is_some() {
            return Err(Error::Conflict(
                "Yürütülebilir ve kapsamı onaylanmış görev gerekli.",
            ));
        }
        let id = t.id.clone();
        self.revision += 1;
        Ok((self.home().to_owned(), id, resume))
    }
    pub fn start_project(
        &mut self,
        text: &str,
    ) -> Result<(PathBuf, String, String, Option<crate::project::Command>)> {
        let (command, argument) = text.split_once(char::is_whitespace).unwrap_or((text, ""));
        let operation = match command {
            "/exec" => "exec",
            "/preflight" => "preflight",
            "/environment-sync" => "sync",
            _ => return Err(Error::Usage("Süreç işlemi geçersiz.")),
        };
        let command = if operation == "exec" {
            Some(serde_json::from_str(argument).map_err(|_| {
                Error::Usage("Çalıştırılacak executable/argv/cwd JSON komutu gerekli.")
            })?)
        } else {
            None
        };
        let id = self.session.task()?.id.clone();
        self.revision += 1;
        Ok((self.home().to_owned(), id, operation.into(), command))
    }
    pub fn process(
        &self,
        session: &str,
        operation: &str,
        after: u64,
        hex: &str,
        request_id: &str,
    ) -> Result<Value> {
        let id = &self.session.task()?.id;
        match operation {
            "poll" => self.session.app.project_poll(id, session, after, 32 * 1024),
            "stdin" | "eof" | "cancel" => self
                .session
                .app
                .project_input(id, session, request_id, operation, hex),
            _ => Err(Error::Usage("Süreç kontrol işlemi geçersiz.")),
        }
    }
    pub async fn command(&mut self, text: &str, approval: Option<&str>, busy: bool) -> Result {
        if busy {
            let mut previous = String::new();
            let id = self.task().ok_or(Error::Store)?;
            if let Some(message) =
                running_control(&self.session.app, &id, Some(text), &mut previous)?
            {
                self.session.ui.notice(&message, &mut std::io::sink())?;
            }
        } else {
            if text.trim() == "/approve" {
                let expected = &self
                    .session
                    .task()?
                    .approval
                    .as_ref()
                    .ok_or(Error::Conflict("Bekleyen kapsam onayı yok."))?
                    .1;
                if approval != Some(expected.as_str()) {
                    return Err(Error::Conflict(
                        "İncelenen kapsam özeti güncel onayla eşleşmiyor.",
                    ));
                }
            }
            self.session.handle(text, &mut std::io::sink()).await?;
        }
        self.revision += 1;
        Ok(())
    }
    pub fn read(&self, view: &str) -> Result<Value> {
        let s = &self.session;
        match view {
            "tasks" => s.app.list(),
            "memory" => s.app.memory(&s.workspace, "list", None, None),
            "extensions" => s.app.extension_catalog(&s.workspace),
            "settings" => s.app.configuration(),
            "instructions" => s.app.project_guidance(&s.workspace),
            "status" => s.app.progress(&s.task()?.id),
            "conversation" => s.app.conversation_view(&s.task()?.id),
            "changes" => s.app.diff_content(&s.task()?.id),
            "logs" => s.app.logs(&s.task()?.id),
            "continuity" => s.app.continuity(&s.task()?.id),
            "diagnostics" => s.app.diagnose(&s.task()?.id),
            "metrics" => s.app.metrics(&s.task()?.id),
            "preview" => {
                use base64::Engine;
                let id = &s.task()?.id;
                let mut manifest = s.app.preview(id, None)?;
                let store = s.app.store(id)?;
                let images=manifest["previews"].as_array().ok_or(Error::Store)?.iter().map(|p|{
                    let raw=store.get_artifact(id,p["image"].as_str().ok_or(Error::Store)?)?;
                    Ok(serde_json::json!({"width":p["width"],"source_digest":manifest["source_digest"],
                        "url":format!("data:image/png;base64,{}",base64::engine::general_purpose::STANDARD.encode(raw))}))
                }).collect::<Result<Vec<_>>>()?;
                manifest["images"] = serde_json::json!(images);
                Ok(manifest)
            }
            _ => Err(Error::Usage("Görünüm desteklenmiyor.")),
        }
    }
    pub fn drain(&self) -> Vec<Value> {
        self.captured
            .lock()
            .map(|mut e| std::mem::take(&mut *e))
            .unwrap_or_default()
    }
}
