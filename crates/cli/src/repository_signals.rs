//! Ranking hints originate from completed host adapters and remain scoped to
//! one source digest. Only paths and optional one-based lines reach ranking.
use melissa_skill_context::runtime::RepositoryHintKind;
use serde_json::Value;
use std::collections::BTreeMap;

pub(crate) struct Hint {
    pub kind: RepositoryHintKind,
    pub snapshot: String,
    pub paths: BTreeMap<String, usize>,
}

fn add(paths: &mut BTreeMap<String, usize>, path: &str, line: usize) {
    let path = path.strip_prefix("/work/").unwrap_or(path);
    if paths.len() < 256
        && !path.is_empty()
        && path.len() <= 2048
        && !path.contains(['\\', ':'])
        && !path.chars().any(char::is_control)
        && path.split('/').all(|p| !matches!(p, "" | "." | ".."))
    {
        paths.entry(path.into()).or_insert(line);
    }
}

fn diagnostic_paths(value: &Value) -> Option<(&str, BTreeMap<String, usize>)> {
    if value["host_source_current"] != true {
        return None;
    }
    let mut paths = BTreeMap::new();
    match value["schema_version"].as_str()? {
        "melissa.sdk-result/v1" if value["details"]["source_unchanged"] == true => {
            for diagnostic in value["details"]["diagnostics"].as_array()? {
                if let Some(path) = diagnostic["file"].as_str() {
                    add(
                        &mut paths,
                        path,
                        diagnostic["line"].as_u64().unwrap_or(0) as usize,
                    );
                }
            }
            Some((value["source_digest"].as_str()?, paths))
        }
        "melissa.rust-build-test/v1" => {
            for diagnostic in value["diagnostics"].as_array()? {
                for span in diagnostic["spans"].as_array().into_iter().flatten() {
                    if span["primary"] == true
                        && span["source_bound"] == true
                        && let Some(path) = span["file"].as_str()
                    {
                        add(
                            &mut paths,
                            path,
                            span["line"].as_u64().unwrap_or(0) as usize,
                        );
                    }
                }
            }
            Some((value["source_snapshot_digest"].as_str()?, paths))
        }
        _ => None,
    }
}

fn diff_paths(text: &str) -> BTreeMap<String, usize> {
    let mut paths = BTreeMap::new();
    let mut header = false;
    let mut file = None;
    for line in text.lines() {
        if line.starts_with("diff --git ") {
            if let Some(path) = file.take() {
                add(&mut paths, path, 0);
            }
            header = true;
        } else if header && let Some(path) = line.strip_prefix("+++ b/") {
            file = Some(path.strip_suffix('\t').unwrap_or(path));
        } else if header && line.starts_with("@@ ") {
            if let Some(path) = file.take() {
                let first = line
                    .split_whitespace()
                    .nth(2)
                    .and_then(|range| range.strip_prefix('+'))
                    .and_then(|range| range.split(',').next())
                    .and_then(|n| n.parse().ok())
                    .unwrap_or(0);
                add(&mut paths, path, first);
            }
            header = false;
        }
    }
    if let Some(path) = file {
        add(&mut paths, path, 0);
    }
    paths
}

pub(crate) fn observe(tool: &str, content: &str, git_source: Option<&str>) -> Option<Hint> {
    let git = tool == "code_git";
    if content.len() > 4 * 1024 * 1024
        || (!git
            && !matches!(
                tool,
                "code_build"
                    | "code_tests"
                    | "code_project_check"
                    | "code_project_build"
                    | "code_project_tests"
                    | "code_project_lint"
                    | "code_project_e2e"
                    | "code_project_doctests"
            ))
    {
        return None;
    }
    let value = melissa_model_gateway::strict_json(content).ok()?;
    if git {
        if value["source_current"] != true
            || value["success"] != true
            || !value["raw_report_artifact"].is_string()
        {
            return None;
        }
        let snapshot = git_source?.into();
        let text = value["stdout"].as_str()?;
        let (kind, paths) = match value.pointer("/operation/operation/operation")?.as_str()? {
            "diff" => (RepositoryHintKind::Diff, diff_paths(text)),
            "log_paths" => {
                let mut paths = BTreeMap::new();
                for path in text.split_terminator('\0') {
                    add(&mut paths, path.trim_start_matches(['\n', '\r']), 0);
                }
                (RepositoryHintKind::History, paths)
            }
            _ => return None,
        };
        Some(Hint {
            kind,
            snapshot,
            paths,
        })
    } else {
        let (snapshot, paths) = diagnostic_paths(&value)?;
        Some(Hint {
            kind: RepositoryHintKind::Diagnostics,
            snapshot: snapshot.into(),
            paths,
        })
    }
}

pub(crate) fn after_edit(value: &Value) -> Option<Hint> {
    if !matches!(value["status"].as_str(), Some("PASS" | "FAIL")) {
        return None;
    }
    let snapshot = value["source_digest"].as_str()?;
    let mut paths = BTreeMap::new();
    if let Some((digest, observed)) = diagnostic_paths(&value["sdk"])
        && digest == snapshot
    {
        paths = observed;
    }
    for path in value["syntax_errors"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        add(&mut paths, path, 0);
    }
    Some(Hint {
        kind: RepositoryHintKind::Diagnostics,
        snapshot: snapshot.into(),
        paths,
    })
}
