//! Lossless append-only message objects plus small authenticated manifests.
//! Historical v1 full checkpoints remain readable by the caller.
use crate::{ErrorKind, GatewayError, Message, RequestLimits, Result};
use melissa_local_store::{ArtifactRequest, LocalStore, StoreError};
use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub schema_version: String,
    pub message_refs: Vec<String>,
    pub total_bytes: u64,
}
#[derive(Default)]
pub struct MessageLog {
    prior: Vec<String>,
}
fn error(_: impl std::fmt::Display) -> GatewayError {
    GatewayError::new(
        ErrorKind::Store,
        "transcript identity, integrity, or publication failure",
    )
}
fn prefix(run: &str) -> String {
    format!("transcript:{run}:")
}
impl MessageLog {
    pub fn adopt(&mut self, manifest: &Manifest) {
        self.prior = manifest.message_refs.clone();
    }
    pub fn save(
        &mut self,
        store: &mut LocalStore,
        task: &str,
        run: &str,
        messages: &[Message],
    ) -> Result<Manifest> {
        if messages.len() < self.prior.len() || messages.len() > RequestLimits::MAX_MESSAGES {
            return Err(error("transcript length"));
        }
        let mut refs = Vec::with_capacity(messages.len());
        let mut total_bytes = 0u64;
        for (n, message) in messages.iter().enumerate() {
            let bytes = serde_json::to_vec(message).map_err(error)?;
            total_bytes = total_bytes
                .checked_add(bytes.len() as u64)
                .ok_or_else(|| error("transcript overflow"))?;
            if total_bytes > 512 * 1024 * 1024 {
                return Err(error("host transcript storage envelope"));
            }
            let id = format!("{}{}", prefix(run), blake3::hash(&bytes).to_hex());
            if let Some(old) = self.prior.get(n) {
                if old != &id {
                    return Err(error("historical message changed"));
                }
            } else {
                match store.artifact_metadata(task, &id) {
                    Ok(meta) => {
                        if meta.logical_type != "transcript.message"
                            || store.get_artifact(task, &id).map_err(error)? != bytes
                        {
                            return Err(error("transcript collision"));
                        }
                    }
                    Err(StoreError::NotFound(_)) => {
                        store
                            .put_artifact(
                                &ArtifactRequest {
                                    artifact_id: id.clone(),
                                    task_id: task.into(),
                                    content_type: "application/json".into(),
                                    logical_type: "transcript.message".into(),
                                    schema_version: "melissa.transcript-message/v1".into(),
                                    retain_until_ms: 0,
                                    legal_hold: false,
                                },
                                &bytes,
                            )
                            .map_err(error)?;
                    }
                    Err(e) => return Err(error(e)),
                }
                // Checkpoint manifests retain their dependency objects even if
                // normal task references are later released for GC.
                store
                    .pin_artifact(
                        task,
                        &id,
                        &format!("transcript:{}", blake3::hash(run.as_bytes())),
                    )
                    .map_err(error)?;
            }
            refs.push(id);
        }
        self.prior = refs.clone();
        Ok(Manifest {
            schema_version: "melissa.transcript-manifest/v1".into(),
            message_refs: refs,
            total_bytes,
        })
    }
    pub fn restore(
        store: &LocalStore,
        task: &str,
        run: &str,
        manifest: &Manifest,
    ) -> Result<Vec<Message>> {
        if manifest.schema_version != "melissa.transcript-manifest/v1"
            || manifest.message_refs.len() > RequestLimits::MAX_MESSAGES
            || manifest.total_bytes > 512 * 1024 * 1024
        {
            return Err(error("invalid transcript manifest"));
        }
        let mut total = 0u64;
        let mut messages = Vec::with_capacity(manifest.message_refs.len());
        for id in &manifest.message_refs {
            let suffix = id
                .strip_prefix(&prefix(run))
                .ok_or_else(|| error("transcript scope"))?;
            if suffix.len() != 64
                || !suffix
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
            {
                return Err(error("transcript reference"));
            }
            let meta = store.artifact_metadata(task, id).map_err(error)?;
            total = total
                .checked_add(meta.plain_bytes)
                .ok_or_else(|| error("transcript size"))?;
            if total > manifest.total_bytes || meta.logical_type != "transcript.message" {
                return Err(error("transcript size/type"));
            }
            let bytes = store.get_artifact(task, id).map_err(error)?;
            if blake3::hash(&bytes).to_hex().as_str() != suffix {
                return Err(error("transcript digest"));
            }
            messages.push(serde_json::from_slice(&bytes).map_err(error)?);
        }
        if total != manifest.total_bytes {
            return Err(error("truncated transcript"));
        }
        Ok(messages)
    }
}
