use crate::{
    Error, Result,
    app::{App, absolute, nonce, safe_path},
    conversation::Mode,
    display,
    recipe::Recipe,
    session_ui::Ui,
};
use serde_json::Value;
use std::{
    fs,
    io::{BufRead, Write},
    path::{Path, PathBuf},
};
mod frontend;
pub(crate) use frontend::FrontendSession;

const HELP: &str = "Bir görev yaz veya aşağıdaki komutları kullan.\n\
  /mode plan|review|develop  Çalışma kipini seç\n\
  /new AMAÇ               Kayıtları koruyarak ayrı görev hazırla\n\
  /instructions           Geçerli kullanıcı/proje talimatlarını göster\n\
  /steer MESAJ            Görevi sıfırlamadan yönlendir\n\
  /questions              Yanıt bekleyen sorular\n\
  /answer KİMLİK YANIT     Hedefli soruyu yanıtla\n\
  /scope DOSYA            Ek dosya için kapsam farkı hazırla\n\
  /service AD             Ek servis gereksinimini kaydet\n\
  /scope-approve ÖZET      Gösterilen hazırlık kapsamını kabul et\n\
  /pause  /cancel         Güvenli duraklatma veya iptal iste\n\
  /doctor                 Windows ve Docker hazırlığını kontrol et\n\
  /diagnose · /metrics · /disk · /retention-preview · /support KLASOR · /otlp DOSYA\n\
  /checkpoints · /changes · /checkpoint ETIKET · /undo-preview JSON · /undo JSON\n\
  /adopt-preview · /adopt-source DIGEST   Değişmiş kaynağı açık incele\n\
  /services [DOSYA]       Yeni görevler için servis/API/DB tercihleri\n\
  /verification [DOSYA]   Proje kontrol/debug tercihini göster veya yeni görevlere ayarla\n\
  /config DOSYA           Özel model ayarlarını içe aktar\n\
  /acceptance DOSYA       Proje dışındaki kabul tarifini seç\n\
  /plan                   Son hazırlığı veya görev planını göster\n\
  /details                Kayda bağlı tam plan ve kapsamı göster\n\
  /create                 Hazır tarifi görev olarak kaydet ve onay kapsamını göster\n\
  /approve                Ekranda gösterilen kesin görev kapsamını onayla\n\
  /run                    Onaylı görevi çalıştır (API ücreti doğabilir)\n\
  /open GÖREV_KİMLİĞİ      Bu projenin önceki görevini aç\n\
  /status  /diff          Durumu veya değişiklik özetini göster\n\
  /resume  /delivery      Güvenli checkpoint'i sürdür veya doğrulanmış teslimi göster\n\
  /help    /exit          Yardım veya çıkış";

struct Draft {
    goal: String,
    path: PathBuf,
    plan: Value,
    ready: bool,
}

struct Task {
    id: String,
    approval: Option<(String, String)>,
    executable: bool,
}

struct Session {
    environment: crate::project::Environment,
    budget: Option<u64>,
    duration: u64,
    extra_checks: std::collections::BTreeMap<String, crate::project::Command>,
    app: App,
    workspace: PathBuf,
    plan_dir: PathBuf,
    acceptance: Option<PathBuf>,
    draft: Option<Draft>,
    task: Option<Task>,
    mode: Mode,
    ui: Ui,
}

pub async fn run(
    home: &Path,
    workspace: &Path,
    input: &mut impl BufRead,
    output: &mut impl Write,
) -> Result {
    #[cfg(windows)]
    if std::io::IsTerminal::is_terminal(&std::io::stdin())
        && std::io::IsTerminal::is_terminal(&std::io::stdout())
    {
        return crate::console::run(home, workspace).await;
    }
    let mut session = Session::new(home, workspace, Ui::new())?;
    session.ui.welcome(
        &session.workspace,
        session.app.configuration().is_ok(),
        output,
    )?;
    loop {
        let context = session.context();
        let Some(line) = session.ui.read_line(input, output, context)? else {
            return Ok(());
        };
        if line.trim() == "/exit" {
            return Ok(());
        }
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Err(error) = session.handle(line, output).await {
            session.ui.notice(&error.to_string(), output)?;
        }
    }
}

