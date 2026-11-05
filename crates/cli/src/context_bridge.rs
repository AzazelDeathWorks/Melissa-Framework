//! Native context services with an exclusive project-memory transaction.
use crate::{
    Error, Result,
    project_memory::{Location, ProjectMemory, namespace},
};
use melissa_model_gateway::{
    ErrorKind, GatewayError, ToolCall,
    tools::{BoundTool, ToolContext, ToolDispatcher, ToolExecution},
};
use melissa_repository::Repository;
use melissa_skill_context::runtime::ContextDispatcher;
use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, atomic::AtomicBool},
};
pub struct ContextBridge {
    pub inner: ContextDispatcher,
    pub repo: Arc<Repository>,
    pub location: Location,
    project: Option<ProjectMemory>,
}
impl ToolDispatcher for ContextBridge {
    fn definitions(&self) -> Vec<BoundTool> {
        self.inner.definitions()
    }
    fn validate_call(&self, c: &ToolCall) -> melissa_model_gateway::Result<()> {
        self.inner.validate_call(c)
    }
    fn execute<'a>(
        &'a mut self,
        c: ToolCall,
        ctx: ToolContext,
        cancel: Arc<AtomicBool>,
    ) -> Pin<Box<dyn Future<Output = melissa_model_gateway::Result<ToolExecution>> + Send + 'a>>
    {
        Box::pin(async move {
            self.validate_call(&c)?;
            let memory = matches!(c.name.as_str(), "memory_search" | "memory_propose");
            if memory && self.project.is_none() {
                let mut project =
                    ProjectMemory::open(&self.location.home, self.location.key.clone(), &self.repo)
                        .map_err(map)?;
                self.inner
                    .host()
                    .lock()
                    .map_err(|_| map(Error::Store))?
                    .use_external_project_memory(std::mem::take(&mut project.book))
                    .map_err(|_| map(Error::Store))?;
                // Keep the exclusive project lease through this agent session.
                // Host review runs after it ends, preventing lost concurrent edits.
                self.project = Some(project);
            }
            let result = self.inner.execute(c, ctx, cancel).await;
            if !memory {
                return result;
            }
            let project = self.project.as_mut().ok_or_else(|| map(Error::Store))?;
            let host = self.inner.host();
            let mut host = host.lock().map_err(|_| map(Error::Store))?;
            if project
                .save_book(host.memory_book_mut(), &namespace(&self.repo))
                .is_err()
            {
                return Ok(ToolExecution {
                    content: "Project memory write needs reconciliation; do not repeat blindly."
                        .into(),
                    success: false,
                    cancelled: false,
                    requires_reconciliation: true,
                    artifact_refs: vec![],
                });
            }
            result
        })
    }
}
fn map(e: Error) -> GatewayError {
    GatewayError::new(ErrorKind::Store, e.to_string())
}

