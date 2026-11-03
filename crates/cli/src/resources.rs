//! Bundled definitions and complete immutable skill material. Historical
//! reports, internal backlog, provider settings and credentials are excluded.
use crate::{Error, Result, app::decode};
use melissa_core_contracts::{
    catalog::Catalog, catalog_models::*, design::SkillPin, validation::Validate,
};
use melissa_skill_context::package::{Package, Registry, SkillPolicy};
use std::collections::BTreeSet;
include!(concat!(env!("OUT_DIR"), "/resources.rs"));
pub fn file(path: &str) -> Result<&'static [u8]> {
    FILES
        .iter()
        .find(|(p, _)| *p == path)
        .map(|(_, b)| *b)
        .ok_or(Error::Store)
}
pub fn catalog() -> Result<Catalog> {
    let mut profiles = std::collections::BTreeMap::new();
    let mut workers = std::collections::BTreeMap::new();
    for (path, bytes) in FILES {
        if path.starts_with("catalog/agents/") {
            let p: AgentProfile = decode(bytes)?;
            p.validate().map_err(|_| Error::Store)?;
            if profiles.insert(p.id.clone(), p).is_some() {
                return Err(Error::Store);
            }
        } else if path.starts_with("catalog/workers/") {
            let w: WorkerType = decode(bytes)?;
            w.validate().map_err(|_| Error::Store)?;
            if workers.insert(w.id.clone(), w).is_some() {
                return Err(Error::Store);
            }
        }
    }
    let index: AgentIndex = decode(file("catalog/agent-index.json")?)?;
    let mut tools: ToolCatalog = decode(file("catalog/tools.json")?)?;
    let aliases: ToolAliases = decode(file("catalog/tool-aliases.json")?)?;
    let modules: ModuleCatalog = decode(file("catalog/modules.json")?)?;
    let policy: RuntimePolicy = decode(file("catalog/runtime-policy.json")?)?;
    index.validate().map_err(|_| Error::Store)?;
    tools.validate().map_err(|_| Error::Store)?;
    policy.validate().map_err(|_| Error::Store)?;
    if index.profiles.len() != profiles.len()
        || index.profiles.iter().any(|p| !profiles.contains_key(&p.id))
    {
        return Err(Error::Store);
    }
    for p in profiles.values() {
        if p.tool_ids
            .iter()
            .any(|id| !tools.tools.iter().any(|t| &t.id == id))
            || p.worker_types.iter().any(|id| !workers.contains_key(id))
        {
            return Err(Error::Store);
        }
    }
    // The product view has no planning backlog. Extend the finite runtime
    // vocabulary here without invoking the full historical catalog validator.
    let prototype = tools
        .tools
        .iter()
        .find(|t| t.id == "fs.read")
        .ok_or(Error::Store)?
        .clone();
    for id in ["memory.search", "memory.propose"] {
        let mut tool = prototype.clone();
        tool.id = id.into();
        tool.family = "memory".into();
        tool.worker_candidates.clear();
        tool.referenced_by = profiles
            .values()
            .filter(|p| p.module == melissa_core_contracts::Module::Code)
            .map(|p| p.id.clone())
            .collect();
        for p in profiles
            .values_mut()
            .filter(|p| p.module == melissa_core_contracts::Module::Code)
        {
            p.tool_ids.push(id.into());
            if id == "memory.propose" {
                p.authority.capability_ceiling.push(id.into());
            }
        }
        tools.tools.push(tool);
    }
    for p in profiles
        .values_mut()
        .filter(|p| p.module == melissa_core_contracts::Module::Code)
    {
        for id in ["code.diagnostics", "code.references", "code.symbols"] {
            if !p.tool_ids.iter().any(|t| t == id) {
                p.tool_ids.push(id.into());
            }
        }
        if p.authority
            .capability_ceiling
            .iter()
            .any(|e| e == "git.read")
        {
            for id in ["git.status", "git.diff", "git.log"] {
                if !p.tool_ids.iter().any(|t| t == id) {
                    p.tool_ids.push(id.into());
                }
            }
        }
    }
    for t in &mut tools.tools {
        t.referenced_by = profiles
            .values()
            .filter(|p| p.tool_ids.contains(&t.id))
            .map(|p| p.id.clone())
            .collect();
    }
    tools.validate().map_err(|_| Error::Store)?;
    Ok(Catalog {
        profiles,
        workers,
        tools,
        aliases,
        modules,
        policy,
        index,
        backlog: BacklogCatalog {
            schema_version: "melissa.backlog/v1".into(),
            items: vec![],
        },
    })
}
pub fn skills(
    profile: &str,
    mut tools: BTreeSet<String>,
    mut capabilities: BTreeSet<String>,
) -> Result<(Vec<SkillPin>, Registry, SkillPolicy)> {
    // CODE07 skill vocabulary alias, never an extra broker grant. The native
    // TaskContract and PatchBroker still require workspace.apply_patch_scoped.
    if capabilities.contains("workspace.apply_patch_scoped") {
        capabilities.insert("workspace.write_scoped".into());
    }
    // The protected host/test step supplies these module prerequisites.
    // This never exposes them to a read-only role's NodeDispatcher.
    tools.extend(["code.build".into(), "code.tests".into()]);
    let all: Vec<SkillPin> = decode(file("skills/code/code07-current.json")?)?;
    let slug = melissa_skill_context::assembly::owner_skill(profile).ok_or(Error::Store)?;
    let id = format!("melissa.code.skill.{slug}");
    let pin = all
        .into_iter()
        .find(|p| p.skill_id == id)
        .ok_or(Error::Store)?;
    let base = format!("skills/code/{slug}/{}", pin.semantic_version);
    let d = file(&format!("{base}/definition.json"))?;
    let v = file(&format!("{base}/version.json"))?;
    let definition: melissa_core_contracts::design::SkillDefinition = decode(d)?;
    let files = definition
        .files
        .iter()
        .map(|f| {
            Ok((
                f.path.clone(),
                file(&format!("{base}/{}", f.path))?.to_vec(),
            ))
        })
        .collect::<Result<_>>()?;
    let package = Package::load(d, v, files, &pin).map_err(|_| Error::Store)?;
    let policy = SkillPolicy {
        epoch: "cli-bundled-v1".into(),
        reviewed: BTreeSet::from([pin.content_digest.clone()]),
        publishers: BTreeSet::from([package.version().publisher_id.clone()]),
        revoked: BTreeSet::new(),
        tools,
        capabilities,
        allow_helpers: true,
    };
    let mut registry = Registry::default();
    registry.insert(package).map_err(|_| Error::Store)?;
    Ok((vec![pin], registry, policy))
}
