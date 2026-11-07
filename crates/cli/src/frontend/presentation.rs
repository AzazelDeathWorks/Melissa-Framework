//! Human terminal summaries. Structured automation output remains unchanged.
use serde_json::Value;
pub(super) fn summary(kind: &str, value: &Value) -> String {
    if kind == "details" {
        return serde_json::to_string_pretty(value).unwrap_or_else(|_| "Kayıt okunamadı.".into());
    }
    if let Some(text) = value["text"].as_str() {
        return text.into();
    }
    let plan = if kind == "plan" {
        &value["plan"]
    } else if value["brief"]["plan"].is_object() {
        &value["brief"]["plan"]
    } else {
        value
    };
    if let Some(goal) = plan["task_goal"].as_str() {
        let mut lines = vec!["GÖREV PLANI".into(), goal.into(), String::new()];
        if let Some(steps) = plan["milestones"].as_array() {
            for (i, step) in steps.iter().enumerate() {
                let profile = step["profile"]
                    .as_str()
                    .unwrap_or("uzman")
                    .strip_prefix("melissa.code.")
                    .unwrap_or("uzman")
                    .replace('-', " ");
                lines.push(format!("{}. {profile}", i + 1));
            }
        }
        if let Some(paths) = plan["scope"]["proposed_writes"].as_array() {
            lines.push(format!("Yazma kapsamı · {} dosya", paths.len()));
            for path in paths {
                if let Some(path) = path.as_str() {
                    lines.push(format!("  {path}"));
                }
            }
        }
        if let Some(digest) = plan["source_digest"].as_str() {
            lines.push(format!("Kaynak · {digest}"));
        }
        lines.push("Tam kapsam: /details · Görev oluştur: /create".into());
        return lines.join("\n");
    }
    if let Some(id) = value["task_id"].as_str() {
        let mut lines = vec![format!("GÖREV · {id}")];
        for key in ["state", "goal", "phase"] {
            if let Some(s) = value[key].as_str() {
                lines.push(format!("{} · {s}", key.replace('_', " ")));
            }
        }
        if value["approval"]["pending"] == true {
            lines.push("Kapsam onayı bekleniyor. /details ile incele, /approve ile onayla.".into());
            if let Some(d) = value["approval"]["digest"].as_str() {
                lines.push(format!("Onay özeti · {d}"));
            }
        }
        if let Some(changes) = value["changes"].as_array() {
            lines.push(format!("Değişiklik · {} dosya", changes.len()));
            for c in changes {
                lines.push(format!(
                    "  {}  {}",
                    c["kind"].as_str().unwrap_or(""),
                    c["path"].as_str().unwrap_or("")
                ));
            }
        }
        if value["task_accepted"] == true {
            lines.push("Görev kabul kontrolleri geçti.".into());
        }
        if let Some(cost) = value["model_budget"]["known_cost_micro_usd"].as_u64() {
            lines.push(format!(
                "Kayıtlı maliyet · {:.6} USD",
                cost as f64 / 1_000_000.0
            ));
        }
        lines.push("Durum /status · Günlük /logs · Kod farkı /diff".into());
        return lines.join("\n");
    }
    let mut lines = vec![];
    tree(value, 0, &mut lines);
    lines.join("\n")
}
fn tree(value: &Value, depth: usize, lines: &mut Vec<String>) {
    let pad = "  ".repeat(depth);
    match value {
        Value::Object(values) => {
            for (key, value) in values {
                if !value.is_null() {
                    lines.push(format!("{pad}{}", key.replace('_', " ")));
                    tree(value, depth + 1, lines);
                }
            }
        }
        Value::Array(values) if values.iter().all(|v| v.is_number()) => {
            lines.push(format!("{pad}{} bayt/sayısal değer", values.len()))
        }
        Value::Array(values) => {
            for value in values {
                tree(value, depth, lines);
            }
        }
        Value::String(s) => lines.extend(s.lines().map(|line| format!("{pad}{line}"))),
        value => lines.push(format!("{pad}{value}")),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn overview_is_readable_and_explicit_details_preserve_the_full_review() {
        let plan = serde_json::json!({"task_goal":"Türkçe inceleme","scope":{"proposed_writes":["main.py"]},"source_digest":"blake3:source","milestones":[{"profile":"melissa.code.reviewer"}],"full_review_marker":"retained"});
        let shown = summary("plan", &serde_json::json!({"plan":plan}));
        assert!(shown.contains("Türkçe inceleme"));
        assert!(shown.contains("main.py"));
        assert!(!shown.contains("full_review_marker"));
        let full = summary("details", &plan);
        assert_eq!(serde_json::from_str::<Value>(&full).unwrap(), plan);
    }
}
