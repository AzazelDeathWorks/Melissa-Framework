use crate::{
    Error, Result,
    app::{App, absolute, nonce, safe_path},
    display,
    recipe::Recipe,
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
    };
    writeln!(output, "Melissa terminal oturumu")?;
    writeln!(
        output,
        "Proje: {}",
        display::safe(&session.workspace.to_string_lossy())
    )?;
    writeln!(
        output,
        "Görevini yaz. Komutlar için /help, çıkmak için /exit."
    )?;
    let mut line = String::new();
    loop {
        write!(output, "melissa> ")?;
        output.flush()?;
        line.clear();
        if input.read_line(&mut line)? == 0 || line.trim() == "/exit" {
            return Ok(());
        }
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Err(error) = session.handle(line, output).await {
            writeln!(output, "{error}")?;
        }
    }
}

impl Session {
    async fn handle(&mut self, line: &str, output: &mut impl Write) -> Result {
        if !line.starts_with('/') {
            return self.prepare(line, output);
        }
        let (command, argument) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
        let argument = argument.trim();
        let result = match (command, argument) {
            ("/help", "") => return Ok(writeln!(output, "{HELP}")?),
            ("/doctor", "") => crate::views::doctor()?,
            ("/config", path) => self.app.configure(&path_argument(path)?)?,
            ("/acceptance", path) => {
                let path = path_argument(path)?;
                Recipe::read(&path)?;
                self.acceptance = Some(path);
                self.draft = None;
                self.task = None;
                return Ok(writeln!(
                    output,
                    "Kabul tarifi seçildi. Görevin amacını yaz."
                )?);
            }
            ("/plan", "") if self.task.is_none() => return self.show_draft(output),
            ("/plan" | "/status", "") => self.app.progress(&self.task()?.id)?,
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
                return Ok(writeln!(
                    output,
                    "Onay kaydedildi. Başlatmak için /run yaz."
                )?);
            }
            ("/run" | "/resume", "") => {
                let id = &self.task()?.id;
                writeln!(output, "Yürütme isteği: {}", display::safe(id))?;
                output.flush()?;
                self.app.run_task(id, command == "/resume").await?
            }
            ("/diff", "") => self.app.diff(&self.task()?.id)?,
            ("/delivery", "") => self.app.delivery(&self.task()?.id, None)?,
            _ => return Err(Error::Usage("Oturum komutu geçersiz. /help yaz.")),
        };
        writeln!(output, "{}", display::human(&result)?)?;
        Ok(())
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
        writeln!(output, "{}", display::json(&draft.plan)?)?;
        writeln!(
            output,
            "Yerel plan: {}",
            display::safe(&draft.path.to_string_lossy())
        )?;
        let next = if draft.ready {
            "Tarifi incele, model ayarlarını /config ile yükle ve /create yaz."
        } else {
            "Yürütme için proje dışındaki kabul tarifini /acceptance DOSYA ile seç ve amacı tekrar yaz."
        };
        writeln!(output, "{next}")?;
        Ok(())
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
        writeln!(output, "{}", display::human(value)?)?;
        if approval.is_some() {
            writeln!(output, "Bu kapsamı onaylamak için /approve yaz.")?;
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
