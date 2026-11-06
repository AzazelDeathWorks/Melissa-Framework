//! Source-bound project validation. Public project results never accept a task
//! or become the harness's own independent verification evidence.
pub mod runner;
pub mod settings;
#[cfg(all(test, windows))]
mod tests;
pub mod tools;
use crate::{Error, Result};
use melissa_verification::Outcome;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    #[default]
    Command,
    Syntax,
    Type,
    Lint,
    Format,
    Test,
    Doctest,
    Nextest,
    Coverage,
    Mutation,
    Regression,
}
impl Kind {
    pub fn tests(&self) -> bool {
        matches!(
            self,
            Self::Test | Self::Doctest | Self::Nextest | Self::Regression
        )
    }
}
fn one() -> u16 {
    1
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Report {
    pub path: String,
    pub format: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Check {
    pub kind: Kind,
    #[serde(default = "one")]
    pub repetitions: u16,
    #[serde(default)]
    pub report: Option<Report>,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Spec {
    #[serde(default)]
    pub checks: BTreeMap<String, Check>,
    #[serde(default)]
    pub after_edit: Vec<String>,
    #[serde(default)]
    pub reproduce: Vec<String>,
    #[serde(default)]
    pub debug: Option<melissa_build_test::debug::Launch>,
}
impl Spec {
    pub fn validate(&self, commands: &BTreeMap<String, crate::project::Command>) -> Result {
        for (name, c) in &self.checks {
            if !commands.contains_key(name) || c.repetitions == 0 || c.repetitions > 1000 {
                return Err(Error::Usage(
                    "Doğrulama adı/tekrar sayısı onaylı komutla eşleşmeli.",
                ));
            }
            if let Some(r) = &c.report
                && (!melissa_workflow::plan::path(&r.path)
                    || !["junit", "llvm_json", "mutants_json"].contains(&r.format.as_str()))
            {
                return Err(Error::Usage("Doğrulama rapor yolu/biçimi geçersiz."));
            }
        }
        for list in [&self.after_edit, &self.reproduce] {
            let mut unique = std::collections::BTreeSet::new();
            for name in list {
                if !commands.contains_key(name) || !unique.insert(name) {
                    return Err(Error::Usage(
                        "Doğrulama seçiminde bilinmeyen/tekrarlı komut.",
                    ));
                }
            }
        }
        if let Some(debug) = &self.debug {
            debug
                .validate()
                .map_err(|_| Error::Usage("Proje debug ayarı geçersiz."))?;
        }
        Ok(())
    }
}
pub fn check(contract: &crate::project::Contract, name: &str) -> Check {
    if let Some(c) = contract
        .verification
        .as_ref()
        .and_then(|s| s.checks.get(name))
    {
        return c.clone();
    }
    let kind = if name.contains("nextest") {
        Kind::Nextest
    } else if name.contains("coverage") {
        Kind::Coverage
    } else if name.contains("mutants") {
        Kind::Mutation
    } else if name.contains("doctest") {
        Kind::Doctest
    } else if name.contains("test") {
        Kind::Test
    } else if name.contains("format") {
        Kind::Format
    } else if name.contains("lint") {
        Kind::Lint
    } else if name.contains("check") {
        Kind::Type
    } else {
        Kind::Command
    };
    Check {
        kind,
        repetitions: 1,
        report: None,
    }
}
pub fn outcome(v: &serde_json::Value) -> Outcome {
    Outcome::from_wire(v["outcome"].as_str().unwrap_or("INCONCLUSIVE"))
}
