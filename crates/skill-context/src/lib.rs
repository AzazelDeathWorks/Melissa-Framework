//! Host-side CODE-07 package, context and persistence primitives.
//! Package text and model memory proposals are data, never execution authority.
use serde::{Deserialize, Serialize};
use std::fmt;

pub mod assembly;
pub mod build_cache;
pub mod context;
pub mod durable;
mod graph_view;
pub mod memory_sources;
pub mod package;
pub mod release;
mod repository_index;
pub mod retrieval;
pub mod runtime;
pub mod scripts;
pub mod state;
mod worker_service;

pub type Result<T> = std::result::Result<T, Error>;
#[derive(Debug)]
pub struct Error(pub String);
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for Error {}
impl From<serde_json::Error> for Error {
    fn from(_: serde_json::Error) -> Self {
        Self("invalid CODE-07 JSON".into())
    }
}
impl From<melissa_local_store::StoreError> for Error {
    fn from(e: melissa_local_store::StoreError) -> Self {
        Self(e.to_string())
    }
}
impl From<melissa_core_contracts::validation::ContractError> for Error {
    fn from(e: melissa_core_contracts::validation::ContractError) -> Self {
        Self(e.to_string())
    }
}
pub(crate) fn require(ok: bool, message: &str) -> Result<()> {
    if ok {
        Ok(())
    } else {
        Err(Error(message.into()))
    }
}
pub fn digest(bytes: &[u8]) -> String {
    format!("blake3:{}", blake3::hash(bytes).to_hex())
}
pub const MAX_STATE_BYTES: usize = melissa_model_gateway::checkpoint::MAX_CHECKPOINT_BYTES;
/// Count escaped JSON bytes without allocating a serialized copy.
pub(crate) fn encoded_len<T: serde::Serialize>(value: &T, limit: usize) -> Result<usize> {
    struct Counter {
        bytes: usize,
        limit: usize,
    }
    impl std::io::Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > self.limit.saturating_sub(self.bytes) {
                return Err(std::io::Error::other("state envelope exceeded"));
            }
            self.bytes += bytes.len();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut count = Counter { bytes: 0, limit };
    serde_json::to_writer(&mut count, value)
        .map_err(|_| Error("context state exceeds envelope or cannot serialize".into()))?;
    Ok(count.bytes)
}
/// Same bound for writing and restoring a checkpoint; abort serialization
/// before an oversized allocation or irreversible CAS publication.
pub fn encode_state<T: serde::Serialize>(value: &T) -> Result<Vec<u8>> {
    melissa_model_gateway::checkpoint::encode(value).map_err(|e| Error(e.to_string()))
}
pub(crate) fn json<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T> {
    melissa_model_gateway::checkpoint::decode(bytes).map_err(|e| Error(e.to_string()))
}

/// Exact project/user/engagement boundary. None does not mean wildcard.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Namespace {
    pub user: String,
    pub project: String,
    pub engagement: Option<String>,
}
impl Namespace {
    pub fn validate(&self) -> Result<()> {
        require(
            !self.user.trim().is_empty()
                && !self.project.trim().is_empty()
                && self
                    .engagement
                    .as_ref()
                    .is_none_or(|x| !x.trim().is_empty()),
            "empty namespace",
        )
    }
}
