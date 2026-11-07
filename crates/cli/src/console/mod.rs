//! Single terminal product surface. Legacy structured task commands remain
//! available to automation; interactive users work in one conversation.
mod chat;
mod configure;
mod execution;
mod interactions;
mod settings;
mod transcript;
mod ui;
use crate::{
    Error, Result,
    app::{App, absolute},
};
use chat::Conversation;
use crossterm::event::{Event, KeyCode, KeyEventKind};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::sync::mpsc;
use ui::Ui;

const HELP: &str = "Bir şey sor veya yapmak istediğini yaz. Sohbet için proje seçmek gerekmez.\n\n/settings  Sağlayıcı, anahtar, model ve tercihler\n/model     Konuşmayı koruyarak model değiştir\n/plan      Plan modu (Shift+Tab ile normal moda dön)\n/apply     Konuşulan planı kapsam incelemesine al\n/new       Yeni konuşma; önceki kayıt korunur\n/resume    Tüm kayıtlı konuşmalar\n/project   Gerektiğinde klasör bağla veya oluştur\n/diff      Değişiklikleri incele\n/undo      Seçilen değişikliği geri al\n/search    Konuşmada ara (Ctrl+F, F3 sonraki)\n/compact   Geçmişi silmeden model bağlamını özetle\n/transcript  Konuşma ve araç ayrıntıları\n/queue     Bekleyen mesajları incele / taslağa geri al\n/exit      Güvenle çık\n\nPgUp/PgDn ve tekerlek geçmişi kaydırır. Ctrl+End sona döner.\nCtrl+O araç ayrıntılarını açar. Ctrl+R önceki mesajlarını gösterir.\nEnter gönderir; Alt+Enter veya Ctrl+J yeni satır ekler.\nYanıt gelirken Enter mesajı sıraya alır; kod çalışırken yeni talimat işe iletilir.\nCtrl+Q mesajı sonraki işe bırakır. Esc işi durdurur.\nTeknik otomasyon komutları: melissa --help-all";

