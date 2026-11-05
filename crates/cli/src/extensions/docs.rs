use super::{
    hooks::{Helper, Runner},
    http::{Endpoint, Http},
};
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{Arc, atomic::AtomicBool},
};
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    pub package: String,
    /// python, node, cargo, runtime; actual installed version is probed first.
    pub ecosystem: String,
    pub endpoint: Endpoint,
    /// Exact installed version -> reviewed version-specific documentation URL/ID.
    pub versions: BTreeMap<String, String>,
    #[serde(default)]
    pub context7: bool,
    #[serde(default)]
    pub search_path: Option<String>,
}
impl Source {
    pub fn validate(&self) -> Result {
        self.endpoint.validate()?;
        if !["python", "node", "cargo", "runtime"].contains(&self.ecosystem.as_str())
            || self.package.is_empty()
            || self.package.len() > 214
            || !self
                .package
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"-_.@/".contains(&c))
        {
            return Err(Error::Usage("Doküman paket kimliği/ecosystem geçersiz."));
        }
        if self.package.contains("..")
            || self.package.starts_with(['.', '/'])
            || if self.ecosystem == "node" {
                self.package.matches('/').count() > 1
                    || (self.package.contains('/') && !self.package.starts_with('@'))
            } else {
                self.package.contains(['/', '@'])
            }
        {
            return Err(Error::Usage("Paket adı dosya yolu veya traversal olamaz."));
        }
        if self.versions.is_empty() {
            return Err(Error::Usage(
                "Doküman için açık sürüm kaynak eşlemesi gerekli.",
            ));
        }
        for (version, url) in &self.versions {
            if version.is_empty() {
                return Err(Error::Usage("Doküman sürümü boş."));
            }
            if self.context7 {
                if !url.starts_with('/')
                    || url.contains(['?', '#', '\\'])
                    || url.split('/').any(|p| p == "..")
                {
                    return Err(Error::Usage("Context7 açık sürümlü library ID gerekli."));
                }
            } else {
                self.check_url(url)?;
            }
        }
        if let Some(path) = &self.search_path {
            self.check_url(&format!("{}{path}", self.endpoint.url))?;
        }
        Ok(())
    }
    pub fn check_url(&self, url: &str) -> Result {
        let base = url::Url::parse(&self.endpoint.url)
            .map_err(|_| Error::Usage("Doküman tabanı geçersiz."))?;
        let target = url::Url::parse(url).map_err(|_| Error::Usage("Doküman URL geçersiz."))?;
        let prefix = base.path().trim_end_matches('/');
        if target.origin() != base.origin()
            || target.path().contains('%')
            || !target.username().is_empty()
            || target.password().is_some()
            || target.query().is_some()
            || target.fragment().is_some()
            || !(target.path() == prefix || target.path().starts_with(&format!("{prefix}/")))
        {
            return Err(Error::Denied("Doküman URL onaylanan origin/yol dışında."));
        }
        Ok(())
    }
}
pub async fn resolve(
    runner: &mut Runner,
    id: &str,
    source: &Source,
    query: &str,
    key: &str,
    cancel: Arc<AtomicBool>,
) -> Result<Value> {
    // Version comes from the selected synchronized worker, not a model guess or
    // a dependency range. The probe is itself a source/environment-bound receipt.
    let code = include_str!("installed.py");
    let mut command = crate::project::Command {
        executable: "python".into(),
        argv: vec![
            "-I".into(),
            "-c".into(),
            code.into(),
            source.ecosystem.clone(),
            source.package.clone(),
        ],
        cwd: ".".into(),
    };
    if (source.ecosystem == "python"
        || (source.ecosystem == "runtime" && source.package == "python"))
        && runner
            .contract
            .sync
            .iter()
            .any(|c| c.executable == "uv" && c.argv.first().is_some_and(|a| a == "sync"))
    {
        command.executable = "uv".into();
        command.argv.splice(
            0..0,
            [
                "run",
                "--offline",
                "--no-sync",
                "--no-python-downloads",
                "python",
            ]
            .map(String::from),
        );
    }
    let receipt = runner
        .helper(
            &format!("docs-probe:{key}:{id}"),
            &Helper {
                command,
                skill: None,
            },
            json!({"package":source.package}),
            cancel.clone(),
        )
        .await?;
    if receipt["status"] != "PASS" {
        return Err(Error::Unavailable(
            "Kurulu paket sürümü doğrulanmadı; sürüm tahminiyle doküman seçilmedi.",
        ));
    }
    let installed: Value = serde_json::from_str(
        receipt["stdout_text"]
            .as_str()
            .ok_or(Error::Unavailable("Paket sürümü çıktısı metin değil."))?,
    )
    .map_err(|_| Error::Unavailable("Paket sürümü çıktısı geçersiz."))?;
    let version = installed["version"]
        .as_str()
        .ok_or(Error::Unavailable("Paket sürümü bulunamadı."))?;
    let target = source.versions.get(version).ok_or(Error::Unavailable(
        "Kurulu sürüm için onaylanan doküman kaynağı yok.",
    ))?;
    let http = Http::new(source.endpoint.clone())?;
    let (url, request) = if source.context7 {
        let url = format!(
            "{}/api/v2/context",
            source.endpoint.url.trim_end_matches('/')
        );
        let request = http.request(reqwest::Method::GET, &url).query(&[
            ("libraryId", target.as_str()),
            ("query", query),
            ("type", "json"),
        ]);
        (url, request)
    } else {
        source.check_url(target)?;
        (target.clone(), http.request(reqwest::Method::GET, target))
    };
    let (status, _, raw) = http.send(request, cancel).await?;
    if status != 200 {
        return Err(Error::Unavailable(
            "Doküman kaynağı başarılı yanıt vermedi; sonuç uydurulmadı.",
        ));
    }
    let content =
        String::from_utf8(raw).map_err(|_| Error::Unavailable("Doküman metni UTF-8 değil."))?;
    Ok(
        json!({"source":id,"package":source.package,"version":version,"installed_environment":installed,"version_basis":"native_installed_probe","version_evidence":receipt,"url":url,"version_target":target,"retrieved_at_ms":crate::app::now()?,"content_digest":crate::digest(content.as_bytes()),"content":content,"untrusted_external_content":true,"authority_granted":false}),
    )
}
pub async fn read(source: &Source, url: &str, cancel: Arc<AtomicBool>) -> Result<Value> {
    if source.context7 {
        return Err(Error::Denied(
            "Context7 sürümlü docs_resolve üzerinden çağrılır.",
        ));
    }
    source.check_url(url)?;
    let http = Http::new(source.endpoint.clone())?;
    let (status, _, raw) = http
        .send(http.request(reqwest::Method::GET, url), cancel)
        .await?;
    if status != 200 {
        return Err(Error::Unavailable("Doküman okuma başarılı değil."));
    }
    let text = String::from_utf8(raw).map_err(|_| Error::Unavailable("Doküman UTF-8 değil."))?;
    Ok(
        json!({"url":url,"content":text,"content_digest":crate::digest(text.as_bytes()),"version_basis":"unverified_read_only_use_resolve_for_installed_version","untrusted_external_content":true}),
    )
}
pub async fn search(source: &Source, query: &str, cancel: Arc<AtomicBool>) -> Result<Value> {
    let http = Http::new(source.endpoint.clone())?;
    let url = if source.context7 {
        format!(
            "{}/api/v2/libs/search",
            source.endpoint.url.trim_end_matches('/')
        )
    } else {
        format!(
            "{}{}",
            source.endpoint.url,
            source.search_path.as_ref().ok_or(Error::Unavailable(
                "Bu resmi kaynak için arama endpoint'i yapılandırılmamış."
            ))?
        )
    };
    let request = if source.context7 {
        http.request(reqwest::Method::GET, &url)
            .query(&[("libraryName", source.package.as_str()), ("query", query)])
    } else {
        source.check_url(&url)?;
        http.request(reqwest::Method::GET, &url)
            .query(&[("q", query)])
    };
    let (status, _, raw) = http.send(request, cancel).await?;
    if status != 200 {
        return Err(Error::Unavailable("Doküman araması başarısız."));
    }
    Ok(
        json!({"url":url,"query":query,"content":String::from_utf8(raw).map_err(|_|Error::Unavailable("Arama UTF-8 değil."))?,"version_basis":"search_discovery_not_version_acceptance","untrusted_external_content":true}),
    )
}
