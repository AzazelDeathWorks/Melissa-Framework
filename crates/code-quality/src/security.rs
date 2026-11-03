//! Bounded secret detection, without copying matched secrets into findings.
//! This versioned rule set does not claim complete DLP or absence of all bugs.
use crate::{Error, Result, digest, source::FrozenSource};
use melissa_repository::Repository;
use regex::Regex;
use serde::Serialize;
use std::{
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
#[derive(Serialize)]
pub struct AuditEvidence {
    source_digest: String,
    lock_digest: String,
    tool_digest: String,
    report_digest: String,
    database_commit: String,
    database_time: String,
    checked_at_ms: u64,
}
impl AuditEvidence {
    pub fn source_digest(&self) -> &str {
        &self.source_digest
    }
    pub fn digest(&self) -> Result<String> {
        Ok(digest(&serde_json::to_vec(self)?))
    }
    pub fn current(&self) -> Result {
        let now = melissa_execution_broker::unix_ms().map_err(|_| Error::Denied("audit clock"))?;
        if now.saturating_sub(self.checked_at_ms) > 72 * 3600 * 1000 {
            return Err(Error::Stale("advisory scan expired"));
        }
        Ok(())
    }
}
pub struct AuditTool {
    executable: PathBuf,
    digest: String,
}
impl AuditTool {
    pub fn pin(path: PathBuf) -> Result<Self> {
        if !path.is_absolute() {
            return Err(Error::Invalid("absolute audit tool"));
        }
        let bytes = std::fs::read(&path).map_err(|_| Error::Denied("audit tool"))?;
        Ok(Self {
            executable: path,
            digest: digest(&bytes),
        })
    }
    pub fn scan(
        &self,
        source: &FrozenSource,
        repo: &Repository,
        raw_output: &Path,
        cancel: &AtomicBool,
    ) -> Result<AuditEvidence> {
        source.current(repo, cancel)?;
        if source
            .snapshot()
            .files
            .iter()
            .any(|f| f.path.split('/').any(|p| p == ".cargo"))
        {
            return Err(Error::Denied("project audit overrides"));
        }
        let bytes = std::fs::read(&self.executable).map_err(|_| Error::Denied("audit tool"))?;
        if digest(&bytes) != self.digest {
            return Err(Error::Stale("audit tool changed"));
        }
        let mut arguments = ["audit", "--json", "--deny", "warnings", "--file"]
            .map(str::to_owned)
            .to_vec();
        arguments.push(
            repo.root_path()
                .join("Cargo.lock")
                .to_str()
                .ok_or(Error::Invalid("Unicode lock path required"))?
                .into(),
        );
        let output =
            crate::host_process::capture(&self.executable, &arguments, repo.root_path(), cancel)?;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(raw_output)
            .map_err(|_| Error::Denied("preserve audit output"))?;
        std::io::Write::write_all(&mut file, &output.stdout)
            .map_err(|_| Error::Denied("audit evidence write"))?;
        let mut stderr = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(raw_output.with_extension("stderr.log"))
            .map_err(|_| Error::Denied("preserve audit stderr"))?;
        std::io::Write::write_all(&mut stderr, &output.stderr)
            .map_err(|_| Error::Denied("audit stderr write"))?;
        let value = melissa_model_gateway::strict_json(
            std::str::from_utf8(&output.stdout).map_err(|_| Error::Invalid("audit UTF8"))?,
        )
        .map_err(|_| Error::Invalid("audit JSON"))?;
        if output.exit_code != 0
            || value["vulnerabilities"]["count"].as_u64() != Some(0)
            || !value["vulnerabilities"]["list"]
                .as_array()
                .is_some_and(Vec::is_empty)
            || !value["warnings"]
                .as_object()
                .is_some_and(|w| w.values().all(|v| v.as_array().is_some_and(Vec::is_empty)))
        {
            return Err(Error::Denied("dependency audit findings or warnings"));
        }
        if value["settings"]["ignore"]
            .as_array()
            .is_some_and(|a| !a.is_empty())
        {
            return Err(Error::Denied("ignored advisories are not accepted"));
        }
        let updated = value["database"]["last-updated"]
            .as_str()
            .ok_or(Error::Invalid("advisory timestamp"))?;
        let database = OffsetDateTime::parse(updated, &Rfc3339)
            .map_err(|_| Error::Invalid("advisory date"))?;
        let now = melissa_execution_broker::unix_ms().map_err(|_| Error::Denied("audit clock"))?;
        let db_ms = u64::try_from(database.unix_timestamp_nanos() / 1_000_000)
            .map_err(|_| Error::Invalid("advisory date range"))?;
        if db_ms > now + 300000 || now.saturating_sub(db_ms) > 72 * 3600 * 1000 {
            return Err(Error::Stale("advisory database older than 72 hours"));
        }
        let commit = value["database"]["last-commit"]
            .as_str()
            .filter(|s| s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit()))
            .ok_or(Error::Invalid("advisory commit"))?;
        source.current(repo, cancel)?;
        Ok(AuditEvidence {
            source_digest: source.snapshot().digest.clone(),
            lock_digest: digest(source.bytes("Cargo.lock")?),
            tool_digest: self.digest.clone(),
            report_digest: digest(&output.stdout),
            database_commit: commit.into(),
            database_time: updated.into(),
            checked_at_ms: now,
        })
    }
}
pub const SEMGREP_IMAGE: &str =
    "sha256:50561c9f881d2250239343e1748402b8ede1e5662f84e696dbb22a911f1bc0b4";
