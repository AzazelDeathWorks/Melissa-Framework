//! The archive carries source bytes once. The worker receives only their seals.
use crate::Snapshot;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

pub const VERSION: &str = "melissa.worker-source-manifest/v1";
pub const VERIFIER: &str = include_str!("source_manifest.py");

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceSeal {
    pub bytes: u64,
    pub sha256: String,
}

pub fn seal(snapshot: &Snapshot) -> BTreeMap<&str, SourceSeal> {
    snapshot
        .files
        .iter()
        .map(|(path, body)| {
            (
                path.as_str(),
                SourceSeal {
                    bytes: body.len() as u64,
                    sha256: format!("{:x}", Sha256::digest(body)),
                },
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_size_depends_on_file_count_and_does_not_embed_bodies() {
        let bytes = vec![0xa5; 8 * 1024 * 1024];
        let snapshot = Snapshot {
            files: BTreeMap::from([("binary.dat".into(), bytes)]),
        };
        let manifest = seal(&snapshot);
        assert_eq!(manifest["binary.dat"].bytes, 8 * 1024 * 1024);
        assert_eq!(manifest["binary.dat"].sha256.len(), 64);
        let encoded = serde_json::to_vec(&manifest).unwrap();
        assert!(encoded.len() < 160);
        assert!(
            !encoded
                .windows(32)
                .any(|span| span == b"a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5")
        );
        eprintln!(
            "WORKER_MANIFEST binary_bytes={} prior_hex_bytes={} manifest_bytes={}",
            manifest["binary.dat"].bytes,
            2 * manifest["binary.dat"].bytes,
            encoded.len()
        );
    }
}
