use crate::{Error, Result};
use serde_json::Value;
use std::collections::BTreeSet;
pub fn value(value: &str) -> String {
    use base64::Engine;
    if value.trim() != value
        || !value
            .bytes()
            .all(|c| (0x20..=0x7e).contains(&c) || c == b'\t')
        || (value.starts_with("=?base64?") && value.ends_with("?="))
    {
        format!(
            "=?base64?{}?=",
            base64::engine::general_purpose::STANDARD.encode(value.as_bytes())
        )
    } else {
        value.into()
    }
}
pub fn parameters(schema: &Value, args: &Value) -> Result<Vec<(String, String)>> {
    fn visit(
        node: &Value,
        args: Option<&Value>,
        reachable: bool,
        out: &mut Vec<(String, String)>,
        seen: &mut BTreeSet<String>,
    ) -> Result {
        if let Some(header) = node.get("x-mcp-header") {
            let name = header
                .as_str()
                .ok_or(Error::Usage("MCP header annotation metin olmalı."))?;
            if !reachable
                || name.is_empty()
                || !name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
                || !seen.insert(name.to_ascii_lowercase())
                || !matches!(
                    node["type"].as_str(),
                    Some("string" | "integer" | "boolean")
                )
            {
                return Err(Error::Usage("MCP header annotation geçersiz."));
            }
            if let Some(v) = args.filter(|v| !v.is_null()) {
                if node["type"] == "integer"
                    && v.as_i64()
                        .is_none_or(|n| n.unsigned_abs() > 9_007_199_254_740_991)
                {
                    return Err(Error::Usage("MCP integer header güvenli aralık dışında."));
                }
                let value = if let Some(v) = v.as_str() {
                    v.into()
                } else {
                    v.to_string()
                };
                out.push((format!("Mcp-Param-{name}"), super::headers::value(&value)));
            }
        }
        if let Some(properties) = node.get("properties").and_then(Value::as_object) {
            for (key, child) in properties {
                visit(child, args.and_then(|v| v.get(key)), reachable, out, seen)?;
            }
        }
        if let Some(object) = node.as_object() {
            for (key, value) in object {
                if key == "properties" || key == "enum" || key == "const" || key == "examples" {
                    continue;
                }
                match value {
                    Value::Object(_) => visit(value, None, false, out, seen)?,
                    Value::Array(v) => {
                        for child in v {
                            visit(child, None, false, out, seen)?
                        }
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    }
    if schema.get("x-mcp-header").is_some() {
        return Err(Error::Usage("MCP kök şeması header olamaz."));
    }
    let mut out = vec![];
    visit(schema, Some(args), true, &mut out, &mut BTreeSet::new())?;
    Ok(out)
}
