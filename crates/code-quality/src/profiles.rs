//! Definition-only inputs for the closed review dispatcher. Historical backlog
//! evidence is deliberately not loaded or promoted into current acceptance.
use crate::{Error, Result};
use melissa_core_contracts::{
    catalog::Catalog, catalog_models::AgentProfile, json::parse_contract, validation::Validate,
};
use melissa_repository::Repository;
use std::collections::BTreeMap;

const DEFINITIONS: &[(&str, &str)] = &[
    ("melissa.code.reviewer", "catalog/agents/code/reviewer.json"),
    (
        "melissa.code.security-reviewer",
        "catalog/agents/code/security-reviewer.json",
    ),
    (
        "melissa.core.independent-qa",
        "catalog/agents/core/independent-qa.json",
    ),
];
const MAX_BYTES: usize = 256 * 1024;

/// Immutable, validated role definitions supplied by the host, not by a model.
/// This type neither validates historical evidence nor grants task authority.
pub struct ReviewProfiles {
    profiles: BTreeMap<String, AgentProfile>,
}
impl ReviewProfiles {
    pub fn from_catalog(catalog: &Catalog) -> Result<Self> {
        let mut profiles = BTreeMap::new();
        for (id, _) in DEFINITIONS {
            let profile = catalog
                .profiles
                .get(*id)
                .ok_or(Error::Invalid("missing review profile"))?;
            profile
                .validate()
                .map_err(|_| Error::Invalid("review profile definition"))?;
            if profile.id != *id {
                return Err(Error::Invalid("review profile identity"));
            }
            profiles.insert((*id).into(), profile.clone());
        }
        Ok(Self { profiles })
    }

    pub fn load_from_repository(repository: &Repository) -> Result<Self> {
        let mut profiles = BTreeMap::new();
        let mut versions = Vec::new();
        for (id, path) in DEFINITIONS {
            let (version, bytes) = repository.read(path)?;
            let profile: AgentProfile = parse_contract(&bytes, MAX_BYTES)
                .map_err(|_| Error::Invalid("review profile definition"))?;
            if profile.id != *id {
                return Err(Error::Invalid("review profile identity"));
            }
            versions.push((*path, version));
            profiles.insert((*id).into(), profile);
        }
        // Detect ordinary changes during loading; no atomic-tree claim.
        for (path, version) in versions {
            if repository.read(path)?.0 != version {
                return Err(Error::Stale("review profile changed while loading"));
            }
        }
        Ok(Self { profiles })
    }

    pub fn profile(&self, id: &str) -> Option<&AgentProfile> {
        self.profiles.get(id)
    }
}
