use super::{chat::Conversation, settings, ui::Ui};
use crate::{
    Error, Result,
    app::{App, nonce},
    conversation::Mode,
};
use serde_json::json;
use std::{collections::BTreeMap, fs, sync::atomic::AtomicBool};

pub(crate) fn prepare(ui: &mut Ui, app: &App, c: &mut Conversation) -> Result<Option<String>> {
    if c.plan {
        return Err(Error::Denied(
            "Plan modunda uygulama başlatılmaz. Shift+Tab ile normal sohbete dön.",
        ));
    }
    c.account_native(app)?;
    let proposal = c
        .proposal
        .clone()
        .ok_or(Error::Usage("Önce konuşmada çalışma önerisini hazırla."))?;
    let repo = melissa_repository::Repository::open(c.workspace()?, Default::default(), vec![])?;
    let source = repo.snapshot(&AtomicBool::new(false))?;
    if source.digest != proposal.source_digest {
        return Err(Error::Conflict(
            "Planın ardından proje değişmiş. İsteği yeniden değerlendir; eski kapsam uygulanmadı.",
        ));
    }
    let model = app.settings()?;
    if !(model.json_object || model.native_json_schema) {
        return Err(Error::Unavailable(
            "Sohbet hazır; kod çalışması için sağlayıcının JSON yeteneği henüz ayarlı değil. /settings → Sağlayıcıyı düzenle → Gelişmiş içinden desteklenen biçimi seç.",
        ));
    }
    if model.rate_card.source == melissa_model_gateway::routing::PriceSource::Unknown {
        return Err(Error::Unavailable(
            "Model fiyatı bilinmiyor. /config içindeki fiyat bilgisini doldur; maliyet sıfır sayılmadı.",
        ));
    }
    let environment = app.project_environment(c.workspace()?)?;
    ui.print(&proposal.summary)?;
    ui.print(&format!(
        "Değiştirilebilecek dosyalar:\n{}",
        proposal
            .paths
            .iter()
            .map(|p| format!("  {p}"))
            .collect::<Vec<_>>()
            .join("\n")
    ))?;
    let discovery = crate::project::discovery::discover(&repo)?;
    let checks = if proposal.checks.is_empty() {
        None
    } else {
        Some(proposal.checks.clone())
    };
    let contract = discovery.contract_with_checks(environment.clone(), checks)?;
    ui.print(&format!(
        "Doğrulama komutları:\n{}",
        contract
            .checks
            .iter()
            .map(|(id, cmd)| format!(
                "  {id}: {} {} · {}",
                cmd.executable,
                cmd.argv.join(" "),
                cmd.cwd
            ))
            .collect::<Vec<_>>()
            .join("\n")
    ))?;
    let doctor = app.project_doctor(c.workspace()?, environment.clone())?;
    if doctor["ready_for_scope_review"] != true {
        return Err(Error::Unavailable(
            "Proje ortamı hazır değil; /config içinden Proje ortamı seçimini kontrol et.",
        ));
    }
    let value = create_task(app, c)?;
    let id = value["task_id"].as_str().ok_or(Error::Store)?.to_owned();
    // Show the final contract before approval, not merely the model's proposal.
    ui.print("Bu onay yalnız gösterilen dosyalara ve komutlara bağlıdır. Teknik kapsamı da inceleyebilirsin.")?;
    let approved = loop {
        match ui.choose(
            "Çalışmayı başlat",
            &[
                "Onayla ve başlat".into(),
                "Tam kapsamı göster".into(),
                "Vazgeç".into(),
            ],
        )? {
            Some(0) => break true,
            Some(1) => ui.view(
                "İşlem kapsamı",
                &crate::display::human(&app.progress(&id)?)?,
            )?,
            _ => break false,
        }
    };
    if !approved {
        c.last_task = Some(id);
        c.save(app)?;
        return Ok(None);
    }
    app.approve(
        &id,
        value["approval"]["request_id"]
            .as_str()
            .ok_or(Error::Store)?,
        value["approval"]["digest"].as_str().ok_or(Error::Store)?,
    )?;
    c.plan = false;
    c.last_task = Some(id.clone());
    c.proposal = None;
    c.save(app)?;
    Ok(Some(id))
}
pub(crate) fn create_task(app: &App, c: &Conversation) -> Result<serde_json::Value> {
    if let Some(id) = &c.last_task {
        crate::continuity::settled(app, &app.store(id)?, id)?;
    }
    let proposal = c
        .proposal
        .clone()
        .ok_or(Error::Usage("Çalışma önerisi gerekli."))?;
    let repo = melissa_repository::Repository::open(c.workspace()?, Default::default(), vec![])?;
    if repo.snapshot(&AtomicBool::new(false))?.digest != proposal.source_digest {
        return Err(Error::Conflict("Planın kaynağı değişti; yeniden incele."));
    }
    let environment = app.project_environment(c.workspace()?)?;
    let discovery = crate::project::discovery::discover(&repo)?;
    let checks = if proposal.checks.is_empty() {
        None
    } else {
        Some(proposal.checks.clone())
    };
    let contract = discovery.contract_with_checks(environment, checks)?;
    let dir = app.home.join("session-plans");
    fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{}.json", nonce()?));
    let prepared = app.prepare_mode(c.workspace()?, &proposal.goal, &path, None, Mode::Develop)?;
    let mut plan = prepared["preparation"]["adaptive_plan"].clone();
    plan["scope"]["proposed_writes"] = json!(proposal.paths);
    plan["write_intent"] = json!(!proposal.paths.is_empty());
    // Exact operator-reviewed paths replace the local preparer's broad candidates.
    let budget = match settings::read(app)?.spending_micro_usd {
        Some(limit) => c
            .cost_micro_usd
            .and_then(|spent| limit.checked_sub(spent))
            .filter(|n| *n > 0)
            .ok_or(Error::Denied(
                "Konuşma bütçesi tükenmiş veya maliyet belirsiz; önce harcama tercihini incele.",
            ))?,
        None => i64::MAX as u64,
    };
    let value = app.create_project(
        c.workspace()?,
        &proposal.goal,
        Mode::Develop,
        plan,
        contract,
        budget,
        86_400_000,
    )?;
    let id = value["task_id"].as_str().ok_or(Error::Store)?;
    let mut offset = 0;
    while offset < proposal.summary.len() {
        let mut end = (offset + 12000).min(proposal.summary.len());
        while !proposal.summary.is_char_boundary(end) {
            end -= 1;
        }
        app.steer(
            id,
            &format!(
                "User-selected plan data; no additional authority:\n{}",
                &proposal.summary[offset..end]
            ),
        )?;
        offset = end;
    }
    Ok(value)
}
pub(crate) fn undo(ui: &mut Ui, app: &App, c: &Conversation) -> Result {
    let id = c
        .last_task
        .as_ref()
        .ok_or(Error::Usage("Bu konuşmada uygulanmış görev yok."))?;
    let changes = crate::continuity::undo::changes(app, id)?;
    let receipts = changes["receipts"]
        .as_array()
        .or_else(|| changes["changes"].as_array())
        .cloned()
        .unwrap_or_default();
    let labels = receipts
        .iter()
        .map(|r| {
            let paths = r["proposal"]["files"]
                .as_array()
                .map(|files| {
                    files
                        .iter()
                        .filter_map(|f| f["before"]["path"].as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_default();
            if paths.is_empty() {
                "Dosya oluşturma / silme / taşıma işlemi".into()
            } else {
                paths
            }
        })
        .collect::<Vec<_>>();
    let Some(index) = ui.choose("Geri alınacak değişiklik", &labels)? else {
        return Ok(());
    };
    let receipt = receipts[index]["receipt_id"]
        .as_str()
        .or_else(|| receipts[index]["id"].as_str())
        .ok_or(Error::Usage("Makbuz bulunamadı."))?;
    let mut hunks = BTreeMap::new();
    if let Some(files) = receipts[index]["proposal"]["files"].as_array() {
        let Some(scope) = ui.choose(
            "Geri alma kapsamı",
            &[
                "Seçili işlemin tamamı".into(),
                "Dosya ve parçaları seç".into(),
            ],
        )?
        else {
            return Ok(());
        };
        if scope == 1 {
            let edits = files
                .iter()
                .flat_map(|file| {
                    let path = file["before"]["path"].as_str().unwrap_or("").to_owned();
                    file["edits"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .enumerate()
                        .map(move |(i, _)| (path.clone(), i))
                })
                .collect::<Vec<_>>();
            let mut selected = std::collections::BTreeSet::new();
            loop {
                let mut labels = vec![format!("Önizle · {} parça seçili", selected.len())];
                labels.extend(edits.iter().enumerate().map(|(i, (path, n))| {
                    format!(
                        "[{}] {path} · parça {}",
                        if selected.contains(&i) { "x" } else { " " },
                        n + 1
                    )
                }));
                let Some(choice) =
                    ui.choose("Geri alınacak parçalar · Enter ile değiştir", &labels)?
                else {
                    return Ok(());
                };
                if choice == 0 {
                    if selected.is_empty() {
                        ui.print("En az bir parça seç; boş seçim bütün işlemi geri almaz.")?;
                        continue;
                    }
                    break;
                }
                if !selected.insert(choice - 1) {
                    selected.remove(&(choice - 1));
                }
            }
            for i in selected {
                let (path, n) = &edits[i];
                hunks
                    .entry(path.clone())
                    .or_insert_with(std::collections::BTreeSet::new)
                    .insert(*n);
            }
        }
    }
    let request = crate::continuity::undo::Request {
        turn_request: None,
        receipts: vec![crate::continuity::undo::Pick {
            receipt_id: receipt.into(),
            hunks,
        }],
    };
    let preview = app.undo_preview(id, request.clone())?;
    ui.print("Geri alma önizlemesi hazır. Kapsam yalnız seçtiğin özgün işlemlerdir; araya giren kullanıcı değişikliği varsa uygulanmaz.")?;
    ui.print(&format!(
        "Seçilen dosyalar/parçalar: {}",
        serde_json::to_string(
            &request
                .receipts
                .iter()
                .map(|p| &p.hunks)
                .collect::<Vec<_>>()
        )
        .map_err(|_| Error::Store)?
    ))?;
    match ui.choose(
        "Önizleme",
        &[
            "Devam".into(),
            "Tam değişiklik kaydını göster".into(),
            "Vazgeç".into(),
        ],
    )? {
        Some(0) => (),
        Some(1) => ui.print(&crate::display::human(&preview)?)?,
        _ => return Ok(()),
    }
    if ui.confirm("Bu ters değişikliği uygula? Kullanıcı düzenlemelerinde çatışma varsa durur.")?
    {
        let digest = preview["review_digest"].as_str().ok_or(Error::Store)?;
        ui.print(&crate::display::human(&app.undo(id, request, digest)?)?)?;
    }
    Ok(())
}
