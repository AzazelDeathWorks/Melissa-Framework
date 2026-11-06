//! Task-owned loopback service bundles, contract checks and disposable DB rehearsals.
pub mod final_check;
pub mod host;
pub mod operator;
pub mod settings;
#[cfg(all(test, windows))]
mod tests;
pub mod tools;
use crate::{
    Error, Result,
    project::{Command, Contract},
};
use melissa_web_development::plan::Database;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ready {
    pub path: String,
    pub status: u16,
    pub timeout_ms: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Service {
    pub command: Command,
    pub port: u16,
    pub ready: Ready,
    #[serde(default)]
    pub depends_on: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Data {
    pub engine: Database,
    #[serde(default)]
    pub schema: Option<String>,
    #[serde(default)]
    pub sample: Option<String>,
    pub provenance: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApiCheck {
    pub service: String,
    pub schema: String,
    pub seed: u64,
    pub examples: u32,
    #[serde(default = "all_phases")]
    pub phases: Vec<String>,
}
fn all_phases() -> Vec<String> {
    ["examples", "coverage", "fuzzing", "stateful"]
        .iter()
        .map(|s| s.to_string())
        .collect()
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Assertion {
    pub sql: String,
    pub rows: Vec<Vec<serde_json::Value>>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rehearsal {
    pub engine: Database,
    pub fresh: Vec<String>,
    #[serde(default)]
    pub fresh_sample: Option<String>,
    pub baseline: String,
    pub sample: String,
    pub upgrade: Vec<String>,
    #[serde(default)]
    pub downgrade: Option<Vec<String>>,
    pub transactional: bool,
    pub provenance: String,
    pub before: Vec<Assertion>,
    pub after: Vec<Assertion>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Required {
    Contract { name: String },
    Migration { name: String },
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Spec {
    #[serde(default)]
    pub acceptance: Vec<Required>,
    #[serde(default)]
    pub backend: melissa_container_executor::CreationBackend,
    #[serde(default)]
    pub services: BTreeMap<String, Service>,
    #[serde(default)]
    pub database: Option<Data>,
    #[serde(default)]
    pub statements: BTreeMap<String, String>,
    #[serde(default)]
    pub api_checks: BTreeMap<String, ApiCheck>,
    #[serde(default)]
    pub rehearsals: BTreeMap<String, Rehearsal>,
    #[serde(default)]
    pub business_checks: Vec<String>,
}
pub fn name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 80
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
}
fn path(s: &str) -> bool {
    melissa_workflow::plan::path(s) && !s.split('/').any(|c| c.starts_with(".melissa-"))
}
fn provenance(s: &str) -> bool {
    ["synthetic", "anonymized"].contains(&s)
}
impl Spec {
    pub fn validate(&self, contract: &Contract) -> Result {
        if self.services.is_empty() && self.database.is_none() && self.rehearsals.is_empty() {
            return Err(Error::Usage(
                "Servis, özel DB veya migration provası gerekli.",
            ));
        }
        let mut ports = BTreeSet::new();
        ports.insert(37642);
        for (id, s) in &self.services {
            s.command.validate()?;
            if !name(id)
                || id == "database"
                || s.port < 1024
                || !ports.insert(s.port)
                || !melissa_web_development::protocol::local_path(&s.ready.path)
                || !(100..=599).contains(&s.ready.status)
                || s.ready.timeout_ms == 0
            {
                return Err(Error::Usage("Servis adı/port/readiness ayarı geçersiz."));
            }
            for d in &s.depends_on {
                if d == id
                    || (!self.services.contains_key(d)
                        && !(d == "database" && self.database.is_some()))
                {
                    return Err(Error::Usage("Servis bağımlılığı kayıtlı değil."));
                }
            }
        }
        let mut complete = BTreeSet::new();
        if self.database.is_some() {
            complete.insert("database".to_string());
        }
        while complete.len() < self.services.len() + usize::from(self.database.is_some()) {
            let old = complete.len();
            for (id, s) in &self.services {
                if s.depends_on.iter().all(|d| complete.contains(d)) {
                    complete.insert(id.clone());
                }
            }
            if old == complete.len() {
                return Err(Error::Usage("Döngülü servis bağımlılığı."));
            }
        }
        if let Some(d) = &self.database
            && (!provenance(&d.provenance)
                || d.schema.as_ref().is_some_and(|p| !path(p))
                || d.sample.as_ref().is_some_and(|p| !path(p)))
        {
            return Err(Error::Usage(
                "DB örneği sentetik/anonimleştirilmiş ve kaynak içinde olmalı.",
            ));
        }
        for (id, p) in &self.statements {
            if !name(id) || !path(p) {
                return Err(Error::Usage("SQL statement kaydı geçersiz."));
            }
        }
        for (id, c) in &self.api_checks {
            if !name(id)
                || !self.services.contains_key(&c.service)
                || !path(&c.schema)
                || c.examples == 0
                || c.phases.is_empty()
                || c.phases
                    .iter()
                    .any(|p| !["examples", "coverage", "fuzzing", "stateful"].contains(&p.as_str()))
            {
                return Err(Error::Usage("API sözleşme kontrolü geçersiz."));
            }
        }
        for (id, r) in &self.rehearsals {
            if !name(id)
                || !provenance(&r.provenance)
                || !path(&r.baseline)
                || !path(&r.sample)
                || r.fresh_sample.as_ref().is_some_and(|p| !path(p))
                || r.fresh.is_empty()
                || r.upgrade.is_empty()
                || r.fresh
                    .iter()
                    .chain(&r.upgrade)
                    .chain(r.downgrade.iter().flatten())
                    .any(|p| !path(p))
                || r.before.is_empty()
                || r.after.is_empty()
                || r.before
                    .iter()
                    .chain(&r.after)
                    .any(|a| a.sql.trim().is_empty())
            {
                return Err(Error::Usage(
                    "Migration prova/örnek veri/invariant tanımı eksik.",
                ));
            }
        }
        for required in &self.acceptance {
            match required {
                Required::Contract { name } if !self.api_checks.contains_key(name) => {
                    return Err(Error::Usage("Son kabul API kontrolü kayıtlı değil."));
                }
                Required::Migration { name } if !self.rehearsals.contains_key(name) => {
                    return Err(Error::Usage("Son kabul migration provası kayıtlı değil."));
                }
                _ => {}
            }
        }
        let mut seen = BTreeSet::new();
        for n in &self.business_checks {
            if !contract.checks.contains_key(n) || !seen.insert(n) {
                return Err(Error::Usage(
                    "İş kuralı kontrolü onaylı proje komutu olmalı.",
                ));
            }
        }
        Ok(())
    }
    pub fn source_paths(&self) -> BTreeSet<&str> {
        let mut p = BTreeSet::new();
        if let Some(d) = &self.database {
            p.extend(d.schema.iter().chain(&d.sample).map(String::as_str));
        }
        p.extend(self.statements.values().map(String::as_str));
        p.extend(self.api_checks.values().map(|c| c.schema.as_str()));
        for r in self.rehearsals.values() {
            p.insert(r.baseline.as_str());
            p.insert(r.sample.as_str());
            p.extend(r.fresh_sample.iter().map(String::as_str));
            p.extend(
                r.fresh
                    .iter()
                    .chain(&r.upgrade)
                    .chain(r.downgrade.iter().flatten())
                    .map(String::as_str),
            );
        }
        p
    }
}
