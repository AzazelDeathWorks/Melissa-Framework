use crate::{Error, Result};
pub const EXTENSION_HELP: &str = "\nSkill/MCP/doküman/hook:\n  Oturum: /skills, /skill-import KLASOR, /extensions [JSON_DOSYASI]\n  melissa --json extension list --workspace PATH\n  melissa --json extension import-skill --path DIR [--digest BLAKE3]\n  melissa --json extension configure --workspace PATH --path JSON\n  melissa --json extension resolve --task TASK --source SOURCE --query TEXT\n  İçerik/ayarlar yeni görev onayına sabitlenir; eski göreve izin eklenmez. Belgeler: docs/EXTENSIONS_TR.md\n";
pub const METRICS_HELP: &str =
    "\nGörev ölçümleri:\n  melissa [--home PATH] --json task metrics ID\n";
pub const SESSION_HELP: &str = "\nTerminal oturumu:\n  melissa\n  melissa [--home PATH] session [--workspace PATH]\n\nGörevini yaz veya oturum komutları için /help yaz.";
pub const PROJECT_HELP: &str = "\nGenel proje ortamı ve komutları:\n  Oturum: /config PATH, /budget USD, /environment [IMAGE|JSON_PATH], /project-doctor\n  Amaç yaz, /create, /details, /approve, /run\n  /preflight, /environment-sync veya /exec JSON_COMMAND modeli çağırmaz; /resume ile model akışı sürer.\n  melissa --json project doctor --workspace PATH\n  melissa --json project preflight|sync TASK_ID\n  melissa --json project exec TASK_ID --command-json JSON_COMMAND\n  melissa --json project poll TASK_ID SESSION_ID [--after N]\n  melissa --json project stdin TASK_ID SESSION_ID --hex HEX_BYTES\n  melissa --json project eof|cancel TASK_ID SESSION_ID\n  Komutlar private snapshot kullanır; host mount/host writeback/ağ yoktur. Belgeler: docs/PROJECT_RUNTIME_TR.md\n";
pub const CAPABILITIES_HELP: &str = "\nProje belleği ve görev hazırlığı:\n  melissa task prepare --workspace PATH --goal TEXT --out NEW_FILE [--acceptance PRIVATE_RECIPE]\n  melissa memory list --workspace PATH\n  melissa memory accept|reject|forget --workspace PATH --id MEMORY_ID --digest REVIEW_DIGEST\n  melissa task replan ID --recipe REVIEWED_RECIPE\n\nPrepare yerel, incelenebilir taslak çıkarır; modeli çalıştırmaz veya kabul cevabı uydurmaz. Bellek önerileri operatör kabul edene kadar aramada kullanılmaz. Replan güvenli pause durumunda yalnız kalan uzman adımlarını değiştirir; çalışan adım korunur, revizyon o adım bittikten sonra etkinleşir. İlk dosya, kabul, süre ve maliyet yetkisi değişmez.\n";
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    path::PathBuf,
};
pub const HELP: &str = "Melissa CLI\n\n  melissa [--home PATH] [--json] init\n  melissa [--home PATH] [--json] profiles\n  melissa [--home PATH] [--json] task new --workspace PATH --goal TEXT [--profile ID] [--exclude PREFIX]...\n  melissa [--home PATH] [--json] task list\n  melissa [--home PATH] [--json] task show|diff|events|cancel ID\n  melissa [--home PATH] [--json] task approve ID --request REQUEST_ID --digest DIGEST\n  melissa [--home PATH] [--json] task evidence ID [--artifact ARTIFACT_ID --out NEW_FILE]\n\nTarifsiz görev yalnız incelemedir. Tarif ve kaynak bağlı onay sonrası task run model/izole worker çalıştırabilir.\nTauri ve masaüstü arayüzü kapsamdan çıkarılmıştır.\n";
pub const EXECUTION_HELP: &str = "\nYürütme ve kontrol:\n  melissa doctor\n  melissa config import PATH | config show\n  melissa task new --workspace PATH --goal TEXT --recipe PRIVATE_RECIPE.json\n  melissa task plan|progress|logs|history ID\n  melissa task run ID | task resume ID\n  melissa task pause ID | task cancel ID\n  melissa task wait ID --timeout-ms 30000\n  melissa task diff ID --content\n  melissa task delivery ID [--out NEW_DIRECTORY]\n\nTarifsiz görev yalnız incelemedir. Tarifli görevde tam kaynak/ayar/kapsam onayı sonrası run ücretli model ve izole worker başlatabilir. Pause güvenli checkpoint bekler; iptal isteği durmuş süreç kanıtı değildir. Tauri/GUI yoktur.\n";
#[derive(Debug)]
pub struct Options {
    pub home: Option<PathBuf>,
    pub json: bool,
    pub command: Command,
}
pub const DOMAIN_HELP: &str = "\nWeb/API/veritabanı (v2 özel tarif):\n  melissa task preview ID [--out NEW_DIRECTORY]\n  melissa task evidence ID [--artifact ARTIFACT_ID --out NEW_FILE]\n\nYerel tarayıcı/API/SQLite/PostgreSQL akışları sabit image üzerinde çalışır. Public development ve private final testleri ayrıdır. Preview kaynağa bağlı PNG ihracıdır; kalıcı web sunucusu açmaz.\n";
#[derive(Debug)]
pub enum Command {
    Verification {
        action: String,
        id: Option<String>,
        workspace: Option<PathBuf>,
        path: Option<PathBuf>,
        request: String,
    },
    RepositoryInspect {
        action: String,
        workspace: PathBuf,
        query: PathBuf,
    },
    Extension {
        action: String,
        workspace: Option<PathBuf>,
        path: Option<PathBuf>,
        digest: Option<String>,
        id: Option<String>,
        source: Option<String>,
        query: Option<String>,
    },
    ProjectDoctor {
        workspace: PathBuf,
    },
    ProjectOperation {
        id: String,
        operation: String,
        command: Option<String>,
    },
    ProjectPoll {
        id: String,
        session: String,
        after: u64,
    },
    ProjectInput {
        id: String,
        session: String,
        operation: String,
        hex: String,
    },
    Help,
    Version,
    Session {
        workspace: Option<PathBuf>,
    },
    Init,
    Profiles,
    Doctor,
    ConfigImport(PathBuf),
    ConfigShow,
    Memory {
        workspace: PathBuf,
        action: String,
        id: Option<String>,
        digest: Option<String>,
    },
    Prepare {
        workspace: PathBuf,
        goal: String,
        out: PathBuf,
        acceptance: Option<PathBuf>,
    },
    Replan {
        id: String,
        recipe: PathBuf,
    },
    New {
        workspace: PathBuf,
        goal: String,
        profile: String,
        excludes: Vec<String>,
        recipe: Option<PathBuf>,
    },
    List,
    Show(String),
    Metrics(String),
    Diff(String),
    Events(String),
    Cancel(String),
    Pause(String),
    Run(String),
    Resume(String),
    Progress(String),
    Logs(String),
    DiffContent(String),
    Wait {
        id: String,
        timeout_ms: u64,
    },
    Delivery {
        id: String,
        out: Option<PathBuf>,
    },
    Preview {
        id: String,
        out: Option<PathBuf>,
    },
    Approve {
        id: String,
        request: String,
        digest: String,
    },
    Evidence {
        id: String,
        artifact: Option<String>,
        out: Option<PathBuf>,
    },
}
fn text(v: OsString) -> Result<String> {
    v.into_string()
        .map_err(|_| Error::Usage("Metin ve kimlikler geçerli Unicode olmalı."))
}
fn flags(
    a: &mut impl Iterator<Item = OsString>,
    allowed: &[&str],
) -> Result<BTreeMap<String, OsString>> {
    let mut map = BTreeMap::new();
    while let Some(flag) = a.next() {
        let key = text(flag)?;
        if !allowed.contains(&key.as_str()) || map.contains_key(&key) {
            return Err(Error::Usage("Bilinmeyen veya tekrarlı seçenek."));
        }
        map.insert(key, a.next().ok_or(Error::Usage("Seçenek değeri eksik."))?);
    }
    Ok(map)
}
fn required(m: &mut BTreeMap<String, OsString>, key: &str) -> Result<OsString> {
    m.remove(key)
        .ok_or(Error::Usage("Zorunlu seçenek eksik; --help kullanın."))
}
pub fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Options> {
    let mut a = args.into_iter().peekable();
    let mut home = None;
    let mut json = false;
    while let Some(flag) = a.peek().and_then(|s| s.to_str()) {
        match flag {
            "--home" => {
                a.next();
                if home.is_some() {
                    return Err(Error::Usage("--home bir kez verilebilir."));
                }
                home = Some(PathBuf::from(
                    a.next().ok_or(Error::Usage("--home için yol gerekli."))?,
                ));
            }
            "--json" => {
                a.next();
                if json {
                    return Err(Error::Usage("--json bir kez verilebilir."));
                }
                json = true;
            }
            _ => break,
        }
    }
    let command = match a
        .next()
        .map(text)
        .transpose()?
        .as_deref()
        .unwrap_or("--help")
    {
        "--help" | "-h" | "help" => Command::Help,
        "--version" => Command::Version,
        "session" => {
            if json {
                return Err(Error::Usage("Terminal oturumu --json ile kullanılamaz."));
            }
            let mut f = flags(&mut a, &["--workspace"])?;
            Command::Session {
                workspace: f.remove("--workspace").map(PathBuf::from),
            }
        }
        "init" => Command::Init,
        "profiles" => Command::Profiles,
        "doctor" => Command::Doctor,
        "verification" => {
            let action = text(a.next().ok_or(Error::Usage(
                "verification configure|show|verify|reproduce gerekli.",
            ))?)?;
            if !["configure", "show", "verify", "reproduce"].contains(&action.as_str()) {
                return Err(Error::Usage("Bilinmeyen verification işlemi."));
            }
            let id = if ["verify", "reproduce"].contains(&action.as_str()) {
                Some(text(
                    a.next().ok_or(Error::Usage("Görev kimliği gerekli."))?,
                )?)
            } else {
                None
            };
            let allowed: &[&str] = match action.as_str() {
                "show" => &["--workspace"],
                "configure" => &["--workspace", "--path"],
                _ => &["--request-json"],
            };
            let mut f = flags(&mut a, allowed)?;
            let workspace = f.remove("--workspace").map(PathBuf::from);
            let path = f.remove("--path").map(PathBuf::from);
            let request = f
                .remove("--request-json")
                .map(text)
                .transpose()?
                .unwrap_or_else(|| "{}".into());
            if (id.is_none() && workspace.is_none())
                || (action == "configure" && path.is_none())
                || (id.is_some() && (workspace.is_some() || path.is_some()))
                || (action == "show" && path.is_some())
            {
                return Err(Error::Usage("Verification seçenekleri işlemle eşleşmiyor."));
            }
            Command::Verification {
                action,
                id,
                workspace,
                path,
                request,
            }
        }
        "repository" => {
            let action = text(a.next().ok_or(Error::Usage(
                "repository impact-map|structural-patch --workspace PATH --query JSON gerekli.",
            ))?)?;
            if !["impact-map", "structural-patch"].contains(&action.as_str()) {
                return Err(Error::Usage("Bilinmeyen repository işlemi."));
            }
            let mut f = flags(&mut a, &["--workspace", "--query"])?;
            Command::RepositoryInspect {
                action,
                workspace: PathBuf::from(
                    f.remove("--workspace")
                        .ok_or(Error::Usage("--workspace gerekli."))?,
                ),
                query: PathBuf::from(
                    f.remove("--query")
                        .ok_or(Error::Usage("--query gerekli."))?,
                ),
            }
        }
        "extension" => {
            let action = text(a.next().ok_or(Error::Usage(
                "extension list|configure|import-skill|resolve gerekli.",
            ))?)?;
            if !["list", "configure", "import-skill", "resolve"].contains(&action.as_str()) {
                return Err(Error::Usage("Bilinmeyen extension işlemi."));
            }
            let mut f = flags(
                &mut a,
                &[
                    "--workspace",
                    "--path",
                    "--digest",
                    "--task",
                    "--source",
                    "--query",
                ],
            )?;
            let workspace = f.remove("--workspace").map(PathBuf::from);
            let path = f.remove("--path").map(PathBuf::from);
            let digest = f.remove("--digest").map(text).transpose()?;
            let id = f.remove("--task").map(text).transpose()?;
            let source = f.remove("--source").map(text).transpose()?;
            let query = f.remove("--query").map(text).transpose()?;
            if match action.as_str() {
                "list" => {
                    workspace.is_none()
                        || path.is_some()
                        || digest.is_some()
                        || id.is_some()
                        || source.is_some()
                        || query.is_some()
                }
                "configure" => {
                    workspace.is_none()
                        || path.is_none()
                        || digest.is_some()
                        || id.is_some()
                        || source.is_some()
                        || query.is_some()
                }
                "import-skill" => {
                    workspace.is_some()
                        || path.is_none()
                        || id.is_some()
                        || source.is_some()
                        || query.is_some()
                }
                _ => {
                    id.is_none()
                        || source.is_none()
                        || query.is_none()
                        || workspace.is_some()
                        || path.is_some()
                        || digest.is_some()
                }
            } {
                return Err(Error::Usage("Extension seçenekleri işlemle uyuşmuyor."));
            }
            Command::Extension {
                action,
                workspace,
                path,
                digest,
                id,
                source,
                query,
            }
        }
        "project" => {
            let action = text(a.next().ok_or(Error::Usage(
                "project doctor|preflight|sync|exec|poll|stdin|eof|cancel gerekli.",
            ))?)?;
            match action.as_str() {
                "doctor" => {
                    let mut f = flags(&mut a, &["--workspace"])?;
                    Command::ProjectDoctor {
                        workspace: required(&mut f, "--workspace")?.into(),
                    }
                }
                "preflight" | "sync" | "exec" => {
                    let id = text(a.next().ok_or(Error::Usage("Görev kimliği gerekli."))?)?;
                    let mut f = flags(&mut a, &["--command-json"])?;
                    let command = f.remove("--command-json").map(text).transpose()?;
                    if (action == "exec") != command.is_some() {
                        return Err(Error::Usage("Yalnız exec için --command-json gerekli."));
                    }
                    Command::ProjectOperation {
                        id,
                        operation: action,
                        command,
                    }
                }
                "poll" => {
                    let id = text(a.next().ok_or(Error::Usage("Görev kimliği gerekli."))?)?;
                    let session = text(a.next().ok_or(Error::Usage("Süreç oturumu gerekli."))?)?;
                    let mut f = flags(&mut a, &["--after"])?;
                    let after = f
                        .remove("--after")
                        .map(text)
                        .transpose()?
                        .map(|v| {
                            v.parse::<u64>()
                                .map_err(|_| Error::Usage("--after sayısal olmalı."))
                        })
                        .transpose()?
                        .unwrap_or(0);
                    Command::ProjectPoll { id, session, after }
                }
                "stdin" | "eof" | "cancel" => {
                    let id = text(a.next().ok_or(Error::Usage("Görev kimliği gerekli."))?)?;
                    let session = text(a.next().ok_or(Error::Usage("Süreç oturumu gerekli."))?)?;
                    let mut f = flags(&mut a, &["--hex"])?;
                    let hex = f.remove("--hex").map(text).transpose()?.unwrap_or_default();
                    if action != "stdin" && !hex.is_empty() {
                        return Err(Error::Usage("Yalnız stdin --hex alır."));
                    }
                    Command::ProjectInput {
                        id,
                        session,
                        operation: action,
                        hex,
                    }
                }
                _ => return Err(Error::Usage("Proje komutu bilinmiyor.")),
            }
        }
        "memory" => {
            let action = text(
                a.next()
                    .ok_or(Error::Usage("memory list|accept|reject|forget gerekli."))?,
            )?;
            if !["list", "accept", "reject", "forget"].contains(&action.as_str()) {
                return Err(Error::Usage("Bellek işlemi geçersiz."));
            }
            let mut f = flags(&mut a, &["--workspace", "--id", "--digest"])?;
            Command::Memory {
                workspace: required(&mut f, "--workspace")?.into(),
                action,
                id: f.remove("--id").map(text).transpose()?,
                digest: f.remove("--digest").map(text).transpose()?,
            }
        }
        "config" => match a.next().map(text).transpose()?.as_deref() {
            Some("show") => Command::ConfigShow,
            Some("import") => Command::ConfigImport(PathBuf::from(
                a.next().ok_or(Error::Usage("Config dosyası gerekli."))?,
            )),
            _ => return Err(Error::Usage("config import PATH veya config show gerekli.")),
        },
        "task" => {
            let action = text(
                a.next()
                    .ok_or(Error::Usage("task komutu gerekli; melissa --help"))?,
            )?;
            match action.as_str() {
                "prepare" => {
                    let mut f = flags(&mut a, &["--workspace", "--goal", "--out", "--acceptance"])?;
                    Command::Prepare {
                        workspace: required(&mut f, "--workspace")?.into(),
                        goal: text(required(&mut f, "--goal")?)?,
                        out: required(&mut f, "--out")?.into(),
                        acceptance: f.remove("--acceptance").map(PathBuf::from),
                    }
                }
                "replan" => {
                    let id = text(a.next().ok_or(Error::Usage("Görev kimliği gerekli."))?)?;
                    let mut f = flags(&mut a, &["--recipe"])?;
                    Command::Replan {
                        id,
                        recipe: required(&mut f, "--recipe")?.into(),
                    }
                }
                "list" => Command::List,
                "new" => {
                    let mut workspace = None;
                    let mut goal = None;
                    let mut profile = None;
                    let mut excludes = vec![];
                    let mut recipe = None;
                    let mut seen = BTreeSet::new();
                    while let Some(flag) = a.next() {
                        let flag = text(flag)?;
                        if ![
                            "--workspace",
                            "--goal",
                            "--profile",
                            "--exclude",
                            "--recipe",
                        ]
                        .contains(&flag.as_str())
                        {
                            return Err(Error::Usage("Bilinmeyen task new seçeneği."));
                        }
                        if flag != "--exclude" && !seen.insert(flag.clone()) {
                            return Err(Error::Usage("Aynı seçenek tekrar verilemez."));
                        }
                        let value = a.next().ok_or(Error::Usage("Seçeneğin değeri eksik."))?;
                        match flag.as_str() {
                            "--workspace" => workspace = Some(PathBuf::from(value)),
                            "--goal" => goal = Some(text(value)?),
                            "--profile" => profile = Some(text(value)?),
                            "--recipe" => recipe = Some(PathBuf::from(value)),
                            _ => excludes.push(text(value)?),
                        }
                    }
                    if recipe.is_some() && seen.contains("--profile") {
                        return Err(Error::Usage(
                            "--recipe ile uzmanlar tarifte seçilir; --profile birlikte kullanılamaz.",
                        ));
                    }
                    Command::New {
                        workspace: workspace.ok_or(Error::Usage("--workspace gerekli."))?,
                        goal: goal.ok_or(Error::Usage("--goal gerekli."))?,
                        profile: profile.unwrap_or("melissa.code.implementer".into()),
                        excludes,
                        recipe,
                    }
                }
                "show" | "metrics" | "diff" | "events" | "history" | "cancel" | "pause" | "run"
                | "resume" | "plan" | "progress" | "logs" => {
                    let id = text(a.next().ok_or(Error::Usage("Görev kimliği gerekli."))?)?;
                    match action.as_str() {
                        "show" => Command::Show(id),
                        "metrics" => Command::Metrics(id),
                        "diff" => {
                            if a.peek().and_then(|s| s.to_str()) == Some("--content") {
                                a.next();
                                Command::DiffContent(id)
                            } else {
                                Command::Diff(id)
                            }
                        }
                        "events" | "history" => Command::Events(id),
                        "pause" => Command::Pause(id),
                        "run" => Command::Run(id),
                        "resume" => Command::Resume(id),
                        "plan" | "progress" => Command::Progress(id),
                        "logs" => Command::Logs(id),
                        _ => Command::Cancel(id),
                    }
                }
                "wait" => {
                    let id = text(a.next().ok_or(Error::Usage("Görev kimliği gerekli."))?)?;
                    if a.next().and_then(|s| s.into_string().ok()).as_deref()
                        != Some("--timeout-ms")
                    {
                        return Err(Error::Usage("--timeout-ms gerekli."));
                    }
                    let timeout_ms =
                        text(a.next().ok_or(Error::Usage("Bekleme değeri gerekli."))?)?
                            .parse::<u64>()
                            .map_err(|_| Error::Usage("Bekleme değeri geçersiz."))?;
                    Command::Wait { id, timeout_ms }
                }
                "delivery" | "preview" => {
                    let id = text(a.next().ok_or(Error::Usage("Görev kimliği gerekli."))?)?;
                    let out = if a.peek().is_some() {
                        if a.next().and_then(|s| s.into_string().ok()).as_deref() != Some("--out") {
                            return Err(Error::Usage("Teslim seçeneği --out olmalı."));
                        }
                        Some(PathBuf::from(
                            a.next().ok_or(Error::Usage("Çıktı klasörü gerekli."))?,
                        ))
                    } else {
                        None
                    };
                    if action == "preview" {
                        Command::Preview { id, out }
                    } else {
                        Command::Delivery { id, out }
                    }
                }
                "approve" => {
                    let id = text(a.next().ok_or(Error::Usage("Görev kimliği gerekli."))?)?;
                    let mut request = None;
                    let mut digest = None;
                    while let Some(flag) = a.next() {
                        let flag = text(flag)?;
                        let v = text(a.next().ok_or(Error::Usage("Onay değeri eksik."))?)?;
                        match flag.as_str() {
                            "--request" if request.is_none() => request = Some(v),
                            "--digest" if digest.is_none() => digest = Some(v),
                            _ => return Err(Error::Usage("Onayda bilinmeyen/tekrarlı seçenek.")),
                        }
                    }
                    Command::Approve {
                        id,
                        request: request.ok_or(Error::Usage("--request gerekli."))?,
                        digest: digest.ok_or(Error::Usage("--digest gerekli."))?,
                    }
                }
                "evidence" => {
                    let id = text(a.next().ok_or(Error::Usage("Görev kimliği gerekli."))?)?;
                    let mut artifact = None;
                    let mut out = None;
                    while let Some(flag) = a.next() {
                        let flag = text(flag)?;
                        let v = a
                            .next()
                            .ok_or(Error::Usage("Kanıt seçeneğinin değeri eksik."))?;
                        match flag.as_str() {
                            "--artifact" if artifact.is_none() => artifact = Some(text(v)?),
                            "--out" if out.is_none() => out = Some(PathBuf::from(v)),
                            _ => return Err(Error::Usage("Kanıtta bilinmeyen/tekrarlı seçenek.")),
                        }
                    }
                    if artifact.is_some() != out.is_some() {
                        return Err(Error::Usage("--artifact ve --out birlikte verilmeli."));
                    }
                    Command::Evidence { id, artifact, out }
                }
                _ => return Err(Error::Usage("Bilinmeyen task komutu; melissa --help.")),
            }
        }
        _ => return Err(Error::Usage("Bilinmeyen komut; melissa --help.")),
    };
    if a.next().is_some() {
        return Err(Error::Usage("Komuttan sonra beklenmeyen argüman."));
    }
    Ok(Options {
        home,
        json,
        command,
    })
}

pub const REPOSITORY_HELP: &str = "\nDepo analizi (model/API çağırmaz):\n  melissa --json repository impact-map|structural-patch --workspace PATH --query JSON_FILE\n  Belgeler: docs/REPOSITORY_SCALE_TR.md\n";

pub const VERIFICATION_HELP: &str = "\nProje doğrulama (model/API çağırmaz):\n  melissa --json verification configure --workspace PATH --path JSON_FILE\n  melissa --json verification show --workspace PATH\n  melissa --json verification reproduce TASK [--request-json JSON]\n  melissa --json verification verify TASK [--request-json JSON]\n  scripts/verify-change.ps1 ve scripts/reproduce-failure.ps1\n  Belgeler: docs/VERIFICATION_TR.md; sonuç PASS değilse çıkış kodu 7.\n";