impl Session {
    fn new(home: &Path, workspace: &Path, ui: Ui) -> Result<Self> {
        let home = absolute(home)?;
        let workspace = absolute(workspace)?;
        if !home.join("home.json").is_file() {
            App::init(&home)?;
        }
        let app = App::open(&home)?;
        let environment = app.project_environment(&workspace)?;
        Ok(Self {
            environment,
            budget: None,
            duration: 3_600_000,
            extra_checks: Default::default(),
            app,
            workspace,
            plan_dir: home.join("session-plans"),
            acceptance: None,
            draft: None,
            task: None,
            mode: Mode::Develop,
            ui,
        })
    }

    fn context(&self) -> &'static str {
        if self
            .task
            .as_ref()
            .is_some_and(|task| task.approval.is_some())
        {
            "ONAY BEKLİYOR  ·  /approve onayla  ·  /details tam kapsam"
        } else if self.task.as_ref().is_some_and(|t| t.executable) {
            "GÖREV  ·  /run çalıştır  ·  /status durum  ·  /diff değişiklikler"
        } else if self.task.is_some() {
            "PROJE HAZIRLIĞI · mesajla yönlendir · /scope ek kapsam · /status durum"
        } else if self.draft.as_ref().is_some_and(|draft| draft.ready) {
            "TARİF HAZIR  ·  /create görev oluştur  ·  /plan incele"
        } else {
            "YEREL PLANLAMA  ·  /help komutlar  ·  ↑↓ geçmiş  ·  Tab tamamla"
        }
    }

    async fn handle(&mut self, line: &str, output: &mut impl Write) -> Result {
        if !line.starts_with('/') {
            if let Some(task) = &self.task {
                let value = self.app.steer(&task.id, line)?;
                return self.ui.notice(&format!("Yönlendirme kaydedildi · revizyon {}. Görev ve harcama geçmişi korundu; etkin yürütme varsa sonraki güvenli model turunda okunur.",value["revision"]),output);
            }
            return self.prepare(line, output);
        }
        let (command, argument) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
        let argument = argument.trim();
        let result = match (command, argument) {
            ("/help", "") => return self.ui.help(HELP, output),
            ("/skills", "") | ("/extensions", "") => self.app.extension_catalog(&self.workspace)?,
            ("/tasks", "") => self.app.list()?,
            ("/memory", "") => self.app.memory(&self.workspace, "list", None, None)?,
            ("/memory", argument) => {
                let parts: Vec<_> = argument.split_whitespace().collect();
                if parts.len() != 3 || !["accept", "reject", "forget"].contains(&parts[0]) {
                    return Err(Error::Usage("/memory accept|reject|forget ID DIGEST"));
                }
                self.app
                    .memory(&self.workspace, parts[0], Some(parts[1]), Some(parts[2]))?
            }
            ("/config-show", "") => self.app.configuration()?,
            ("/extensions", path) => self
                .app
                .configure_extensions(&self.workspace, &path_argument(path)?)?,
            ("/skill-import", path) if !path.is_empty() => {
                self.app.import_skill(&path_argument(path)?, None)?
            }
            ("/doctor", "") => crate::views::doctor()?,
            ("/services", "") => {
                serde_json::json!({"services":crate::services::settings::load(&self.app,&self.workspace)?})
            }
            ("/services", path) => crate::services::settings::configure(
                &self.app,
                &self.workspace,
                &path_argument(path)?,
            )?,
            ("/verification", "") => {
                serde_json::json!({"verification":crate::project_verification::settings::load(&self.app,&self.workspace)?})
            }
            ("/verification", path) => crate::project_verification::settings::configure(
                &self.app,
                &self.workspace,
                Path::new(path),
            )?,
            ("/project-doctor", "") => self
                .app
                .project_doctor(&self.workspace, self.environment.clone())?,
            ("/environment", "") => {
                serde_json::json!({"command":"project environment","environment":self.environment})
            }
            ("/environment", value) => {
                let environment = if value.starts_with("sha256:") {
                    let mut e = self.environment.clone();
                    e.image_id = value.into();
                    e
                } else {
                    let path = path_argument(value)?;
                    let repo = melissa_repository::Repository::open(
                        path.parent().ok_or(Error::Io)?,
                        Default::default(),
                        vec![],
                    )?;
                    crate::app::decode(
                        &repo
                            .read(path.file_name().and_then(|s| s.to_str()).ok_or(Error::Io)?)?
                            .1,
                    )?
                };
                let result = self.app.save_environment(&self.workspace, environment)?;
                self.environment = self.app.project_environment(&self.workspace)?;
                result
            }
            ("/budget", value) => {
                self.budget = Some(dollars(value)?);
                return self.ui.notice(
                    "Görev harcama tercihi kaydedildi; model çıktı kapasitesi değiştirilmedi.",
                    output,
                );
            }
            ("/duration", value) => {
                let minutes = value
                    .parse::<u64>()
                    .map_err(|_| Error::Usage("/duration DAKİKA"))?;
                self.duration = minutes
                    .checked_mul(60_000)
                    .filter(|n| *n > 0)
                    .ok_or(Error::Usage("Süre geçersiz."))?;
                self.environment.limits.wall_time_ms = self.duration;
                self.environment.limits.cpu_time_ms = self.duration;
                self.environment.validate()?;
                return self.ui.notice("Toplam görev ve yeni süreç süresi tercihi kaydedildi; onaylı eski görev değişmez.",output);
            }
            ("/check", value) => {
                let command:crate::project::Command=serde_json::from_str(value).map_err(|_|Error::Usage("/check {\"executable\":\"python\",\"argv\":[\"-m\",\"unittest\",\"discover\"],\"cwd\":\".\"}"))?;
                command.validate()?;
                self.extra_checks
                    .insert(format!("operator-{}", self.extra_checks.len() + 1), command);
                return self.ui.notice(
                    "Proje kabul komutu eklendi; /create onay kapsamında gösterilecek.",
                    output,
                );
            }
            ("/preflight" | "/environment-sync" | "/exec", value) => {
                let operation = match command {
                    "/preflight" => "preflight",
                    "/environment-sync" => "sync",
                    _ => "exec",
                };
                let cmd = if operation == "exec" {
                    Some(serde_json::from_str(value).map_err(|_| {
                        Error::Usage("/exec için executable/argv/cwd JSON nesnesi gerekli.")
                    })?)
                } else {
                    None
                };
                let id = self.task()?.id.clone();
                let run = Box::pin(self.app.run_project_operation(&id, operation, cmd));
                let mut previous = String::new();
                self.ui
                    .working(run, output, |line| {
                        running_control(&self.app, &id, line, &mut previous)
                    })
                    .await?
            }
            ("/new", goal) if !goal.is_empty() => {
                if self
                    .task
                    .as_ref()
                    .is_some_and(|t| self.app.live(&t.id).is_ok_and(|s| s.active()))
                {
                    return Err(Error::Conflict(
                        "Önce etkin görevi duraklat veya bitmesini bekle.",
                    ));
                }
                self.task = None;
                self.draft = None;
                return self.prepare(goal, output);
            }
            ("/mode", mode) => {
                let mode = Mode::parse(mode)?;
                if let Some(task) = &self.task {
                    self.app.conversation_mode(&task.id, mode)?;
                }
                self.mode = mode;
                if let Some(draft) = &self.draft {
                    let goal = draft.goal.clone();
                    return self.prepare(&goal, output);
                }
                return self.ui.notice(
                    &format!("Kip: {mode:?}. Çalışma yetkisi bu komutla genişlemez."),
                    output,
                );
            }
            ("/instructions", "") => self.app.project_guidance(&self.workspace)?,
            ("/steer", message) => self.app.steer(&self.task()?.id, message)?,
            ("/scope", path) if !path.is_empty() => self.app.propose_scope(
                &self.task()?.id,
                std::collections::BTreeSet::from([path.to_owned()]),
                Default::default(),
            )?,
            ("/service", name) if !name.is_empty() => self.app.propose_scope(
                &self.task()?.id,
                Default::default(),
                std::collections::BTreeSet::from([name.to_owned()]),
            )?,
            ("/scope-approve", hash) if !hash.is_empty() => {
                self.app.approve_scope(&self.task()?.id, hash)?
            }
            ("/questions", "") => self.app.conversation_view(&self.task()?.id)?,
            ("/answer", answer) => {
                let (id, answer) = answer
                    .split_once(char::is_whitespace)
                    .ok_or(Error::Usage("/answer SORU_KİMLİĞİ YANIT"))?;
                self.app
                    .answer_question(&self.task()?.id, id, answer.trim())?
            }
            ("/pause", "") => self.app.request_control(&self.task()?.id, "pause")?,
            ("/cancel", "") => self.app.cancel(&self.task()?.id)?,
            ("/config", path) => self.app.configure(&path_argument(path)?)?,
            ("/acceptance", path) => {
                if self.task.is_some() {
                    return Err(Error::Conflict(
                        "Seçili görev korunur. Yeni kabul tarifi için /new ile ayrı görev hazırla.",
                    ));
                }
                let path = path_argument(path)?;
                Recipe::read(&path)?;
                self.acceptance = Some(path);
                self.draft = None;
                self.task = None;
                return self
                    .ui
                    .notice("Kabul tarifi seçildi. Görevin amacını yaz.", output);
            }
            ("/plan", "") if self.task.is_none() => return self.show_draft(output),
            ("/plan", "") => self.app.conversation_view(&self.task()?.id)?,
            ("/diagnose", "") => self.app.diagnose(&self.task()?.id)?,
            ("/metrics", "") => self.app.metrics(&self.task()?.id)?,
            ("/disk", "") => self.app.disk(&self.task()?.id)?,
            ("/retention-preview", "") => self.app.retention_preview(&self.task()?.id)?,
            ("/support", path) => self.app.support(&self.task()?.id, Path::new(path))?,
            ("/otlp", path) => {
                self.app
                    .export_otlp(&self.task()?.id, Some(Path::new(path)), None)
                    .await?
            }
            ("/checkpoints", "") | ("/continuity", "") => self.app.continuity(&self.task()?.id)?,
            ("/changes", "") => crate::continuity::undo::changes(&self.app, &self.task()?.id)?,
            ("/checkpoint", label) => self.app.checkpoint(&self.task()?.id, label)?,
            ("/adopt-preview", "") => self.app.adopt_preview(&self.task()?.id)?,
            ("/adopt-source", digest) => self.app.adopt_source(&self.task()?.id, digest)?,
            ("/undo-preview", input) => {
                let request = serde_json::from_value(
                    melissa_model_gateway::strict_json(input)
                        .map_err(|_| Error::Usage("Geri alma JSON geçersiz."))?,
                )
                .map_err(|_| Error::Usage("Geri alma seçimi geçersiz."))?;
                self.app.undo_preview(&self.task()?.id, request)?
            }
            ("/undo", input) => {
                #[derive(serde::Deserialize)]
                #[serde(deny_unknown_fields)]
                struct Input {
                    selection: crate::continuity::undo::Request,
                    digest: String,
                }
                let request: Input = serde_json::from_value(
                    melissa_model_gateway::strict_json(input)
                        .map_err(|_| Error::Usage("Geri alma JSON geçersiz."))?,
                )
                .map_err(|_| Error::Usage("Geri alma incelemesi geçersiz."))?;
                self.app
                    .undo(&self.task()?.id, request.selection, &request.digest)?
            }
            ("/status", "") => {
                let id = &self.task()?.id;
                let mut result = self.app.progress(id)?;
                result["conversation"] = self.app.conversation_view(id)?;
                result
            }
            ("/details", "") if self.task.is_none() => {
                let draft = self
                    .draft
                    .as_ref()
                    .ok_or(Error::Usage("Önce bir amaç yaz."))?;
                self.ui.details(&draft.plan, output)?;
                return self
                    .ui
                    .notice(&format!("Plan kaydı {}", draft.path.display()), output);
            }
            ("/details", "") => {
                let value = self.app.progress(&self.task()?.id)?;
                return self.ui.details(&value, output);
            }
            ("/create", "") => return self.create(output),
            ("/open", id) if !id.is_empty() => {
                return self.select_task(&self.app.show(id)?, output);
            }
            ("/approve", "") => {
                let task = self.task()?;
                let (request, digest) = task.approval.as_ref().ok_or(Error::Usage(
                    "Gösterilen ve onay bekleyen bir yürütme planı gerekli.",
                ))?;
                self.app.approve(&task.id, request, digest)?;
                self.task.as_mut().ok_or(Error::Store)?.approval = None;
                return self.ui.notice(if self.task()?.executable {"Onay kaydedildi. Başlatmak için /run yaz."}else{"Plan onayı kaydedildi. Bu kayıt yalnız hazırlıktır. Yürütme için /new, model ayarı, /budget ve proje kontrolleriyle yeni kapsam oluştur."},output);
            }
            ("/run" | "/resume", "") => {
                if !self.task()?.executable {
                    return Err(Error::Unavailable(
                        "Bu kayıt proje hazırlığıdır; API çağrısı yapılmadı. /new ardından model ayarı, /budget ve proje kontrolleriyle yürütme kapsamı oluştur.",
                    ));
                }
                let id = &self.task()?.id;
                writeln!(output, "Yürütme isteği: {}", display::safe(id))?;
                output.flush()?;
                let run = Box::pin(self.app.run_task(id, command == "/resume"));
                let mut previous = String::new();
                self.ui
                    .working(run, output, |line| {
                        running_control(&self.app, id, line, &mut previous)
                    })
                    .await?
            }
            ("/diff", "") => self.app.diff(&self.task()?.id)?,
            ("/diff-content", "") => self.app.diff_content(&self.task()?.id)?,
            ("/logs", "") => self.app.logs(&self.task()?.id)?,
            ("/preview", "") => self.app.preview(&self.task()?.id, None)?,
            ("/delivery", "") => self.app.delivery(&self.task()?.id, None)?,
            _ => return Err(Error::Usage("Oturum komutu geçersiz. /help yaz.")),
        };
        self.ui.result(&result, output)
    }

    fn task(&self) -> Result<&Task> {
        self.task.as_ref().ok_or(Error::Usage(
            "Görev seçili değil. Bir amaç yaz veya /open GÖREV_KİMLİĞİ kullan.",
        ))
    }

    fn prepare(&mut self, goal: &str, output: &mut impl Write) -> Result {
        safe_path(&self.plan_dir)?;
        fs::create_dir_all(&self.plan_dir)?;
        let path = self.plan_dir.join(format!("{}.json", nonce()?));
        let goal = if let Some(draft) = &self.draft {
            if draft.goal != goal {
                format!("{}\nEk yönlendirme: {goal}", draft.goal)
            } else {
                goal.into()
            }
        } else {
            goal.into()
        };
        let result = self.app.prepare_mode(
            &self.workspace,
            &goal,
            &path,
            self.acceptance.as_deref(),
            self.mode,
        )?;
        let preparation = &result["preparation"];
        self.task = None;
        self.draft = Some(Draft {
            goal,
            path,
            plan: preparation["adaptive_plan"].clone(),
            ready: preparation["ready_to_create"] == true,
        });
        self.show_draft(output)
    }

    fn show_draft(&self, output: &mut impl Write) -> Result {
        let draft = self
            .draft
            .as_ref()
            .ok_or(Error::Usage("Önce bir amaç yaz."))?;
        self.ui.draft(&draft.plan, &draft.path, draft.ready, output)
    }

    fn create(&mut self, output: &mut impl Write) -> Result {
        if self.task.is_some() {
            return Err(Error::Conflict("Bu oturumda zaten bir görev seçili."));
        }
        let draft = self
            .draft
            .as_ref()
            .ok_or(Error::Usage("Önce görev amacını yaz."))?;
        let task = if draft.ready && self.mode != Mode::Plan {
            let mut recipe = Recipe::read(&draft.path)?;
            if self.mode == Mode::Review {
                for step in &mut recipe.agents {
                    step.write = false;
                }
                recipe.writable_paths.clear();
            }
            let task = self
                .app
                .create_execution(&self.workspace, &draft.goal, vec![], recipe)?;
            self.app.attach_conversation(
                task["task_id"].as_str().ok_or(Error::Store)?,
                self.mode,
                draft.plan.clone(),
            )?;
            task
        } else if self.mode != Mode::Plan
            && self.budget.is_some()
            && self.app.configuration().is_ok()
        {
            let repo =
                melissa_repository::Repository::open(&self.workspace, Default::default(), vec![])?;
            let discovery = crate::project::discovery::discover(&repo)?;
            let mut contract = if self.extra_checks.is_empty() {
                discovery.contract(self.environment.clone())?
            } else {
                discovery.contract_with_checks(
                    self.environment.clone(),
                    Some(self.extra_checks.clone()),
                )?
            };
            if let Some(spec) =
                crate::project_verification::settings::load(&self.app, &self.workspace)?
            {
                spec.validate(&contract.checks)?;
                contract.verification = Some(spec);
            }
            if let Some(spec) = crate::services::settings::load(&self.app, &self.workspace)? {
                spec.validate(&contract)?;
                contract.services = Some(spec);
            }
            self.app.create_project(
                &self.workspace,
                &draft.goal,
                self.mode,
                draft.plan.clone(),
                contract,
                self.budget.ok_or(Error::Store)?,
                self.duration,
            )?
        } else {
            self.app.create_conversation(
                &self.workspace,
                &draft.goal,
                self.mode,
                draft.plan.clone(),
            )?
        };
        self.select_task(&task, output)
    }

    fn select_task(&mut self, value: &Value, output: &mut impl Write) -> Result {
        let stored_brief = value["task_id"]
            .as_str()
            .map(|id| self.app.conversation(id))
            .transpose()?
            .flatten();
        let workspace = value["workspace"]
            .as_str()
            .or_else(|| stored_brief.as_ref().map(|b| b.workspace.as_str()))
            .ok_or(Error::Conflict(
                "Görev projesi doğrulanamadı. Etkin çalıştırıcı kapandıktan sonra tekrar aç.",
            ))?;
        // Task records store the path resolved from the repository handle.
        // Windows can supply the session with a short name for that same root.
        let repo =
            melissa_repository::Repository::open(&self.workspace, Default::default(), vec![])?;
        if Path::new(workspace) != repo.root_path() {
            return Err(Error::Denied("Bu görev başka bir proje klasörüne bağlı."));
        }
        let id = value["task_id"].as_str().ok_or(Error::Store)?.to_owned();
        if self.app.conversation(&id)?.is_none() {
            self.app
                .attach_conversation(&id, Mode::Develop, value["execution_plan"].clone())?;
        }
        if let Some(brief) = self.app.conversation(&id)? {
            self.mode = brief.mode;
        }
        let approval = if value["approval"]["pending"] == true {
            let request = value["approval"]["request_id"]
                .as_str()
                .ok_or(Error::Store)?;
            let digest = value["approval"]["digest"].as_str().ok_or(Error::Store)?;
            Some((request.to_owned(), digest.to_owned()))
        } else {
            None
        };
        self.ui.result(value, output)?;
        self.ui.result(&self.app.conversation_view(&id)?, output)?;
        if approval.is_some() {
            self.ui
                .notice("Bu kapsamı onaylamak için /approve yaz.", output)?;
        }
        let executable =
            value["execution_plan"].is_object() || self.app.live(&id).is_ok_and(|s| s.active());
        self.task = Some(Task {
            id,
            approval,
            executable,
        });
        self.draft = None;
        Ok(())
    }
}

