//! Operator-owned local web/API/database recipes. Private oracles never enter
//! the workflow packet, skill context or model-visible development tool.
use crate::{Error, Result, digest};
use melissa_repository::{Repository, RepositorySnapshot};
use melissa_web_development::{
    plan::{App, Baseline, Database, Migration, Registration, Statement},
    protocol::{Command, Service},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

#[path = "domain_runtime.rs"]
mod runtime;
pub(crate) use runtime::{final_verify, model_tool, tool_definitions};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Frontend,
    Backend,
    Database,
}
impl Kind {
    pub fn profile(self) -> &'static str {
        match self {
            Self::Frontend => "melissa.code.frontend",
            Self::Backend => "melissa.code.backend",
            Self::Database => "melissa.code.database",
        }
    }
    pub fn effect(self) -> &'static str {
        match self {
            Self::Frontend => "browser.interact_scoped",
            Self::Backend => "network.test_scoped",
            Self::Database => "database.local_fixture",
        }
    }
    pub fn service(self) -> Service {
        match self {
            Self::Frontend => Service::Browser,
            Self::Backend => Service::Api,
            Self::Database => Service::Database,
        }
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum MigrationBaseline {
    Initial,
    After { name: String },
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MigrationSpec {
    pub source_path: String,
    pub baseline: MigrationBaseline,
    pub transactional: bool,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "operator", rename_all = "snake_case", deny_unknown_fields)]
pub enum Expectation {
    Equals { pointer: String, value: Value },
    Contains { pointer: String, value: Value },
    Exists { pointer: String },
    Length { pointer: String, value: usize },
}
impl Expectation {
    pub fn matches(&self, reply: &Value) -> bool {
        match self {
            Self::Equals { pointer, value } => reply.pointer(pointer) == Some(value),
            Self::Exists { pointer } => reply.pointer(pointer).is_some(),
            Self::Length { pointer, value } => reply.pointer(pointer).is_some_and(|v| match v {
                Value::Array(a) => a.len() == *value,
                Value::Object(o) => o.len() == *value,
                Value::String(s) => s.chars().count() == *value,
                _ => false,
            }),
            Self::Contains { pointer, value } => {
                reply.pointer(pointer).is_some_and(|v| match (v, value) {
                    (Value::String(a), Value::String(b)) => a.contains(b),
                    (Value::Array(a), b) => a.contains(b),
                    _ => false,
                })
            }
        }
    }
    fn pointer(&self) -> &str {
        match self {
            Self::Equals { pointer, .. }
            | Self::Contains { pointer, .. }
            | Self::Exists { pointer }
            | Self::Length { pointer, .. } => pointer,
        }
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Step {
    pub operation: Command,
    #[serde(default)]
    pub expect_error: bool,
    #[serde(default)]
    pub expect: Vec<Expectation>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Flow {
    pub id: String,
    pub steps: Vec<Step>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Preview {
    pub path: String,
    pub widths: Vec<u32>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DomainRecipe {
    pub kind: Kind,
    pub app: Option<App>,
    pub database: Option<Database>,
    pub seed_path: Option<String>,
    #[serde(default)]
    pub migrations: BTreeMap<String, MigrationSpec>,
    #[serde(default)]
    pub statements: BTreeMap<String, String>,
    #[serde(default)]
    pub public_checks: Vec<Flow>,
    pub private_checks: Vec<Flow>,
    pub preview: Option<Preview>,
    /// This fixture is part of the private approved recipe, never a model tool
    /// argument or an editable source file. It is used only in final QA.
    pub private_seed_sql: Option<String>,
}
fn path(p: &str) -> bool {
    p != "/" && melissa_workflow::plan::path(p) && !p.split('/').any(|s| s.starts_with(".melissa-"))
}
fn name(n: &str) -> bool {
    !n.is_empty()
        && n.len() <= 80
        && n.bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-_".contains(&c))
}
impl DomainRecipe {
    pub fn validate_operation(&self, op: &Command) -> Result {
        op.validate()
            .map_err(|_| Error::Denied("Domain komutu geçersiz."))?;
        if matches!(op, Command::Start {} | Command::Close {})
            || op.service() != Some(self.kind.service())
        {
            return Err(Error::Denied(
                "Oturum/servis kapsamı host tarafından seçilir.",
            ));
        }
        if matches!(op,Command::Execute{statement,..} if !self.statements.contains_key(statement))
            || matches!(op,Command::Migration{name} if !self.migrations.contains_key(name))
            || matches!(op,Command::SqlParallel{scripts} if scripts.iter().flat_map(|s|&s.steps).any(|s|matches!(s,melissa_web_development::protocol::SqlStep::Execute{statement,..} if !self.statements.contains_key(statement))))
        {
            return Err(Error::Denied(
                "SQL yalnız onaylı statement/migration adıyla yürütülür.",
            ));
        }
        Ok(())
    }
    pub fn validate(&self) -> Result {
        if self.private_checks.len() < 2
            || self.private_checks.len() > 64
            || self.public_checks.len() > 64
            || (self.kind == Kind::Database
                && (self.database.is_none() || self.seed_path.is_none() || self.app.is_some()))
            || (self.kind != Kind::Database && self.app.is_none())
            || self.seed_path.as_ref().is_some_and(|p| !path(p))
            || self.migrations.len() > 32
            || self.statements.len() > 128
            || self
                .private_seed_sql
                .as_ref()
                .is_some_and(|s| self.database.is_none() || s.is_empty() || s.len() > 1024 * 1024)
        {
            return Err(Error::Usage(
                "Domain ortamı, özel testler veya fixture kapsamı geçersiz.",
            ));
        }
        if let Some(app) = &self.app {
            let p = match app {
                App::Static { directory } => directory,
                App::Python { entry } | App::JavaScript { entry } | App::TypeScript { entry } => {
                    entry
                }
            };
            if !path(p) {
                return Err(Error::Denied("Uygulama yolu proje içinde olmalı."));
            }
        }
        let mut ids = BTreeSet::new();
        let mut private_digests = BTreeSet::new();
        for (flows, private) in [(&self.public_checks, false), (&self.private_checks, true)] {
            for flow in flows {
                if !name(&flow.id)
                    || !ids.insert(flow.id.clone())
                    || flow.steps.is_empty()
                    || flow.steps.len() > 64
                    || (private && !flow.steps.iter().any(|s| !s.expect.is_empty()))
                {
                    return Err(Error::Usage(
                        "Test akışları farklı kimlik ve gerçek gözlem gerektirir.",
                    ));
                }
                for s in &flow.steps {
                    self.validate_operation(&s.operation)?;
                    if s.expect.len() > 64
                        || (s.expect_error && s.expect.is_empty())
                        || s.expect.iter().any(|e| {
                            e.pointer().len() > 2048
                                || (!e.pointer().is_empty() && !e.pointer().starts_with('/'))
                        })
                    {
                        return Err(Error::Usage("Test karşılaştırması geçersiz."));
                    }
                }
                if private {
                    let raw = serde_json::to_vec(&flow.steps).map_err(|_| Error::Store)?;
                    if !private_digests.insert(digest(&raw)) {
                        return Err(Error::Usage("Aynı gizli akış iki bağımsız test sayılamaz."));
                    }
                }
            }
        }
        for (id, m) in &self.migrations {
            if !name(id) || !path(&m.source_path) || !m.transactional {
                return Err(Error::Denied(
                    "İlk CLI migration kapsamı tam transaction gerektirir.",
                ));
            }
            if let MigrationBaseline::After { name: after } = &m.baseline {
                if after == id || !self.migrations.contains_key(after) {
                    return Err(Error::Denied("Migration başlangıç bağı geçersiz."));
                }
                let mut seen = BTreeSet::from([id.as_str()]);
                let mut next = after.as_str();
                loop {
                    if !seen.insert(next) {
                        return Err(Error::Denied("Migration bağı döngülü."));
                    }
                    match &self.migrations[next].baseline {
                        MigrationBaseline::Initial => break,
                        MigrationBaseline::After { name } => next = name,
                    }
                }
            }
        }
        if self.statements.iter().any(|(n, p)| !name(n) || !path(p)) {
            return Err(Error::Denied("SQL statement dosya kapsamı geçersiz."));
        }
        if let Some(p) = &self.preview
            && (self.kind != Kind::Frontend
                || !melissa_web_development::protocol::local_path(&p.path)
                || p.widths.len() < 2
                || !p.widths.iter().any(|w| *w <= 600)
                || !p.widths.iter().any(|w| *w >= 1000)
                || p.widths.iter().collect::<BTreeSet<_>>().len() != p.widths.len()
                || p.widths.len() > 4
                || p.widths.iter().any(|w| !(240..=3840).contains(w)))
        {
            return Err(Error::Usage("Önizleme viewport/yolu geçersiz."));
        }
        if self.kind == Kind::Frontend && self.preview.is_none() {
            return Err(Error::Usage(
                "Frontend tarifi dar ve geniş PNG önizleme viewport'ları gerektirir.",
            ));
        }
        Ok(())
    }
    pub fn acceptance_digest(&self) -> Result<String> {
        self.validate()?;
        Ok(digest(&serde_json::to_vec(&json!({"kind":self.kind,"private_checks":self.private_checks,"private_seed_sql":self.private_seed_sql,"negative_control_required":true,"image":melissa_web_development::plan::WEB_IMAGE})).map_err(|_|Error::Store)?))
    }
    pub fn public_plan(&self) -> Value {
        json!({"kind":self.kind,"app":self.app,"database":self.database,"seed_path":self.seed_path,"migration_names":self.migrations.keys().collect::<Vec<_>>(),"statement_names":self.statements.keys().collect::<Vec<_>>(),"public_checks":self.public_checks,"private_check_count":self.private_checks.len(),"private_expected_answers_exposed":false,"private_fixture_exposed":false,"preview":self.preview,"sessions_close_before_model_continues":true})
    }
    pub(crate) fn registration(
        &self,
        repo: &Repository,
        source: &RepositorySnapshot,
        baseline: Option<&str>,
    ) -> Result<Registration> {
        let file_digest = |p: &str| -> Result<String> {
            let v = source
                .files
                .iter()
                .find(|f| f.path == p)
                .ok_or(Error::Denied("Kayıtlı SQL dosyası kaynakta yok."))?;
            let (current, _) = repo.read(p)?;
            if current != *v {
                return Err(Error::Conflict("SQL kaynağı değişti."));
            }
            Ok(v.digest.clone())
        };
        let mut migrations = BTreeMap::new();
        if let Some(baseline) = baseline {
            for (id, m) in &self.migrations {
                migrations.insert(
                    id.clone(),
                    Migration {
                        source_path: m.source_path.clone(),
                        source_digest: file_digest(&m.source_path)?,
                        baseline: match &m.baseline {
                            MigrationBaseline::Initial => Baseline::Schema {
                                digest: baseline.into(),
                            },
                            MigrationBaseline::After { name } => {
                                Baseline::After { name: name.clone() }
                            }
                        },
                        transactional: m.transactional,
                    },
                );
            }
        }
        let statements = self
            .statements
            .iter()
            .map(|(id, p)| {
                Ok((
                    id.clone(),
                    Statement {
                        source_path: p.clone(),
                        source_digest: file_digest(p)?,
                    },
                ))
            })
            .collect::<Result<_>>()?;
        Ok(Registration {
            app: self.app.clone(),
            database: self.database,
            seed_path: self.seed_path.clone(),
            migrations,
            statements,
            services: BTreeSet::from([self.kind.service()]),
        })
    }
}

#[cfg(test)]
#[path = "domain_tests.rs"]
mod tests;
