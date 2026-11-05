//! Host verification policy over the approved task and source scope.
use crate::recipe::Recipe;
use serde_json::{Value, json};

pub(crate) struct Policy {
    pub require_full_public_suite: bool,
    pub require_static_security: bool,
    pub risk: &'static str,
}
impl Policy {
    pub fn for_recipe(recipe: &Recipe) -> Self {
        let specialist = |name: &str| recipe.agents.iter().any(|s| s.profile == name);
        let security = specialist("melissa.code.security-reviewer");
        let broad = security
            || specialist("melissa.code.architect")
            || recipe.domain.is_some()
            || recipe.writable_paths.len() > 3
            || recipe.writable_paths.iter().any(|path| {
                let name = path.rsplit('/').next().unwrap_or(path);
                matches!(
                    name,
                    "Cargo.lock"
                        | "Cargo.toml"
                        | "package.json"
                        | "package-lock.json"
                        | "pyproject.toml"
                        | "requirements.txt"
                ) || path.split('/').any(|part| part == "migrations")
            });
        Self {
            require_full_public_suite: broad && !recipe.development_checks.is_empty(),
            require_static_security: security,
            risk: if security {
                "high"
            } else if broad {
                "elevated"
            } else {
                "routine"
            },
        }
    }
    pub fn plan(&self, recipe: &Recipe) -> Value {
        json!({
            "risk":self.risk,
            "syntax_type":"source-bound checks after edits and approved SDK checks",
            "targeted_unit":if recipe.development_checks.is_empty() {"not_configured"} else {"approved public case selection"},
            "integration":if recipe.domain.is_some() {"protected domain scenarios"} else {"available SDK and public development procedures"},
            "broader_regression":if self.require_full_public_suite {"full approved public suite required"} else {"as needed within task grants"},
            "protected_final_qa":"required",
            "static_security":if self.require_static_security {"required"} else {"not_required_by_recipe"},
            "release_security_license":"separate acceptance evidence required"
        })
    }
}
