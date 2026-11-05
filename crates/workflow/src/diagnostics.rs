//! Bounded, source-versioned interpretation of trusted adapter receipts.
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorClass {
    Syntax,
    Compiler,
    Build,
    Test,
    Lint,
    Repository,
    Environment,
    Unknown,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Location {
    pub affected_file: Option<String>,
    pub affected_symbol: Option<String>,
    pub line_range: Option<[u64; 2]>,
    pub code: Option<String>,
    pub message: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Diagnostic {
    pub error_class: ErrorClass,
    pub tool: String,
    pub source_digest: String,
    pub related_diagnostics: Vec<Location>,
    pub failed_tests: Vec<String>,
    pub targeted_case_ids: Vec<String>,
    pub last_patch: Option<String>,
    pub evidence_ref: String,
    pub repairable: bool,
    pub fingerprint: String,
}
pub(crate) fn bounded(text: &str, size: usize) -> String {
    let mut end = text.len().min(size);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}
fn path(value: &Value) -> Option<String> {
    let path = value.as_str()?;
    let path = path.strip_prefix("/work/").unwrap_or(path);
    (path != "/" && crate::plan::path(path)).then(|| path.into())
}
pub(crate) fn classify(
    tool: &str,
    source: &str,
    evidence: &str,
    patch: Option<String>,
    content: &str,
) -> Diagnostic {
    let value: Value = serde_json::from_str(content).unwrap_or(Value::Null);
    let after = &value["after_edit"];
    let sdk = if after["sdk"].is_object() {
        &after["sdk"]
    } else {
        &value
    };
    let details = &sdk["details"];
    let status = sdk["status"].as_str().unwrap_or_default();
    let failed_tests = details["runs"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|r| matches!(r["status"].as_str(), Some("failed" | "fail" | "error")))
        .filter_map(|r| r["id"].as_str().map(|id| bounded(id, 256)))
        .take(32)
        .collect::<Vec<_>>();
    let mut related_diagnostics = details["diagnostics"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|d| d["level"] == "error")
        .take(24)
        .map(|d| {
            let line = d["line"].as_u64().filter(|line| *line > 0);
            Location {
                affected_file: path(&d["file"]),
                affected_symbol: d["symbol"].as_str().map(|v| bounded(v, 160)),
                line_range: line
                    .map(|line| [line, d["end_line"].as_u64().unwrap_or(line).max(line)]),
                code: d["code"].as_str().map(|v| bounded(v, 100)),
                message: bounded(d["message"].as_str().unwrap_or_default(), 384),
            }
        })
        .collect::<Vec<_>>();
    let syntax = after["syntax_errors"].as_array().filter(|a| !a.is_empty());
    if let Some(errors) = syntax {
        related_diagnostics.extend(errors.iter().take(8).filter_map(|item| {
            Some(Location {
                affected_file: Some(path(item)?),
                affected_symbol: None,
                line_range: None,
                code: None,
                message: "Source parser reported a syntax error".into(),
            })
        }));
    }
    let error_class = if syntax.is_some() {
        ErrorClass::Syntax
    } else if matches!(
        status,
        "missing_sdk"
            | "missing_dependency"
            | "unsupported"
            | "no_tests"
            | "incomplete"
            | "timed_out"
            | "cancelled"
            | "requires_reconciliation"
            | "source_changed"
            | "flaky"
            | "unavailable"
            | "BLOCKED"
    ) {
        ErrorClass::Environment
    } else if status == "failed" && (tool.contains("test") || !failed_tests.is_empty()) {
        ErrorClass::Test
    } else if status == "failed" && (tool.contains("lint") || tool.contains("format")) {
        ErrorClass::Lint
    } else if status == "failed" && (tool.contains("check") || !related_diagnostics.is_empty()) {
        ErrorClass::Compiler
    } else if status == "failed" && tool.contains("build") {
        ErrorClass::Build
    } else if tool.starts_with("fs_") || tool.starts_with("code_git") {
        ErrorClass::Repository
    } else {
        ErrorClass::Unknown
    };
    let repairable = matches!(
        error_class,
        ErrorClass::Syntax
            | ErrorClass::Compiler
            | ErrorClass::Build
            | ErrorClass::Test
            | ErrorClass::Lint
    );
    let fingerprint = crate::digest(
        &serde_json::to_vec(&(
            &error_class,
            tool,
            source,
            &failed_tests,
            related_diagnostics
                .iter()
                .map(|d| (&d.affected_file, &d.line_range, &d.code, &d.message))
                .collect::<Vec<_>>(),
        ))
        .expect("bounded diagnostic identity"),
    );
    Diagnostic {
        error_class,
        tool: tool.into(),
        source_digest: source.into(),
        related_diagnostics,
        failed_tests,
        targeted_case_ids: value["targeted_case_ids"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|id| id.as_str().map(|id| bounded(id, 128)))
            .take(128)
            .collect(),
        last_patch: patch,
        evidence_ref: evidence.into(),
        repairable,
        fingerprint,
    }
}