pub(crate) fn running_control(
    app: &App,
    id: &str,
    line: Option<&str>,
    previous: &mut String,
) -> Result<Option<String>> {
    if let Some(line) = line {
        let (command, arg) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
        let arg = arg.trim();
        let value=match command {
            "/pause"|"/cancel"=>app.request_control(id,&command[1..])?,
            "/exit"=>{app.request_control(id,"pause")?;return Ok(Some("Güvenli duraklatma istendi. Görev durduktan sonra /exit ile çıkabilirsin.".into()));}
            "/answer"=>{let (q,a)=arg.split_once(char::is_whitespace).ok_or(Error::Usage("/answer SORU_KİMLİĞİ YANIT"))?;app.answer_question(id,q,a.trim())?}
            "/scope"=>app.propose_scope(id,std::collections::BTreeSet::from([arg.to_owned()]),Default::default())?,
            "/service"=>app.propose_scope(id,Default::default(),std::collections::BTreeSet::from([arg.to_owned()]))?,
            "/scope-approve"=>app.approve_scope(id,arg)?,
            "/status"=>{previous.clear();return running_control(app,id,None,previous);}
            "/questions"=>app.conversation_view(id)?,
            "/steer"=>app.steer(id,arg)?,
            ""=>return Ok(None),
            _ if line.starts_with('/')=>return Ok(Some("Çalışırken: mesaj, /answer, /scope, /service, /scope-approve, /status, /pause veya /cancel.".into())),
            _=>app.steer(id,line)?,
        };
        let message = if let Some(delta) = value["brief"]["pending_scope"].as_object() {
            format!(
                "Ek dosyalar: {}\nEk servisler: {}\nİnceledikten sonra /scope-approve {}\nBu fark hazırlık kapsamıdır; çalışan tarife yetki eklemez.",
                delta["add_writes"],
                delta["add_services"],
                delta["digest"].as_str().ok_or(Error::Store)?
            )
        } else if matches!(command, "/status" | "/questions") {
            let questions = value["brief"]["questions"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|q| q["answer"].is_null())
                .map(|q| {
                    format!(
                        "Soru {}: {}\n/answer {} YANIT",
                        q["id"].as_str().unwrap_or("?"),
                        q["text"].as_str().unwrap_or("?"),
                        q["id"].as_str().unwrap_or("?")
                    )
                })
                .collect::<Vec<_>>();
            if questions.is_empty() {
                format!("Görev {id} · bekleyen soru yok. Aşama ve olaylar canlı akışta gösterilir.")
            } else {
                questions.join("\n")
            }
        } else if matches!(command, "/pause" | "/cancel") {
            format!(
                "{} isteği kaydedildi; yürütücünün duruş onayı bekleniyor.",
                &command[1..]
            )
        } else {
            format!(
                "Kaydedildi · revizyon {}. Görev ve harcama geçmişi korundu.",
                value["revision"]
            )
        };
        return Ok(Some(message));
    }
    let live = match app.live(id) {
        Ok(v) => v,
        Err(_) => return Ok(None),
    };
    let brief = app.conversation(id)?;
    let questions = brief
        .as_ref()
        .map(|b| {
            b.questions
                .iter()
                .filter(|q| q.answer.is_none())
                .map(|q| format!("Soru {}: {}\n/answer {} YANIT", q.id, q.text, q.id))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let message = format!(
        "{} · olay {} · kayıtlı maliyet {} mikro USD{}",
        live.phase,
        live.events["total"],
        live.model_budget["known_cost_micro_usd"],
        if questions.is_empty() {
            String::new()
        } else {
            format!("\n{}", questions.join("\n"))
        }
    );
    if &message == previous {
        return Ok(None);
    }
    *previous = message.clone();
    Ok(Some(message))
}

fn path_argument(path: &str) -> Result<PathBuf> {
    let path = path
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or(path);
    if path.is_empty() {
        return Err(Error::Usage("Dosya yolu gerekli."));
    }
    absolute(Path::new(path))
}
fn dollars(s: &str) -> Result<u64> {
    let (whole, part) = s.split_once('.').unwrap_or((s, ""));
    if whole.is_empty()
        || !whole.bytes().all(|b| b.is_ascii_digit())
        || part.len() > 6
        || !part.bytes().all(|b| b.is_ascii_digit())
    {
        return Err(Error::Usage(
            "/budget pozitif USD tutarı; en fazla altı ondalık basamak.",
        ));
    }
    let base = whole
        .parse::<u64>()
        .ok()
        .and_then(|n| n.checked_mul(1_000_000))
        .ok_or(Error::Usage("Bütçe taşması."))?;
    let fraction = if part.is_empty() {
        0
    } else {
        part.parse::<u64>()
            .map_err(|_| Error::Usage("Bütçe geçersiz."))?
            * 10u64.pow(6 - part.len() as u32)
    };
    base.checked_add(fraction)
        .filter(|n| *n > 0 && *n <= i64::MAX as u64)
        .ok_or(Error::Usage("Bütçe geçersiz."))
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    #[tokio::test]
    async fn session_records_recipe_free_project_and_steers_same_task_then_opens_separate_work_explicitly()
     {
        let t = tempfile::tempdir().unwrap();
        let home = t.path().join("home");
        let workspace = t.path().join("project");
        fs::create_dir(&workspace).unwrap();
        fs::write(workspace.join("main.py"), "pass\n").unwrap();
        App::init(&home).unwrap();
        let mut s = Session {
            environment: Default::default(),
            budget: None,
            duration: 3_600_000,
            extra_checks: Default::default(),
            app: App::open(&home).unwrap(),
            workspace: workspace.clone(),
            plan_dir: home.join("session-plans"),
            acceptance: None,
            draft: None,
            task: None,
            mode: Mode::Develop,
            ui: Ui::new(),
        };
        let mut out = vec![];
        s.handle("main.py hatasını düzelt", &mut out).await.unwrap();
        s.handle("Mevcut API'yi koru", &mut out).await.unwrap();
        assert!(
            s.draft
                .as_ref()
                .unwrap()
                .goal
                .contains("main.py hatasını düzelt")
        );
        s.handle("/create", &mut out).await.unwrap();
        let first = s.task().unwrap().id.clone();
        let before = s.app.show(&first).unwrap();
        s.handle("Önce kök nedeni anlat", &mut out).await.unwrap();
        assert_eq!(s.task().unwrap().id, first);
        s.handle("/scope extra.py", &mut out).await.unwrap();
        let b = s.app.conversation(&first).unwrap().unwrap();
        let hash = b.pending_scope.unwrap()["digest"]
            .as_str()
            .unwrap()
            .to_owned();
        s.handle(&format!("/scope-approve {hash}"), &mut out)
            .await
            .unwrap();
        s.handle("/mode review", &mut out).await.unwrap();
        assert_eq!(s.task().unwrap().id, first);
        s.handle("/status", &mut out).await.unwrap();
        let after = s.app.show(&first).unwrap();
        assert_eq!(before["model_budget"], after["model_budget"]);
        assert_eq!(before["approval"], after["approval"]);
        assert!(
            s.handle("/acceptance missing.json", &mut out)
                .await
                .is_err()
        );
        assert_eq!(s.task().unwrap().id, first);
        s.handle("/new main.py incele", &mut out).await.unwrap();
        s.handle("/create", &mut out).await.unwrap();
        assert_ne!(s.task().unwrap().id, first);
        assert_eq!(
            s.app.conversation(&first).unwrap().unwrap().messages.len(),
            1
        );
        assert!(
            s.app
                .conversation(&s.task().unwrap().id)
                .unwrap()
                .unwrap()
                .scope
                .writes
                .is_empty()
        );
        let shown = String::from_utf8(out).unwrap();
        assert!(shown.contains(&hash));
    }
    #[tokio::test]
    async fn pipe_session_can_prepare_and_record_without_model_config_or_private_acceptance() {
        let t = tempfile::tempdir().unwrap();
        let home = t.path().join("home");
        let repo = t.path().join("project");
        fs::create_dir(&repo).unwrap();
        fs::write(repo.join("main.py"), "pass\n").unwrap();
        let mut input =
            std::io::Cursor::new(b"/mode plan\nmain.py incele\n/create\n/status\n/exit\n");
        let mut output = vec![];
        run(&home, &repo, &mut input, &mut output).await.unwrap();
        let app = App::open(&home).unwrap();
        let tasks = app.list().unwrap();
        assert_eq!(tasks["tasks"].as_array().unwrap().len(), 1);
        let id = tasks["tasks"][0]["task_id"].as_str().unwrap();
        assert_eq!(app.conversation(id).unwrap().unwrap().mode, Mode::Plan);
        assert!(!app.show(id).unwrap()["execution_plan"].is_object());
        assert!(
            app.store(id)
                .unwrap()
                .journal()
                .model_requests(id)
                .unwrap()
                .is_empty()
        );
    }
}
