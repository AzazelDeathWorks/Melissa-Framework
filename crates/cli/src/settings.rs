//! Encrypted local configuration. Import stores references, never resolves
//! an API secret. Secret resolution happens only after exact run approval.
use crate::{
    Error, Result,
    app::{App, absolute, bytes, decode, nonce},
    digest,
};
use melissa_model_gateway::{
    Protocol,
    routing::{Capabilities, Measurements, RateCard},
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
pub struct PoolModel {
    pub id: String,
    pub settings: Settings,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LabVariant {
    #[default]
    Production,
    ContextStateOff,
    CompactionOff,
    RepositoryIndexCacheOff,
    ProjectMemoryOff,
    AdaptiveReplanningOff,
}
impl LabVariant {
    fn is_production(&self) -> bool {
        *self == Self::Production
    }
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
    #[serde(
        default,
        skip_serializing_if = "melissa_model_gateway::transport::PromptCache::is_disabled"
    )]
    pub prompt_cache: melissa_model_gateway::transport::PromptCache,
    pub rate_card: RateCard,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub model_pool: Vec<PoolModel>,
    #[serde(default, skip_serializing_if = "Measurements::is_empty")]
    pub measurements: Measurements,
    #[serde(
        default,
        skip_serializing_if = "crate::model_tools::ToolProfile::is_precision"
    )]
    pub tool_profile: crate::model_tools::ToolProfile,
    #[serde(default, skip_serializing_if = "LabVariant::is_production")]
    pub(crate) lab_variant: LabVariant,
}
fn default_request_timeout() -> u64 {
    1_800_000
}
fn is_default_request_timeout(value: &u64) -> bool {
    *value == default_request_timeout()
}
impl Settings {
    pub fn validate(&self) -> Result {
        if self.model_pool.len() > 15 {
            return Err(Error::Usage("Model pool supports at most 16 models."));
        }
        if !self.lab_variant.is_production() && !self.model_pool.is_empty() {
            return Err(Error::Usage(
                "A single-feature lab control requires one fixed model.",
            ));
        }
        let mut ids = std::collections::BTreeSet::from(["cli-selected"]);
        let mut models = std::collections::BTreeSet::from([(&self.provider_id, &self.model)]);
        self.validate_model()?;
        for entry in &self.model_pool {
            if !melissa_workflow::plan::identifier(&entry.id)
                || !ids.insert(&entry.id)
                || !entry.settings.model_pool.is_empty()
                || !entry.settings.lab_variant.is_production()
                || !models.insert((&entry.settings.provider_id, &entry.settings.model))
            {
                return Err(Error::Usage("Invalid or duplicate model pool entry."));
            }
            entry.settings.validate_model()?;
        }
        Ok(())
    }
    fn validate_model(&self) -> Result {
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
        crate::model_pool::output_capability(
            &self.registration("configuration-check", endpoint, self.request_timeout_ms),
            self,
            "configuration-validation",
        )?;
        Ok(())
    }
    pub(crate) fn models(&self) -> impl Iterator<Item = (&str, &Settings)> {
        std::iter::once(("cli-selected", self)).chain(
            self.model_pool
                .iter()
                .map(|entry| (entry.id.as_str(), &entry.settings)),
        )
    }
    pub(crate) fn model_summary(&self) -> Value {
        json!(self.models().map(|(id, m)| json!({
            "id":id,"provider":m.provider_id,"model":m.model,"origin":url::Url::parse(&m.endpoint).ok().map(|url|url.origin().ascii_serialization()),
            "protocol":m.protocol,"tool_profile":m.tool_profile,"measurements":m.measurements,"lab_variant":m.lab_variant,
            "context_tokens":m.context_tokens,"max_output_tokens":m.max_output_tokens,"request_timeout_ms":m.request_timeout_ms,
            "json_object":m.json_object,"native_json_schema":m.native_json_schema,
            "capabilities":m.capabilities,
            "compatibility":{
                "basis":"configured_not_live_verified",
                "protocol":m.protocol,"streaming":m.capabilities.streaming,
                "tool_calling":m.capabilities.tool_calling,"vision":m.capabilities.vision,
                "json_object":m.json_object,"native_json_schema":m.native_json_schema,
                "continuation_scope":"same_provider_model_protocol",
                "automatic_retry":"proven_not_sent_only",
                "unknown_usage":"retain_reservation_and_reconcile",
                "chat_completions_compatibility_implied":false
            },
            "rate_card_digest":m.rate_card.digest(),
            "input_micro_usd_per_million":m.rate_card.input_micro_usd_per_million,
            "output_micro_usd_per_million":m.rate_card.output_micro_usd_per_million
        })).collect::<Vec<_>>())
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
        endpoint = endpoint
            .with_prompt_cache(self.prompt_cache)
            .map_err(|_| Error::Usage("Sağlayıcı cache ayarı protokolle uyuşmuyor."))?;
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
            .map_err(|_| Error::Store)?
            .with_lab_controls(
                self.lab_variant == LabVariant::ContextStateOff,
                self.lab_variant == LabVariant::CompactionOff,
            );
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
        for (_, model) in value.models() {
            let _ = model.context_compiler()?;
        }
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
            json!({"command":"config show","settings_digest":s.digest()?,"provider":s.provider_id,"model":s.model,"endpoint":s.endpoint,"protocol":s.protocol,"context_tokens":s.context_tokens,"max_output_tokens":s.max_output_tokens,"capabilities":s.capabilities,"json_object":s.json_object,"native_json_schema":s.native_json_schema,"local_tokenizer":s.local_tokenizer.as_ref().map(|c| json!({"blake3":c.blake3,"measurement":"native_wire_only"})),"credential":"[REDACTED: reference only]","secret_resolved":false,"models":s.model_summary()}),
        )
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn compatibility_summary_is_explicitly_configured_and_does_not_resolve_credentials() {
        let settings: Settings = serde_json::from_slice(include_bytes!(
            "../../../examples/cli/settings.template.json"
        ))
        .unwrap();
        let summary = settings.model_summary();
        assert_eq!(
            summary[0]["compatibility"]["basis"],
            "configured_not_live_verified"
        );
        assert_eq!(
            summary[0]["compatibility"]["automatic_retry"],
            "proven_not_sent_only"
        );
        assert_eq!(
            summary[0]["compatibility"]["chat_completions_compatibility_implied"],
            false
        );
        assert!(!summary.to_string().contains("credential"));
    }
    #[test]
    fn lab_variant_changes_the_approved_settings_and_only_one_model_is_allowed() {
        let mut settings: Settings = serde_json::from_slice(include_bytes!(
            "../../../examples/cli/settings.template.json"
        ))
        .unwrap();
        let digest = settings.digest().unwrap();
        assert!(
            serde_json::to_value(&settings)
                .unwrap()
                .get("lab_variant")
                .is_none()
        );
        let alternative = settings.clone();
        settings.lab_variant = LabVariant::ContextStateOff;
        assert_ne!(settings.digest().unwrap(), digest);
        assert!(
            settings
                .context_compiler()
                .unwrap()
                .policy_id()
                .contains(":lab:true:false")
        );
        settings.model_pool.push(PoolModel {
            id: "lab-test".into(),
            settings: alternative,
        });
        assert!(
            settings
                .validate()
                .unwrap_err()
                .to_string()
                .contains("lab control")
        );
    }
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
