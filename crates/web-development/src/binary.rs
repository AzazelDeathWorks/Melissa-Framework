//! Large PNG/ZIP bytes use bounded acknowledged frames, separately from JSON.
use super::*;
use base64::{Engine, engine::general_purpose::STANDARD};
use sha2::{Digest, Sha256};
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Descriptor {
    id: String,
    kind: String,
    bytes: u64,
    sha256: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Part {
    schema_version: String,
    seq: u64,
    artifact_id: String,
    offset: u64,
    total_bytes: u64,
    data_hex: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Part64 {
    schema_version: String,
    seq: u64,
    artifact_id: String,
    offset: u64,
    total_bytes: u64,
    data_base64: String,
}
fn decode_base64_part(
    frame: serde_json::Value,
    seq: u64,
    artifact_id: &str,
    offset: u64,
    total: u64,
) -> Result<Vec<u8>> {
    let part: Part64 = serde_json::from_value(frame)?;
    let remaining = total
        .checked_sub(offset)
        .ok_or(Error::Invalid("binary frame offset exceeds total"))?
        .min(24576) as usize;
    if part.schema_version != "melissa.web-binary/v2"
        || part.seq != seq
        || part.artifact_id != artifact_id
        || part.offset != offset
        || part.total_bytes != total
        || remaining == 0
        || part.data_base64.len() != remaining.div_ceil(3) * 4
    {
        return Err(Error::Invalid("binary frame source/sequence/size mismatch"));
    }
    let payload = STANDARD
        .decode(&part.data_base64)
        .map_err(|_| Error::Invalid("binary frame base64"))?;
    if payload.len() != remaining || STANDARD.encode(&payload) != part.data_base64 {
        return Err(Error::Invalid("binary frame base64 is not canonical"));
    }
    Ok(payload)
}
impl WebSession {
    pub(super) fn receive_binary(&mut self, reply: &Reply, deadline: Instant) -> Result<()> {
        let Some(value) = reply.body.as_ref().and_then(|b| b.get("binary_artifact")) else {
            return Ok(());
        };
        let meta: Descriptor = serde_json::from_value(value.clone())?;
        let limit = self.plan.binary_artifact_limit();
        if meta.bytes == 0
            || meta.bytes > limit
            || meta.id.len() > 10
            || !meta
                .id
                .strip_prefix('a')
                .is_some_and(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
            || !["png", "trace"].contains(&meta.kind.as_str())
            || (meta.kind == "trace" && !reply.closed)
            || (meta.kind == "png"
                && reply
                    .body
                    .as_ref()
                    .is_none_or(|b| b["content_type"] != "image/png"))
        {
            return Err(Error::Invalid(
                "binary artifact descriptor outside host envelope",
            ));
        }
        let mut bytes = Vec::with_capacity(meta.bytes as usize);
        while (bytes.len() as u64) < meta.bytes {
            if self.cancel.load(Ordering::Acquire)
                || self.revoked.load(Ordering::Acquire)
                || Instant::now() >= deadline
            {
                return Err(Error::Invalid("binary artifact interrupted"));
            }
            let raw = match self.answers.recv_timeout(Duration::from_millis(40)) {
                Ok(b) => b,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(_) => return Err(Error::Invalid("binary artifact pipe closed")),
            };
            let frame = melissa_model_gateway::strict_json(
                std::str::from_utf8(&raw).map_err(|_| Error::Invalid("binary frame encoding"))?,
            )
            .map_err(|_| Error::Invalid("binary frame JSON"))?;
            let offset = bytes.len() as u64;
            if self.plan.binary_base64 {
                bytes.extend(decode_base64_part(
                    frame, reply.seq, &meta.id, offset, meta.bytes,
                )?);
            } else {
                let part: Part = serde_json::from_value(frame)?;
                let remaining = (meta.bytes - bytes.len() as u64).min(24576) as usize;
                if part.schema_version != "melissa.web-binary/v1"
                    || part.seq != reply.seq
                    || part.artifact_id != meta.id
                    || part.offset != bytes.len() as u64
                    || part.total_bytes != meta.bytes
                    || part.data_hex.len() != remaining * 2
                {
                    return Err(Error::Invalid("binary frame source/sequence/size mismatch"));
                }
                for pair in part.data_hex.as_bytes().as_chunks::<2>().0 {
                    let digit = |b: u8| -> Result<u8> {
                        match b {
                            b'0'..=b'9' => Ok(b - b'0'),
                            b'a'..=b'f' => Ok(b - b'a' + 10),
                            _ => Err(Error::Invalid("binary frame hex")),
                        }
                    };
                    bytes.push(digit(pair[0])? * 16 + digit(pair[1])?);
                }
            }
            let mut ack = serde_json::to_vec(&json!({"artifact_ack":reply.seq,"offset":offset}))?;
            ack.push(b'\n');
            self.input
                .as_ref()
                .ok_or(Error::Invalid("binary input closed"))?
                .send(ack)
                .map_err(|_| Error::Invalid("binary ACK send failed"))?;
        }
        if format!("sha256:{:x}", Sha256::digest(&bytes)) != meta.sha256 {
            return Err(Error::Invalid("binary artifact SHA256 mismatch"));
        }
        let id = format!(
            "{}:web-binary:{}:{}",
            self.context.attempt_id, reply.seq, meta.kind
        );
        self.store.put_artifact(
            &melissa_local_store::ArtifactRequest {
                artifact_id: id.clone(),
                task_id: self.context.task_id.clone(),
                content_type: if meta.kind == "png" {
                    "image/png"
                } else {
                    "application/zip"
                }
                .into(),
                logical_type: format!("web.{}", meta.kind),
                schema_version: if self.plan.binary_base64 {
                    "melissa.web-binary/v2"
                } else {
                    "melissa.web-binary/v1"
                }
                .into(),
                retain_until_ms: 0,
                legal_hold: true,
            },
            &bytes,
        )?;
        self.artifacts.push(id.clone());
        let receipt = format!("{id}:receipt");
        put(
            &mut self.store,
            &self.context.task_id,
            &receipt,
            "web.binary-receipt",
            &serde_json::to_vec(
                &json!({"schema_version":"melissa.web-binary-receipt/v1","artifact_id":id,"web_reply_artifact_id":format!("{}:web-reply:{}",self.context.attempt_id,reply.seq),"seq":reply.seq,"descriptor":value,"source_digest":reply.source_digest,"blake3":digest(&bytes)}),
            )?,
        )?;
        self.artifacts.push(receipt);
        Ok(())
    }
}

#[cfg(test)]
mod base64_tests {
    use super::*;
    fn frame() -> serde_json::Value {
        json!({"schema_version":"melissa.web-binary/v2","seq":3,"artifact_id":"a2","offset":0,"total_bytes":2,"data_base64":"AP8="})
    }
    #[test]
    fn canonical_base64_is_exact_and_wrong_source_sequence_size_are_rejected() {
        assert_eq!(
            decode_base64_part(frame(), 3, "a2", 0, 2).unwrap(),
            [0, 255]
        );
        for (seq, id, offset, total) in [
            (4, "a2", 0, 2),
            (3, "a3", 0, 2),
            (3, "a2", 1, 2),
            (3, "a2", 0, 3),
            (3, "a2", 3, 2),
        ] {
            assert!(decode_base64_part(frame(), seq, id, offset, total).is_err());
        }
    }
    #[test]
    fn malformed_noncanonical_and_mixed_legacy_fields_are_rejected() {
        for encoded in ["AP8", "AP8=\n", "AP8==", "AP9=", "AP_=", "@@@@"] {
            let mut value = frame();
            value["data_base64"] = json!(encoded);
            assert!(
                decode_base64_part(value, 3, "a2", 0, 2).is_err(),
                "{encoded:?}"
            );
        }
        let mut mixed = frame();
        mixed["data_hex"] = json!("00ff");
        assert!(decode_base64_part(mixed, 3, "a2", 0, 2).is_err());
        let mut legacy = frame();
        legacy["schema_version"] = json!("melissa.web-binary/v1");
        assert!(decode_base64_part(legacy, 3, "a2", 0, 2).is_err());
    }
}
