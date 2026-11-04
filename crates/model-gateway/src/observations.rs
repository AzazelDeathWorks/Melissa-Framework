//! Deterministic, loss-aware views of known developer observations. These are
//! navigation aids, never acceptance evidence. Unknown formats remain verbatim.
use serde_json::{Value, json};

const MAX_ITEMS: usize = 16;
const MAX_EXCERPT: usize = 2048;

fn excerpt(text: &str, limit: usize) -> &str {
    let mut end = text.len().min(limit);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

fn bounded(value: &Value) -> Value {
    match value {
        Value::String(s) => json!(excerpt(s, MAX_EXCERPT)),
        Value::Array(a) => Value::Array(a.iter().take(MAX_ITEMS).map(bounded).collect()),
        Value::Object(o) => Value::Object(
            o.iter()
                .take(64)
                .map(|(k, v)| (k.clone(), bounded(v)))
                .collect(),
        ),
        v => v.clone(),
    }
}

fn selected(value: &Value, keys: &[&str]) -> Value {
    let mut out = serde_json::Map::new();
    for key in keys {
        if let Some(v) = value.get(key) {
            out.insert((*key).into(), bounded(v));
        }
    }
    Value::Object(out)
}

/// Only the actual dispatcher observation is inspected. A matching word in a
/// source file or arbitrary console log does not establish a successful test.
pub fn normalize(tool: &str, raw: &str) -> Option<Value> {
    if !matches!(
        tool,
        "fs_search"
            | "code_git"
            | "code_build"
            | "code_tests"
            | "code_check"
            | "code_rust_test"
            | "code_rust_check"
            | "code_project_check"
            | "code_project_test"
            | "code_project_tests"
            | "code_project_build"
            | "code_project_doctests"
            | "code_project_e2e"
            | "code_project_lint"
    ) {
        return None;
    }
    let value = crate::strict_json(raw).ok()?;
    let schema = value["schema_version"].as_str().unwrap_or("");
    let mut view = if schema == "melissa.repository-tool-result/v1" && tool == "fs_search" {
        let result = value.get("result")?;
        let hits = result["hits"].as_array()?;
        json!({"kind":"repository_search","matches":hits.iter().take(MAX_ITEMS).map(|h|json!({
            "file":h["path"],"line_range":[h["line"],h.get("end_line").unwrap_or(&h["line"])],"byte_offset":h["byte_offset"],
            "content_digest":h["digest"],"match_excerpt":h["match_excerpt"],
            "excerpt_start_byte":h["excerpt_start_byte"],"excerpt_end_byte":h["excerpt_end_byte"],
            "excerpt_status":if h["match_excerpt"].is_string(){"bounded source bytes; exact digest and byte range"}else{"not supplied or non-UTF8; use scoped source read"}
        })).collect::<Vec<_>>(),"match_count":hits.len(),"source_truncated":result["truncated"],
            "snapshot_digest":result["snapshot_digest"],"source_visibility_digest":value["source_visibility_digest"]})
    } else if schema == "melissa.sdk-result/v1"
        && value.get("details").is_some()
        && value.get("raw_worker_artifact_id").is_some()
        && matches!(
            tool,
            "code_project_check"
                | "code_project_test"
                | "code_project_tests"
                | "code_project_doctests"
                | "code_project_e2e"
                | "code_project_lint"
                | "code_project_build"
                | "code_build"
                | "code_tests"
        )
    {
        let details = &value["details"];
        let runs = details["runs"].as_array();
        let failed: Vec<_> = runs
            .into_iter()
            .flatten()
            .filter(|r| r["status"] == "failed")
            .collect();
        json!({"kind":"build_test","status":value["status"],
            "exit_status":details["steps"].as_array().map(|s|s.iter().take(MAX_ITEMS).map(|s|selected(s,&["stage","exit_code"])).collect::<Vec<_>>()),
            "passed_count":runs.map(|r|r.iter().filter(|r|r["status"]=="passed").count()),
            "skipped_count":runs.map(|r|r.iter().filter(|r|r["status"]=="skipped").count()),
            "counts_scope":"individual reported runs, including repetitions; not unique test identities",
            "failed_count":runs.map(|_|failed.len()),"failed_tests":failed.iter().take(MAX_ITEMS).map(|r|bounded(r)).collect::<Vec<_>>(),
            "diagnostics":diagnostics(&details["diagnostics"]),"discovered_count":details["discovered"].as_array().map(Vec::len),
            "source_digest":value["source_digest"],"host_source_current":value["host_source_current"],
            "source_truncated":details["truncated"],"worker_error":bounded(&details["error"]),
            "protocol_error":bounded(&value["protocol_error"]),"warnings":bounded(&value["warnings"]),
            "raw_artifact_ids":bounded(&value["raw_artifact_ids"])})
    } else if schema == "melissa.rust-build-test/v1"
        && value.get("raw_logs").is_some()
        && value.get("test_runs").is_some()
        && matches!(
            tool,
            "code_build" | "code_tests" | "code_check" | "code_rust_test" | "code_rust_check"
        )
    {
        let failures: Vec<_> = value["test_runs"]
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|r| r["tests"].as_object().into_iter().flatten())
            .filter(|(_, s)| s.as_str() == Some("failed"))
            .map(|(name, status)| json!({"name":name,"status":status}))
            .collect();
        json!({"kind":"cargo_build_test","outcome":value["outcome"],
            "exit_status":value["raw_logs"].as_array().map(|logs|logs.iter().take(MAX_ITEMS).map(|l|selected(l,&["stage","exit_code"])).collect::<Vec<_>>()),
            "passed_count":value["counts"]["passed"],"counts":value["counts"],
            "failed_tests":failures.into_iter().take(MAX_ITEMS).collect::<Vec<_>>(),
            "diagnostics":diagnostics(&value["diagnostics"]),"first_error":bounded(&value["first_error"]),
            "source_snapshot_digest":value["source_snapshot_digest"],"host_source_current":value["host_source_current"],
            "source_truncated":value["truncated"],"protocol_error":bounded(&value["protocol_error"]),"warnings":bounded(&value["warnings"])})
    } else if tool == "code_git"
        && value
            .pointer("/operation/operation/operation")
            .and_then(Value::as_str)
            .or_else(|| {
                value
                    .pointer("/operation/operation")
                    .and_then(Value::as_str)
            })
            == Some("diff")
    {
        let stdout = value["stdout"].as_str()?;
        let mut changed_files = Vec::new();
        let mut hunks: Vec<Value> = Vec::new();
        let mut old_path = "";
        let mut current_file = "";
        let mut current_hunk: Option<usize> = None;
        for line in stdout.lines() {
            if current_hunk.is_none()
                && let Some(file) = line.strip_prefix("--- ")
            {
                old_path = file;
            }
            if let Some(header) = line.strip_prefix("diff --git ") {
                if changed_files.len() < MAX_ITEMS {
                    changed_files.push(json!({"display_header":excerpt(header,512)}));
                }
                current_file = "";
                current_hunk = None;
            } else if current_hunk.is_none()
                && let Some(file) = line.strip_prefix("+++ ")
            {
                current_file = if file == "/dev/null" { old_path } else { file };
                if let Some(changed) = changed_files.last_mut() {
                    changed["old_display_path"] = json!(excerpt(old_path, 512));
                    changed["new_display_path"] = json!(excerpt(file, 512));
                }
            } else if line.starts_with("@@") {
                current_hunk = None;
                if hunks.len() < MAX_ITEMS {
                    current_hunk = Some(hunks.len());
                    hunks.push(json!({"file":excerpt(current_file,512),"header":excerpt(line,512),"excerpt":excerpt(line,512),"partial":true}));
                }
            } else if let Some(index) = current_hunk
                && (line.starts_with(' ') || line.starts_with('+') || line.starts_with('-'))
            {
                let hunk = &mut hunks[index];
                let old = hunk["excerpt"].as_str().unwrap_or("");
                if old.len() < 512 {
                    hunk["excerpt"] = json!(format!("{old}\n{}", excerpt(line, 512 - old.len())));
                }
            }
        }
        json!({"kind":"git_diff","changed_files":changed_files,"current_hunks":hunks,
            "path_encoding":"Git display names; quoted paths are not unescaped","hunk_excerpt_line_endings":"normalized LF; raw bytes available through diff_ref",
            "diff_excerpt":excerpt(stdout,4096),"source_digest":value["source_digest"],
            "source_current":value["source_current"],"success":value["success"],"stderr":bounded(&value["stderr"]),
            "raw_report_artifact":value["raw_report_artifact"]})
    } else {
        return None;
    };
    view["view_is_partial"] = json!(true);
    view["authority"] = json!("untrusted_data");
    view["task_accepted"] = json!(false);
    view["protected_acceptance"] = json!(false);
    view["notice"] = json!(
        "Bounded navigation view. Read full_log_ref/diff_ref before relying on omitted details. Counts and statuses are observations, not independent acceptance."
    );
    // Shrink lists, never statuses, totals or the byte ranges of an excerpt.
    // Every view is explicitly partial and paired with the verbatim artifact.
    while serde_json::to_vec(&view).ok()?.len() > 12 * 1024 {
        let mut reduced = false;
        for key in [
            "matches",
            "failed_tests",
            "diagnostics",
            "exit_status",
            "raw_artifact_ids",
            "changed_files",
            "current_hunks",
            "warnings",
        ] {
            if let Some(items) = view[key].as_array_mut()
                && items.len() > 1
            {
                items.truncate(items.len().div_ceil(2));
                reduced = true;
            }
        }
        if !reduced {
            return None;
        }
    }
    Some(view)
}

fn diagnostics(value: &Value) -> Value {
    json!(
        value
            .as_array()
            .into_iter()
            .flatten()
            .take(MAX_ITEMS)
            .map(|d| {
                let primary=d["spans"].as_array().and_then(|spans|spans.iter().find(|s|s["primary"]==true).or_else(||spans.first()));
                json!({"diagnostic_code":d["code"],"affected_file":d.get("file").or_else(||primary.and_then(|s|s.get("file"))),
            "line_range":[d.get("line").or_else(||primary.and_then(|s|s.get("line"))),primary.and_then(|s|s.get("end_line")).or_else(||d.get("line"))],"spans":bounded(&d["spans"]),
            "level":d["level"],"message":bounded(&d["message"]),"related_symbols":null,
            "related_symbols_status":"not supplied by diagnostic source"})
            })
            .collect::<Vec<_>>()
    )
}
