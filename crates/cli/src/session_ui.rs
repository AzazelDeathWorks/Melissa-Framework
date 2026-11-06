mod input;

use crate::{Result, display};
use crossterm::{
    cursor::MoveToColumn,
    queue,
    style::{Color, Stylize},
    terminal::{Clear, ClearType},
};
use serde_json::Value;
use std::{
    future::Future,
    io::{IsTerminal, Write},
    path::Path,
    time::Duration,
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

const ACCENT: Color = Color::Rgb {
    r: 167,
    g: 139,
    b: 250,
};
const MUTED: Color = Color::Rgb {
    r: 148,
    g: 163,
    b: 184,
};
const NOTICE: Color = Color::Rgb {
    r: 251,
    g: 191,
    b: 36,
};

pub(super) const COMMANDS: &[(&str, &str)] = &[
    ("/checkpoints", "Görev checkpoint ve kalan iş görünümü"),
    ("/continuity", "Kaynak, bütçe ve devam durumu"),
    ("/changes", "Tur bazlı native dosya değişiklikleri"),
    ("/checkpoint", "ETIKET · kaynak kaydı, yeni yetki vermez"),
    (
        "/undo-preview",
        "JSON · seçili makbuz/parça geri alma incelemesi",
    ),
    ("/undo", "JSON {selection,digest} · incelenmiş geri alma"),
    (
        "/adopt-preview",
        "Değişmiş kaynağı ve geçersizleşecek eski çıktıları incele",
    ),
    (
        "/adopt-source",
        "DIGEST · aynı bütçeyle fresh uzman koşuları",
    ),
    (
        "/project-doctor",
        "Proje, kilit dosyaları, komutlar ve ortam tercihi",
    ),
    (
        "/environment",
        "IMAGE veya JSON DOSYASI · projeye özel ortam",
    ),
    ("/budget", "USD · yeni görevin harcama tercihi"),
    ("/duration", "DAKİKA · yeni görev/süreç süresi"),
    ("/check", "JSON KOMUT · yeni göreve kabul kontrolü ekle"),
    (
        "/preflight",
        "Onaylı proje ortamında sürüm/kapasite kontrolü",
    ),
    (
        "/environment-sync",
        "Onaylı bağımlılıkları izole kopyada hazırla",
    ),
    (
        "/exec",
        "JSON KOMUT · onaylı projede model çağrısı olmadan çalıştır",
    ),
    ("/help", "Komutlar ve kısayollar"),
    ("/doctor", "Windows ve Docker hazırlığı"),
    (
        "/services",
        "[DOSYA] · yeni görevlerin servis/API/DB tercihi",
    ),
    (
        "/verification",
        "[DOSYA] · proje kontrol/debug tercihi; yeni görevler için",
    ),
    ("/skills", "Kullanıcı/proje skill kataloğu ve sabit pinler"),
    (
        "/skill-import",
        "KLASÖR · skill içeriğini sabit sürüm olarak içe aktar",
    ),
    (
        "/extensions",
        "[JSON DOSYASI] · proje MCP, doküman, helper ve hook kapsamı",
    ),
    ("/config", "DOSYA · özel model ayarları"),
    ("/acceptance", "DOSYA · proje dışındaki kabul tarifi"),
    ("/plan", "Hazırlık ve görev planı"),
    ("/details", "Kayda bağlı tam plan ve kapsam"),
    ("/create", "Hazır tarifi görev olarak kaydet"),
    ("/approve", "Gösterilen kapsamı onayla"),
    ("/run", "Onaylı görevi çalıştır · API ücreti doğabilir"),
    ("/open", "GÖREV_KİMLİĞİ · önceki görevi aç"),
    ("/status", "Güncel görev durumu"),
    ("/diff", "Dosya değişiklikleri"),
    ("/resume", "Güvenli checkpoint'ten devam et"),
    ("/delivery", "Doğrulanmış teslim"),
    ("/mode", "plan | review | develop · çalışma kipi"),
    ("/new", "Yeni amaç · önceki görev kayıtları korunur"),
    ("/instructions", "Kullanıcı ve proje AGENTS.md talimatları"),
    ("/steer", "MESAJ · seçili görevi yönlendir"),
    ("/questions", "Yanıt bekleyen hedefli sorular"),
    ("/answer", "SORU_KİMLİĞİ YANIT"),
    ("/scope", "DOSYA · ek yazma kapsamı öner"),
    ("/service", "AD · ek servis gereksinimi öner"),
    (
        "/scope-approve",
        "ÖZET · gösterilen hazırlık kapsamını kabul et",
    ),
    ("/pause", "Güvenli checkpoint'te duraklat"),
    ("/cancel", "Görev iptali iste"),
    ("/exit", "Oturumu kapat"),
];

pub(super) struct Ui {
    pub interactive: bool,
    colors: bool,
    history: Vec<String>,
}

impl Ui {
    pub fn new() -> Self {
        let interactive = std::io::stdin().is_terminal()
            && std::io::stdout().is_terminal()
            && std::env::var("TERM").as_deref() != Ok("dumb");
        #[cfg(windows)]
        let ansi = crossterm::ansi_support::supports_ansi();
        #[cfg(not(windows))]
        let ansi = true;
        Self {
            interactive,
            colors: interactive
                && ansi
                && std::env::var_os("NO_COLOR").is_none_or(|value| value.is_empty()),
            history: vec![],
        }
    }

    fn paint(&self, text: &str, color: Color) -> String {
        if self.colors {
            text.with(color).to_string()
        } else {
            text.to_owned()
        }
    }

    fn width(&self) -> usize {
        crossterm::terminal::size()
            .map_or(80, |(width, _)| usize::from(width))
            .clamp(12, 94)
    }

    fn card(&self, title: &str, lines: &[String], output: &mut impl Write) -> Result {
        let inner = self.width().saturating_sub(8);
        writeln!(output)?;
        writeln!(
            output,
            "  {}",
            self.paint(&format!("╭{}╮", "─".repeat(inner + 2)), ACCENT)
        )?;
        for text in std::iter::once(title)
            .chain(std::iter::once(""))
            .chain(lines.iter().map(String::as_str))
        {
            for line in wrap(&display::safe(text), inner) {
                let padding = inner.saturating_sub(line.width());
                let text = if text == title {
                    self.paint(&line, ACCENT)
                } else {
                    line
                };
                writeln!(
                    output,
                    "  {} {text}{} {}",
                    self.paint("│", ACCENT),
                    " ".repeat(padding),
                    self.paint("│", ACCENT)
                )?;
            }
        }
        writeln!(
            output,
            "  {}",
            self.paint(&format!("╰{}╯", "─".repeat(inner + 2)), ACCENT)
        )?;
        Ok(())
    }

    pub fn welcome(&self, workspace: &Path, configured: bool, output: &mut impl Write) -> Result {
        if !self.interactive {
            writeln!(output, "Melissa terminal oturumu")?;
            writeln!(
                output,
                "Proje: {}",
                display::safe(&workspace.to_string_lossy())
            )?;
            writeln!(
                output,
                "Görevini yaz. Komutlar için /help, çıkmak için /exit."
            )?;
            return Ok(());
        }
        let name = workspace
            .file_name()
            .unwrap_or(workspace.as_os_str())
            .to_string_lossy();
        self.card(
            &format!("◆ MELISSA   v{}", env!("CARGO_PKG_VERSION")),
            &[
                "Kodunu anla. Planını incele. Birlikte geliştir.".into(),
                String::new(),
                format!("PROJE       {name}"),
                workspace.display().to_string(),
                String::new(),
                format!(
                    "MODEL       {}",
                    if configured {
                        "Ayarlar kayıtlı · çağrı görev onayı sonrası"
                    } else {
                        "Kurulum bekliyor · /config DOSYA"
                    }
                ),
                "ÇALIŞMA     Kaynağa bağlı plan ve açık onay".into(),
            ],
            output,
        )?;
        writeln!(
            output,
            "  {}",
            self.paint(
                "Bir görev yaz. Örneğin, main.py dosyasındaki hatayı düzelt.",
                MUTED
            )
        )?;
        Ok(())
    }

    pub fn help(&self, plain: &str, output: &mut impl Write) -> Result {
        if !self.interactive {
            return Ok(writeln!(output, "{plain}")?);
        }
        let mut lines: Vec<_> = COMMANDS
            .iter()
            .map(|(command, description)| format!("{command:<13} {description}"))
            .collect();
        lines.extend([
            String::new(),
            "↑ ↓  Geçmiş    Tab  Komut tamamla    Esc  Satırı temizle".into(),
            "Ctrl+C  Yazıyı temizle · boş satırda çık".into(),
        ]);
        self.card("KOMUTLAR", &lines, output)
    }

    pub fn notice(&self, message: &str, output: &mut impl Write) -> Result {
        if !self.interactive {
            return Ok(writeln!(output, "{message}")?);
        }
        for line in wrap(&display::safe(message), self.width().saturating_sub(5)) {
            writeln!(output, "  {} {line}", self.paint("›", NOTICE))?;
        }
        Ok(())
    }

    pub fn draft(&self, plan: &Value, path: &Path, ready: bool, output: &mut impl Write) -> Result {
        if !self.interactive {
            writeln!(output, "{}", display::json(plan)?)?;
            writeln!(
                output,
                "Yerel plan: {}",
                display::safe(&path.to_string_lossy())
            )?;
        } else {
            let mut lines = vec![
                plan["task_goal"].as_str().unwrap_or("").into(),
                String::new(),
            ];
            if let Some(files) = plan["scope"]["proposed_writes"].as_array() {
                lines.push(format!("DOSYALAR    {} yazılabilir dosya", files.len()));
                lines.extend(
                    files
                        .iter()
                        .filter_map(Value::as_str)
                        .map(|path| format!("  · {path}")),
                );
            }
            lines.push(String::new());
            lines.push("UZMANLAR".into());
            if let Some(steps) = plan["milestones"].as_array() {
                lines.extend(steps.iter().enumerate().map(|(index, step)| {
                    format!(
                        "  {}. {}",
                        index + 1,
                        role(step["profile"].as_str().unwrap_or(""))
                    )
                }));
            }
            lines.push(String::new());
            lines.push(format!(
                "KABUL       {}",
                if ready {
                    "Operatör tarifi bağlı"
                } else {
                    "Proje hazırlığı · özel test tarifi gerekmiyor"
                }
            ));
            lines.push("Tam plan ve kayıt yolu /details".into());
            self.card("GÖREV PLANI", &lines, output)?;
        }
        self.notice(if ready {
            "Tarifi incele, model ayarlarını /config ile yükle ve /create yaz."
        } else {
            "Planı incele. /config, /budget ve /environment ile proje yürütmesini hazırla; /create kapsamı gösterir. Bunlar yoksa /create yalnız hazırlık kaydeder. Özel test tarifi zorunlu değil."
        }, output)
    }

    pub fn result(&self, value: &Value, output: &mut impl Write) -> Result {
        if matches!(
            value["command"].as_str(),
            Some(
                "task continuity"
                    | "task checkpoint"
                    | "task changes"
                    | "task undo-preview"
                    | "task undo"
                    | "task adopt-preview"
                    | "task adopt-source"
            )
        ) {
            if !self.interactive {
                return Ok(writeln!(output, "{}", display::human(value)?)?);
            }
            let lines = display::human(value)?
                .lines()
                .map(str::to_owned)
                .collect::<Vec<_>>();
            return self.card("DEVAM VE DEĞİŞİKLİK İNCELEMESİ", &lines, output);
        }
        if value["command"] == "task conversation" {
            if !self.interactive {
                return Ok(writeln!(output, "{}", display::json(value)?)?);
            }
            let brief = &value["brief"];
            let mut lines = vec![
                format!(
                    "GÖREV {} · revizyon {} · kip {}",
                    text(&value["task_id"]),
                    brief["revision"],
                    text(&brief["mode"])
                ),
                text(&brief["goal"]).into(),
                "Gereksinim → alt işler → /diff → test kayıtları → bağımsız kabul".into(),
            ];
            if let Some(steps) = brief["plan"]["milestones"].as_array() {
                for step in steps {
                    lines.push(format!(
                        "  {} · {}",
                        text(&step["id"]),
                        role(text(&step["profile"]))
                    ));
                }
            }
            if let Some(delta) = brief["pending_scope"].as_object() {
                lines.push(format!("EK DOSYALAR {}", delta["add_writes"]));
                lines.push(format!("EK SERVİSLER {}", delta["add_services"]));
                lines.push(format!(
                    "İnceledikten sonra: /scope-approve {}",
                    text(&delta["digest"])
                ));
            }
            if let Some(questions) = brief["questions"].as_array() {
                for q in questions.iter().filter(|q| q["answer"].is_null()) {
                    lines.push(format!("SORU {} · {}", text(&q["id"]), text(&q["text"])));
                    lines.push(format!("/answer {} YANIT", text(&q["id"])));
                }
            }
            lines.push(if brief["runtime_scope_amendments"]==true {"Görev/harcama geçmişi korunur. Ek dosyalar güvenli duraklama ve kapsam onayından sonra /resume ile etkinleşir."}else{"Görev/harcama geçmişi korunur. Hazırlık kapsamı, çalışan tarife yeni yetki vermez."}.into());
            return self.card("KONUŞMA VE KAPSAM", &lines, output);
        }
        if !self.interactive {
            return Ok(writeln!(output, "{}", display::human(value)?)?);
        }
        let mut lines = vec![];
        let title = match value["command"].as_str() {
            Some("doctor") => {
                lines.push(format!(
                    "WINDOWS     {} / {}",
                    text(&value["host"]),
                    text(&value["architecture"])
                ));
                lines.push(format!(
                    "DOCKER      {}",
                    text(&value["release_readiness"]["status"])
                ));
                lines.push(format!(
                    "SDK WORKER  {}",
                    if value["isolated_SDK_image_present"] == true {
                        "Hazır"
                    } else {
                        "Yükleme bekliyor"
                    }
                ));
                lines.push(String::new());
                lines.push("Model bağlantısı bu kontrolde çağrılmaz.".into());
                "SİSTEM HAZIRLIĞI"
            }
            Some("config show") => {
                lines.push(format!("MODEL       {}", text(&value["model"])));
                lines.push(format!("BAĞLAM      {} token", value["context_tokens"]));
                lines.push("Model ayarları şifreli olarak kaydedildi.".into());
                "MODEL AYARLARI"
            }
            _ => {
                lines.push(format!("GÖREV       {}", text(&value["task_id"])));
                lines.push(format!("DURUM       {}", state(text(&value["state"]))));
                if let Some(goal) = value["goal"].as_str() {
                    lines.push(goal.into());
                }
                if let Some(plan) = value["execution_plan"].as_object() {
                    lines.push(String::new());
                    if let Some(cost) = plan.get("max_cost_micro_usd").and_then(Value::as_u64) {
                        lines.push(format!("BÜTÇE       {:.2} USD", cost as f64 / 1_000_000.0));
                    }
                    if let Some(ms) = plan.get("wall_time_ms").and_then(Value::as_u64) {
                        lines.push(format!("SÜRE        {} saniye", ms / 1000));
                    }
                    if let Some(models) = plan.get("approved_models").and_then(Value::as_array) {
                        lines.push("ONAYLI MODELLER".into());
                        lines.extend(models.iter().map(|model| {
                            format!(
                                "  · {} / {}",
                                text(&model["provider"]),
                                text(&model["model"])
                            )
                        }));
                    }
                    for (field, label) in [
                        ("writable_paths", "YAZMA KAPSAMI"),
                        ("protected_paths", "KORUNAN DOSYALAR"),
                    ] {
                        if let Some(files) = plan.get(field).and_then(Value::as_array) {
                            lines.push(label.into());
                            lines.extend(
                                files
                                    .iter()
                                    .filter_map(Value::as_str)
                                    .map(|file| format!("  · {file}")),
                            );
                        }
                    }
                    lines.push("Tam kaynak, model ve kabul kapsamı için /details.".into());
                }
                if let Some(changes) = value["changes"].as_array() {
                    lines.push(format!("DEĞİŞİKLİK  {} dosya", changes.len()));
                    lines.extend(changes.iter().map(|change| {
                        format!("  {}  {}", text(&change["kind"]), text(&change["path"]))
                    }));
                }
                if let Some(files) = value["files"].as_array() {
                    lines.push("TESLİM DOSYALARI".into());
                    lines.extend(
                        files
                            .iter()
                            .filter_map(Value::as_str)
                            .map(|file| format!("  · {file}")),
                    );
                }
                if let Some(cost) = value["model_budget"]["known_cost_micro_usd"].as_u64() {
                    lines.push(format!(
                        "KAYITLI MALİYET  {:.6} USD",
                        cost as f64 / 1_000_000.0
                    ));
                }
                if value["task_accepted"] == true {
                    lines.push("Onaylı görev kontrolleri geçti.".into());
                }
                "GÖREV"
            }
        };
        self.card(title, &lines, output)
    }

    pub async fn working<F, C>(
        &self,
        future: F,
        output: &mut impl Write,
        mut control: C,
    ) -> Result<Value>
    where
        F: Future<Output = Result<Value>>,
        C: FnMut(Option<&str>) -> Result<Option<String>>,
    {
        if !self.interactive {
            return future.await;
        }
        let editor = input::RunningInput::new(output)?;
        let mut editor = Some(editor);
        let mut io_error = None;
        let mut interval = tokio::time::interval(Duration::from_millis(120));
        let mut future = std::pin::pin!(future);
        let mut index = 0;
        loop {
            tokio::select! {
                result = &mut future => {
                    drop(editor.take());
                    if let Some(error)=io_error {return Err(error);}
                    queue!(output, MoveToColumn(0), Clear(ClearType::CurrentLine))?;
                    writeln!(output)?;
                    return result;
                }
                _ = interval.tick() => {
                    if let Some(editor)=editor.as_mut() {
                        let draw=(||->Result {
                            if let Some(line)=editor.poll()? {
                                let message=match control(Some(line.trim())) {Ok(v)=>v,Err(e)=>Some(e.to_string())};
                                if let Some(message)=message {
                                    queue!(output,MoveToColumn(0),Clear(ClearType::CurrentLine))?;
                                    write!(output,"{}\r\n",message.lines().map(display::safe).collect::<Vec<_>>().join("\r\n"))?;
                                }
                            }
                            if index%8==0 && let Some(message)=control(None)? {
                                queue!(output,MoveToColumn(0),Clear(ClearType::CurrentLine))?;
                                write!(output,"{}\r\n",message.lines().map(display::safe).collect::<Vec<_>>().join("\r\n"))?;
                            }
                            editor.draw(self,output)
                        })();
                        if let Err(error)=draw {
                            // Preserve the run future until its effects settle.
                            // A terminal failure requests cancellation, it does
                            // not detach a worker by returning from this loop.
                            let _=control(Some("/cancel"));io_error=Some(error);
                        }
                    }
                    if io_error.is_some() {drop(editor.take());}
                    index += 1;
                }
            }
        }
    }
}

fn text(value: &Value) -> &str {
    value.as_str().unwrap_or("—")
}

fn state(value: &str) -> &str {
    match value {
        "awaiting_approval" => "Onay bekliyor",
        "queued" => "Onaylı · çalıştırmaya hazır",
        "running" => "Çalışıyor",
        "completed" => "Tamamlandı",
        "failed" => "Başarısız",
        "cancelled" => "İptal edildi",
        "paused" => "Güvenli checkpoint'te duraklatıldı",
        _ => value,
    }
}

fn role(value: &str) -> String {
    match value {
        "melissa.code.debugger" => "Hata ayıklama".into(),
        "melissa.code.implementer" => "Uygulama".into(),
        "melissa.code.test-engineer" => "Test mühendisliği".into(),
        "melissa.code.reviewer" => "Kod incelemesi".into(),
        "melissa.code.architect" => "Mimari".into(),
        "melissa.code.security-reviewer" => "Güvenlik incelemesi".into(),
        _ => value
            .strip_prefix("melissa.code.")
            .unwrap_or(value)
            .replace('-', " "),
    }
}

fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut lines = vec![];
    let mut line = String::new();
    for word in text.split_word_bounds() {
        if line.width() + word.width() > width && word.width() <= width && !line.is_empty() {
            lines.push(std::mem::take(&mut line));
        }
        for grapheme in word.graphemes(true) {
            if line.width() + grapheme.width() > width && !line.is_empty() {
                lines.push(std::mem::take(&mut line));
            }
            line.push_str(grapheme);
        }
    }
    lines.push(line);
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrapping_preserves_unicode_clusters_and_visible_cell_widths() {
        let value = "Türkçe e\u{301} 東京 🦀";
        let lines = wrap(value, 8);
        assert_eq!(lines.concat(), value);
        assert!(lines.iter().all(|line| line.width() <= 8));
        assert!(lines.iter().any(|line| line.contains("e\u{301}")));
    }
}
