use super::{settings, ui::Ui};
use crate::{
    Error, Result,
    app::App,
    settings::{Credential, Settings},
};
use melissa_model_gateway::{
    Protocol,
    routing::{Capabilities, PriceSource, RateCard},
};

fn number(ui: &mut Ui, label: &str, default: Option<u64>) -> Result<Option<u64>> {
    loop {
        let Some(s) = ui.input(
            label,
            &default.map(|v| v.to_string()).unwrap_or_default(),
            false,
        )?
        else {
            return Ok(None);
        };
        if let Ok(n) = s.trim().parse::<u64>()
            && n > 0
        {
            return Ok(Some(n));
        }
        ui.print("Pozitif bir sayı yaz; modelin belgelenen kapasitesini kullan.")?;
    }
}
pub(crate) fn money(s: &str) -> Result<u64> {
    let (whole, fraction) = s.trim().split_once('.').unwrap_or((s.trim(), ""));
    if whole.is_empty()
        || fraction.len() > 6
        || !whole.bytes().all(|b| b.is_ascii_digit())
        || !fraction.bytes().all(|b| b.is_ascii_digit())
    {
        return Err(Error::Usage("Geçerli USD tutarı yaz."));
    }
    whole
        .parse::<u64>()
        .ok()
        .and_then(|n| n.checked_mul(1_000_000))
        .and_then(|n| {
            fraction
                .parse::<u64>()
                .unwrap_or(0)
                .checked_mul(10u64.pow((6 - fraction.len()) as u32))
                .and_then(|f| n.checked_add(f))
        })
        .ok_or(Error::Usage("Tutar çok büyük."))
}
pub(crate) async fn provider(ui: &mut Ui, app: &App, existing: Option<Settings>) -> Result {
    let Some(name) = ui.input(
        "Sağlayıcı adı (harf, sayı, - veya _)",
        existing
            .as_ref()
            .map(|s| s.provider_id.as_str())
            .unwrap_or("custom"),
        false,
    )?
    else {
        return Ok(());
    };
    let Some(protocol) = ui.choose(
        "API uyumluluğu · sağlayıcının desteklediği protokol",
        &[
            "OpenAI uyumlu · Chat Completions".into(),
            "OpenAI Responses".into(),
            "Anthropic Messages".into(),
        ],
    )?
    else {
        return Ok(());
    };
    let protocol = match protocol {
        0 => Protocol::OpenAiChatCompletions,
        1 => Protocol::OpenAiResponses,
        _ => Protocol::AnthropicMessages,
    };
    let Some(base) = ui.input_checked(
        "Base URL (API sunucu adresi)",
        "Örnek: https://api.example.com/v1\nBuraya API anahtarını yazma; anahtar sonraki adımda sorulacak.\nBase URL veya seçilen protokolün tam istek adresi kabul edilir.",
        existing.as_ref().map(|s| s.endpoint.as_str()).unwrap_or(""),
        false,
        |value| settings::endpoint(value, protocol).map(|_| ()),
    )?
    else {
        return Ok(());
    };
    let endpoint = settings::endpoint(&base, protocol)?;
    let mut modes = vec![
        "Anahtarı güvenli kaydet".into(),
        "Ortam değişkeni kullan".into(),
        "Yerel anahtar dosyası kullan".into(),
    ];
    if crate::settings::local_endpoint(&url::Url::parse(&endpoint).map_err(|_| Error::Io)?) {
        modes.push("Anahtarsız yerel sunucu".into());
    }
    if existing.is_some() {
        modes.push("Mevcut kimlik bilgisini koru".into());
    }
    let Some(auth) = ui.choose("API anahtarı · nasıl kullanılsın?", &modes)? else {
        return Ok(());
    };
    let credential = match modes[auth].as_str() {
        "Anahtarı güvenli kaydet" => {
            let Some(secret) = ui.input_checked(
                "API anahtarı (API Key)",
                "Sağlayıcının verdiği anahtarı yapıştır. Base URL buraya yazılmaz.\nAnahtar maskelenir, şifreli saklanır ve sohbet geçmişine yazılmaz.",
                "",
                true,
                settings::validate_key_input,
            )?
            else {
                return Ok(());
            };
            settings::save_key(app, zeroize::Zeroizing::new(secret.trim().to_owned()))?
        }
        "Ortam değişkeni kullan" => {
            let Some(s) = ui.input("Değişken adı", "MELISSA_API_KEY", false)? else {
                return Ok(());
            };
            Credential::Environment {
                name: s.to_string(),
            }
        }
        "Yerel anahtar dosyası kullan" => {
            let Some(s) = ui.input("Anahtar dosyasının tam yolu", "", false)? else {
                return Ok(());
            };
            Credential::File {
                path: s.to_string().into(),
            }
        }
        "Anahtarsız yerel sunucu" => Credential::None,
        _ => existing.as_ref().ok_or(Error::Io)?.credential.clone(),
    };
    // The temporary record is used only for GET /models, never generation or persistence.
    let mut probe: Settings = serde_json::from_str(include_str!(
        "../../../../examples/cli/settings.template.json"
    ))
    .map_err(|_| Error::Store)?;
    probe.provider_id = name.to_string();
    probe.endpoint = endpoint.clone();
    probe.protocol = protocol;
    probe.credential = credential.clone();
    let catalog = models(&probe).await.unwrap_or_default();
    let mut labels = catalog.iter().map(|m| m.id.clone()).collect::<Vec<_>>();
    labels.push("Model kimliğini elle gir".into());
    let picked = if catalog.is_empty() {
        None
    } else {
        match ui.choose("Model seç · yazıp ara", &labels)? {
            Some(i) => Some(i),
            None => return Ok(()),
        }
    };
    let info = picked.and_then(|i| catalog.get(i));
    let model = if let Some(info) = info {
        zeroize::Zeroizing::new(info.id.clone())
    } else {
        let Some(model) = ui.input(
            "Model kimliği",
            existing.as_ref().map(|s| s.model.as_str()).unwrap_or(""),
            false,
        )?
        else {
            return Ok(());
        };
        model
    };
    let same_identity = existing.as_ref().is_some_and(|s| {
        s.provider_id == *name
            && s.endpoint == endpoint
            && s.model == *model
            && s.protocol == protocol
    });
    let context_tokens = match info.and_then(|m| m.context).or_else(|| {
        existing
            .as_ref()
            .filter(|_| same_identity)
            .map(|s| s.context_tokens)
    }) {
        Some(n) => n,
        None => {
            let Some(n) = number(
                ui,
                "Sağlayıcı kapasite bildirmedi · belgelenen bağlam kapasitesi",
                None,
            )?
            else {
                return Ok(());
            };
            n
        }
    };
    let max_output_tokens = match info.and_then(|m| m.output).or_else(|| {
        existing
            .as_ref()
            .filter(|_| same_identity)
            .map(|s| s.max_output_tokens)
    }) {
        Some(n) => n,
        None => {
            let Some(n) = number(
                ui,
                "Sağlayıcı kapasite bildirmedi · modelin tam çıktı kapasitesi",
                None,
            )?
            else {
                return Ok(());
            };
            n
        }
    };
    let advanced = ui.choose(
        "Bağlantıyı tamamla",
        &[
            "Kaydet · metin ve araç çağrıları".into(),
            "Gelişmiş · JSON ve görsel yetenekleri".into(),
        ],
    )?;
    if advanced.is_none() {
        return Ok(());
    }
    let mut json_object = existing
        .as_ref()
        .filter(|_| same_identity)
        .is_some_and(|s| s.json_object);
    let mut native_json_schema = existing
        .as_ref()
        .filter(|_| same_identity)
        .is_some_and(|s| s.native_json_schema);
    let mut vision = existing
        .as_ref()
        .filter(|_| same_identity)
        .is_some_and(|s| s.capabilities.vision);
    if advanced == Some(1) {
        let Some(format) = ui.choose(
            "Doğrulanmış JSON desteği",
            &[
                "Bilinmiyor / kapalı".into(),
                "JSON object".into(),
                "JSON Schema".into(),
                "İkisi de".into(),
            ],
        )?
        else {
            return Ok(());
        };
        json_object = format == 1 || format == 3;
        native_json_schema = format == 2 || format == 3;
        let Some(input) = ui.choose(
            "Görsel desteği",
            &["Yalnız metin".into(), "Metin ve PNG görsel".into()],
        )?
        else {
            return Ok(());
        };
        vision = input == 1;
    }
    let rate_card = existing
        .as_ref()
        .filter(|_| same_identity)
        .map(|s| s.rate_card.clone())
        .unwrap_or(RateCard {
            source: PriceSource::Unknown,
            input_micro_usd_per_million: 0,
            output_micro_usd_per_million: 0,
            cached_input_micro_usd_per_million: None,
            cache_creation_micro_usd_per_million: None,
        });
    let mut settings = Settings {
        schema_version: "melissa.cli-settings/v1".into(),
        provider_id: name.to_string(),
        endpoint,
        protocol,
        model: model.to_string(),
        credential,
        context_tokens,
        max_output_tokens,
        request_timeout_ms: 1_800_000,
        local_tokenizer: None,
        capabilities: Capabilities {
            tool_calling: true,
            streaming: true,
            structured_output: native_json_schema,
            vision,
        },
        json_object,
        native_json_schema,
        single_attempt: false,
        prompt_cache: Default::default(),
        rate_card,
        model_pool: vec![],
        measurements: Default::default(),
        tool_profile: Default::default(),
        lab_variant: Default::default(),
    };
    if same_identity && let Some(old) = &existing {
        settings.request_timeout_ms = old.request_timeout_ms;
        settings.local_tokenizer = old.local_tokenizer.clone();
        settings.single_attempt = old.single_attempt;
        settings.prompt_cache = old.prompt_cache;
        settings.model_pool = old.model_pool.clone();
        settings.measurements = old.measurements.clone();
        settings.tool_profile = old.tool_profile;
        settings.lab_variant = old.lab_variant;
    }
    settings::save_provider(app, settings)?;
    ui.print("Sağlayıcı kaydedildi. Bağlantı ve model yetenekleri henüz canlı doğrulanmadı.")
}
#[derive(Clone)]
struct ModelInfo {
    id: String,
    context: Option<u64>,
    output: Option<u64>,
}
async fn models(settings: &Settings) -> Result<Vec<ModelInfo>> {
    use futures_util::StreamExt;
    let endpoint = settings.endpoint_with_key(settings.key()?)?;
    let mut url = endpoint.url.clone();
    let path = url.path().trim_end_matches('/');
    let prefix = path
        .strip_suffix("chat/completions")
        .or_else(|| path.strip_suffix("responses"))
        .or_else(|| path.strip_suffix("messages"))
        .ok_or(Error::Usage("Model listeleme adresi bulunamadı."))?;
    url.set_path(&format!("{prefix}models"));
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|_| Error::Io)?;
    let response = endpoint
        .authorize(client.get(url))
        .send()
        .await
        .map_err(|_| {
            Error::Unavailable("Model listesine bağlanılamadı; adresi ve anahtarı kontrol et.")
        })?;
    if !response.status().is_success() {
        return Err(Error::Unavailable(
            "Model listesi alınamadı. Sağlayıcı listeleme desteklemiyor veya anahtarın yetkisi yok; elle model ekleyebilirsin.",
        ));
    }
    let mut bytes = vec![];
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| Error::Io)?;
        if bytes.len().saturating_add(chunk.len()) > 2 * 1024 * 1024 {
            return Err(Error::Usage("Model listesi çok büyük."));
        }
        bytes.extend_from_slice(&chunk);
    }
    let value: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|_| Error::Usage("Sağlayıcı model listesi biçimi desteklenmiyor."))?;
    let list = value["data"]
        .as_array()
        .or_else(|| value["models"].as_array())
        .ok_or(Error::Usage("Model listesi bulunamadı."))?;
    Ok(list
        .iter()
        .filter_map(|v| {
            let id = v["id"].as_str().or_else(|| v["model"].as_str())?;
            if id.is_empty() || id.len() > 256 || id.chars().any(char::is_control) {
                return None;
            }
            Some(ModelInfo {
                id: id.into(),
                context: ["context_length", "max_model_len", "context_window"]
                    .iter()
                    .find_map(|k| v[k].as_u64())
                    .filter(|n| *n > 0),
                output: ["max_output_tokens", "max_completion_tokens"]
                    .iter()
                    .find_map(|k| v[k].as_u64())
                    .filter(|n| *n > 0),
            })
        })
        .collect())
}
pub(crate) async fn model(ui: &mut Ui, app: &App) -> Result<bool> {
    let providers = settings::providers(app)?;
    if providers.is_empty() {
        ui.print("Henüz model yok. /config içinden sağlayıcı ekle.")?;
        return Ok(false);
    }
    let mut labels = providers
        .iter()
        .map(|s| format!("{} / {}", s.provider_id, s.model))
        .collect::<Vec<_>>();
    labels.push("Seçili sağlayıcıdan modelleri getir".into());
    let Some(i) = ui.choose("Model seç · yazıp ara", &labels)? else {
        return Ok(false);
    };
    let selected = if i == providers.len() {
        let mut selected = app.settings()?;
        let models = models(&selected).await?;
        let Some(index) = ui.choose(
            "Sağlayıcı modelleri · yazıp ara",
            &models.iter().map(|m| m.id.clone()).collect::<Vec<_>>(),
        )?
        else {
            return Ok(false);
        };
        let info = &models[index];
        selected.model = info.id.clone();
        selected.rate_card.source = PriceSource::Unknown;
        selected.local_tokenizer = None;
        selected.context_tokens = match info.context {
            Some(n) => n,
            None => match number(ui, "Modelin belgelenen bağlam kapasitesi", None)? {
                Some(n) => n,
                None => return Ok(false),
            },
        };
        selected.max_output_tokens = match info.output {
            Some(n) => n,
            None => match number(ui, "Modelin belgelenen tam çıktı kapasitesi", None)? {
                Some(n) => n,
                None => return Ok(false),
            },
        };
        selected.measurements = Default::default();
        selected.model_pool.clear();
        selected
    } else {
        providers[i].clone()
    };
    if !ui.confirm("Bu model seçilsin mi? Konuşmanın görünür geçmişi korunur.")? {
        return Ok(false);
    }
    settings::save_provider(app, selected.clone())?;
    ui.print(&format!(
        "Seçili model: {} / {}",
        selected.provider_id, selected.model
    ))?;
    Ok(true)
}
async fn probe(ui: &mut Ui, app: &App) -> Result {
    let settings = app.settings()?;
    let endpoint = settings.endpoint_with_key(settings.key()?)?;
    let mut url = endpoint.url.clone();
    let path = url.path().trim_end_matches('/');
    let prefix = path
        .strip_suffix("chat/completions")
        .or_else(|| path.strip_suffix("responses"))
        .or_else(|| path.strip_suffix("messages"))
        .ok_or(Error::Usage("Model listesi adresi belirlenemedi."))?;
    url.set_path(&format!("{}models", prefix));
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|_| Error::Io)?;
    match endpoint.authorize(client.get(url)).send().await{
        Ok(response) if response.status().is_success()=>ui.print("Model listesi bağlantısı başarılı. Bu kontrol model üretimini veya araç yeteneklerini doğrulamaz."),
        Ok(response)=>ui.print(&format!("Bağlantı HTTP {} döndürdü. 401/403: anahtar/yetki; 404: bu sağlayıcı model listelemeyi desteklemeyebilir. Model çağrısı yapılmadı.",response.status().as_u16())),
        Err(_)=>ui.print("Sunucuya bağlanılamadı. Adresi ve ağ erişimini kontrol et; anahtar hata metnine eklenmedi."),
    }
}
async fn environment(ui: &mut Ui, app: &App, workspace: Option<&std::path::Path>) -> Result {
    let workspace = workspace.ok_or(Error::Usage("Önce /project ile proje seç."))?;
    let mut command = tokio::process::Command::new("docker");
    command.args(["image", "ls", "--no-trunc", "--format", "{{json .}}"]);
    command.kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(0x08000000);
    let output = tokio::time::timeout(std::time::Duration::from_secs(8), command.output())
        .await
        .map_err(|_| {
            Error::Unavailable("Docker yanıt vermedi; Docker Desktop'ı açıp yeniden dene.")
        })?
        .map_err(|_| {
            Error::Unavailable(
                "Docker bulunamadı; kod çalıştırmak için hazırlanmış worker gerekiyor.",
            )
        })?;
    if !output.status.success() {
        return Err(Error::Unavailable(
            "Docker motoruna erişilemiyor; Docker Desktop'ı kontrol et.",
        ));
    }
    let images = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .collect::<Vec<_>>();
    if images.is_empty() {
        ui.print("Hazırlanmış worker bulunamadı. Eşleşen sürümün worker arşivini kur; rastgele bir image yeterli sayılmaz.")?;
        return Ok(());
    }
    let labels = images
        .iter()
        .map(|v| {
            format!(
                "{}:{} · {}",
                v["Repository"].as_str().unwrap_or(""),
                v["Tag"].as_str().unwrap_or(""),
                v["ID"].as_str().unwrap_or("")
            )
        })
        .collect::<Vec<_>>();
    if let Some(index) = ui.choose("Bu projede kullanılacak yerel worker", &labels)? {
        let mut environment = app.project_environment(workspace)?;
        environment.image_id = images[index]["ID"].as_str().ok_or(Error::Io)?.into();
        app.save_environment(workspace, environment)?;
        ui.print("Proje ortamı kaydedildi. Araç ve bağımlılık uygunluğu çalıştırma öncesinde ayrıca denetlenir.")?;
    }
    Ok(())
}
pub(crate) async fn menu(ui: &mut Ui, app: &App, workspace: Option<&std::path::Path>) -> Result {
    loop {
        let options = vec![
            "Sağlayıcı ekle".into(),
            "Sağlayıcıyı düzenle".into(),
            "Sağlayıcıyı kaldır".into(),
            "Model seç".into(),
            "Bağlantıyı dene · model listesi".into(),
            "Model fiyat bilgisi".into(),
            "Harcama tercihi".into(),
            "API kullanım izni".into(),
            "Proje ortamı".into(),
            "Geri".into(),
        ];
        let Some(choice) = ui.choose("Ayarlar · bu bilgisayar", &options)? else {
            return Ok(());
        };
        if matches!(choice, 3..=5) && settings::providers(app)?.is_empty() {
            provider(ui, app, None).await?;
            continue;
        }
        let result = match choice {
            0 => provider(ui, app, None).await,
            1 | 2 => {
                let all = settings::providers(app)?;
                if let Some(i) = ui.choose(
                    "Sağlayıcı seç",
                    &all.iter()
                        .map(|s| format!("{} / {}", s.provider_id, s.model))
                        .collect::<Vec<_>>(),
                )? {
                    if choice==1{provider(ui,app,Some(all[i].clone())).await}else if ui.confirm("Sağlayıcıyı seçimden kaldır? Eski görevlerin şifreli kayıtları korunur; sunucu anahtarı iptal edilmez.")?{settings::remove_provider(app,i)}else{Ok(())}
                } else {
                    Ok(())
                }
            }
            3 => model(ui, app).await.map(|_| ()),
            4 => probe(ui, app).await,
            5 => {
                let mut s = app.settings()?;
                let Some(input) = ui.input("Girdi fiyatı · 1 milyon token için USD", "", false)?
                else {
                    continue;
                };
                let Some(output) = ui.input("Çıktı fiyatı · 1 milyon token için USD", "", false)?
                else {
                    continue;
                };
                s.rate_card = RateCard {
                    source: PriceSource::OperatorDeclared {
                        note: "User-entered in terminal settings; not independently verified"
                            .into(),
                    },
                    input_micro_usd_per_million: money(&input)?,
                    output_micro_usd_per_million: money(&output)?,
                    cached_input_micro_usd_per_million: None,
                    cache_creation_micro_usd_per_million: None,
                };
                settings::save_provider(app, s)
            }
            6 => {
                let Some(s) = ui.input(
                    "Konuşma harcama tavanı · USD; boş bırak: kullanıcı tarafından tavan seçilmedi",
                    "",
                    false,
                )?
                else {
                    continue;
                };
                let mut p = settings::read(app)?;
                p.spending_micro_usd = if s.trim().is_empty() {
                    None
                } else {
                    Some(money(&s)?)
                };
                settings::write(app, &p)
            }
            7 => {
                let consent=ui.confirm("Seçtiğin API'ye mesaj ve gerekli görünür proje içeriği gönderilebilir; ücret doğabilir. İzin ver?")?;
                let mut p = settings::read(app)?;
                p.api_consent = consent;
                settings::write(app, &p)
            }
            8 => environment(ui, app, workspace).await,
            _ => return Ok(()),
        };
        if let Err(e) = result {
            ui.print(&e.to_string())?;
        }
    }
}
