use crate::{Error, Result};
use serde_json::Value;
fn bidi(c: char) -> bool {
    matches!(c,'\u{200e}'|'\u{200f}'|'\u{202a}'..='\u{202e}'|'\u{2066}'..='\u{2069}'|'\u{2028}'|'\u{2029}')
}
pub fn json(value: &Value) -> Result<String> {
    let raw = serde_json::to_string_pretty(value).map_err(|_| Error::Store)?;
    Ok(raw
        .chars()
        .map(|c| {
            if bidi(c) {
                format!("\\u{:04x}", c as u32)
            } else {
                c.to_string()
            }
        })
        .collect())
}
pub fn safe(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c.is_control() || bidi(c) {
                format!("\\u{{{:x}}}", c as u32)
            } else {
                c.to_string()
            }
        })
        .collect()
}
pub fn human(v: &Value) -> Result<String> {
    if v["command"] == "task continuity" {
        let mut out = format!(
            "Görev sürekliliği: {}\nDurum: {} · aşama: {}\nKaynak: {}\nİşlemler uzlaştırılmış: {}\n",
            safe(v["task_id"].as_str().unwrap_or("?")),
            safe(v["state"].as_str().unwrap_or("?")),
            safe(v["last_control_phase"].as_str().unwrap_or("unknown")),
            safe(v["source_digest"].as_str().unwrap_or("?")),
            v["settled"]
        );
        if let Some(checkpoints) = v["checkpoints"].as_array() {
            out.push_str(&format!("Checkpoint: {}\n", checkpoints.len()));
            for checkpoint in checkpoints {
                out.push_str(&format!(
                    "  {} · {} · kaynak {}\n",
                    safe(checkpoint["id"].as_str().unwrap_or("?")),
                    safe(
                        checkpoint["label"]
                            .as_str()
                            .or_else(|| checkpoint["phase"].as_str())
                            .unwrap_or("kayıt")
                    ),
                    if checkpoint["source_current"] == true {
                        "güncel"
                    } else {
                        "eski veya bilinmiyor"
                    }
                ));
            }
        }
        if let Some(steps) = v["remaining_milestones"].as_array() {
            out.push_str(&format!("Kalan uzman adımı: {}\n", steps.len()));
            for step in steps {
                out.push_str(&format!(
                    "  {} · {} · {}\n",
                    safe(step["id"].as_str().unwrap_or("?")),
                    safe(step["profile"].as_str().unwrap_or("?")),
                    safe(step["state"].as_str().unwrap_or("?"))
                ));
            }
        }
        out.push_str(&format!(
            "Model: {} etkin / {} belirsiz · kayıtlı maliyet {} mikro USD\n",
            v["model_budget"]["active_requests"],
            v["model_budget"]["unknown_requests"],
            v["model_budget"]["known_cost_micro_usd"]
        ));
        out.push_str(
            "Aynı güvenli kaynak: /resume. Kaynak değiştiyse önce /adopt-preview ile incele.\n",
        );
        return Ok(out);
    }

    if let Some(id) = v["task_id"].as_str() {
        let mut out = format!("Görev: {}\n", safe(id));
        if let Some(state) = v["state"].as_str() {
            out.push_str(&format!("Durum: {}\n", safe(state)));
        }
        if let Some(goal) = v["goal"].as_str() {
            out.push_str(&format!("Amaç: {}\n", safe(goal)));
        }
        if let Some(path) = v["workspace"].as_str() {
            out.push_str(&format!("Proje: {}\n", safe(path)));
        }
        if let Some(phase) = v["phase"]
            .as_str()
            .or_else(|| v["last_control_phase"].as_str())
        {
            out.push_str(&format!("Aşama: {}\n", safe(phase)));
        }
        if let Some(b) = v.get("model_budget") {
            out.push_str(&format!(
                "Model: {} etkin, {} belirsiz istek; bilinen kullanım {} giriş / {} çıkış token.\n",
                b["active_requests"],
                b["unknown_requests"],
                b["known_input_tokens"],
                b["known_output_tokens"]
            ));
            if let Some(cost) = b["known_cost_micro_usd"].as_u64() {
                out.push_str(&format!(
                    "Kayıtlı maliyet üst hesabı: {:.6} USD (fatura değil).\n",
                    cost as f64 / 1_000_000.0
                ));
            }
        }
        if let Some(plan) = v["execution_plan"].as_object() {
            out.push_str(&format!(
                "Onay kapsamı: {}\n",
                json(&serde_json::Value::Object(plan.clone()))?
            ));
        }
        if let Some(slots) = v["workflow"]["slots"].as_object() {
            for (id, slot) in slots {
                out.push_str(&format!("Uzman adımı {}: {}\n", safe(id), slot["phase"]));
            }
        }
        if let Some(workers) = v["workflow"]["host_workers"]
            .as_array()
            .or_else(|| v["native_workers"].as_array())
        {
            for w in workers {
                out.push_str(&format!(
                    "İzole süreç {}: {}\n",
                    safe(w["worker_id"].as_str().unwrap_or("?")),
                    safe(w["observation"].as_str().unwrap_or("?"))
                ));
            }
        }
        if v["approval"]["pending"] == true {
            out.push_str(&format!(
                "İnceleme onayı bekliyor.\nİstek: {}\nÖzet: {}\n",
                safe(v["approval"]["request_id"].as_str().unwrap_or("")),
                safe(v["approval"]["digest"].as_str().unwrap_or(""))
            ));
        }
        if v["execution_backend"] == "not_connected_in_first_CLI_slice"
            || v.get("worker_started") == Some(&Value::Bool(false))
        {
            out.push_str("Bu ilk katman görev hazırlama içindir; model/worker başlatılmadı.\n");
        }
        if v.get("changes").is_some()
            || v.get("artifacts").is_some()
            || v.get("events").is_some()
            || v.get("output").is_some()
            || v.get("previews").is_some()
            || v.get("receipt").is_some()
            || v.get("plan").is_some()
            || v.get("preparation").is_some()
            || v.get("metrics").is_some()
            || v["command"] == "task adopt-source"
        {
            out.push_str(&json(v)?);
        }
        Ok(out)
    } else {
        json(v)
    }
}
