use crate::{Error, Result, digest};
use melissa_repository::{Repository, RepositorySnapshot};
use serde::Serialize;
use std::{
    collections::BTreeMap,
    sync::atomic::{AtomicBool, Ordering},
};

/// Only captured through handle-pinned Repository reads; never deserialized.
#[derive(Serialize)]
pub struct FrozenSource {
    snapshot: RepositorySnapshot,
    #[serde(skip)]
    files: BTreeMap<String, Vec<u8>>,
}
impl std::fmt::Debug for FrozenSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FrozenSource")
            .field("digest", &self.snapshot.digest)
            .field("file_count", &self.files.len())
            .finish_non_exhaustive()
    }
}
impl FrozenSource {
    pub fn snapshot(&self) -> &RepositorySnapshot {
        &self.snapshot
    }
    pub fn capture(repo: &Repository, cancel: &AtomicBool) -> Result<Self> {
        let snapshot = repo.snapshot(cancel)?;
        let mut files = BTreeMap::new();
        for version in &snapshot.files {
            if cancel.load(Ordering::Acquire) {
                return Err(Error::Cancelled);
            }
            let (observed, bytes) = repo.read(&version.path)?;
            if observed != *version {
                return Err(Error::Stale("file changed during capture"));
            }
            files.insert(version.path.clone(), bytes);
        }
        if repo.snapshot(cancel)? != snapshot {
            return Err(Error::Stale("tree changed during capture"));
        }
        Ok(Self { snapshot, files })
    }
    pub fn current(&self, repo: &Repository, cancel: &AtomicBool) -> Result {
        if cancel.load(Ordering::Acquire) {
            return Err(Error::Cancelled);
        }
        if repo.snapshot(cancel)? != self.snapshot {
            return Err(Error::Stale("reviewed tree changed"));
        }
        Ok(())
    }
    pub fn bytes(&self, path: &str) -> Result<&[u8]> {
        self.files
            .get(path)
            .map(Vec::as_slice)
            .ok_or(Error::Denied("file outside captured source"))
    }
    pub fn changes_from(&self, baseline: &Self) -> Result<Vec<Change>> {
        if self.snapshot.root_identity != baseline.snapshot.root_identity {
            return Err(Error::Denied("baseline belongs to another workspace"));
        }
        let mut paths = self
            .files
            .keys()
            .chain(baseline.files.keys())
            .collect::<Vec<_>>();
        paths.sort();
        paths.dedup();
        Ok(paths
            .into_iter()
            .filter_map(|p| {
                let before = baseline.files.get(p);
                let after = self.files.get(p);
                (before != after).then(|| Change {
                    path: p.clone(),
                    before_digest: before.map(|v| digest(v)),
                    after_digest: after.map(|v| digest(v)),
                    kind: match (before, after) {
                        (None, _) => ChangeKind::Added,
                        (_, None) => ChangeKind::Deleted,
                        _ => ChangeKind::Modified,
                    },
                })
            })
            .collect())
    }
}
#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    Added,
    Modified,
    Deleted,
}
#[derive(Debug, Serialize)]
pub struct Change {
    pub path: String,
    pub before_digest: Option<String>,
    pub after_digest: Option<String>,
    pub kind: ChangeKind,
}
