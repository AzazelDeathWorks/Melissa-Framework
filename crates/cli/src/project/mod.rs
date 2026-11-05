//! Project-scoped native execution through the existing authenticated broker.
pub mod discovery;
mod disk;
pub mod runtime;
pub mod task;
#[cfg(all(test, windows))]
pub(crate) mod tests;
pub mod tools;
use crate::{Error, Result};
use melissa_container_executor::ContainerLimits;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Command {
    pub executable: String,
    #[serde(default)]
    pub argv: Vec<String>,
    #[serde(default = "dot")]
    pub cwd: String,
}
fn dot() -> String {
    ".".into()
}
impl Command {
    pub fn validate(&self) -> Result {
        if self.executable.is_empty()
            || self.executable.contains(['\0', '\n', '\r'])
            || self.executable.len() > 4096
            || self.argv.len() > 1024
            || self.argv.iter().any(|a| a.contains('\0'))
            || (self.cwd != "." && !melissa_workflow::plan::path(&self.cwd))
        {
            return Err(Error::Usage(
                "Komut/argüman veya proje içi çalışma klasörü geçersiz.",
            ));
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Environment {
    pub image_id: String,
    pub limits: ContainerLimits,
    /// Exact versions, not mutable 'latest' selectors. Missing tools block.
    #[serde(default)]
    pub versions: BTreeMap<String, String>,
    /// Trusted operator-selected tools inside the immutable image, never host paths.
    #[serde(default)]
    pub executables: BTreeMap<String, String>,
    #[serde(default)]
    pub use_mise: bool,
}
impl Default for Environment {
    fn default() -> Self {
        let mut limits = crate::development::limits();
        limits.wall_time_ms = 3_600_000;
        limits.cpu_time_ms = 3_600_000;
        limits.export_bytes = 0;
        Self {
            image_id: melissa_build_test::toolchain::COMPLETE_IMAGE.into(),
            limits,
            versions: BTreeMap::new(),
            executables: BTreeMap::new(),
            use_mise: false,
        }
    }
}
impl Environment {
    pub fn validate(&self) -> Result {
        if self.limits.export_bytes != 0 {
            return Err(Error::Usage(
                "Proje süreçleri host dosyası aktaramaz; export_bytes sıfır olmalı.",
            ));
        }
        let req = melissa_container_executor::ContainerRequest {
            schema_version: "melissa.container-exec/v1".into(),
            image_id: self.image_id.clone(),
            executable: "/usr/bin/python3".into(),
            argv: vec![],
            cwd: ".".into(),
            env: BTreeMap::new(),
            limits: self.limits.clone(),
        };
        req.validate().map_err(|_| {
            Error::Usage("Ortam değişmez image kimliği ve geçerli kaynak sınırları gerektirir.")
        })?;
        for (tool, version) in &self.versions {
            if !["python", "node", "rust", "npm", "pnpm", "yarn", "uv"].contains(&tool.as_str())
                || version.is_empty()
                || !version
                    .bytes()
                    .all(|b| b.is_ascii_digit() || b".-".contains(&b))
                || !version.as_bytes()[0].is_ascii_digit()
            {
                return Err(Error::Usage(
                    "Araç sürümü açık sayısal sürüm olmalı; latest/etiket/komut kullanılamaz.",
                ));
            }
        }
        for (tool, path) in &self.executables {
            if ![
                "python", "node", "cargo", "rustc", "npm", "pnpm", "yarn", "uv", "mise",
            ]
            .contains(&tool.as_str())
                || !path.starts_with('/')
                || path.contains(['\\', '\0'])
                || path.split('/').any(|p| p == "..")
            {
                return Err(Error::Usage(
                    "Araç eşlemesi image içindeki açık Linux yolu olmalı.",
                ));
            }
        }
        Ok(())
    }
    pub fn fingerprint(&self) -> Result<String> {
        Ok(crate::digest(&crate::app::bytes(self)?))
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Contract {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extensions: Option<crate::extensions::Bundle>,
    pub environment: Environment,
    pub checks: BTreeMap<String, Command>,
    #[serde(default)]
    pub sync: Vec<Command>,
}
impl Contract {
    pub fn public_summary(&self) -> Result<serde_json::Value> {
        let mut value = serde_json::to_value(self).map_err(|_| Error::Store)?;
        if let Some(bundle) = &self.extensions {
            value["extensions"] = bundle.context();
        }
        Ok(value)
    }
    pub fn validate(&self) -> Result {
        self.environment.validate()?;
        if let Some(extensions) = &self.extensions {
            extensions.validate()?;
        }
        if self.checks.is_empty() {
            return Err(Error::Usage(
                "En az bir açık proje kabul komutu gerekli; /check ile ekle.",
            ));
        }
        for (id, c) in &self.checks {
            if id.is_empty()
                || !id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
            {
                return Err(Error::Usage("Kontrol adı geçersiz."));
            }
            c.validate()?;
        }
        for c in &self.sync {
            c.validate()?;
        }
        Ok(())
    }
}
