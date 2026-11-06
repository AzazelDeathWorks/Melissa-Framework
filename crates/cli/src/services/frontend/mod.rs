//! Reviewed browser capabilities and source-bound, operator-owned visual baselines.
pub mod baseline;
pub mod preview;
#[cfg(all(test, windows))]
mod tests;
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Viewport {
    pub name: String,
    pub width: u32,
    pub height: u32,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Step {
    Navigate {
        path: String,
    },
    Fill {
        selector: String,
        value: String,
    },
    Click {
        selector: String,
    },
    Press {
        selector: String,
        key: String,
    },
    Wait {
        selector: String,
        state: String,
        timeout_ms: u32,
    },
    AssertText {
        selector: String,
        text: String,
    },
    AssertVisible {
        selector: String,
        visible: bool,
    },
    Checkpoint {
        name: String,
    },
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Visual {
    pub viewport: String,
    pub checkpoint: String,
    /// Immutable host record, accepted by an explicit operator CLI action.
    pub baseline_id: String,
    pub max_diff_ratio: f64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Check {
    pub viewports: Vec<Viewport>,
    pub steps: Vec<Step>,
    pub accessibility: bool,
    #[serde(default)]
    pub visual: Vec<Visual>,
    #[serde(default)]
    pub fail_on_console_error: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Spec {
    pub service: String,
    #[serde(default)]
    pub allowed_services: Vec<String>,
    #[serde(default)]
    pub websocket: bool,
    #[serde(default)]
    pub service_workers: bool,
    #[serde(default)]
    pub devtools: bool,
    #[serde(default)]
    pub checks: BTreeMap<String, Check>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case", deny_unknown_fields)]
pub enum Command {
    Navigate {
        page: String,
        path: String,
    },
    Inspect {
        page: String,
    },
    Elements {
        page: String,
        selector: String,
    },
    Fill {
        page: String,
        selector: String,
        value: String,
    },
    Click {
        page: String,
        selector: String,
    },
    Press {
        page: String,
        selector: String,
        key: String,
    },
    Wait {
        page: String,
        selector: String,
        state: String,
        timeout_ms: u32,
    },
    Viewport {
        page: String,
        width: u32,
        height: u32,
    },
    Screenshot {
        page: String,
    },
    Accessibility {
        page: String,
    },
    Capabilities,
    Verify {
        name: String,
    },
    Devtools {
        page: String,
        mode: String,
    },
}
impl Command {
    pub fn validate(&self, spec: &Spec) -> Result {
        match self {
            Self::Verify { name } if !spec.checks.contains_key(name) => {
                return Err(Error::Denied("Frontend kontrolü kayıtlı değil."));
            }
            Self::Devtools { page, mode } => {
                if !spec.devtools
                    || !super::name(page)
                    || !["diagnostics", "performance"].contains(&mode.as_str())
                {
                    return Err(Error::Denied("DevTools yeteneği/kipi onaylı değil."));
                }
            }
            Self::Accessibility { page } if !super::name(page) => {
                return Err(Error::Usage("Browser sayfası geçersiz."));
            }
            Self::Verify { .. } | Self::Capabilities | Self::Accessibility { .. } => {}
            _ => {
                let value = serde_json::to_value(self).map_err(|_| Error::Store)?;
                let command: melissa_web_development::protocol::Command =
                    serde_json::from_value(value)
                        .map_err(|_| Error::Usage("Browser komutu geçersiz."))?;
                command
                    .validate()
                    .map_err(|_| Error::Usage("Browser kapsamı geçersiz."))?;
            }
        }
        Ok(())
    }
}
impl Spec {
    pub fn validate(&self, services: &super::Spec) -> Result {
        if !services.services.contains_key(&self.service)
            || self
                .allowed_services
                .iter()
                .any(|s| !services.services.contains_key(s))
        {
            return Err(Error::Usage("Browser yalnız kayıtlı servisleri açabilir."));
        }
        if services.services.values().any(|s| s.port == 38299) {
            return Err(Error::Usage("38299 özel browser tanılama portudur."));
        }
        for (name, check) in &self.checks {
            if !super::name(name) || check.viewports.is_empty() || check.steps.is_empty() {
                return Err(Error::Usage("Frontend senaryosu/viewport eksik."));
            }
            let mut views = BTreeSet::new();
            let mut checkpoints = BTreeSet::new();
            for v in &check.viewports {
                if !super::name(&v.name) || !views.insert(v.name.as_str()) {
                    return Err(Error::Usage("Viewport adı tekrarlı/geçersiz."));
                }
                Command::Viewport {
                    page: "check".into(),
                    width: v.width,
                    height: v.height,
                }
                .validate(self)?;
            }
            if !matches!(check.steps.first(), Some(Step::Navigate { .. })) {
                return Err(Error::Usage("Frontend senaryosu navigate ile başlamalı."));
            }
            for step in &check.steps {
                let mut v = serde_json::to_value(step).map_err(|_| Error::Store)?;
                let action = v.as_object_mut().unwrap().remove("action").unwrap();
                match step {
                    Step::Checkpoint { name } => {
                        if !super::name(name) || !checkpoints.insert(name.as_str()) {
                            return Err(Error::Usage("Checkpoint adı tekrarlı/geçersiz."));
                        }
                    }
                    Step::AssertText { selector, text } => {
                        if text.len() > 65536 {
                            return Err(Error::Usage("Beklenen metin kapasitesi."));
                        }
                        Command::Elements {
                            page: "check".into(),
                            selector: selector.clone(),
                        }
                        .validate(self)?;
                    }
                    Step::AssertVisible { selector, .. } => {
                        Command::Elements {
                            page: "check".into(),
                            selector: selector.clone(),
                        }
                        .validate(self)?;
                    }
                    _ => {
                        v["command"] = action;
                        v["page"] = serde_json::json!("check");
                        let c: Command = serde_json::from_value(v)
                            .map_err(|_| Error::Usage("Frontend adımı geçersiz."))?;
                        c.validate(self)?;
                    }
                }
            }
            if checkpoints.is_empty() {
                return Err(Error::Usage("En az bir gözlenen checkpoint gerekli."));
            }
            let mut pairs = BTreeSet::new();
            for v in &check.visual {
                if !views.contains(v.viewport.as_str())
                    || !checkpoints.contains(v.checkpoint.as_str())
                    || !pairs.insert((&v.viewport, &v.checkpoint))
                    || !baseline::valid_id(&v.baseline_id)
                    || !v.max_diff_ratio.is_finite()
                    || !(0.0..=1.0).contains(&v.max_diff_ratio)
                {
                    return Err(Error::Usage("Görsel referans/checkpoint/eşik geçersiz."));
                }
            }
        }
        Ok(())
    }
}
