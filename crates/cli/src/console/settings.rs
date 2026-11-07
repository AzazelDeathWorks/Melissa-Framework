use crate::{
    Error, Result,
    app::{App, bytes, nonce, safe_path},
    settings::{Credential, Settings},
};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};
use zeroize::Zeroizing;

#[derive(Default, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Preferences {
    #[serde(default)]
    pub providers: Vec<Settings>,
    #[serde(default)]
    pub last_project: Option<PathBuf>,
    #[serde(default)]
    pub spending_micro_usd: Option<u64>,
    #[serde(default)]
    pub api_consent: bool,
    #[serde(default)]
    pub drafts: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    pub revision: u64,
    #[serde(default)]
    pub history: Vec<String>,
    #[serde(default)]
    pub queues: std::collections::BTreeMap<String, Vec<String>>,
}
pub(crate) fn read(app: &App) -> Result<Preferences> {
    let path = app.home.join("console-preferences.enc");
    safe_path(&path)?;
    if !path.exists() {
        return Ok(Preferences::default());
    }
    let encrypted = fs::read(path)?;
    let plain = Zeroizing::new(
        app.key
            .open(&encrypted, b"melissa.console-preferences/v1")?,
    );
    crate::app::decode(&plain)
}
pub(crate) fn write(app: &App, value: &Preferences) -> Result {
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(app.home.join("console-preferences.lock"))?;
    lock.try_lock()
        .map_err(|_| Error::Conflict("Ayarlar başka bir oturumda düzenleniyor."))?;
    if read(app)?.revision != value.revision {
        return Err(Error::Conflict(
            "Ayarlar değişti; güncel ayarları yeniden aç.",
        ));
    }
    let mut value = value.clone();
    value.revision = value.revision.checked_add(1).ok_or(Error::Store)?;
    atomic(
        app,
        &app.home.join("console-preferences.enc"),
        &bytes(&value)?,
        b"melissa.console-preferences/v1",
    )
}
pub(crate) fn atomic(app: &App, path: &Path, plain: &[u8], aad: &[u8]) -> Result {
    safe_path(path)?;
    let parent = path.parent().ok_or(Error::Io)?;
    fs::create_dir_all(parent)?;
    let encrypted = app.key.seal(plain, aad)?;
    let temp = parent.join(format!("{}.tmp", nonce()?));
    let mut file = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temp)?;
    file.write_all(&encrypted)?;
    file.sync_all()?;
    drop(file);
    safe_path(path)?;
    fs::rename(temp, path)?;
    Ok(())
}
pub(crate) fn save_key(app: &App, secret: Zeroizing<String>) -> Result<Credential> {
    melissa_model_gateway::transport::ApiKey::new(secret.to_string())
        .map_err(|_| Error::Usage("Anahtar biçimi geçersiz."))?;
    let id = nonce()?;
    let path = app.home.join("credentials").join(format!("{id}.enc"));
    atomic(
        app,
        &path,
        secret.as_bytes(),
        format!("melissa.credential/v1:{id}").as_bytes(),
    )?;
    Ok(Credential::Stored {
        home: app.home.clone(),
        id,
    })
}
pub(crate) fn providers(app: &App) -> Result<Vec<Settings>> {
    let mut p = read(app)?.providers;
    if let Ok(current) = app.settings()
        && !p
            .iter()
            .any(|s| s.provider_id == current.provider_id && s.model == current.model)
    {
        p.push(current);
    }
    Ok(p)
}
pub(crate) fn save_provider(app: &App, settings: Settings) -> Result {
    settings.validate()?;
    let mut preferences = read(app)?;
    preferences
        .providers
        .retain(|s| s.provider_id != settings.provider_id || s.model != settings.model);
    preferences.providers.push(settings.clone());
    write(app, &preferences)?;
    app.save_settings(&settings)?;
    Ok(())
}
pub(crate) fn remove_provider(app: &App, index: usize) -> Result {
    let mut all = providers(app)?;
    if index >= all.len() {
        return Err(Error::Usage("Sağlayıcı seçimi güncel değil."));
    }
    let selected = all.remove(index);
    let mut prefs = read(app)?;
    prefs.providers = all.clone();
    write(app, &prefs)?;
    if app
        .settings()
        .is_ok_and(|s| s.provider_id == selected.provider_id && s.model == selected.model)
    {
        if let Some(next) = all.first() {
            app.save_settings(next)?;
        } else {
            let path = app.home.join("settings.enc");
            safe_path(&path)?;
            fs::remove_file(path)?;
        }
    }
    // Credential envelopes referenced by historical task settings remain intact.
    // Removing a provider stops selection; revocation belongs to the provider.
    Ok(())
}
pub(crate) fn endpoint(base: &str, protocol: melissa_model_gateway::Protocol) -> Result<String> {
    let base = base.trim();
    if !base
        .get(..8)
        .is_some_and(|s| s.eq_ignore_ascii_case("https://"))
        && !base
            .get(..7)
            .is_some_and(|s| s.eq_ignore_ascii_case("http://"))
    {
        return Err(Error::Usage(
            "Base URL https:// ile başlamalı. API anahtarı sonraki ayrı alana yazılır.",
        ));
    }
    if base.chars().any(char::is_whitespace) || base.contains('\\') {
        return Err(Error::Usage(
            "Base URL boşluk, satır sonu veya ters eğik çizgi içeremez.",
        ));
    }
    let mut url = url::Url::parse(base)
        .map_err(|_| Error::Usage("Base URL geçersiz. Örnek: https://api.example.com/v1"))?;
    if url.host_str().is_none() {
        return Err(Error::Usage("Base URL bir sunucu adı içermeli."));
    }
    if url.host_str() == Some("localhost") {
        url.set_host(Some("127.0.0.1")).map_err(|_| Error::Io)?;
    }
    if url.username() != ""
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(Error::Usage(
            "Base URL içine anahtar, kullanıcı bilgisi veya sorgu ekleme; anahtar ayrı alana yazılır.",
        ));
    }
    if url.scheme() != "https" && !crate::settings::local_endpoint(&url) {
        return Err(Error::Denied(
            "HTTP yalnız bu bilgisayarın loopback adresinde kullanılabilir.",
        ));
    }
    let suffix = match protocol {
        melissa_model_gateway::Protocol::OpenAiChatCompletions => "/chat/completions",
        melissa_model_gateway::Protocol::OpenAiResponses => "/responses",
        melissa_model_gateway::Protocol::AnthropicMessages => "/messages",
    };
    let path = url.path().trim_end_matches('/');
    if ["/chat/completions", "/responses", "/messages"]
        .iter()
        .any(|other| path.ends_with(other))
        && !path.ends_with(suffix)
    {
        return Err(Error::Usage(
            "Adres seçilen API türüyle uyuşmuyor. Base URL gir veya API türünü değiştir.",
        ));
    }
    let path = if path.ends_with(suffix) {
        path.to_owned()
    } else {
        format!("{}{suffix}", if path.is_empty() { "/v1" } else { path })
    };
    url.set_path(&path);
    Ok(url.into())
}
pub(crate) fn validate_key_input(secret: &str) -> Result<()> {
    let secret = secret.trim();
    if secret
        .get(..7)
        .is_some_and(|s| s.eq_ignore_ascii_case("http://"))
        || secret
            .get(..8)
            .is_some_and(|s| s.eq_ignore_ascii_case("https://"))
    {
        return Err(Error::Usage(
            "Bu alan API anahtarı içindir. Sunucu adresi Base URL alanına yazılır.",
        ));
    }
    melissa_model_gateway::transport::ApiKey::new(secret.to_owned())
        .map(|_| ())
        .map_err(|_| Error::Usage("API anahtarı boş olamaz veya boşluk/satır sonu içeremez."))
}
#[cfg(all(test, windows))]
mod tests {
    use super::*;
    #[test]
    fn keys_are_encrypted_and_not_in_preferences() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        App::init(&home).unwrap();
        let app = App::open(&home).unwrap();
        let secret = "local-test-credential-not-a-real-key";
        let credential = save_key(&app, Zeroizing::new(secret.into())).unwrap();
        let Credential::Stored { home, id } = credential else {
            panic!()
        };
        let bytes = fs::read(home.join("credentials").join(format!("{id}.enc"))).unwrap();
        assert!(!String::from_utf8_lossy(&bytes).contains(secret));
        write(&app, &Preferences::default()).unwrap();
        assert!(
            !String::from_utf8_lossy(&fs::read(app.home.join("console-preferences.enc")).unwrap())
                .contains(secret)
        );
    }
    #[test]
    fn local_endpoints_are_explicit_and_no_remote_http() {
        use melissa_model_gateway::Protocol as P;
        assert_eq!(
            endpoint("http://localhost:11434/v1", P::OpenAiChatCompletions).unwrap(),
            "http://127.0.0.1:11434/v1/chat/completions"
        );
        assert!(endpoint("http://example.com/v1", P::OpenAiResponses).is_err());
        assert!(endpoint("https://key@example.com/v1", P::OpenAiResponses).is_err());
    }
    #[test]
    fn base_urls_and_complete_endpoints_follow_the_selected_protocol() {
        use melissa_model_gateway::Protocol as P;
        for (protocol, suffix) in [
            (P::OpenAiChatCompletions, "chat/completions"),
            (P::OpenAiResponses, "responses"),
            (P::AnthropicMessages, "messages"),
        ] {
            for base in ["https://api.example.com", "https://api.example.com/v1/"] {
                assert_eq!(
                    endpoint(base, protocol).unwrap(),
                    format!("https://api.example.com/v1/{suffix}")
                );
            }
            assert_eq!(
                endpoint(
                    &format!("https://api.example.com/api/v1/{suffix}/"),
                    protocol
                )
                .unwrap(),
                format!("https://api.example.com/api/v1/{suffix}")
            );
        }
        assert!(
            endpoint(
                "https://api.example.com/v1/messages",
                P::OpenAiChatCompletions
            )
            .is_err()
        );
        assert_eq!(
            endpoint(
                "https://api.example.com/custom-responses",
                P::OpenAiResponses
            )
            .unwrap(),
            "https://api.example.com/custom-responses/responses"
        );
    }
    #[test]
    fn setup_errors_never_echo_misplaced_credentials() {
        use melissa_model_gateway::Protocol as P;
        let fake = "fixture-key-not-a-real-credential";
        for value in [
            fake.to_owned(),
            format!("https://{fake}@example.com"),
            format!("https://example.com?key={fake}"),
        ] {
            let error = endpoint(&value, P::OpenAiChatCompletions)
                .unwrap_err()
                .to_string();
            assert!(!error.contains(fake));
        }
        assert!(validate_key_input("https://api.example.com/v1").is_err());
        assert!(validate_key_input("").is_err());
        assert!(validate_key_input(fake).is_ok());
        assert!(endpoint("https://api.example.com/v1\n/path", P::OpenAiResponses).is_err());
    }
}
