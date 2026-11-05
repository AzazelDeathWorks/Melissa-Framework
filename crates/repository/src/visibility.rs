//! Source visibility is narrower than a task's general workspace read grant.
//! This host-owned policy does not replace handle/identity checks on access.
use crate::{Error, Result, paths};

#[derive(Debug, Clone)]
pub struct SourceVisibilityPolicy {
    excluded_prefixes: Vec<String>,
    sensitive_defaults: bool,
    sensitive_exceptions: Vec<String>,
}

impl SourceVisibilityPolicy {
    /// Exact host policy, including any defaults chosen by the caller.
    pub fn new(excluded_prefixes: Vec<String>) -> Result<Self> {
        for prefix in &excluded_prefixes {
            paths::relative(prefix)?;
        }
        Ok(Self {
            excluded_prefixes,
            sensitive_defaults: false,
            sensitive_exceptions: vec![],
        })
    }
    /// Ordinary model-visible source. Exceptions are exact trusted-host paths,
    /// never globs, directories, model arguments or overrides of explicit excludes.
    pub fn source(
        excluded_prefixes: Vec<String>,
        sensitive_exceptions: Vec<String>,
    ) -> Result<Self> {
        let mut policy = Self::new(excluded_prefixes)?;
        for path in &sensitive_exceptions {
            paths::relative(path)?;
        }
        policy.sensitive_defaults = true;
        policy.sensitive_exceptions = sensitive_exceptions;
        Ok(policy)
    }
    pub fn policy_digest_material(&self) -> (&'static str, bool, &[String], &[String]) {
        (
            "melissa.source-visibility/v2",
            self.sensitive_defaults,
            &self.excluded_prefixes,
            &self.sensitive_exceptions,
        )
    }
    pub fn protects_sensitive_paths(&self) -> bool {
        self.sensitive_defaults
    }
    pub fn sensitive_path(path: &str) -> bool {
        path.split('/').any(|component| {
            let name = component.to_ascii_lowercase();
            matches!(
                name.as_str(),
                ".secrets"
                    | ".private"
                    | ".ssh"
                    | ".aws"
                    | ".azure"
                    | ".gnupg"
                    | ".npmrc"
                    | ".pypirc"
                    | ".netrc"
                    | "_netrc"
                    | "credentials.json"
                    | "id_rsa"
                    | "id_dsa"
                    | "id_ecdsa"
                    | "id_ed25519"
            ) || ((name == ".env" || name.starts_with(".env."))
                && !matches!(
                    name.as_str(),
                    ".env.example" | ".env.sample" | ".env.template" | ".env.dist"
                ))
                || [
                    ".pem",
                    ".key",
                    ".p12",
                    ".pfx",
                    ".jks",
                    ".keystore",
                    ".dpapi",
                ]
                .iter()
                .any(|extension| name.ends_with(extension))
        })
    }
    pub fn excluded_prefixes(&self) -> &[String] {
        &self.excluded_prefixes
    }
    pub fn excludes(&self, path: &str) -> bool {
        path.split('/').any(|s| s.eq_ignore_ascii_case(".git"))
            || (self.sensitive_defaults
                && Self::sensitive_path(path)
                && !self
                    .sensitive_exceptions
                    .iter()
                    .any(|p| paths::key(p) == paths::key(path)))
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
    /// Traverse a sensitive parent only to reach an exact host-approved file.
    /// This does not grant direct reading/listing of the parent or its siblings.
    pub(crate) fn permits_traversal(&self, path: &str) -> bool {
        self.allows(path)
            || (paths::relative(path).is_ok()
                && !path
                    .split('/')
                    .any(|part| part.eq_ignore_ascii_case(".git"))
                && !self
                    .excluded_prefixes
                    .iter()
                    .any(|prefix| paths::beneath(path, prefix))
                && self
                    .sensitive_exceptions
                    .iter()
                    .any(|exception| paths::beneath(exception, path)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sensitive_policy_is_component_wide_case_insensitive_and_template_aware() {
        let policy =
            SourceVisibilityPolicy::source(vec!["excluded".into()], vec!["approved.pem".into()])
                .unwrap();
        for path in [
            ".env",
            "nested/.ENV.production",
            ".ssh/id_ed25519",
            "keys/client.PEM",
            "nested/.aws/config",
            ".secrets/key.txt",
        ] {
            assert!(!policy.allows(path), "{path}");
        }
        for path in [
            ".env.example",
            "nested/.env.template",
            "approved.pem",
            "src/main.rs",
        ] {
            assert!(policy.allows(path), "{path}");
        }
        assert!(!policy.allows("nested/approved.pem"));
        assert!(!policy.allows("excluded/source.rs"));
        assert!(!policy.allows(".git/config"));
    }
}
