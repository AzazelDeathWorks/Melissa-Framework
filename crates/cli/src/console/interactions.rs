use super::ui::Ui;
use crate::{Result, app::App};
use std::collections::BTreeSet;

#[derive(Default)]
pub(crate) struct Pending {
    seen: BTreeSet<String>,
    pub question: Option<String>,
    pub scope_after_pause: Option<(String, String)>,
}
impl Pending {
    pub fn refresh(&mut self, ui: &mut Ui, app: &App, task: &str) -> Result {
        let conversation = app.conversation_view(task)?;
        let brief = &conversation["brief"];
        self.question = brief["questions"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|q| q["answer"].is_null())
            .and_then(|q| q["id"].as_str())
            .map(String::from);
        for q in brief["questions"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|q| q["answer"].is_null())
        {
            let Some(id) = q["id"].as_str() else { continue };
            let key = format!("{task}:question:{id}");
            if self.seen.insert(key) {
                let title = format!(
                    "Melissa soruyor: {}",
                    q["text"].as_str().unwrap_or("Yanıtın nedir?")
                );
                if let Some(answer) = ui.input(&title, "", false)? {
                    app.answer_question(task, id, &answer)?;
                    self.question = None;
                } else {
                    ui.print(
                        "Soru bekliyor. Mesaj alanına yazdığın sonraki yanıt bu soruya iletilecek.",
                    )?;
                }
            }
        }
        if let Some(scope) = brief["pending_scope"].as_object()
            && let Some(digest) = scope["digest"].as_str()
        {
            let key = format!("{task}:scope:{digest}");
            if self.seen.insert(key) {
                ui.print(&format!(
                    "Ek çalışma kapsamı isteniyor.\nDosyalar: {}\nServisler: {}",
                    scope.get("add_writes").unwrap_or(&serde_json::Value::Null),
                    scope
                        .get("add_services")
                        .unwrap_or(&serde_json::Value::Null)
                ))?;
                if ui.choose(
                    "Kapsam genişletme",
                    &[
                        "Güvenli duraklat ve incele".into(),
                        "Şimdilik beklet".into(),
                    ],
                )? == Some(0)
                {
                    app.request_control(task, "pause")?;
                    self.scope_after_pause = Some((task.into(), digest.into()));
                }
            }
        }
        Ok(())
    }
    pub fn review_scope(&mut self, ui: &mut Ui, app: &App) -> Result<Option<String>> {
        let Some((task, expected)) = self.scope_after_pause.take() else {
            return Ok(None);
        };
        let view = app.conversation_view(&task)?;
        let scope = &view["brief"]["pending_scope"];
        if scope["digest"].as_str() != Some(expected.as_str()) {
            ui.print("Kapsam değişti; yeni kapsamı tekrar incele.")?;
            return Ok(None);
        }
        ui.print(&format!(
            "Ek dosyalar: {}\nEk servisler: {}",
            scope["add_writes"], scope["add_services"]
        ))?;
        if ui.confirm("Gösterilen ek kapsamı onayla ve çalışmayı sürdür?")? {
            app.approve_scope(&task, &expected)?;
            Ok(Some(task))
        } else {
            Ok(None)
        }
    }
    pub fn answer_or_steer(&mut self, app: &App, task: &str, text: &str) -> Result {
        if let Some(id) = self.question.take() {
            app.answer_question(task, &id, text)?;
        } else {
            app.steer(task, text)?;
        }
        Ok(())
    }
    pub fn show_again(&mut self) {
        self.seen.clear();
    }
}
