//! Host-observed source identities for memory. File bodies are discarded as
//! soon as the exact version has been checked through a pinned repository read.
use crate::{
    Error, Namespace, Result, require,
    retrieval::{Domain, SearchScope, SourceRef},
};
use melissa_repository::Repository;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::atomic::AtomicBool,
};

pub struct ObservedSources {
    references: BTreeMap<String, (SourceRef, u64)>,
    pub bytes_read: u64,
    pub files_read: usize,
}

impl ObservedSources {
    pub fn observe<'a>(
        repo: &Repository,
        namespace: &Namespace,
        ids: impl IntoIterator<Item = &'a str>,
        now_ms: u64,
        expires_at_ms: u64,
        cancel: &AtomicBool,
    ) -> Result<Self> {
        let mut observations = Self {
            references: BTreeMap::new(),
            bytes_read: 0,
            files_read: 0,
        };
        let paths: BTreeSet<_> = ids.into_iter().collect();
        require(
            paths.len() <= repo.limits().max_files,
            "memory source count exceeds repository envelope",
        )?;
        for id in paths {
            if !repo.source_visibility().allows(id) {
                continue;
            }
            let Some((version, content)) = repo
                .read_source_cancellable(id, cancel)
                .map_err(|error| Error(error.to_string()))?
            else {
                continue;
            };
            observations.files_read += 1;
            observations.bytes_read += version.bytes;
            require(
                observations.bytes_read <= repo.limits().max_total_bytes,
                "memory source bytes exceed repository envelope",
            )?;
            if std::str::from_utf8(&content).is_err() {
                continue;
            }
            let reference = SourceRef {
                id: version.path.clone(),
                version: format!("{}:{}", version.identity, version.digest),
                namespace: namespace.clone(),
                domain: Domain::Code,
                digest: version.digest,
                observed_at_ms: now_ms,
                valid_until_ms: expires_at_ms.min(now_ms.saturating_add(60_000)),
                symbols: vec![],
                links: vec![],
            };
            observations
                .references
                .insert(version.path, (reference, version.bytes));
        }
        Ok(observations)
    }

    pub fn references(&self) -> impl Iterator<Item = &SourceRef> {
        self.references.values().map(|(reference, _)| reference)
    }

    pub fn select<'a>(
        &'a self,
        ids: impl IntoIterator<Item = &'a str>,
    ) -> Result<Vec<(&'a SourceRef, u64)>> {
        ids.into_iter()
            .map(|id| {
                self.references
                    .get(id)
                    .map(|(reference, bytes)| (reference, *bytes))
                    .ok_or_else(|| Error("unknown or out-of-scope memory source".into()))
            })
            .collect()
    }

    pub fn scope(&self, namespace: &Namespace, now_ms: u64) -> SearchScope {
        SearchScope {
            namespace: namespace.clone(),
            domain: Domain::Code,
            now_ms,
            current: self
                .references()
                .map(|r| (r.id.clone(), (r.version.clone(), r.digest.clone())))
                .collect(),
        }
    }
}
