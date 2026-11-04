//! Source visibility is narrower than a task's general workspace read grant.
//! This host-owned policy does not replace handle/identity checks on access.
use crate::{Error, Result, paths};

#[derive(Debug, Clone)]
pub struct SourceVisibilityPolicy {
    excluded_prefixes: Vec<String>,
}
impl SourceVisibilityPolicy {
    /// Exact host policy, including any defaults chosen by the caller.
    pub fn new(excluded_prefixes: Vec<String>) -> Result<Self> {
        for prefix in &excluded_prefixes {
            paths::relative(prefix)?;
        }
        Ok(Self { excluded_prefixes })
    }
    pub fn excluded_prefixes(&self) -> &[String] {
        &self.excluded_prefixes
    }
    pub fn excludes(&self, path: &str) -> bool {
        path.split('/').any(|s| s.eq_ignore_ascii_case(".git"))
            || self
                .excluded_prefixes
                .iter()
                .any(|p| paths::beneath(path, p))
    }
    pub fn check(&self, path: &str) -> Result {
        paths::relative(path)?;
        if path.split('/').any(|s| s.eq_ignore_ascii_case(".git")) {
            return Err(Error::Denied("Git metadata is outside file capability"));
        }
        if self.excludes(path) {
            return Err(Error::Denied(
                "path is outside repository source visibility",
            ));
        }
        Ok(())
    }
    pub fn allows(&self, path: &str) -> bool {
        self.check(path).is_ok()
    }
}
