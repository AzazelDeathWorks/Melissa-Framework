//! Operator-authored scope and protected oracle, never model-generated policy.
use crate::{
    Error, Result,
    app::{absolute, decode},
    resources,
};
use melissa_code_quality::protected::{Case, Language, ProgramSpec, Suite};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Step {
    pub profile: String,
    pub goal: String,
    pub write: bool,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Check {
    pub id: String,
    pub input: Value,
    pub expected: Value,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Recipe {
    pub schema_version: String,
    pub agents: Vec<Step>,
    pub writable_paths: BTreeSet<String>,
    pub protected_paths: BTreeSet<String>,
    #[serde(default = "default_language")]
    pub language: Language,
    #[serde(default)]
    pub entry: String,
    pub negative_workspace: PathBuf,
    #[serde(default)]
    pub checks: Vec<Check>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub development_checks: Vec<Check>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain: Option<crate::domain::DomainRecipe>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<crate::project::Contract>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate_schema: Option<Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub report_checks: Vec<crate::domain::Expectation>,
    #[serde(skip)]
    pub(crate) source_ref: Option<PathBuf>,
    pub max_cost_micro_usd: u64,
    pub wall_time_ms: u64,
}
fn default_language() -> Language {
    Language::Python
}
impl Recipe {
    /// New executable tasks always keep independent testing and final review.
    /// This is done before the task contract and its operator approval exist.
    /// Deserializing/resuming a historical recipe never rewrites its authority.
    pub(crate) fn with_serial_verification(mut self) -> Result<Self> {
        self.validate()?;
        let review = if self
            .agents
            .last()
            .is_some_and(|s| s.profile == "melissa.code.reviewer" && !s.write)
        {
            self.agents.pop().expect("checked")
        } else {
            Step {profile:"melissa.code.reviewer".into(),goal:"Independently inspect final source and recorded evidence; report remaining defects and submit a source-bound candidate for protected host QA.".into(),write:false}
        };
        if !self
            .agents
            .last()
            .is_some_and(|s| s.profile == "melissa.code.test-engineer" && !s.write)
        {
            self.agents.push(Step{profile:"melissa.code.test-engineer".into(),goal:"Run authorized checks on final source, inspect failures, and retain source-bound evidence. Do not claim task acceptance.".into(),write:false});
        }
        if self.agents.len() == 1 {
            self.agents.insert(
                0,
                Step {
                    profile: "melissa.code.repository-analyst".into(),
                    goal: "Inspect the task's source, dependencies and verification scope.".into(),
                    write: false,
                },
            );
        }
        self.agents.push(review);
        self.validate()?;
        Ok(self)
    }
    pub fn read(path: &Path) -> Result<Self> {
        let path = absolute(path)?;
        let r = melissa_repository::Repository::open(
            path.parent().ok_or(Error::Io)?,
            Default::default(),
            vec![],
        )?;
        let mut recipe: Self = decode(
            &r.read(path.file_name().and_then(|s| s.to_str()).ok_or(Error::Io)?)?
                .1,
        )?;
        recipe.source_ref = Some(path);
        recipe.validate()?;
        Ok(recipe)
    }
    pub fn validate(&self) -> Result {
        if ![
            "melissa.cli-recipe/v1",
            "melissa.cli-recipe/v2",
            "melissa.cli-recipe/v3",
        ]
        .contains(&self.schema_version.as_str())
            || self.agents.is_empty()
            || self.agents.len() > 128
            || self.wall_time_ms == 0
            || crate::app::now()?
                .checked_add(self.wall_time_ms)
                .is_none_or(|end| end > i64::MAX as u64)
            || self.max_cost_micro_usd == 0
            || self.max_cost_micro_usd > i64::MAX as u64
            || self.writable_paths.len() > 10000
            || (self.domain.is_none()
                && self.project.is_none()
                && !melissa_workflow::plan::path(&self.entry))
        {
            return Err(Error::Usage(
                "Yürütme tarifi/kota/program kapsamı geçersiz.",
            ));
        }
        if self.project.is_none() {
            absolute(&self.negative_workspace)?;
        }
        let catalog = resources::catalog()?;
        for s in &self.agents {
            let p = catalog
                .profiles
                .get(&s.profile)
                .ok_or(Error::Usage("Tarifte bilinmeyen uzman."))?;
            if p.module != melissa_core_contracts::Module::Code
                || s.goal.trim().is_empty()
                || s.goal.len() > 16384
                || !p.tool_ids.iter().any(|t| t == "evidence.attach")
                || (s.write
                    && (!p
                        .authority
                        .capability_ceiling
                        .iter()
                        .any(|e| e == "workspace.apply_patch_scoped")
                        || (self.writable_paths.is_empty() && self.project.is_none())))
            {
                return Err(Error::Denied("Uzmanın araç/yazma kapsamı uygun değil."));
            }
        }
        for path in self.writable_paths.iter().chain(&self.protected_paths) {
            if path == "/" || !melissa_workflow::plan::path(path) {
                return Err(Error::Denied("Dosyalar açık ve proje içi olmalı."));
            }
        }
        for path in &self.writable_paths {
            if path.split('/').any(|s| s.eq_ignore_ascii_case("AGENTS.md"))
                || self.protected_paths.iter().any(|p| {
                    path.eq_ignore_ascii_case(p)
                        || path.to_uppercase().starts_with(&(p.to_uppercase() + "/"))
                })
            {
                return Err(Error::Denied("Yazma listesinde korunan dosya var."));
            }
        }
        if let Some(project) = &self.project {
            if self.schema_version != "melissa.cli-recipe/v3"
                || self.domain.is_some()
                || !self.entry.is_empty()
                || !self.checks.is_empty()
                || !self.development_checks.is_empty()
                || !self.report_checks.is_empty()
                || self.candidate_schema.is_some()
                || !self.negative_workspace.as_os_str().is_empty()
            {
                return Err(Error::Usage(
                    "Proje yürütmesi özel oracle/domain alanlarıyla karıştırılamaz.",
                ));
            }
            project.validate()?;
        } else if self.schema_version == "melissa.cli-recipe/v3" {
            return Err(Error::Usage("v3 proje sözleşmesi eksik."));
        } else if let Some(domain) = &self.domain {
            if self.schema_version != "melissa.cli-recipe/v2"
                || !self.entry.is_empty()
                || !self.checks.is_empty()
                || !self.development_checks.is_empty()
            {
                return Err(Error::Usage(
                    "Domain tarifi program alanlarıyla karıştırılamaz.",
                ));
            }
            domain.validate()?;
            if !self
                .agents
                .iter()
                .any(|s| s.profile == domain.kind.profile())
            {
                return Err(Error::Denied(
                    "Domain tarifi o domain'in yetkili uzmanını içermeli.",
                ));
            }
            if domain
                .seed_path
                .as_ref()
                .is_some_and(|p| self.writable_paths.contains(p))
            {
                return Err(Error::Denied(
                    "Test başlangıç verisi yazılabilir kaynak olamaz.",
                ));
            }
        } else {
            self.suite()?;
            if !self.development_checks.is_empty() {
                self.development_suite()?;
            }
            if self.schema_version == "melissa.cli-recipe/v2" && self.development_checks.is_empty()
            {
                return Err(Error::Usage(
                    "v2 program tarifinde özel son testlerden ayrı geliştirme testleri gerekli.",
                ));
            }
        }
        if let Some(schema) = &self.candidate_schema
            && (self.schema_version != "melissa.cli-recipe/v2"
                || schema["type"] != "object"
                || schema["properties"]["source_digest"]["type"] != "string"
                || !schema["required"]
                    .as_array()
                    .is_some_and(|a| a.contains(&json!("source_digest"))))
        {
            return Err(Error::Usage(
                "Aday şeması source_digest alanını zorunlu tutmalı.",
            ));
        }
        melissa_model_gateway::schema::validate_schema(&self.output_schema())
            .map_err(|_| Error::Usage("Aday çıktı şeması desteklenmiyor."))?;
        Ok(())
    }
    pub fn suite(&self) -> Result<Suite> {
        Suite::new(
            self.checks
                .iter()
                .map(|c| Case {
                    id: c.id.clone(),
                    input: c.input.clone(),
                    expected: c.expected.clone(),
                })
                .collect(),
        )
        .map_err(|_| Error::Usage("En az iki farklı korumalı kabul girdisi gerekli."))
    }
    pub fn program(&self) -> ProgramSpec {
        ProgramSpec {
            language: self.language,
            entry: self.entry.clone(),
        }
    }
    pub fn acceptance_digest(&self) -> Result<String> {
        if let Some(project) = &self.project {
            return Ok(crate::digest(&crate::app::bytes(project)?));
        }
        let base = match &self.domain {
            Some(d) => d.acceptance_digest()?,
            None => self.suite()?.digest().into(),
        };
        if self.schema_version == "melissa.cli-recipe/v1" {
            return Ok(base);
        }
        Ok(crate::digest(&serde_json::to_vec(&json!({"source_oracle_digest":base,"private_report_checks":self.report_checks,"candidate_schema":self.output_schema()})).map_err(|_|Error::Store)?))
    }
    pub fn output_schema(&self) -> Value {
        self.candidate_schema.clone().unwrap_or_else(||json!({"type":"object","properties":{"summary":{"type":"string"},"source_digest":{"type":"string"}},"required":["summary","source_digest"],"additionalProperties":false}))
    }
    pub fn development_suite(&self) -> Result<Suite> {
        self.development_selection(None)
    }
    pub(crate) fn development_selection(&self, ids: Option<&BTreeSet<String>>) -> Result<Suite> {
        if let Some(ids) = ids
            && (ids.len() < 2
                || ids
                    .iter()
                    .any(|id| !self.development_checks.iter().any(|case| &case.id == id)))
        {
            return Err(Error::Denied(
                "Targeted cases must be distinct IDs from the approved public suite.",
            ));
        }
        Suite::new(
            self.development_checks
                .iter()
                .filter(|case| ids.is_none_or(|ids| ids.contains(&case.id)))
                .map(|c| Case {
                    id: c.id.clone(),
                    input: c.input.clone(),
                    expected: c.expected.clone(),
                })
                .collect(),
        )
        .map_err(|_| Error::Usage("No valid public development suite is configured."))
    }
    pub fn public_plan(&self) -> Value {
        if let Some(project) = &self.project {
            return json!({"agents":self.agents,"writable_paths":self.writable_paths,"protected_paths":self.protected_paths,"project":project.public_summary().unwrap_or(serde_json::Value::Null),"scope_amendments":"operator-approved paths at safe pause/resume; same task, budget and deadline","acceptance_kind":"host_source_bound_project_checks","private_oracle":false,"max_cost_micro_usd":self.max_cost_micro_usd,"wall_time_ms":self.wall_time_ms,"source_access":"visible_snapshot_only_no_host_writeback"});
        }
        if let Some(domain) = &self.domain {
            return json!({"agents":self.agents,"writable_paths":self.writable_paths,"protected_paths":self.protected_paths,"workload":domain.public_plan(),"expected_answers_sent_to_model":false,"max_cost_micro_usd":self.max_cost_micro_usd,"wall_time_ms":self.wall_time_ms});
        }
        json!({"agents":self.agents,"writable_paths":self.writable_paths,"protected_paths":self.protected_paths,"language":self.language,"entry":self.entry,"protected_checks":self.checks.len(),"expected_answers_sent_to_model":false,"max_cost_micro_usd":self.max_cost_micro_usd,"wall_time_ms":self.wall_time_ms,"process_backend":"isolated_CODE03_SDK","source_access":"selected_repository_only"})
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Execution {
    pub recipe: Recipe,
    pub settings_digest: String,
    pub expires_at_ms: u64,
    pub negative_files: std::collections::BTreeMap<String, Vec<u8>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_digest: Option<String>,
}
pub(crate) const EXECUTION: &str = "cli:execution:v1";
