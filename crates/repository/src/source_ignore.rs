//! Repository-local ignore rules, admitted before source bodies are read.
//! No ambient Git configuration or global ignore files are consulted.
use crate::{Error, Repository, Result, paths};
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use std::{io::Read, path::Path, sync::Arc};

#[derive(Clone, Default)]
pub(crate) struct Rules(Vec<(String, bool, Arc<Gitignore>, String)>);
impl Rules {
    pub fn enter(&self, repo: &Repository, relative: &str) -> Result<Self> {
        let mut next = self.clone();
        if !repo.source_visibility().protects_sensitive_paths() {
            return Ok(next);
        }
        for name in [".gitignore", ".ignore"] {
            let path = if relative.is_empty() {
                name.into()
            } else {
                format!("{relative}/{name}")
            };
            if !repo.source_visibility().allows(&path) {
                continue;
            }
            let absolute = repo.root_path().join(&path);
            match std::fs::symlink_metadata(&absolute) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e.into()),
                Ok(_) => (),
            }
            let mut pin = paths::pin(&absolute, false, false)?;
            // Rules are bounded independently of large source-file capacity.
            let mut bytes = Vec::new();
            (&mut pin.file)
                .take(1024 * 1024 + 1)
                .read_to_end(&mut bytes)?;
            if bytes.len() > 1024 * 1024 {
                return Err(Error::Quota("ignore file bytes"));
            }
            let text = std::str::from_utf8(&bytes).map_err(|_| Error::Invalid("ignore UTF-8"))?;
            let mut builder = GitignoreBuilder::new(repo.root_path().join(relative));
            for line in text.lines() {
                builder
                    .add_line(Some(absolute.clone()), line)
                    .map_err(|_| Error::Invalid("ignore rule"))?;
            }
            next.0.push((
                relative.into(),
                name == ".ignore",
                Arc::new(
                    builder
                        .build()
                        .map_err(|_| Error::Invalid("ignore matcher"))?,
                ),
                crate::digest(&bytes),
            ));
        }
        next.0
            .sort_by_key(|(p, local, _, _)| (*local, p.split('/').count()));
        Ok(next)
    }
    pub fn excludes(&self, repo: &Repository, path: &str, directory: bool) -> bool {
        if !repo.source_visibility().protects_sensitive_paths() {
            return false;
        }
        if path.split('/').any(|c| {
            ["node_modules", "target", ".venv", "__pycache__"]
                .iter()
                .any(|n| c.eq_ignore_ascii_case(n))
        }) {
            return true;
        }
        let mut ignored = false;
        for (parent, _, matcher, _) in &self.0 {
            if parent.is_empty() || paths::beneath(path, parent) {
                let matched = matcher.matched(repo.root_path().join(path), directory);
                if matched.is_ignore() {
                    ignored = true;
                } else if matched.is_whitelist() {
                    ignored = false;
                }
            }
        }
        ignored
    }
}
/// Bind historical evidence/cache admission to every active ignore input,
/// including an ignore file which excludes its own name. No source body reads.
pub(crate) fn policy_digest(repo: &Repository) -> Result<Option<String>> {
    fn walk(
        repo: &Repository,
        parent: &paths::Pinned,
        relative: &str,
        inherited: &Rules,
        inputs: &mut std::collections::BTreeSet<(String, bool, String)>,
        count: &mut usize,
    ) -> Result {
        let rules = inherited.enter(repo, relative)?;
        inputs.extend(
            rules
                .0
                .iter()
                .map(|(p, b, _, d)| (p.clone(), *b, d.clone())),
        );
        for entry in std::fs::read_dir(&parent.canonical)? {
            let entry = entry?;
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| Error::Invalid("ignore directory name"))?;
            let path = if relative.is_empty() {
                name.clone()
            } else {
                format!("{relative}/{name}")
            };
            let kind = entry.file_type()?;
            if !repo.source_visibility().permits_traversal(&path)
                || rules.excludes(repo, &path, kind.is_dir())
            {
                continue;
            }
            *count += 1;
            if *count > repo.limits().max_files {
                return Err(Error::Quota("ignore directory admission"));
            }
            if kind.is_dir() {
                paths::with_read_child(parent, &name, true, |pin| {
                    walk(repo, pin, &path, &rules, inputs, count)
                })?;
            }
        }
        Ok(())
    }
    let mut inputs = std::collections::BTreeSet::new();
    if repo.source_visibility().protects_sensitive_paths() {
        walk(repo, &repo.root, "", &Rules::default(), &mut inputs, &mut 0)?;
    }
    if inputs.is_empty() {
        Ok(None)
    } else {
        Ok(Some(crate::digest(&serde_json::to_vec(&inputs)?)))
    }
}
pub(crate) fn check(repo: &Repository, path: &str) -> Result {
    if !repo.source_visibility().protects_sensitive_paths() {
        return Ok(());
    }
    let mut rules = Rules::default().enter(repo, "")?;
    let mut current = String::new();
    let parts: Vec<_> = path.split('/').collect();
    for (i, part) in parts.iter().enumerate() {
        if !current.is_empty() {
            current.push('/');
        }
        current.push_str(part);
        let dir = i + 1 < parts.len() || Path::new(&repo.root_path().join(path)).is_dir();
        if rules.excludes(repo, &current, dir) {
            return Err(Error::Denied("path excluded by repository ignore rules"));
        }
        if i + 1 < parts.len() {
            rules = rules.enter(repo, &current)?;
        }
    }
    Ok(())
}
