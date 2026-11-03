//! Structural preflight of untrusted developer reports, never final acceptance.
use crate::{Error, Result, bounded, digest_valid};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckKind {
    Build,
    Tests,
    Format,
    Lint,
    Security,
    Dependency,
    PackageSmoke,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    Passed,
    Failed,
    NotRun,
    Blocked,
    Unknown,
    Unsupported,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TestCounts {
    pub discovered: u64,
    pub passed: u64,
    pub failed: u64,
    pub skipped: u64,
    pub filtered: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckClaim {
    pub kind: CheckKind,
    pub status: CheckStatus,
    pub source_digest: String,
    pub verifier_digest: String,
    pub environment_digest: String,
    pub log_digest: String,
    pub artifact_digest: Option<String>,
    pub exit_code: Option<i32>,
    pub truncated: bool,
    pub requires_reconciliation: bool,
    pub tests: Option<TestCounts>,
}
/// Trusted host policy. No Deserialize; tool arguments cannot select requirements.
pub struct CandidatePolicy {
    source_digest: String,
    artifact_digest: String,
    environment_digest: String,
    required: BTreeMap<CheckKind, String>,
}
#[derive(Debug, Serialize)]
pub struct CandidateAssessment {
    pub schema_version: &'static str,
    pub status: &'static str,
    pub blockers: Vec<String>,
    pub claim_authenticity: &'static str,
    pub protected_acceptance: bool,
    pub task_accepted: bool,
}
impl CandidatePolicy {
    pub fn new(
        source_digest: String,
        artifact_digest: String,
        environment_digest: String,
        required: BTreeMap<CheckKind, String>,
    ) -> Result<Self> {
        if ![&source_digest, &artifact_digest, &environment_digest]
            .iter()
            .all(|s| digest_valid(s))
            || required.is_empty()
            || required.values().any(|s| !digest_valid(s))
        {
            return Err(Error::Invalid("host candidate policy identities"));
        }
        Ok(Self {
            source_digest,
            artifact_digest,
            environment_digest,
            required,
        })
    }
    pub fn assess(&self, claims: &[CheckClaim]) -> Result<CandidateAssessment> {
        if claims.len() > 64 {
            return Err(Error::Invalid("check count"));
        }
        let mut seen = BTreeSet::new();
        let mut blockers = vec![];
        for c in claims {
            if !seen.insert(c.kind.clone()) {
                return Err(Error::Invalid("duplicate check kind"));
            }
            if !self.required.contains_key(&c.kind) {
                return Err(Error::Invalid("unregistered check kind"));
            }
            if c.source_digest != self.source_digest {
                blockers.push(format!("{:?}: stale source", c.kind));
            }
            if Some(&c.verifier_digest) != self.required.get(&c.kind) {
                blockers.push(format!("{:?}: verifier mismatch", c.kind));
            }
            if c.environment_digest != self.environment_digest {
                blockers.push(format!("{:?}: environment mismatch", c.kind));
            }
            if !digest_valid(&c.log_digest) {
                blockers.push(format!("{:?}: raw log identity missing", c.kind));
            }
            if c.status != CheckStatus::Passed
                || c.exit_code != Some(0)
                || c.truncated
                || c.requires_reconciliation
            {
                blockers.push(format!(
                    "{:?}: incomplete, failed or uncertain result",
                    c.kind
                ));
            }
            if c.kind == CheckKind::Tests {
                let complete = c.tests.as_ref().is_some_and(|n| {
                    n.discovered > 0
                        && n.discovered == n.passed
                        && n.failed == 0
                        && n.skipped == 0
                        && n.filtered == 0
                });
                if !complete {
                    blockers.push("Tests: zero, skipped, filtered, failed or missing tests".into());
                }
            } else if c.tests.is_some() {
                return Err(Error::Invalid("test counts on a non-test claim"));
            }
            if matches!(c.kind, CheckKind::Build | CheckKind::PackageSmoke)
                && c.artifact_digest.as_ref() != Some(&self.artifact_digest)
            {
                blockers.push(format!("{:?}: different package bytes", c.kind));
            }
            if !bounded(&c.source_digest, 128) {
                return Err(Error::Invalid("claim identity bounds"));
            }
        }
        for kind in self.required.keys() {
            if !seen.contains(kind) {
                blockers.push(format!("{kind:?}: NOT_RUN"));
            }
        }
        Ok(CandidateAssessment {
            schema_version: "melissa.quality-claim-preflight/v1",
            status: if blockers.is_empty() {
                "READY_FOR_INDEPENDENT_VERIFICATION"
            } else {
                "BLOCKED"
            },
            blockers,
            claim_authenticity: "UNTRUSTED_DEVELOPER_CLAIMS",
            protected_acceptance: false,
            task_accepted: false,
        })
    }
}