enum Finished {
    Chat(Box<Conversation>),
    Execution(Value),
}
enum Output {
    Message(String),
    Delta(melissa_model_gateway::visible_stream::VisibleDelta),
    AgentDelta(melissa_model_gateway::visible_stream::VisibleDelta),
    Tool {
        id: String,
        name: String,
        state: String,
        detail: String,
    },
}
fn drain(
    ui: &mut Ui,
    receiver: &mut mpsc::UnboundedReceiver<Output>,
    live: &mut String,
    stream: &mut Option<u64>,
) -> Result {
    while let Ok(output) = receiver.try_recv() {
        match output {
            Output::AgentDelta(d) => ui.transcript.append_agent(d.stream_id, &d.text),
            Output::Tool {
                id,
                name,
                state,
                detail,
            } => ui.transcript.tool(&id, &name, &state, &detail),
            Output::Message(s) => {
                ui.print(&s)?;
                live.clear();
                *stream = None;
            }
            Output::Delta(delta) => {
                if *stream != Some(delta.stream_id) {
                    if !live.is_empty() {
                        ui.print(live)?;
                    }
                    live.clear();
                    *stream = Some(delta.stream_id);
                }
                live.push_str(&delta.text);
            }
        }
    }
    Ok(())
}
fn start_execution(
    home: &Path,
    id: String,
    resume: bool,
    sender: mpsc::UnboundedSender<Output>,
    session: String,
) -> Job {
    let owned_home = home.to_owned();
    let conversation_id = session.clone();
    let task = id.clone();
    let tool_sender = sender.clone();
    let tool_observer: melissa_model_gateway::visible_stream::ToolObserver =
        Arc::new(move |event| {
            let _ = tool_sender.send(Output::Tool {
                id: event.id,
                name: event.name,
                state: event.state,
                detail: String::new(),
            });
        });
    let observer: melissa_model_gateway::visible_stream::Observer = Arc::new(move |d| {
        let _ = sender.send(Output::AgentDelta(d));
    });
    Job {
        handle: tokio::spawn(async move {
            melissa_model_gateway::visible_stream::observe(observer, async {
                let app = App::open(&owned_home)?;
                let root = chat::load(&app, &conversation_id)?;
                let _lock = chat::session_lock(&app, &root)?;
                if root.last_task.as_deref() != Some(id.as_str()) {
                    return Err(Error::Conflict(
                        "Konuşmanın etkin görevi değişti; eski iş otomatik başlatılmadı.",
                    ));
                }
                melissa_model_gateway::visible_stream::observe_tools(
                    tool_observer,
                    app.run_task(&id, resume),
                )
                .await
                .map(Finished::Execution)
            })
            .await
        }),
        cancel: Arc::new(AtomicBool::new(false)),
        task: Some(task),
        session,
        allow_execution: false,
    }
}
struct Job {
    handle: tokio::task::JoinHandle<Result<Finished>>,
    cancel: Arc<AtomicBool>,
    task: Option<String>,
    session: String,
    allow_execution: bool,
}
fn start_chat(
    home: &Path,
    c: Conversation,
    text: String,
    sender: mpsc::UnboundedSender<Output>,
) -> Job {
    let allow_execution = text != "/compact";
    let owned_home = home.to_owned();
    let tx = sender.clone();
    let notice = Arc::new(move |update| {
        let output = match update {
            chat::Update::Text(s) => Output::Message(s),
            chat::Update::ToolState {
                id,
                name,
                state,
                detail,
            } => Output::Tool {
                id,
                name,
                state,
                detail,
            },
        };
        let _ = tx.send(output);
    });
    let observer: melissa_model_gateway::visible_stream::Observer = Arc::new(move |d| {
        let _ = sender.send(Output::Delta(d));
    });
    let cancel = Arc::new(AtomicBool::new(false));
    let token = cancel.clone();
    let session = c.id.clone();
    Job {
        handle: tokio::spawn(async move {
            melissa_model_gateway::visible_stream::observe(observer, async {
                let app = App::open(&owned_home)?;
                chat::turn(&app, c, &text, token, notice)
                    .await
                    .map(|c| Finished::Chat(Box::new(c)))
            })
            .await
        }),
        cancel,
        task: None,
        session,
        allow_execution,
    }
}
fn retain_model_context(
    ui: &mut Ui,
    app: &App,
    conversation: &mut Option<Conversation>,
    before: Option<crate::settings::Settings>,
) -> Result {
    let Ok(after) = app.settings() else {
        return Ok(());
    };
    let Some(c) = conversation.as_mut() else {
        return Ok(());
    };
    if c.model == after.model
        && c.protocol == after.protocol
        && c.endpoint == after.endpoint
        && c.provider_id == after.provider_id
    {
        return Ok(());
    }
    let cross_provider = c.endpoint != after.endpoint || c.provider_id != after.provider_id;
    if cross_provider
        && !ui.confirm("Konuşmanın görünür içeriği seçtiğin yeni sağlayıcıya gönderilsin mi?")?
    {
        if let Some(old) = before {
            app.save_settings(&old)?;
        }
        return Ok(());
    }
    c.rebind(&after)?;
    c.save(app)?;
    ui.print("Model değişti; konuşma korunuyor. Sağlayıcıya özel gizli devam verisi aktarılmaz.")?;
    if !after.capabilities.vision {
        ui.print(
            "Bu model metin kullanıyor; önceki görseller kayıtta korunur, modele gönderilmez.",
        )?;
    }
    Ok(())
}
impl Job {
    fn request_stop(&self, ui: &mut Ui, app: &App) -> Result {
        self.cancel.store(true, Ordering::Release);
        if let Some(id) = &self.task
            && let Err(e) = app.request_control(id, "pause")
        {
            ui.print(&format!(
                "Durdurma isteği kaydedilemedi: {e}. Çalıştırıcının sonucu bekleniyor."
            ))?;
        }
        Ok(())
    }
}
fn open_app(home: &Path) -> Result<App> {
    if !home.join("home.json").is_file() {
        App::init(home)?;
    }
    App::open(home)
}
pub(crate) fn suitable_project(app: &App, path: &Path) -> bool {
    if std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_owned))
        .is_some_and(|p| path.canonicalize().ok() == p.canonicalize().ok())
    {
        return false;
    }
    melissa_repository::Repository::open(path, Default::default(), vec![])
        .is_ok_and(|repo| app.require_separate_workspace(&repo).is_ok())
}
async fn project(ui: &mut Ui, app: &App) -> Result<Option<PathBuf>> {
    let last = settings::read(app)?.last_project;
    let mut options = vec![
        "Proje klasörü seç".into(),
        "Yeni proje klasörü oluştur".into(),
        "Ayarları aç".into(),
    ];
    if let Some(path) = &last {
        options.insert(0, format!("Son proje · {}", path.display()));
    }
    loop {
        let Some(index) = ui.choose("Melissa · bir proje seç", &options)? else {
            return Ok(None);
        };
        let label = &options[index];
        if label.starts_with("Son proje")
            && let Some(path) = &last
        {
            if suitable_project(app, path) {
                return Ok(Some(path.clone()));
            }
            ui.print("Son proje bulunamadı veya bu kapsam kullanılamaz.")?;
            continue;
        }
        if label == "Ayarları aç" {
            configure::menu(ui, app, None).await?;
            continue;
        }
        let Some(path) = ui.input("Proje klasörünün tam yolu", "", false)? else {
            continue;
        };
        let path = PathBuf::from(path.trim().trim_matches('"'));
        let path = if path.is_absolute() {
            path
        } else {
            std::env::current_dir()?.join(path)
        };
        let path = match absolute(&path) {
            Ok(path) => path,
            Err(e) => {
                ui.print(&e.to_string())?;
                continue;
            }
        };
        if label == "Yeni proje klasörü oluştur" && !path.exists() {
            crate::app::safe_path(&path)?;
            if fs::create_dir_all(&path).is_err() {
                ui.print("Klasör oluşturulamadı; erişebildiğin bir konum seç.")?;
                continue;
            }
        }
        if suitable_project(app, &path) {
            return Ok(Some(path));
        }
        ui.print("Bu klasör proje olarak açılamadı. Melissa veri klasörünü içermeyen bir proje klasörü seç.")?;
    }
}
fn replay(ui: &mut Ui, c: &Conversation) -> Result {
    ui.clear();
    ui.print(&format!("\n{}", c.title))?;
    for m in &c.messages {
        match m {
            melissa_model_gateway::Message::User { text } => ui.print(&format!("❯ {text}"))?,
            melissa_model_gateway::Message::UserImages { text, images } => {
                ui.print(&format!("❯ {text}\n[{} görsel eki]", images.len()))?
            }
            melissa_model_gateway::Message::Assistant { response } => {
                ui.print(&response.text)?;
                for call in &response.calls {
                    ui.transcript.tool(&call.id, &call.name, "kayıtta", "");
                }
            }
            melissa_model_gateway::Message::ToolResult {
                call_id,
                content,
                is_error,
            }
            | melissa_model_gateway::Message::ToolImages {
                call_id,
                content,
                is_error,
                ..
            } => {
                let name = c
                    .messages
                    .iter()
                    .find_map(|m| match m {
                        melissa_model_gateway::Message::Assistant { response } => response
                            .calls
                            .iter()
                            .find(|call| &call.id == call_id)
                            .map(|c| c.name.as_str()),
                        _ => None,
                    })
                    .unwrap_or("Araç");
                ui.transcript.tool(
                    call_id,
                    name,
                    if *is_error { "hata" } else { "tamamlandı" },
                    content,
                );
            }
        }
    }
    Ok(())
}
fn resume(ui: &mut Ui, app: &App, workspace: Option<&Path>) -> Result<Option<Conversation>> {
    let all = chat::list(app, workspace)?;
    let labels = all
        .iter()
        .map(|c| {
            format!(
                "{} · {}{}",
                c.title,
                c.model,
                if c.pending.is_some() {
                    " · yarım çağrı"
                } else {
                    ""
                }
            )
        })
        .collect::<Vec<_>>();
    let Some(i) = ui.choose("Konuşmalar · yazıp ara", &labels)? else {
        return Ok(None);
    };
    let mut c = all[i].clone();
    if c.pending.is_some() {
        ui.print("Önceki model çağrısının sonucu belirsiz. Kayıt korundu; çağrı otomatik tekrarlanmayacak. Fiyatı bilinmeyen kullanım olabilir.")?;
        if !ui.confirm("Belirsiz çağrıyı tekrar etmeden bu konuşmada yeni mesajlara devam et?")?
        {
            return Ok(None);
        }
        c.host_notes.push(format!("User acknowledged unresolved invocation {:?}; not replayed; accounting remains unknown.",c.pending));
        c.pending = None;
        c.cost_micro_usd = None;
        c.save(app)?;
    }
    if let Some(draft) = settings::read(app)?
        .drafts
        .get(&format!("session:{}", c.id))
    {
        c.draft = draft.clone();
    }
    replay(ui, &c)?;
    Ok(Some(c))
}
pub async fn settings_dialog(home: &Path) -> Result {
    let app = open_app(home)?;
    let mut ui = Ui::new()?;
    configure::menu(&mut ui, &app, None).await
}
pub async fn run(home: &Path, workspace: &Path) -> Result {
    let app = open_app(home)?;
    let mut ui = Ui::new()?;
    let mut workspace = suitable_project(&app, workspace).then(|| workspace.to_owned());
    if let Some(path) = &workspace {
        let mut p = settings::read(&app)?;
        p.last_project = Some(path.clone());
        settings::write(&app, &p)?;
    }
    let mut input = ui::editor();
    let draft_key = workspace
        .as_ref()
        .map(|p| format!("project:{}", p.display()))
        .unwrap_or_else(|| "start".into());
    if let Some(saved) = settings::read(&app)?.drafts.get(&draft_key) {
        input.insert_str(saved);
    }
    let mut paste = crate::frontend::paste::Paste::default();
    let mut history = settings::read(&app)?.history;
    let mut history_at = history.len();
    let mut history_draft = String::new();
    let mut queue: std::collections::VecDeque<String> = settings::read(&app)?
        .queues
        .get(&draft_key)
        .cloned()
        .unwrap_or_default()
        .into();
    let mut queue_key = draft_key.clone();
    let mut last_queue = queue.clone();
    let mut advance_queue = false;
    let mut conversation: Option<Conversation> = None;
    let mut plan = false;
    let mut job: Option<Job> = None;
    let mut exit_after_stop = false;
    let mut plan_after_stop = false;
    let mut live = String::new();
    let mut interactions = interactions::Pending::default();
    let mut interaction_poll = Instant::now();
    let (sender, mut receiver) = mpsc::unbounded_channel::<Output>();
    let mut live_stream = None;
    let mut saved_at = Instant::now();
    let mut dirty = false;
    let mut last_draft = String::new();
    loop {
        let current_key = conversation
            .as_ref()
            .map(|c| format!("session:{}", c.id))
            .unwrap_or_else(|| {
                workspace
                    .as_ref()
                    .map(|p| format!("project:{}", p.display()))
                    .unwrap_or_else(|| "start".into())
            });
        if queue != last_queue || current_key != queue_key {
            let mut prefs = settings::read(&app)?;
            prefs.queues.remove(&queue_key);
            prefs
                .queues
                .insert(current_key.clone(), queue.iter().cloned().collect());
            settings::write(&app, &prefs)?;
            queue_key = current_key;
            last_queue = queue.clone();
        }
        if let Some(c) = &conversation {
            plan = c.plan;
        }
        if !exit_after_stop && interaction_poll.elapsed() > Duration::from_millis(500) {
            if let Some(active) = &job
                && let Some(id) = &active.task
                && !active.handle.is_finished()
                && let Err(e) = interactions.refresh(&mut ui, &app, id)
            {
                ui.print(&e.to_string())?;
            }
            interaction_poll = Instant::now();
        }
        drain(&mut ui, &mut receiver, &mut live, &mut live_stream)?;
        if job.as_ref().is_some_and(|j| j.handle.is_finished()) {
            let completed = job.take().ok_or(Error::Store)?;
            let outcome = completed.handle.await;
            drain(&mut ui, &mut receiver, &mut live, &mut live_stream)?;
            ui.transcript
                .finish_running(if completed.cancel.load(Ordering::Acquire) {
                    "durduruldu"
                } else if outcome.as_ref().is_ok_and(|r| r.is_ok()) {
                    "tamamlandı"
                } else {
                    "sonuç doğrulanmadı"
                });
            if !live.is_empty() {
                if !outcome.as_ref().is_ok_and(|r| r.is_ok()) {
                    ui.print("Yanıt tamamlanmadı; aşağıdaki metin kısmi.")?;
                }
                ui.print(&live)?;
                live.clear();
                live_stream = None;
            }
            let mut explain_execution = false;
            match outcome {
                Ok(Ok(Finished::Chat(c))) => {
                    let mut c = *c;
                    if plan_after_stop {
                        c.plan = true;
                        plan = true;
                        plan_after_stop = false;
                    }
                    c.save(&app)?;
                    conversation = Some(c);
                    if !exit_after_stop
                        && !completed.cancel.load(Ordering::Acquire)
                        && queue.is_empty()
                        && let Some(reason) = conversation
                            .as_mut()
                            .and_then(|c| c.workspace_request.take())
                    {
                        ui.print(&reason)?;
                        if let Some(path) = project(&mut ui, &app).await? {
                            workspace = Some(path.clone());
                            let c = conversation.as_mut().ok_or(Error::Store)?;
                            c.workspace = Some(path);
                            c.host_notes.push("User attached this workspace through the folder picker. Continue the user's original request; scope approval is still required for effects.".into());
                            c.save(&app)?;
                            job = Some(start_chat(home, c.clone(), String::new(), sender.clone()));
                        } else {
                            let c = conversation.as_mut().ok_or(Error::Store)?;
                            c.host_notes.push("User cancelled folder selection. Do not ask again unless a new file-work request requires it.".into());
                            c.save(&app)?;
                            ui.print("Klasör bağlanmadı. Sohbete devam edebilirsin.")?;
                        }
                    }
                    if !exit_after_stop
                        && completed.allow_execution
                        && queue.is_empty()
                        && !completed.cancel.load(Ordering::Acquire)
                        && conversation
                            .as_ref()
                            .is_some_and(|c| !c.plan && c.proposal.is_some())
                    {
                        let c = conversation.as_mut().ok_or(Error::Store)?;
                        match execution::prepare(&mut ui, &app, c) {
                            Ok(Some(id)) => {
                                job = Some(start_execution(
                                    home,
                                    id,
                                    false,
                                    sender.clone(),
                                    c.id.clone(),
                                ));
                            }
                            Ok(None) => (),
                            Err(e) => ui.print(&e.to_string())?,
                        }
                    }
                    advance_queue = !completed.cancel.load(Ordering::Acquire);
                }
                Ok(Ok(Finished::Execution(value))) => {
                    explain_execution = !completed.cancel.load(Ordering::Acquire);
                    advance_queue = !completed.cancel.load(Ordering::Acquire);
                    ui.print(&format!(
                        "Çalışma durumu: {}\n{}\nDeğişiklikler /diff · geçmiş /resume",
                        value["state"].as_str().unwrap_or("kayıtlarda"),
                        if value["task_accepted"] == true {
                            "Bağımsız kabul geçti."
                        } else {
                            "Son kabul doğrulanmadı; kayıtlar korunuyor."
                        }
                    ))?;
                    if let Some(c) = conversation.as_mut() {
                        c.account_native(&app)?;
                        c.host_notes.push(format!("Native execution record: {}. Explain what actually changed and what checks passed or remain unresolved. Do not submit the same work again or start a new task without a new user request.",json!({"task":c.last_task,"state":value["state"],"accepted":value["task_accepted"],"evidence":"Use durable task evidence; do not invent checks"})));
                        if plan_after_stop {
                            c.plan = true;
                            plan = true;
                            plan_after_stop = false;
                        }
                        c.save(&app)?;
                    }
                }
                result => {
                    advance_queue = false;
                    let message = match result {
                        Ok(Err(e)) => e.to_string(),
                        Err(_) => "Çalıştırıcı durdu; kayıtları /resume ile incele.".into(),
                        _ => unreachable!(),
                    };
                    ui.print(&message)?;
                    if let Ok(mut c) = chat::load(&app, &completed.session) {
                        if completed.task.is_some() {
                            if c.account_native(&app).is_err() {
                                c.cost_micro_usd = None;
                            }
                            c.save(&app)?;
                        }
                        conversation = Some(c);
                    }
                }
            }
            let native_settled = completed.task.as_ref().is_none_or(|id| {
                app.store(id)
                    .is_ok_and(|store| crate::continuity::settled(&app, &store, id).is_ok())
            });
            if !native_settled {
                ui.print("Önceki çalışmanın duruşu veya çağrı hesabı tam doğrulanamadı. Kayıtlar korundu; bu konuşmada yeni uygulama ve geri alma uzlaştırma olmadan başlatılmaz.")?;
            }
            if plan_after_stop {
                plan = true;
                plan_after_stop = false;
                if let Some(c) = conversation.as_mut() {
                    c.plan = true;
                    c.save(&app)?;
                }
            }
            if !exit_after_stop
                && native_settled
                && let Some(c) = conversation.as_mut()
            {
                match interactions.review_scope(&mut ui, &app) {
                    Ok(Some(id)) => {
                        c.plan = false;
                        plan = false;
                        c.save(&app)?;
                        job = Some(start_execution(
                            home,
                            id,
                            true,
                            sender.clone(),
                            c.id.clone(),
                        ))
                    }
                    Ok(None) => (),
                    Err(e) => ui.print(&e.to_string())?,
                }
            }
            if explain_execution
                && native_settled
                && !exit_after_stop
                && job.is_none()
                && let Some(c) = &conversation
            {
                job = Some(start_chat(home, c.clone(), String::new(), sender.clone()));
            }
            live.clear();
            if exit_after_stop {
                break;
            }
        }
        let draft = input.lines().join("\n");
        if draft != last_draft {
            last_draft = draft.clone();
            dirty = true;
        }
        if dirty && saved_at.elapsed() > Duration::from_millis(500) {
            let key = conversation
                .as_ref()
                .map(|c| format!("session:{}", c.id))
                .unwrap_or_else(|| {
                    workspace
                        .as_ref()
                        .map(|p| format!("project:{}", p.display()))
                        .unwrap_or_else(|| "start".into())
                });
            let mut prefs = settings::read(&app)?;
            prefs.drafts.insert(key, draft.clone());
            settings::write(&app, &prefs)?;
            if job.is_none()
                && let Some(c) = conversation.as_mut()
            {
                c.draft = draft.clone();
                c.save(&app)?;
            }
            dirty = false;
            saved_at = Instant::now();
        }
        let selected = app.settings().ok();
        let model = selected
            .as_ref()
            .map(|s| s.model.as_str())
            .unwrap_or("model seçilmedi");
        let name = workspace
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| "sohbet · klasör bağlı değil".into());
        let cost = conversation
            .as_ref()
            .and_then(|c| c.cost_micro_usd)
            .map(|n| format!("${:.4}", n as f64 / 1e6))
            .unwrap_or_else(|| "maliyet bilinmiyor".into());
        let footer = ui::status_line(
            model,
            &name,
            if job.is_some() {
                "çalışıyor"
            } else if plan {
                "PLAN"
            } else {
                "hazır"
            },
            &cost,
            queue.len(),
            ui.terminal.size()?.width.saturating_sub(4),
        );
        ui.draw(&mut input, &footer, &live, plan)?;
        let event = if job.is_none() && advance_queue && draft.is_empty() && !queue.is_empty() {
            input.insert_str(queue.pop_front().unwrap_or_default());
            // Submit through the same command path on the next frame.
            ui.draw(&mut input, &footer, &live, plan)?;
            Some(Event::Key(crossterm::event::KeyEvent::new(
                KeyCode::Enter,
                crossterm::event::KeyModifiers::NONE,
            )))
        } else {
            match ui.key()? {
                Some(event) => paste.push(event),
                None => paste.flush_escape(),
            }
        };
        let draft = input.lines().join("\n");
        let Some(event) = event else {
            continue;
        };
        if ui.navigate(&event) {
            continue;
        }
        match event {
            Event::Paste(s) => {
                input.insert_str(s.replace("\r\n", "\n").replace('\r', "\n"));
            }
            Event::Key(k) if k.kind != KeyEventKind::Release => {
                if ui::control(&k, 'f') {
                    ui.search()?;
                } else if ui::control(&k, 'q') && !draft.trim().is_empty() {
                    queue.push_back(draft.clone());
                    input = ui::editor();
                    advance_queue = true;
                    ui.print("Mesaj sıraya alındı. /queue ile inceleyebilirsin.")?;
                } else if k.code == KeyCode::BackTab {
                    if let Some(active) = &job {
                        active.request_stop(&mut ui, &app)?;
                        advance_queue = false;
                        plan_after_stop = true;
                        ui.print("Etkin iş durunca Plan'a geçilecek.")?;
                    } else {
                        plan = !plan;
                        if let Some(c) = conversation.as_mut() {
                            c.plan = plan;
                            c.save(&app)?;
                        }
                    }
                } else if k.code == KeyCode::Esc || ui::control(&k, 'c') {
                    if let Some(active) = &job {
                        active.request_stop(&mut ui, &app)?;
                        advance_queue = false;
                        ui.print("Durdurma istendi; kayıt korunuyor.")?;
                    } else if draft.is_empty() && ui::control(&k, 'c') {
                        break;
                    } else if ui::control(&k, 'c') {
                        input = ui::editor();
                    } else {
                        ui.transcript.search.clear();
                    }
                } else if ui::control(&k, 'v') || ui::control(&k, 'y') {
                    if let Ok(s) = crate::frontend::paste::clipboard() {
                        input.insert_str(s);
                    }
                } else if k.code == KeyCode::Enter && !k.modifiers.is_empty()
                    || ui::control(&k, 'j')
                {
                    input.insert_newline();
                } else if k.code == KeyCode::Enter && !draft.trim().is_empty() {
                    if job.is_some() {
                        if draft.trim() == "/exit" {
                            if let Some(active) = &job {
                                active.request_stop(&mut ui, &app)?;
                                advance_queue = false;
                            }
                            exit_after_stop = true;
                            input = ui::editor();
                            ui.print("Güvenli duruş bekleniyor; kayıtlar korunarak kapanacak.")?;
                            continue;
                        }
                        if let Some(id) = job.as_ref().and_then(|j| j.task.as_ref()) {
                            if draft.trim() == "/questions" {
                                interactions.show_again();
                                interactions.refresh(&mut ui, &app, id)?;
                            } else {
                                match interactions.answer_or_steer(&app, id, &draft) {
                                    Ok(()) => {
                                        ui.print(&format!(
                                            "❯ {draft}\nMesaj güvenli model turuna iletilecek."
                                        ))?;
                                        input = ui::editor();
                                    }
                                    Err(e) => ui.print(&e.to_string())?,
                                }
                            }
                        } else {
                            queue.push_back(draft.clone());
                            input = ui::editor();
                            advance_queue = true;
                            ui.print("Mesaj sıraya alındı; mevcut yanıtın ardından devam edecek.")?;
                        }
                        continue;
                    }
                    let text = draft.trim().to_owned();
                    input = ui::editor();
                    let (command, arg) =
                        text.split_once(char::is_whitespace).unwrap_or((&text, ""));
                    if command == "/exit" {
                        break;
                    }
                    let result: Result = async { match command {
                        "/help" => ui.print(HELP),
                        "/config" | "/settings" => {
                            let before = app.settings().ok();
                            configure::menu(&mut ui, &app, workspace.as_deref()).await?;
                            retain_model_context(&mut ui, &app, &mut conversation, before)
                        }
                        "/model" | "/models" => {
                            let before = app.settings().ok();
                            configure::model(&mut ui, &app).await?;
                            retain_model_context(&mut ui, &app, &mut conversation, before)
                        }
                        "/search" => ui.search(),
                        "/queue" => {
                            let labels = queue.iter().cloned().collect::<Vec<_>>();
                            if let Some(i) = ui.choose("Bekleyen mesajlar · seçerek taslağa geri al", &labels)? {
                                input.insert_str(queue.remove(i).unwrap_or_default());
                            }
                            Ok(())
                        }
                        "/transcript" => { let text = ui.transcript.plain(); ui.view("Konuşma kaydı", &text) }
                        "/apply" if plan => Err(Error::Usage("Plan modu açık; Shift+Tab ile normal sohbete dön.")),
                        "/apply" => {
                            let c = conversation.as_mut().ok_or(Error::Usage("Önce yapılacak işi konuşmada belirt."))?;
                            if let Some(id) = execution::prepare(&mut ui, &app, c)? { job = Some(start_execution(home, id, false, sender.clone(), c.id.clone())); }
                            Ok(())
                        }
                        "/new" | "/clear" => {
                            if let Some(c) = conversation.as_mut() {
                                c.save(&app)?;
                            }
                            conversation = None;
                            queue.clear(); ui.clear();
                            ui.print("Yeni konuşma. Önceki kayıtlar /resume içinde.")
                        }
                        "/project" => {
                            if let Some(c) = conversation.as_mut() {
                                c.save(&app)?;
                            }
                            if let Some(p) = project(&mut ui, &app).await? {
                                workspace = Some(p.clone());
                                let mut prefs = settings::read(&app)?;
                                prefs.last_project = Some(p);
                                settings::write(&app, &prefs)?;
                                if let Some(c) = conversation.as_mut() {
                                    c.workspace = workspace.clone(); c.proposal = None; c.last_task = None;
                                    c.host_notes.push("User attached a different workspace. Reinspect sources before making code claims; earlier file observations may be stale.".into()); c.save(&app)?;
                                }
                                ui.print("Klasör bağlandı; konuşma korunuyor.")?;
                            }
                            Ok(())
                        }
                        "/resume" => {
                            if let Some(mut c) = resume(&mut ui, &app, None)? {
                                queue = settings::read(&app)?.queues.get(&format!("session:{}", c.id)).cloned().unwrap_or_default().into();
                                queue_key = format!("session:{}", c.id); last_queue = queue.clone(); advance_queue = false;
                                workspace = c.workspace.clone();
                                plan = c.plan;
                                input = ui::editor(); input.insert_str(&c.draft);
                                if let Some(id)=c.last_task.clone(){let progress=app.progress(&id)?;if progress["state"]=="paused" && ui.confirm("Duraklatılmış çalışmayı da sürdür?")?{c.plan=false;plan=false;c.save(&app)?;job=Some(start_execution(home,id,true,sender.clone(),c.id.clone()));}}
                                conversation = Some(c);
                            }
                            Ok(())
                        }
                        "/diff" => {
                            let id = conversation
                                .as_ref()
                                .and_then(|c| c.last_task.as_ref())
                                .ok_or(Error::Usage(
                                    "Bu konuşmada henüz uygulanmış değişiklik yok.",
                                ));
                            match id {
                                Ok(id) => ui.view("Değişiklikler", &crate::display::human(&app.diff_content(id)?)?),
                                Err(e) => Err(e),
                            }
                        }
                        "/undo" => match &conversation {
                            Some(c) => execution::undo(&mut ui, &app, c),
                            None => Err(Error::Usage("Önce bir konuşma aç.")),
                        },
                        "/plan" if arg.trim().is_empty() => {
                            plan = true;
                            if let Some(c) = conversation.as_mut() {
                                c.plan = true;
                                c.save(&app)?;
                            }
                            ui.print("Plan açık. Dosyalar değiştirilmez; isteğini yaz. Shift+Tab normal sohbete döner.")
                        }
                        _ if text.starts_with('/') && command != "/plan" && command != "/compact" => {
                            Err(Error::Usage("Komut bulunamadı. /help ile kısa listeyi aç."))
                        }
                        _ => {
                            if command == "/plan" {
                                plan = true;
                            }
                                if !app.home.join("settings.enc").try_exists()? {
                                    configure::provider(&mut ui, &app, None).await?;
                                }
                                if !app.home.join("settings.enc").try_exists()? {
                                    input.insert_str(&text);
                                    ui.print("Bağlantı kurulmadı; mesajın taslakta korundu. /settings ile sağlayıcı ekleyebilirsin.")?;
                                    return Ok(());
                                }
                                let selected_settings = app.settings()?;
                                let mut prefs = settings::read(&app)?;
                                if !prefs.api_consent {
                                    prefs.api_consent=ui.confirm("Seçilen API mesajlarını ve gerekli görünür proje içeriğini alabilir; ücret doğabilir. İzin ver?")?;
                                    settings::write(&app, &prefs)?;
                                }
                                if prefs.api_consent {
                                    if conversation.is_none() {
                                        conversation =
                                            Some(match workspace.as_deref() { Some(path) => Conversation::new(path, &selected_settings)?, None => Conversation::new_in(None, &selected_settings)? });
                                    }
                                    let mut c = conversation.clone().ok_or(Error::Store)?;
                                    c.plan = plan;
                                    c.draft.clear();
                                    c.save(&app)?;
                                    let text = if command == "/plan" {
                                        arg.to_owned()
                                    } else {
                                        text.clone()
                                    };
                                    ui.print(&format!("❯ {text}"))?;
                                    history.push(text.clone());
                                    history_at = history.len();
                                    let mut prefs = settings::read(&app)?; prefs.history = history.clone(); settings::write(&app, &prefs)?;
                                    job = Some(start_chat(home, c, text, sender.clone()));
                                } else {input.insert_str(&text);ui.print("API izni verilmedi; isteğin taslakta korundu.")?;}
                                Ok(())
                        }
                    }}.await;
                    if let Err(e) = result {
                        if !text.starts_with('/') || command == "/plan" {
                            input.insert_str(&text);
                        }
                        ui.print(&e.to_string())?;
                    }
                } else if k.code == KeyCode::Tab {
                    let choices = if draft.starts_with('/') && !draft.contains(char::is_whitespace)
                    {
                        [
                            "/help",
                            "/search",
                            "/compact",
                            "/queue",
                            "/transcript",
                            "/apply",
                            "/config",
                            "/settings",
                            "/model",
                            "/plan",
                            "/new",
                            "/resume",
                            "/diff",
                            "/undo",
                            "/project",
                            "/exit",
                        ]
                        .iter()
                        .filter(|s| s.starts_with(&draft))
                        .map(|s| s.to_string())
                        .collect::<Vec<_>>()
                    } else if let Some(token) = draft
                        .split_whitespace()
                        .last()
                        .filter(|s| s.starts_with('@'))
                        && let Some(path) = &workspace
                    {
                        let repo =
                            melissa_repository::Repository::open(path, Default::default(), vec![])?;
                        repo.snapshot(&AtomicBool::new(false))?
                            .files
                            .into_iter()
                            .filter(|f| f.path.to_lowercase().contains(&token[1..].to_lowercase()))
                            .map(|f| format!("@\"{}\"", f.path))
                            .collect::<Vec<_>>()
                    } else {
                        vec![]
                    };
                    let choice = if choices.len() == 1 {
                        Some(choices[0].clone())
                    } else {
                        ui.choose("Tamamla · yazıp ara", &choices)?
                            .map(|i| choices[i].clone())
                    };
                    if let Some(choice) = choice {
                        let prefix = if draft.starts_with('/') {
                            ""
                        } else {
                            draft
                                .rsplit_once(char::is_whitespace)
                                .map(|(s, _)| s)
                                .unwrap_or("")
                        };
                        input = ui::editor();
                        input.insert_str(format!(
                            "{}{}{} ",
                            prefix,
                            if prefix.is_empty() { "" } else { " " },
                            choice
                        ));
                    }
                } else if ui::control(&k, 'r') {
                    if let Some(index) = ui.choose("Mesaj geçmişi", &history)? {
                        input = ui::editor();
                        input.insert_str(&history[index]);
                    }
                } else if k.code == KeyCode::Up
                    && input.lines().len() == 1
                    && history_at > 0
                    && (draft.is_empty() || history_at < history.len())
                {
                    if history_at == history.len() {
                        history_draft = draft.clone();
                    }
                    history_at -= 1;
                    input = ui::editor();
                    input.insert_str(&history[history_at]);
                } else if k.code == KeyCode::Down && history_at < history.len() {
                    history_at += 1;
                    input = ui::editor();
                    if let Some(s) = history.get(history_at) {
                        input.insert_str(s);
                    } else {
                        input.insert_str(&history_draft);
                    }
                } else {
                    if k.modifiers.contains(
                        crossterm::event::KeyModifiers::CONTROL
                            | crossterm::event::KeyModifiers::ALT,
                    ) {
                        if let KeyCode::Char(c) = k.code {
                            input.insert_str(c.to_string());
                        }
                    } else {
                        input.input(k);
                    }
                }
            }
            _ => (),
        }
    }
    let key = conversation
        .as_ref()
        .map(|c| format!("session:{}", c.id))
        .unwrap_or_else(|| {
            workspace
                .as_ref()
                .map(|p| format!("project:{}", p.display()))
                .unwrap_or_else(|| "start".into())
        });
    let mut prefs = settings::read(&app)?;
    prefs
        .queues
        .insert(key.clone(), queue.into_iter().collect());
    prefs.drafts.insert(key, input.lines().join("\n"));
    settings::write(&app, &prefs)?;
    if let Some(c) = conversation.as_mut() {
        c.draft = input.lines().join("\n");
        c.save(&app)?;
    }
    Ok(())
}
