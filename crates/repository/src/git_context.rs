//! Read-only provenance. Never execute Git or follow a linked .git file to an
//! arbitrary host path. Unsupported metadata stays unknown, not a guessed branch.
use crate::{Error, Repository, Result, paths};
use serde::{Deserialize, Serialize};
use std::{fs, io::Read};
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GitContext {
    pub status: String,
    pub branch: Option<String>,
    pub head: Option<String>,
}
impl GitContext {
    pub fn scope_key(&self) -> Option<String> {
        match self.status.as_str() {
            "not_git" => Some("not_git".into()),
            "observed" => self
                .branch
                .as_ref()
                .map(|b| format!("branch:{b}"))
                .or_else(|| self.head.as_ref().map(|h| format!("detached:{h}"))),
            _ => None,
        }
    }
}
fn hash(s: &str) -> bool {
    [40, 64].contains(&s.len()) && s.bytes().all(|b| b.is_ascii_hexdigit())
}
fn read(path: &std::path::Path, limit: u64) -> Result<Vec<u8>> {
    let pin = paths::pin(path, false, false)?;
    let mut bytes = vec![];
    pin.file.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(Error::Quota("Git provenance bytes"));
    }
    Ok(bytes)
}
pub fn observe(repo: &Repository) -> GitContext {
    let root = repo.root_path().join(".git");
    if fs::symlink_metadata(&root).is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound) {
        return GitContext {
            status: "not_git".into(),
            branch: None,
            head: None,
        };
    }
    let observed = (|| -> Result<GitContext> {
        let _guard = paths::pin(&root, true, false)?;
        let before = read(&root.join("HEAD"), 4096)?;
        let text = std::str::from_utf8(&before)
            .map_err(|_| Error::Invalid("Git HEAD encoding"))?
            .trim();
        let (mut branch, mut head) = (None, None);
        if let Some(reference) = text.strip_prefix("ref: ") {
            paths::relative(reference)?;
            if !reference.starts_with("refs/heads/") || reference.contains('@') {
                return Err(Error::Unsupported("symbolic Git provenance"));
            }
            branch = Some(reference.to_owned());
            match read(&root.join(reference.replace('/', "\\")), 4096) {
                Ok(raw) => {
                    let h = std::str::from_utf8(&raw)
                        .map_err(|_| Error::Invalid("Git ref encoding"))?
                        .trim();
                    if !hash(h) {
                        return Err(Error::Invalid("Git ref hash"));
                    }
                    head = Some(h.to_owned());
                }
                Err(Error::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {
                    if root.join("packed-refs").exists() {
                        let packed = read(&root.join("packed-refs"), 1024 * 1024)?;
                        let packed = std::str::from_utf8(&packed)
                            .map_err(|_| Error::Invalid("packed refs encoding"))?;
                        for line in packed.lines() {
                            if let Some((h, r)) = line.split_once(' ')
                                && r == reference
                            {
                                if !hash(h) {
                                    return Err(Error::Invalid("packed ref hash"));
                                }
                                head = Some(h.into());
                            }
                        }
                    }
                }
                Err(error) => return Err(error),
            }
        } else if hash(text) {
            head = Some(text.into());
        } else {
            return Err(Error::Invalid("Git HEAD shape"));
        }
        if before != read(&root.join("HEAD"), 4096)? {
            return Err(Error::Stale("Git HEAD changed"));
        }
        Ok(GitContext {
            status: "observed".into(),
            branch,
            head,
        })
    })();
    observed.unwrap_or(GitContext {
        status: "unavailable".into(),
        branch: None,
        head: None,
    })
}
