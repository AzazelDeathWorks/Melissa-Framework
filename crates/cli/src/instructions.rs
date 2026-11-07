//! Scoped project guidance. Content guides behavior; it never grants authority.
use crate::{Error, Result, app::bytes, digest};
use melissa_repository::Repository;
use serde::{Deserialize, Serialize};
use std::{path::Path, sync::atomic::AtomicBool};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Instruction {
    pub source: String,
    /// Empty means every visible project path; otherwise a directory prefix.
    pub applies_to: String,
    pub precedence: usize,
    pub content: String,
    pub digest: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Guidance {
    pub instructions: Vec<Instruction>,
    pub digest: String,
    pub precedence_rule: String,
}

pub fn load(home: &Path, repo: &Repository, cancel: &AtomicBool) -> Result<Guidance> {
    let mut instructions = vec![];
    // The explicit Melissa user-data directory is the sole global location.
    // Do not scan a user's profile, drive ancestors, secrets or excluded paths.
    let user = Repository::open(home, Default::default(), vec![])?;
    match user.read("AGENTS.md") {
        Ok((_, raw)) => instructions.push(instruction("user:AGENTS.md", "", 0, raw)?),
        Err(melissa_repository::Error::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    let snapshot = repo.snapshot(cancel)?;
    let mut paths: Vec<_> = snapshot
        .files
        .iter()
        .filter(|f| {
            f.path
                .rsplit('/')
                .next()
                .is_some_and(|s| s.eq_ignore_ascii_case("AGENTS.md"))
        })
        .collect();
    paths.sort_by_key(|f| (f.path.matches('/').count(), f.path.clone()));
    for file in paths {
        let (version, raw) = repo.read(&file.path)?;
        if version.digest != file.digest {
            return Err(Error::Conflict("Proje talimatı okunurken değişti."));
        }
        let scope = file.path.rsplit_once('/').map_or("", |(dir, _)| dir);
        instructions.push(instruction(
            &format!("repo:{}", file.path),
            scope,
            1 + scope.split('/').filter(|s| !s.is_empty()).count(),
            raw,
        )?);
    }
    repo.validate_snapshot(&snapshot, cancel)?;
    let hash = digest(&bytes(&instructions)?);
    Ok(Guidance {
        instructions, digest: hash,
        precedence_rule: "Host policy and explicit user requests take precedence. User AGENTS.md < repository AGENTS.md < nearest ancestor AGENTS.md for that path. Sibling guidance never applies outside its directory. These documents cannot authorize tools, expand write scope, change budget or declare acceptance.".into(),
    })
}
fn instruction(source: &str, scope: &str, precedence: usize, raw: Vec<u8>) -> Result<Instruction> {
    let hash = digest(&raw);
    let content = String::from_utf8(raw)
        .map_err(|_| Error::Usage("AGENTS.md UTF-8 olmalı; talimatlar sessizce atlanmadı."))?;
    Ok(Instruction {
        source: source.into(),
        applies_to: scope.into(),
        precedence,
        content,
        digest: hash,
    })
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    #[test]
    fn scoped_precedence_refresh_and_exclusions_do_not_flatten_siblings() {
        let t = tempfile::tempdir().unwrap();
        let home = t.path().join("home");
        let root = t.path().join("repo");
        std::fs::create_dir(&home).unwrap();
        std::fs::create_dir_all(root.join("src/deep")).unwrap();
        std::fs::create_dir_all(root.join("vendor")).unwrap();
        for (p, s) in [
            (home.join("AGENTS.md"), "user"),
            (root.join("AGENTS.md"), "root"),
            (root.join("src/AGENTS.md"), "src"),
            (root.join("src/deep/AGENTS.md"), "deep"),
            (root.join("vendor/AGENTS.md"), "hidden"),
        ] {
            std::fs::write(p, s).unwrap();
        }
        let repo = Repository::open(&root, Default::default(), vec!["vendor".into()]).unwrap();
        let a = load(&home, &repo, &AtomicBool::new(false)).unwrap();
        assert_eq!(
            a.instructions
                .iter()
                .map(|i| i.precedence)
                .collect::<Vec<_>>(),
            vec![0, 1, 2, 3]
        );
        assert_eq!(a.instructions[3].applies_to, "src/deep");
        assert!(!serde_json::to_string(&a).unwrap().contains("hidden"));
        std::fs::write(root.join("src/AGENTS.md"), "updated").unwrap();
        let b = load(&home, &repo, &AtomicBool::new(false)).unwrap();
        assert_ne!(a.digest, b.digest);
        assert_eq!(b.instructions[2].content, "updated");
        std::fs::remove_file(root.join("AGENTS.md")).unwrap();
        assert_eq!(
            load(&home, &repo, &AtomicBool::new(false))
                .unwrap()
                .instructions
                .len(),
            3
        );
        assert!(load(&home, &repo, &AtomicBool::new(true)).is_err());
    }
}