pub fn runtime(
    app: &crate::app::App,
    store: &melissa_local_store::LocalStore,
    t: &melissa_core_contracts::wire::TaskContractV1,
    profile: &str,
    expires: u64,
    enabled: &std::collections::BTreeSet<String>,
    persist_repository_index: bool,
) -> Result<ContextBridge> {
    let catalog = crate::resources::catalog()?;
    let repo = Arc::new(app.source(&app.workspace(store, &t.task_id)?)?);
    let p = catalog.profiles.get(profile).ok_or(Error::Store)?;
    let (pins, registry, policy) = crate::resources::skills(
        profile,
        p.tool_ids.iter().cloned().collect(),
        t.allowed_effects.iter().cloned().collect(),
    )?;
    let mut names = std::collections::BTreeSet::from([
        "skill_load".into(),
        "context_template".into(),
        "context_factory".into(),
        "skill_helper".into(),
        "context_script".into(),
    ]);
    if enabled.contains("fs.search") {
        names.insert("context_search".into());
    }
    if enabled.contains("memory.search") {
        names.insert("memory_search".into());
    }
    if enabled.contains("memory.propose") && t.allowed_effects.iter().any(|s| s == "memory.propose")
    {
        names.insert("memory_propose".into());
    }
    let mut host = melissa_skill_context::runtime::ContextRuntime::open(
        repo.clone(),
        store.connect()?,
        catalog,
        melissa_skill_context::runtime::RuntimeGrant {
            task_id: t.task_id.clone(),
            profile_id: profile.into(),
            principal_id: "cli:operator".into(),
            resource_id: t.workspace_ref.clone(),
            authorization_ref: t.authorization_refs[0].clone(),
            namespace: namespace(&repo),
            expires_at_ms: expires,
            policy_epoch: "cli-context-v1".into(),
            enabled: names,
            skill_policy: policy,
            pins,
        },
        registry,
        &[],
    )
    .map_err(|_| Error::Store)?;
    if persist_repository_index && enabled.contains("fs.search") {
        let project = namespace(&repo).project;
        let id = project.strip_prefix("blake3:").ok_or(Error::Store)?;
        let path = app.home.join("projects").join(id).join("repository-index");
        if crate::app::safe_path(&path).is_ok()
            && let Ok(cache) =
                melissa_local_store::RepositoryIndexCache::open(&path, app.key.clone())
        {
            host = host.with_repository_index_cache(cache);
        }
    }
    let native_repo = melissa_repository::tools::RepositoryDispatcher::new(
        app.source(&app.workspace(store, &t.task_id)?)?,
        store.connect()?,
        melissa_repository::tools::RepositoryToolPolicy {
            files: melissa_repository::PatchPolicy {
                task_id: t.task_id.clone(),
                principal_id: "cli:operator".into(),
                profile_id: profile.into(),
                resource_id: t.workspace_ref.clone(),
                authorization_ref: t.authorization_refs[0].clone(),
                expires_at_ms: expires,
                epoch: 1,
                writable_paths: Default::default(),
                protected_paths: Default::default(),
            },
            read_repository: true,
            enabled_tools: std::collections::BTreeSet::from([
                "fs_discover".into(),
                "code_polyglot_symbols".into(),
                "code_unified_propose".into(),
            ]),
        },
    )?;
    let development = crate::development::DevelopmentTools::new(
        app, store, t, profile, expires, enabled, "", false,
    )?;
    let mut routes = std::collections::BTreeMap::new();
    let mut backends: Vec<Box<dyn ToolDispatcher>> = vec![Box::new(native_repo)];
    use melissa_skill_context::runtime::NativeScriptRoute;
    routes.insert(
        "inspect_environment".into(),
        NativeScriptRoute {
            wire_name: "fs_discover".into(),
            fixed_input: Some(
                serde_json::to_value(melissa_repository::discovery::DiscoveryOptions::default())
                    .map_err(|_| Error::Store)?,
            ),
            cache_build: false,
        },
    );
    routes.insert(
        "build_symbol_index".into(),
        NativeScriptRoute {
            wire_name: "code_polyglot_symbols".into(),
            fixed_input: Some(serde_json::json!({})),
            cache_build: false,
        },
    );
    if enabled.contains("code.patch_propose") {
        routes.insert(
            "generate_patch_preview".into(),
            NativeScriptRoute {
                wire_name: "code_unified_propose".into(),
                fixed_input: None,
                cache_build: false,
            },
        );
    }
    if let Some(code) = development.code {
        if code
            .definitions()
            .iter()
            .any(|d| d.definition.name == "code_project_tests")
        {
            let operation = melissa_build_test::tools::CodeOperation::Procedure {
                language: crate::development::language(&repo)?,
                operation: melissa_build_test::full::Operation::Test {
                    suite: melissa_build_test::full::TestSuite::UnitIntegration,
                    repetitions: 1,
                },
                harness: None,
            };
            routes.insert(
                "run_targeted_tests".into(),
                NativeScriptRoute {
                    wire_name: "code_project_tests".into(),
                    fixed_input: Some(serde_json::to_value(operation).map_err(|_| Error::Store)?),
                    cache_build: true,
                },
            );
        }
        backends.push(Box::new(code));
    }
    let router = melissa_repository::tools::ToolRouter::new(backends).map_err(|_| Error::Store)?;
    let host = host
        .with_native_scripts(Box::new(router), routes)
        .map_err(|_| Error::Store)?;
    Ok(ContextBridge {
        inner: ContextDispatcher::new(host),
        repo,
        location: Location {
            home: app.home.clone(),
            key: app.key.clone(),
        },
        project: None,
    })
}
