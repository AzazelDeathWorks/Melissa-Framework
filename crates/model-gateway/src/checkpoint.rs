//! Durable state uses its own symmetric envelope, not the smaller provider
//! wire JSON limit. Large transcripts remain separate immutable objects.
use crate::{ErrorKind, GatewayError, Result};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

pub const MAX_CHECKPOINT_BYTES: usize = 16 * 1024 * 1024;

pub fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    struct Bounded(Vec<u8>);
    impl std::io::Write for Bounded {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > MAX_CHECKPOINT_BYTES.saturating_sub(self.0.len()) {
                return Err(std::io::Error::other("checkpoint envelope exceeded"));
            }
            self.0.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut output = Bounded(Vec::new());
    serde_json::to_writer(&mut output, value).map_err(|_| failure())?;
    // Apply the reader's nesting/duplicate-key rules before publication too.
    decode::<serde_json::Value>(&output.0)?;
    Ok(output.0)
}
pub fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T> {
    #[derive(Deserialize)]
    #[serde(transparent)]
    struct State<T>(T);
    impl<T> melissa_core_contracts::validation::Validate for State<T> {
        fn validate(&self) -> melissa_core_contracts::validation::ContractResult {
            Ok(())
        }
    }
    melissa_core_contracts::json::parse_contract::<State<T>>(bytes, MAX_CHECKPOINT_BYTES)
        .map(|v| v.0)
        .map_err(|_| failure())
}
fn failure() -> GatewayError {
    GatewayError::new(
        ErrorKind::Store,
        "checkpoint exceeds its envelope or has invalid JSON",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_checkpoint_byte_boundary_round_trips_and_next_byte_is_rejected() {
        let value = "x".repeat(MAX_CHECKPOINT_BYTES - 2);
        let bytes = encode(&value).unwrap();
        assert_eq!(bytes.len(), MAX_CHECKPOINT_BYTES);
        assert_eq!(decode::<String>(&bytes).unwrap(), value);
        assert!(encode(&(value + "x")).is_err());
    }
    #[test]
    fn writer_rejects_reader_incompatible_nesting_and_reader_rejects_duplicates() {
        let mut nested = serde_json::Value::Null;
        for _ in 0..140 {
            nested = serde_json::json!([nested]);
        }
        assert!(encode(&nested).is_err());
        assert!(decode::<serde_json::Value>(br#"{"a":1,"a":2}"#).is_err());
    }
}
