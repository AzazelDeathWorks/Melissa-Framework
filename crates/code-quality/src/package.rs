//! Byte integrity only. A matching package is not proof it was built or tested.
use crate::{Error, Result, digest, digest_valid, source::FrozenSource};
use melissa_repository::Repository;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, sync::atomic::AtomicBool};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageFile {
    pub path: String,
    pub digest: String,
    pub bytes: u64,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageManifest {
    pub schema_version: String,
    pub source_digest: String,
    pub environment_digest: String,
    pub lockfiles: BTreeMap<String, String>,
    pub files: Vec<PackageFile>,
}
/// Cannot be deserialized from model claims.
#[derive(Debug, Serialize)]
pub struct PackageIntegrity {
    pub schema_version: &'static str,
    pub manifest_digest: String,
    pub package_digest: String,
    pub source_digest: String,
    pub verified_files: usize,
    pub build_provenance: &'static str,
    pub protected_acceptance: bool,
    pub task_accepted: bool,
}
impl PackageManifest {
    pub fn capture(
        source: &FrozenSource,
        package: &Repository,
        environment_digest: String,
        lock_paths: &[String],
        cancel: &AtomicBool,
    ) -> Result<Self> {
        if !digest_valid(&environment_digest) {
            return Err(Error::Invalid("environment identity"));
        }
        let captured = FrozenSource::capture(package, cancel)?;
        if !captured.snapshot().excluded_prefixes.is_empty() {
            return Err(Error::Invalid(
                "package capture must include all runtime files; use open_for_package",
            ));
        }
        if captured.snapshot().files.is_empty() {
            return Err(Error::Invalid("empty package"));
        }
        let mut lockfiles = BTreeMap::new();
        for path in lock_paths {
            if lockfiles
                .insert(path.clone(), digest(source.bytes(path)?))
                .is_some()
            {
                return Err(Error::Invalid("duplicate lock path"));
            }
        }
        if lockfiles.is_empty() {
            return Err(Error::Invalid("explicit captured dependency lock required"));
        }
        Ok(Self {
            schema_version: "melissa.package-integrity-manifest/v1".into(),
            source_digest: source.snapshot().digest.clone(),
            environment_digest,
            lockfiles,
            files: captured
                .snapshot()
                .files
                .iter()
                .map(|f| PackageFile {
                    path: f.path.clone(),
                    digest: f.digest.clone(),
                    bytes: f.bytes,
                })
                .collect(),
        })
    }
    pub fn verify(
        &self,
        source: &FrozenSource,
        source_repo: &Repository,
        package: &Repository,
        expected_environment: &str,
        required_locks: &[String],
        cancel: &AtomicBool,
    ) -> Result<PackageIntegrity> {
        source.current(source_repo, cancel)?;
        if self.schema_version != "melissa.package-integrity-manifest/v1"
            || self.source_digest != source.snapshot().digest
            || self.environment_digest != expected_environment
            || !digest_valid(expected_environment)
        {
            return Err(Error::Stale("package source/environment identity"));
        }
        let expected = required_locks
            .iter()
            .cloned()
            .collect::<std::collections::BTreeSet<_>>();
        if expected.is_empty()
            || expected.len() != required_locks.len()
            || self
                .lockfiles
                .keys()
                .cloned()
                .collect::<std::collections::BTreeSet<_>>()
                != expected
        {
            return Err(Error::Invalid("required lock set differs"));
        }
        for (p, d) in &self.lockfiles {
            if digest(source.bytes(p)?) != *d {
                return Err(Error::Stale("lock bytes differ"));
            }
        }
        if self.files.is_empty() || self.files.len() > 100000 {
            return Err(Error::Invalid("package file count"));
        }
        let captured = FrozenSource::capture(package, cancel)?;
        if !captured.snapshot().excluded_prefixes.is_empty() {
            return Err(Error::Invalid("package capture cannot omit files"));
        }
        let mut expected_files = BTreeMap::new();
        for f in &self.files {
            if !digest_valid(&f.digest)
                || expected_files
                    .insert(&f.path, (&f.digest, f.bytes))
                    .is_some()
            {
                return Err(Error::Invalid("duplicate package path or invalid digest"));
            }
            if f.path
                .split('/')
                .any(|p| p.is_empty() || p == "." || p == "..")
                || f.path.contains(['\\', ':', '\0'])
            {
                return Err(Error::Invalid("package path spelling"));
            }
        }
        if expected_files.len() != captured.snapshot().files.len() {
            return Err(Error::Stale("extra or missing package file"));
        }
        for f in &captured.snapshot().files {
            if expected_files.get(&f.path) != Some(&(&f.digest, f.bytes)) {
                return Err(Error::Stale("package bytes differ"));
            }
        }
        source.current(source_repo, cancel)?;
        Ok(PackageIntegrity {
            schema_version: "melissa.package-byte-verification/v1",
            manifest_digest: digest(&serde_json::to_vec(self)?),
            package_digest: captured.snapshot().digest.clone(),
            source_digest: self.source_digest.clone(),
            verified_files: self.files.len(),
            build_provenance: "NOT_ATTESTED_REQUIRES_NATIVE_BUILD_AND_SMOKE",
            protected_acceptance: false,
            task_accepted: false,
        })
    }
}
