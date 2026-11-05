//! Encrypted local configuration. Import stores references, never resolves
//! an API secret. Secret resolution happens only after exact run approval.
use crate::{
    Error, Result,
    app::{App, absolute, bytes, decode, nonce},
    digest,
};
use melissa_model_gateway::{
    Protocol,
    routing::{Capabilities, RateCard},
    transport::{ApiKey, EndpointTrust, ProviderEndpoint},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};
use zeroize::Zeroizing;
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Credential {
    File { path: PathBuf },
    Environment { name: String },
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub schema_version: String,
    pub provider_id: String,
    pub endpoint: String,
    pub protocol: Protocol,
    pub model: String,
    pub credential: Credential,
    pub context_tokens: u64,
    pub max_output_tokens: u64,
    #[serde(
        default = "default_request_timeout",
        skip_serializing_if = "is_default_request_timeout"
    )]
    pub request_timeout_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_tokenizer: Option<melissa_model_gateway::local_tokenizer::LocalTokenizerConfig>,
    pub capabilities: Capabilities,
    pub json_object: bool,
    pub native_json_schema: bool,
    pub single_attempt: bool,
    pub rate_card: RateCard,
}
fn default_request_timeout() -> u64 {
    1_800_000
}
fn is_default_request_timeout(value: &u64) -> bool {
    *value == default_request_timeout()
}
impl Settings {
    pub fn validate(&self) -> Result {
        if let Some(config) = &self.local_tokenizer {
            config
                .validate()
                .map_err(|_| Error::Usage("Yerel tokenizer yolu/özeti geçersiz."))?;
            absolute(&config.path)?;
        }
        if self.schema_version != "melissa.cli-settings/v1"
            || self.provider_id.is_empty()
            || self.provider_id.len() > 64
            || !self
                .provider_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
            || self.model.trim().is_empty()
            || self.model.len() > 256
            || self.context_tokens == 0
            || self.max_output_tokens == 0
            || self.max_output_tokens >= self.context_tokens
            || self.request_timeout_ms == 0
            || self.request_timeout_ms > 1_800_000
            || !self.capabilities.tool_calling
            || !self.capabilities.streaming
            || !(self.json_object || self.native_json_schema)
        {
            return Err(Error::Usage(
                "Sağlayıcı/model/yetenek yapılandırması geçersiz.",
            ));
        }
        let u = url::Url::parse(&self.endpoint).map_err(|_| Error::Usage("Endpoint geçersiz."))?;
        if u.scheme() != "https"
            || !u.username().is_empty()
            || u.password().is_some()
            || u.query().is_some()
            || u.fragment().is_some()
        {
            return Err(Error::Denied("Remote endpoint yalın HTTPS adresi olmalı."));
        }
        match &self.credential {
            Credential::File { path } => {
                absolute(path)?;
            }
            Credential::Environment { name } => {
                if name.is_empty()
                    || name.len() > 256
                    || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
                {
                    return Err(Error::Usage("Credential değişken adı geçersiz."));
                }
            }
        }
        let endpoint = self.endpoint_with_key(
            ApiKey::new("configuration-validation".into()).map_err(|_| Error::Store)?,
        )?;
        self.rate_card
            .validate(&endpoint)
            .map_err(|_| Error::Usage("Maliyet tarifesi kaynağı geçersiz."))
    }
    pub(crate) fn endpoint_with_key(&self, key: ApiKey) -> Result<ProviderEndpoint> {
        let mut endpoint = ProviderEndpoint::new(
            self.provider_id.clone(),
            format!("provider:{}", self.provider_id),
            format!("authorization:provider:{}", self.provider_id),
            self.protocol,
            EndpointTrust::RemoteHttps,
            &self.endpoint,
            key,
        )
        .map_err(|_| Error::Usage("Endpoint yapılandırması geçersiz."))?;
        if self.protocol == Protocol::OpenAiResponses && self.capabilities.vision {
            endpoint = endpoint
                .with_openai_image_detail(melissa_model_gateway::openai::ImageDetail::High)
                .map_err(|_| Error::Usage("Görsel aktarım politikası geçersiz."))?;
        }
        if self.single_attempt {
            let reviewed_origin = endpoint.url.origin().ascii_serialization();
            endpoint
                .with_single_attempt_origin(&reviewed_origin)
                .map_err(|_| Error::Usage("Tek-deneme protokol ayarı desteklenmiyor."))
        } else {
            Ok(endpoint)
        }
    }
    pub(crate) fn key(&self) -> Result<ApiKey> {
        match &self.credential {
            Credential::Environment { name } => ApiKey::from_env(name)
                .map_err(|_| Error::Unavailable("Yapılandırılmış credential değişkeni açılamadı.")),
            Credential::File { path } => {
                let path = absolute(path)?;
                let mut file = fs::File::open(path).map_err(|_| {
                    Error::Unavailable("Yapılandırılmış credential dosyası açılamadı.")
                })?;
                let mut data = Zeroizing::new(String::new());
                (&mut file)
                    .take(8193)
                    .read_to_string(&mut data)
                    .map_err(|_| Error::Denied("Credential dosyası geçersiz."))?;
                if data.len() > 8192 {
                    return Err(Error::Denied("Credential dosyası boyutu geçersiz."));
                }
                ApiKey::new(data.trim().to_owned())
                    .map_err(|_| Error::Denied("Credential dosyası biçimi geçersiz."))
            }
        }
    }
    pub fn digest(&self) -> Result<String> {
        Ok(digest(&bytes(self)?))
    }
    pub(crate) fn context_compiler(
        &self,
    ) -> Result<melissa_model_gateway::context_compiler::ContextCompiler> {
        let compiler = melissa_model_gateway::context_compiler::ContextCompiler::default()
            .with_intelligence(Default::default())
            .map_err(|_| Error::Store)?;
        match &self.local_tokenizer {
            Some(config) => Ok(compiler.with_wire_tokenizer(
                melissa_model_gateway::local_tokenizer::LocalWireTokenizer::load(
                    config,
                    &self.endpoint,
                    self.protocol,
                    &self.model,
                )
                .map_err(|_| {
                    Error::Usage("Yerel tokenizer açılamadı veya özeti/biçimi geçersiz.")
                })?,
            )),
            None => Ok(compiler),
        }
    }
}
impl App {
    pub fn configure(&self, path: &Path) -> Result<Value> {
        let path = absolute(path)?;
        let repo = melissa_repository::Repository::open(
            path.parent().ok_or(Error::Io)?,
            Default::default(),
            vec![],
        )?;
        let (_, data) = repo.read(path.file_name().and_then(|s| s.to_str()).ok_or(Error::Io)?)?;
        let value: Settings = decode(&data)?;
        value.validate()?;
        // Validate the pinned public artifact without resolving any API key.
        let _ = value.context_compiler()?;
        let encoded = self.key.seal(&bytes(&value)?, b"melissa.cli.settings.v1")?;
        let temp = self.home.join(format!("settings-{}.tmp", nonce()?));
        let mut file = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temp)?;
        file.write_all(&encoded)?;
        file.sync_all()?;
        drop(file);
        crate::app::safe_path(&self.home.join("settings.enc"))?;
        fs::rename(&temp, self.home.join("settings.enc"))?;
        self.configuration()
    }
    pub(crate) fn settings(&self) -> Result<Settings> {
        let repo = melissa_repository::Repository::open(&self.home, Default::default(), vec![])?;
        let (_, cipher) = repo
            .read("settings.enc")
            .map_err(|_| Error::Usage("Önce config import PATH ile sağlayıcı ayarını kaydedin."))?;
        let plain = Zeroizing::new(self.key.open(&cipher, b"melissa.cli.settings.v1")?);
        let s: Settings = decode(&plain)?;
        s.validate()?;
        Ok(s)
    }
    pub fn configuration(&self) -> Result<Value> {
        let s = self.settings()?;
        Ok(
            json!({"command":"config show","settings_digest":s.digest()?,"provider":s.provider_id,"model":s.model,"endpoint":s.endpoint,"protocol":s.protocol,"context_tokens":s.context_tokens,"max_output_tokens":s.max_output_tokens,"capabilities":s.capabilities,"json_object":s.json_object,"native_json_schema":s.native_json_schema,"local_tokenizer":s.local_tokenizer.as_ref().map(|c| json!({"blake3":c.blake3,"measurement":"native_wire_only"})),"credential":"[REDACTED: reference only]","secret_resolved":false}),
        )
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn optional_tokenizer_preserves_legacy_digest_and_binds_new_approvals() {
        let mut s: Settings = serde_json::from_slice(include_bytes!(
            "../../../examples/cli/settings.template.json"
        ))
        .unwrap();
        let legacy = s.digest().unwrap();
        assert!(
            serde_json::to_value(&s)
                .unwrap()
                .get("local_tokenizer")
                .is_none()
        );
        assert!(
            s.context_compiler()
                .unwrap()
                .policy_id()
                .starts_with("native-context/v1:utf8-framing:intelligence/v2:")
        );
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tokenizer.json");
        let bytes = include_bytes!("../../model-gateway/tests/fixtures/local-tokenizer.json");
        std::fs::write(&path, bytes).unwrap();
        s.local_tokenizer = Some(
            melissa_model_gateway::local_tokenizer::LocalTokenizerConfig {
                path: path.clone(),
                blake3: blake3::hash(bytes).to_hex().to_string(),
            },
        );
        let selected = s.digest().unwrap();
        assert_ne!(legacy, selected);
        assert!(
            s.context_compiler()
                .unwrap()
                .policy_id()
                .contains("hf-wire-observation/v1:")
        );
        std::fs::write(path, b"{}").unwrap();
        assert!(s.context_compiler().is_err());
        // Resolution of the deliberately invalid template credential never ran.
        s.local_tokenizer = None;
        assert_eq!(s.digest().unwrap(), legacy);
    }
    #[test]
    fn explicit_vision_uses_compatible_high_detail_without_resolving_credentials() {
        let mut s: Settings = serde_json::from_slice(include_bytes!(
            "../../../examples/cli/settings.template.json"
        ))
        .unwrap();
        s.capabilities.vision = true;
        let selected = s
            .endpoint_with_key(ApiKey::new("test-placeholder-not-a-secret".into()).unwrap())
            .unwrap();
        assert_eq!(
            selected.openai_image_detail(),
            melissa_model_gateway::openai::ImageDetail::High
        );
        s.capabilities.vision = false;
        let legacy = s
            .endpoint_with_key(ApiKey::new("test-placeholder-not-a-secret".into()).unwrap())
            .unwrap();
        assert_eq!(
            legacy.openai_image_detail(),
            melissa_model_gateway::openai::ImageDetail::Original
        );
    }
}
