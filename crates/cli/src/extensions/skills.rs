use crate::{
    Error, Result,
    app::{App, bytes},
};
use melissa_repository::Repository;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::Path, sync::atomic::AtomicBool};
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Package {
    pub name: String,
    pub description: String,
    pub origin: String,
    pub digest: String,
    pub files: BTreeMap<String, Vec<u8>>,
}
fn metadata(raw: &[u8]) -> Result<(String, String)> {
    let text = std::str::from_utf8(raw)
        .map_err(|_| Error::Usage("SKILL.md UTF-8 olmalı."))?
        .trim_start_matches('\u{feff}')
        .replace("\r\n", "\n");
    let start = text
        .strip_prefix("---\n")
        .ok_or(Error::Usage("Skill YAML başlığı gerekli."))?;
    let end = start
        .lines()
        .position(|line| line == "---")
        .ok_or(Error::Usage("Skill YAML kapanış satırı gerekli."))?;
    let front = start.lines().take(end).collect::<Vec<_>>().join("\n");
    // A deliberately finite scalar YAML subset; unsupported metadata fails,
    // rather than turning YAML tags/anchors/objects into execution semantics.
    let mut fields = BTreeMap::<String, String>::new();
    let mut current = String::new();
    for line in front.lines() {
        if line.starts_with([' ', '\t']) {
            if current == "description" {
                fields
                    .entry(current.clone())
                    .or_default()
                    .push_str(&format!(" {}", line.trim()));
            }
            continue;
        }
        if line.trim().is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, value) = line
            .split_once(':')
            .ok_or(Error::Usage("Skill YAML skaleri geçersiz."))?;
        current = key.to_owned();
        let value = value.trim();
        let parsed = if value.starts_with('"') {
            serde_json::from_str::<String>(value)
                .map_err(|_| Error::Usage("Skill alıntılı metni geçersiz."))?
        } else if value.starts_with('\'') {
            value
                .strip_prefix('\'')
                .and_then(|v| v.strip_suffix('\''))
                .ok_or(Error::Usage("Skill alıntılı metni geçersiz."))?
                .replace("''", "'")
        } else if matches!(value, ">" | ">-" | "|" | "|-") {
            String::new()
        } else {
            value.split(" #").next().unwrap_or(value).into()
        };
        if fields.insert(current.clone(), parsed).is_some() {
            return Err(Error::Usage("Skill YAML alanı tekrarlı."));
        }
    }
    let name = fields
        .remove("name")
        .ok_or(Error::Usage("Skill name gerekli."))?;
    let description = fields
        .remove("description")
        .ok_or(Error::Usage("Skill description gerekli."))?
        .trim()
        .to_string();
    if name.is_empty()
        || name.len() > 64
        || name.starts_with('-')
        || name.ends_with('-')
        || name.contains("--")
        || !name
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
        || description.is_empty()
        || description.chars().count() > 1024
    {
        return Err(Error::Usage(
            "Skill adı/açıklaması standart sınırlarını karşılamıyor.",
        ));
    }
    Ok((name, description))
}
impl Package {
    pub fn validate(&self) -> Result {
        let (name, desc) = metadata(
            self.files
                .get("SKILL.md")
                .ok_or(Error::Usage("SKILL.md eksik."))?,
        )?;
        if name != self.name
            || desc != self.description
            || self.digest != crate::digest(&bytes(&self.files)?)
        {
            return Err(Error::Denied("Skill içerik pini doğrulanmadı."));
        }
        let visibility = melissa_repository::SourceVisibilityPolicy::source(vec![], vec![])?;
        for path in self.files.keys() {
            visibility.check(path)?;
        }
        if self.files.len() > 512 || bytes(&self.files)?.len() > 4 * 1024 * 1024 {
            return Err(Error::Unavailable(
                "Skill 512 dosya/4 MiB aktarım kapasitesini aşıyor.",
            ));
        }
        Ok(())
    }
    pub fn read_from(repo: &Repository, prefix: &str, origin: String) -> Result<Self> {
        let snapshot = repo.snapshot(&AtomicBool::new(false))?;
        let mut files = BTreeMap::new();
        for file in &snapshot.files {
            if let Some(relative) = file.path.strip_prefix(prefix) {
                let (version, raw) = repo.read(&file.path)?;
                if version.digest != file.digest {
                    return Err(Error::Conflict("Skill okunurken değişti."));
                }
                files.insert(relative.to_owned(), raw);
            }
        }
        let (name, description) = metadata(
            files
                .get("SKILL.md")
                .ok_or(Error::Usage("SKILL.md eksik veya kapsam dışında."))?,
        )?;
        let p = Self {
            name,
            description,
            origin,
            digest: crate::digest(&bytes(&files)?),
            files,
        };
        p.validate()?;
        repo.validate_snapshot(&snapshot, &AtomicBool::new(false))?;
        Ok(p)
    }
}
pub fn catalog(app: &App, repo: &Repository) -> Result<BTreeMap<String, Package>> {
    let mut result = BTreeMap::new();
    let imports = app.home.join("skill-imports/current");
    crate::app::safe_path(&imports)?;
    if imports.exists() {
        for file in std::fs::read_dir(&imports)? {
            let path = file?.path();
            crate::app::safe_path(&path)?;
            if path.extension().and_then(|v| v.to_str()) != Some("enc") {
                continue;
            }
            let file_id = path.file_stem().and_then(|v| v.to_str()).ok_or(Error::Io)?;
            let plain = zeroize::Zeroizing::new(app.key.open(
                &std::fs::read(&path)?,
                format!("melissa.skill-current/v1:{file_id}").as_bytes(),
            )?);
            let p: Package = crate::app::decode(&plain)?;
            p.validate()?;
            if p.name != file_id {
                return Err(Error::Store);
            }
            if let Some(old) = result.insert(p.name.clone(), p.clone())
                && old.digest != p.digest
            {
                return Err(Error::Conflict(
                    "Aynı adlı farklı import pini var; ayrı skill adı kullan.",
                ));
            }
        }
    }
    let user_path = app.home.join("skills");
    crate::app::safe_path(&user_path)?;
    let user = if user_path.is_dir() {
        Some(Repository::open(&user_path, Default::default(), vec![])?)
    } else {
        None
    };
    let mut roots = vec![(repo, ".melissa/skills/", "project")];
    if let Some(user) = &user {
        roots.insert(0, (user, "", "user"));
    }
    for (r, base, origin) in roots {
        let snapshot = r.snapshot(&AtomicBool::new(false))?;
        for f in &snapshot.files {
            if f.path.starts_with(base) && f.path.ends_with("/SKILL.md") {
                let rest = &f.path[base.len()..];
                if rest.matches('/').count() != 1 {
                    continue;
                }
                let prefix = f.path.strip_suffix("SKILL.md").ok_or(Error::Store)?;
                let p = Package::read_from(r, prefix, format!("{origin}:{}", f.path))?;
                if rest.split('/').next() != Some(p.name.as_str()) {
                    return Err(Error::Usage("Skill adı klasör adıyla aynı olmalı."));
                }
                result.insert(p.name.clone(), p);
            }
        }
        r.validate_snapshot(&snapshot, &AtomicBool::new(false))?;
    }
    Ok(result)
}
pub fn select(packages: &BTreeMap<String, Package>, goal: &str) -> Vec<String> {
    let lower = goal.to_lowercase();
    packages
        .iter()
        .filter(|(id, p)| {
            lower.contains(id.as_str())
                || p.description
                    .to_lowercase()
                    .split(|c: char| !c.is_alphanumeric())
                    .any(|w| {
                        w.len() > 4 && lower.split(|c: char| !c.is_alphanumeric()).any(|g| g == w)
                    })
        })
        .map(|(id, _)| id.clone())
        .collect()
}
impl App {
    pub fn import_skill(&self, path: &Path, expected: Option<&str>) -> Result<Value> {
        let repo = Repository::open(&crate::app::absolute(path)?, Default::default(), vec![])?;
        let p = Package::read_from(&repo, "", "explicit-import".into())?;
        if expected.is_some_and(|d| d != p.digest) {
            return Err(Error::Denied(
                "İstenen skill pini uyuşmadı; import yapılmadı.",
            ));
        }
        let id = p.digest.strip_prefix("blake3:").ok_or(Error::Store)?;
        let dir = self.home.join("skill-imports/objects");
        crate::app::safe_path(&dir)?;
        std::fs::create_dir_all(&dir)?;
        let target = dir.join(format!("{id}.enc"));
        crate::app::safe_path(&target)?;
        if !target.exists() {
            use std::io::Write;
            let cipher = self.key.seal(
                &bytes(&p)?,
                format!("melissa.skill-import/v1:{id}").as_bytes(),
            )?;
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(target)?;
            f.write_all(&cipher)?;
            f.sync_all()?;
        }
        let current = self.home.join("skill-imports/current");
        crate::app::safe_path(&current)?;
        std::fs::create_dir_all(&current)?;
        let target = current.join(format!("{}.enc", p.name));
        crate::app::safe_path(&target)?;
        let temp = target.with_extension(format!("{}.tmp", crate::app::nonce()?));
        use std::io::Write;
        let cipher = self.key.seal(
            &bytes(&p)?,
            format!("melissa.skill-current/v1:{}", p.name).as_bytes(),
        )?;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        f.write_all(&cipher)?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(temp, target)?;
        Ok(
            json!({"command":"skill import","name":p.name,"digest":p.digest,"status":"PASS","active_task_changed":false,"authority_granted":false}),
        )
    }
    pub fn extension_catalog(&self, workspace: &Path) -> Result<Value> {
        let repo = Repository::open(
            &crate::app::absolute(workspace)?,
            Default::default(),
            vec![],
        )?;
        self.require_separate_workspace(&repo)?;
        let packages = catalog(self, &repo)?;
        let config = self.extension_config(&repo)?;
        config.validate(&packages)?;
        Ok(
            json!({"command":"extensions catalog","skills":packages.iter().map(|(id,p)|json!({"id":id,"description":p.description,"digest":p.digest,"origin":p.origin})).collect::<Vec<_>>(),"config":config,"secret_resolved":false,"runtime_preflight":"NOT_RUN"}),
        )
    }
}
