//! Offline, hash-pinned tokenization of the serialized native JSON envelope.
//! This measures wire text, NOT a provider's rendered prompt. It never replaces
//! authoritative input counts, capacity checks or monetary reservations.
use crate::{GatewayError, Protocol, Result, transport::ProviderEndpoint};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    fs::File,
    io::Read,
    path::{Path, PathBuf},
};
use tokenizers::Tokenizer;

const MAX_ARTIFACT_BYTES: u64 = 32 * 1024 * 1024;
const MAX_WIRE_BYTES: usize = 2 * 1024 * 1024;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalTokenizerConfig {
    pub path: PathBuf,
    /// Lowercase BLAKE3 of the exact tokenizer.json bytes, without a prefix.
    pub blake3: String,
}
impl LocalTokenizerConfig {
    pub fn validate(&self) -> Result<()> {
        if !self.path.is_absolute()
            || self.blake3.len() != 64
            || !self
                .blake3
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(GatewayError::invalid(
                "local tokenizer path/digest configuration",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WireTokenMeasurement {
    pub token_count: u64,
    pub artifact_digest: String,
    pub method: String,
}

pub struct LocalWireTokenizer {
    tokenizer: Tokenizer,
    endpoint: url::Url,
    protocol: Protocol,
    model: String,
    digest: String,
    policy: String,
}
impl LocalWireTokenizer {
    pub fn load(
        config: &LocalTokenizerConfig,
        endpoint: &str,
        protocol: Protocol,
        model: &str,
    ) -> Result<Self> {
        config.validate()?;
        let endpoint =
            url::Url::parse(endpoint).map_err(|_| GatewayError::invalid("tokenizer endpoint"))?;
        if model.trim().is_empty()
            || model.len() > 256
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || endpoint.query().is_some()
            || endpoint.fragment().is_some()
        {
            return Err(GatewayError::invalid("tokenizer binding"));
        }
        let bytes = read_artifact(&config.path)?;
        if blake3::hash(&bytes).to_hex().as_str() != config.blake3 {
            return Err(GatewayError::invalid("local tokenizer digest mismatch"));
        }
        let mut tokenizer = Tokenizer::from_bytes(bytes)
            .map_err(|_| GatewayError::invalid("unsupported local tokenizer artifact"))?;
        // A tokenizer artifact may contain training-time truncation/padding.
        // Counting must never silently discard or pad any of the supplied text.
        tokenizer
            .with_truncation(None)
            .map_err(|_| GatewayError::invalid("tokenizer truncation"))?;
        tokenizer.with_padding(None);
        let binding = serde_json::to_vec(&(endpoint.as_str(), protocol, model, &config.blake3))
            .map_err(|_| GatewayError::invalid("tokenizer binding encoding"))?;
        let policy = format!("hf-wire-observation/v1:{}", blake3::hash(&binding).to_hex());
        Ok(Self {
            tokenizer,
            endpoint,
            protocol,
            model: model.into(),
            digest: config.blake3.clone(),
            policy,
        })
    }
    pub fn policy_id(&self) -> &str {
        &self.policy
    }
    /// Complete UTF-8 native JSON, without added special tokens. This is an
    /// observation only: upstream chat templates, image expansion and opaque
    /// state cannot be recovered from JSON by a text tokenizer.
    pub fn measure(
        &self,
        endpoint: &ProviderEndpoint,
        model: &str,
        wire: &Value,
    ) -> Result<Option<WireTokenMeasurement>> {
        if endpoint.url != self.endpoint
            || endpoint.protocol != self.protocol
            || model != self.model
        {
            return Ok(None);
        }
        let mut writer = BoundedWire(Vec::new());
        if serde_json::to_writer(&mut writer, wire).is_err() {
            return Ok(None);
        }
        let text = std::str::from_utf8(&writer.0)
            .map_err(|_| GatewayError::invalid("tokenizer wire UTF-8"))?;
        let encoded = self
            .tokenizer
            .encode(text, false)
            .map_err(|_| GatewayError::invalid("local tokenizer encoding failed"))?;
        Ok(Some(WireTokenMeasurement {
            token_count: encoded.len() as u64,
            artifact_digest: format!("blake3:{}", self.digest),
            method: "hf-tokenizers-0.22.2-native-json; not provider input tokens".into(),
        }))
    }
}
fn read_artifact(path: &Path) -> Result<Vec<u8>> {
    let file =
        File::open(path).map_err(|_| GatewayError::invalid("local tokenizer unavailable"))?;
    let metadata = file
        .metadata()
        .map_err(|_| GatewayError::invalid("local tokenizer metadata"))?;
    if !metadata.is_file() || metadata.len() > MAX_ARTIFACT_BYTES {
        return Err(GatewayError::invalid("local tokenizer artifact limit"));
    }
    let mut bytes = Vec::new();
    file.take(MAX_ARTIFACT_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| GatewayError::invalid("local tokenizer read"))?;
    if bytes.len() as u64 > MAX_ARTIFACT_BYTES {
        return Err(GatewayError::invalid("local tokenizer artifact limit"));
    }
    Ok(bytes)
}
struct BoundedWire(Vec<u8>);
impl std::io::Write for BoundedWire {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > MAX_WIRE_BYTES.saturating_sub(self.0.len()) {
            return Err(std::io::Error::other("tokenizer wire observation limit"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
