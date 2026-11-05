use crate::{
    Error, Result,
    app::{App, absolute, nonce, safe_path},
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

const HELP: &str = "Bir görev yaz veya aşağıdaki komutları kullan.\n\
  /doctor                 Windows ve Docker hazırlığını kontrol et\n\
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
}

struct Session {
    app: App,
    workspace: PathBuf,
    plan_dir: PathBuf,
    acceptance: Option<PathBuf>,
    draft: Option<Draft>,
    task: Option<Task>,
    ui: Ui,
}

pub async fn run(
    home: &Path,
    workspace: &Path,
    input: &mut impl BufRead,
    output: &mut impl Write,
) -> Result {
    let workspace = absolute(workspace)?;
    let home = absolute(home)?;
    if !home.join("home.json").is_file() {
        App::init(&home)?;
    }
    let mut session = Session {
        app: App::open(&home)?,
        workspace,
        plan_dir: home.join("session-plans"),
        acceptance: None,
        draft: None,
        task: None,
        ui: Ui::new(),
    };
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
    fn context(&self) -> &'static str {
        if self
            .task
            .as_ref()
            .is_some_and(|task| task.approval.is_some())
        {
            "ONAY BEKLİYOR  ·  /approve onayla  ·  /details tam kapsam"
        } else if self.task.is_some() {
            "GÖREV  ·  /run çalıştır  ·  /status durum  ·  /diff değişiklikler"
        } else if self.draft.as_ref().is_some_and(|draft| draft.ready) {
            "TARİF HAZIR  ·  /create görev oluştur  ·  /plan incele"
        } else {
            "YEREL PLANLAMA  ·  /help komutlar  ·  ↑↓ geçmiş  ·  Tab tamamla"
        }
    }

    async fn handle(&mut self, line: &str, output: &mut impl Write) -> Result {
        if !line.starts_with('/') {
            return self.prepare(line, output);
        }
        let (command, argument) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
        let argument = argument.trim();
        let result = match (command, argument) {
            ("/help", "") => return self.ui.help(HELP, output),
            ("/doctor", "") => crate::views::doctor()?,
            ("/config", path) => self.app.configure(&path_argument(path)?)?,
            ("/acceptance", path) => {
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
            ("/plan" | "/status", "") => self.app.progress(&self.task()?.id)?,
            ("/details", "") if self.task.is_none() => {
                let draft = self
                    .draft
                    .as_ref()
                    .ok_or(Error::Usage("Önce bir amaç yaz."))?;
                writeln!(output, "{}", display::json(&draft.plan)?)?;
                return self
                    .ui
                    .notice(&format!("Plan kaydı {}", draft.path.display()), output);
            }
            ("/details", "") => {
                let value = self.app.progress(&self.task()?.id)?;
                return Ok(writeln!(output, "{}", display::human(&value)?)?);
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
                return self
                    .ui
                    .notice("Onay kaydedildi. Başlatmak için /run yaz.", output);
            }
            ("/run" | "/resume", "") => {
                let id = &self.task()?.id;
                writeln!(output, "Yürütme isteği: {}", display::safe(id))?;
                output.flush()?;
                let run = Box::pin(self.app.run_task(id, command == "/resume"));
                self.ui.working(run, output).await?
            }
            ("/diff", "") => self.app.diff(&self.task()?.id)?,
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
        self.task = None;
        self.draft = None;
        safe_path(&self.plan_dir)?;
        fs::create_dir_all(&self.plan_dir)?;
        let path = self.plan_dir.join(format!("{}.json", nonce()?));
        let result = self
            .app
            .prepare(&self.workspace, goal, &path, self.acceptance.as_deref())?;
        let preparation = &result["preparation"];
        self.draft = Some(Draft {
            goal: goal.into(),
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
            .filter(|draft| draft.ready)
            .ok_or(Error::Usage(
                "Önce kabul tarifini seç ve görev amacını yaz.",
            ))?;
        let task = self.app.create_execution(
            &self.workspace,
            &draft.goal,
            vec![],
            Recipe::read(&draft.path)?,
        )?;
        self.select_task(&task, output)
    }

    fn select_task(&mut self, value: &Value, output: &mut impl Write) -> Result {
        let workspace = value["workspace"].as_str().ok_or(Error::Conflict(
            "Görev projesi doğrulanamadı. Etkin çalıştırıcı kapandıktan sonra tekrar aç.",
        ))?;
        if !workspace.eq_ignore_ascii_case(&self.workspace.to_string_lossy()) {
            return Err(Error::Denied("Bu görev başka bir proje klasörüne bağlı."));
        }
        let id = value["task_id"].as_str().ok_or(Error::Store)?.to_owned();
        let approval =
            if value["approval"]["pending"] == true && value["execution_plan"].is_object() {
                let request = value["approval"]["request_id"]
                    .as_str()
                    .ok_or(Error::Store)?;
                let digest = value["approval"]["digest"].as_str().ok_or(Error::Store)?;
                Some((request.to_owned(), digest.to_owned()))
            } else {
                None
            };
        self.ui.result(value, output)?;
        if approval.is_some() {
            self.ui
                .notice("Bu kapsamı onaylamak için /approve yaz.", output)?;
        }
        self.task = Some(Task { id, approval });
        Ok(())
    }
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