pub const RULES: &str = include_str!("semgrep-rules.yml");
#[derive(Debug, Serialize)]
pub struct SecretFinding {
    pub rule_id: &'static str,
    pub path: String,
    pub line: usize,
    pub file_digest: String,
}
pub fn scan_secrets(source: &FrozenSource) -> Result<Vec<SecretFinding>> {
    let rules = [
        (
            "private-key",
            r"-----BEGIN (?:RSA |EC |OPENSSH |DSA )?PRIVATE KEY-----",
        ),
        ("aws-access-key", r"\b(?:AKIA|ASIA)[A-Z0-9]{16}\b"),
        (
            "github-token",
            r"\b(?:ghp_|gho_|ghu_|ghs_)[A-Za-z0-9]{30,}\b",
        ),
        (
            "api-secret-assignment",
            r#"(?i)(?:api[_-]?key|access[_-]?token|client[_-]?secret|password)\s*[:=]\s*["'][A-Za-z0-9_+/=.\-]{16,}["']"#,
        ),
    ];
    let compiled: Vec<_> = rules
        .into_iter()
        .map(|(id, r)| {
            Regex::new(r)
                .map(|r| (id, r))
                .map_err(|_| Error::Invalid("secret rule"))
        })
        .collect::<Result<_>>()?;
    let mut result = vec![];
    for f in &source.snapshot().files {
        let bytes = source.bytes(&f.path)?;
        let Ok(text) = std::str::from_utf8(bytes) else {
            continue;
        };
        for (id, rule) in &compiled {
            for m in rule.find_iter(text) {
                result.push(SecretFinding {
                    rule_id: id,
                    path: f.path.clone(),
                    line: bytes[..m.start()].iter().filter(|&&b| b == b'\n').count() + 1,
                    file_digest: f.digest.clone(),
                });
                if result.len() > 10000 {
                    return Err(Error::Invalid("secret findings overflow"));
                }
            }
        }
    }
    Ok(result)
}
pub fn rules_digest() -> String {
    digest(RULES.as_bytes())
}
pub(crate) fn semgrep_clean(bytes: &[u8]) -> Result<String> {
    let text = std::str::from_utf8(bytes).map_err(|_| Error::Invalid("SAST UTF-8"))?;
    let value =
        melissa_model_gateway::strict_json(text).map_err(|_| Error::Invalid("SAST protocol"))?;
    if !value["results"].as_array().is_some_and(Vec::is_empty)
        || !value["errors"].as_array().is_some_and(Vec::is_empty)
        || !value["paths"]["scanned"]
            .as_array()
            .is_some_and(|a| !a.is_empty())
        || !value["version"].is_string()
    {
        return Err(Error::Denied(
            "SAST findings, parse errors or zero coverage",
        ));
    }
    Ok(value["version"].as_str().unwrap().into())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn semgrep_zero_coverage_errors_duplicate_json_and_findings_are_not_clean() {
        for bytes in [br#"{"version":"1","results":[],"errors":[],"paths":{"scanned":[]}}"#.as_slice(),br#"{"version":"1","results":[{}],"errors":[],"paths":{"scanned":["a.rs"]}}"#,br#"{"version":"1","results":[],"errors":[{}],"paths":{"scanned":["a.rs"]}}"#,br#"{"version":"1","results":[],"results":[],"errors":[],"paths":{"scanned":["a.rs"]}}"#] { assert!(semgrep_clean(bytes).is_err()); }
        assert_eq!(
            semgrep_clean(
                br#"{"version":"1.0","results":[],"errors":[],"paths":{"scanned":["a.rs"]}}"#
            )
            .unwrap(),
            "1.0"
        );
    }
}
