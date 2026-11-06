//! CLI bindings to actual isolated SDK procedures and semantic services.
use crate::{
    Error, Result,
    app::{App, now},
    resources,
};
use melissa_build_test::{
    full::{Language, Operation, TestSuite},
    semantic::{SemanticMethod, SemanticQuery},
    tools::{CodeDispatcher, CodeOperation, CodeToolPolicy},
};
use melissa_container_executor::ContainerLimits;
use melissa_core_contracts::wire::TaskContractV1;
use melissa_execution_broker::policy::*;
use melissa_local_store::LocalStore;
use melissa_model_gateway::{
    ErrorKind, GatewayError, ToolCall, ToolDefinition,
    tools::{BoundTool, ToolContext, ToolDispatcher, ToolExecution},
};
use melissa_repository::Repository;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    future::Future,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

pub fn limits() -> ContainerLimits {
    ContainerLimits {
        wall_time_ms: 90000,
        cpu_time_ms: 60000,
        cpu_rate_basis_points: 10000,
        memory_bytes: 2 * 1024 * 1024 * 1024,
        disk_bytes: 512 * 1024 * 1024,
        output_bytes: 4 * 1024 * 1024,
        export_bytes: 32 * 1024 * 1024,
        max_processes: 128,
    }
}
pub fn policy() -> HostPolicy {
    let l = limits();
    HostPolicy {
        revision: "cli-development-v1".into(),
        revocation_epoch: 1,
        issuer: "cli:host".into(),
        audience: "executor:container".into(),
        key_id: "host-template".into(),
        max_permit_lifetime_ms: l.wall_time_ms,
        ceiling: ProcessLimits {
            wall_time_ms: l.wall_time_ms,
            cpu_time_ms: l.cpu_time_ms,
            cpu_rate_basis_points: 10000,
            cpu_mode: CpuLimitMode::MonitoredTotal,
            memory_bytes: l.memory_bytes,
            disk_bytes: l.disk_bytes,
            disk_mode: DiskLimitMode::Strict,
            output_bytes: l.output_bytes + l.export_bytes,
            max_processes: l.max_processes,
            network: NetworkPolicy::Deny,
        },
        enforcement: EnforcementCapabilities {
            filesystem_isolation: true,
            network_isolation: true,
            cpu_time: false,
            memory: true,
            disk_quota: true,
            process_tree: true,
            output_limit: true,
        },
        allow_monitored_disk: false,
        allow_monitored_cpu: true,
        allowed_env_names: BTreeSet::new(),
        grants: BTreeMap::new(),
        control_grants: BTreeMap::new(),
        revoked_authorization_refs: BTreeSet::new(),
        revoked_permit_ids: BTreeSet::new(),
        revoked_nonces: BTreeSet::new(),
    }
}
pub fn language(repo: &Repository) -> Result<Language> {
    Ok(detected_language(repo)?.unwrap_or(Language::Python))
}
pub fn detected_language(repo: &Repository) -> Result<Option<Language>> {
    let s = repo.snapshot(&AtomicBool::new(false))?;
    let has = |suffix: &str| s.files.iter().any(|f| f.path.ends_with(suffix));
    Ok(if has("Cargo.toml") || has(".rs") {
        Some(Language::Rust)
    } else if has(".ts") || has(".tsx") {
        Some(Language::TypeScript)
    } else if has(".js") || has(".mjs") || has(".cjs") {
        Some(Language::JavaScript)
    } else if has(".py") || has(".pyi") {
        Some(Language::Python)
    } else {
        None
    })
}
pub struct DevelopmentTools {
    pub code: Option<CodeDispatcher>,
    repo: Arc<Repository>,
    task: TaskContractV1,
    language: Language,
    tools: BTreeSet<String>,
    write: bool,
    entry: String,
    debug: bool,
    git: Option<CodeDispatcher>,
    store: LocalStore,
    expires: u64,
    git_capture_allowed: bool,
}
impl DevelopmentTools {
    #[allow(clippy::too_many_arguments)] // One trusted task/role/scope binding, never model arguments.
    pub fn new(
        app: &App,
        store: &LocalStore,
        t: &TaskContractV1,
        profile: &str,
        expires: u64,
        tools: &BTreeSet<String>,
        entry: &str,
        write: bool,
    ) -> Result<Self> {
        let repo = Arc::new(app.source(&app.workspace(store, &t.task_id)?)?);
        let project_contract = app
            .execution(store, &t.task_id)?
            .and_then(|e| e.recipe.project);
        let debug_project = project_contract.as_ref().and_then(|p| {
            p.verification
                .as_ref()
                .and_then(|v| v.debug.clone())
                .map(|launch| melissa_build_test::debug::Project {
                    launch,
                    image_id: p.environment.image_id.clone(),
                    executables: p.environment.executables.clone(),
                    versions: p.environment.versions.clone(),
                    rust_version: p.environment.versions.get("rust").cloned(),
                })
        });
        let lang = if let Some(p) = &debug_project {
            p.launch.language
        } else {
            language(&repo)?
        };
        let entry = debug_project
            .as_ref()
            .map_or(entry, |p| p.launch.entry.as_str());
        let c = resources::catalog()?;
        let p = c.profiles.get(profile).ok_or(Error::Store)?;
        let process = p
            .authority
            .capability_ceiling
            .iter()
            .any(|e| e == "process.execute_scoped")
            && t.allowed_effects
                .iter()
                .any(|e| e == "process.execute_scoped");
        let mut ops = BTreeMap::new();
        if process && detected_language(&repo)?.is_some() {
            for (name, op) in [
                ("code_project_build", Operation::Build {}),
                ("code_project_check", Operation::Check {}),
                ("code_project_lint", Operation::Lint {}),
                ("code_format_check", Operation::FormatCheck {}),
                (
                    "code_project_tests",
                    Operation::Test {
                        suite: TestSuite::UnitIntegration,
                        repetitions: 1,
                    },
                ),
                (
                    "code_project_doctests",
                    Operation::Test {
                        suite: TestSuite::Doctest,
                        repetitions: 1,
                    },
                ),
                (
                    "code_project_e2e",
                    Operation::Test {
                        suite: TestSuite::EndToEnd,
                        repetitions: 1,
                    },
                ),
            ] {
                let operation = CodeOperation::Procedure {
                    language: lang,
                    operation: op,
                    harness: None,
                };
                if tools.contains(operation.canonical()) {
                    ops.insert(name.into(), operation);
                }
            }
            if (write || debug_project.is_some())
                && (project_contract.is_none() || debug_project.is_some())
                && tools.contains("code.diagnostics")
                && p.tool_ids.iter().any(|s| s == "process.exec")
                && !entry.is_empty()
            {
                ops.insert(
                    "cli_debug_native".into(),
                    CodeOperation::Debug {
                        language: lang,
                        entry: entry.into(),
                        breakpoints: vec![melissa_build_test::debug::Breakpoint {
                            path: entry.into(),
                            line: 1,
                        }],
                        expected_binary_sha256: None,
                    },
                );
            }
        }
        if tools.contains("code.references") {
            ops.insert(
                "cli_semantic_native".into(),
                CodeOperation::Semantic {
                    language: lang,
                    query: SemanticQuery {
                        method: SemanticMethod::Symbols,
                        path: entry.into(),
                        line: 0,
                        character: 0,
                    },
                    build_scripts: false,
                },
            );
        }
        let debug = ops.contains_key("cli_debug_native");
        let mut code = if ops.is_empty() {
            None
        } else {
            Some(
                CodeDispatcher::new(
                    app.source(&app.workspace(store, &t.task_id)?)?,
                    store.connect()?,
                    c,
                    CodeToolPolicy {
                        task_id: t.task_id.clone(),
                        profile_id: profile.into(),
                        principal_id: "cli:operator".into(),
                        resource_id: t.workspace_ref.clone(),
                        authorization_ref: t.authorization_refs[0].clone(),
                        expires_at_ms: expires,
                        limits: limits(),
                        operations: ops,
                        process_policy: Some(policy()),
                    },
                )
                .map_err(|_| Error::Denied("SDK kapsamı görev/uzman yetkisiyle uyuşmuyor."))?,
            )
        };
        if debug
            && let Some(project) = debug_project.clone()
            && let Some(c) = code.take()
        {
            code = Some(
                c.with_project_debug(project)
                    .map_err(|_| Error::Denied("Onaylı proje debug ayarı bağlanamadı."))?,
            );
        }
        let git_capture_allowed = app
            .workspace(store, &t.task_id)?
            .explicit_excludes
            .is_empty()
            && melissa_repository::git::metadata_available(&repo);
        Ok(Self {
            code,
            repo,
            task: t.clone(),
            language: lang,
            tools: tools.clone(),
            write,
            entry: entry.into(),
            debug,
            git: None,
            store: store.connect()?,
            expires,
            git_capture_allowed,
        })
    }
    fn semantic(&self, c: &ToolCall) -> melissa_model_gateway::Result<CodeOperation> {
        let q: SemanticQuery =
            serde_json::from_value(melissa_model_gateway::strict_json(&c.arguments)?)
                .map_err(|_| GatewayError::invalid("semantic query"))?;
        if !melissa_workflow::plan::path(&q.path) || q.path == "/" {
            return Err(GatewayError::invalid("semantic project path"));
        }
        self.repo
            .read(&q.path)
            .map_err(|_| GatewayError::invalid("semantic file not in current scope"))?;
        let lang = match melissa_repository::discovery::language(&q.path) {
            Some("rust") => Language::Rust,
            Some("python") => Language::Python,
            Some("typescript" | "tsx") => Language::TypeScript,
            Some("javascript") => Language::JavaScript,
            _ => return Err(GatewayError::unsupported("semantic language unavailable")),
        };
        let op = CodeOperation::Semantic {
            language: lang,
            query: q,
            build_scripts: false,
        };
        if !self.tools.contains(op.canonical()) {
            return Err(GatewayError::invalid(
                "semantic method not allowed for role",
            ));
        }
        Ok(op)
    }
    fn git(&self, c: &ToolCall) -> melissa_model_gateway::Result<CodeOperation> {
        if !self.git_capture_allowed {
            return Err(GatewayError::new(
                ErrorKind::Authorization,
                "Git full capture requires ordinary in-root metadata and no explicit source exclusions",
            ));
        }
        let value = melissa_model_gateway::strict_json(&c.arguments)?;
        let operation: melissa_repository::git::GitOperation =
            serde_json::from_value(value.clone())
                .map_err(|_| GatewayError::invalid("Git operation"))?;
        if serde_json::to_value(&operation).map_err(|_| GatewayError::invalid("Git encoding"))?
            != value
        {
            return Err(GatewayError::invalid("unexpected Git fields"));
        }
        if matches!(
            operation,
            melissa_repository::git::GitOperation::WorktreeCreate { .. }
        ) && !self.write
        {
            return Err(GatewayError::invalid(
                "worktree creation requires a writing step",
            ));
        }
        let op = CodeOperation::Git { operation };
        if !self.tools.contains(op.canonical()) {
            return Err(GatewayError::invalid("Git operation outside role"));
        }
        Ok(op)
    }
    fn debug(&self, c: &ToolCall) -> melissa_model_gateway::Result<CodeOperation> {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Input {
            breakpoints: Vec<melissa_build_test::debug::Breakpoint>,
        }
        let input: Input =
            serde_json::from_value(melissa_model_gateway::strict_json(&c.arguments)?)
                .map_err(|_| GatewayError::invalid("debug breakpoint input"))?;
        if !self.debug || input.breakpoints.is_empty() || input.breakpoints.len() > 64 {
            return Err(GatewayError::invalid("debug scope or breakpoint count"));
        }
        for b in &input.breakpoints {
            if b.line == 0 || self.repo.read(&b.path).is_err() {
                return Err(GatewayError::invalid("debug source breakpoint"));
            }
        }
        Ok(CodeOperation::Debug {
            language: self.language,
            entry: self.entry.clone(),
            breakpoints: input.breakpoints,
            expected_binary_sha256: None,
        })
    }
}
impl ToolDispatcher for DevelopmentTools {
    fn definitions(&self) -> Vec<BoundTool> {
        let mut d = self
            .code
            .as_ref()
            .map(|c| c.definitions())
            .unwrap_or_default();
        d.retain(|d| !d.definition.name.starts_with("cli_"));
        if self.debug {
            d.push(BoundTool{canonical_id:"code.diagnostics".into(),definition:ToolDefinition{name:"code_debug_start".into(),description:"Start an isolated DAP session at selected source breakpoints (one-based lines) using the host-selected entry. Use code_debug_command with its session ID to inspect/step/close. No host attach or arbitrary evaluation.".into(),parameters:json!({"type":"object","properties":{"breakpoints":{"type":"array","minItems":1,"maxItems":64,"items":{"type":"object","properties":{"path":{"type":"string"},"line":{"type":"integer","minimum":1}},"required":["path","line"],"additionalProperties":false}}},"required":["breakpoints"],"additionalProperties":false}),strict:false}});
        }
        if self.git_capture_allowed
            && self.tools.contains("git.status")
            && self.task.allowed_effects.iter().any(|e| e == "git.read")
        {
            d.push(BoundTool{canonical_id:"git.status".into(),definition:ToolDefinition{name:"code_git".into(),description:"Scoped Git on a captured private copy: status, diff(staged), log(max_entries 1..1000), log_paths(max_entries 1..1000; records ranking evidence), worktree_list, worktree_create(name, exact commit). No commit/push/host changes, hooks or credentials. Worktree creation needs explicit role/write authority; output is a quarantined artifact. Re-captures source and metadata on each call.".into(),parameters:json!({"type":"object","properties":{"operation":{"type":"string","enum":["status","diff","log","log_paths","worktree_list","worktree_create"]},"staged":{"type":"boolean"},"max_entries":{"type":"integer","minimum":1,"maximum":1000},"name":{"type":"string"},"commit":{"type":"string"}},"required":["operation"],"additionalProperties":false}),strict:false}});
        }
        if self.tools.contains("code.references") {
            d.push(BoundTool{canonical_id:"code.references".into(),definition:ToolDefinition{name:"code_semantic".into(),description:"Real rust-analyzer/TypeScript/Pyright query over the current isolated source. Zero-based UTF-16 line/character. No build scripts; does not claim syntactic matches are semantic references.".into(),parameters:json!({"type":"object","properties":{"method":{"type":"string","enum":["definition","references","symbols","diagnostics","hover","type_definition","implementation"]},"path":{"type":"string"},"line":{"type":"integer","minimum":0},"character":{"type":"integer","minimum":0}},"required":["method","path","line","character"],"additionalProperties":false}),strict:false}});
        }
        d
    }
    fn validate_call(&self, c: &ToolCall) -> melissa_model_gateway::Result<()> {
        if c.name == "code_result_page"
            && self
                .git
                .as_ref()
                .is_some_and(|d| d.validate_call(c).is_ok())
        {
            return Ok(());
        }
        if c.name == "code_semantic" {
            self.semantic(c)?;
            Ok(())
        } else if c.name == "code_git" {
            self.git(c)?;
            Ok(())
        } else if c.name == "code_debug_start" {
            self.debug(c)?;
            Ok(())
        } else if c.name.starts_with("cli_") {
            Err(GatewayError::invalid("internal query route"))
        } else {
            self.code
                .as_ref()
                .ok_or_else(|| GatewayError::invalid("SDK route unavailable"))?
                .validate_call(c)
        }
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
            if c.name == "code_result_page"
                && self
                    .git
                    .as_ref()
                    .is_some_and(|d| d.validate_call(&c).is_ok())
            {
                return self.git.as_mut().unwrap().execute(c, ctx, cancel).await;
            }
            if !matches!(
                c.name.as_str(),
                "code_semantic" | "code_git" | "code_debug_start"
            ) {
                return self
                    .code
                    .as_mut()
                    .ok_or_else(|| GatewayError::invalid("SDK missing"))?
                    .execute(c, ctx, cancel)
                    .await;
            }
            let op = if c.name == "code_semantic" {
                self.semantic(&c)?
            } else if c.name == "code_git" {
                self.git(&c)?
            } else {
                self.debug(&c)?
            };
            let arguments = serde_json::to_string(&op)
                .map_err(|_| GatewayError::invalid("semantic encoding"))?;
            let name = if c.name == "code_semantic" {
                "cli_semantic_native"
            } else if c.name == "code_git" {
                "cli_git_native"
            } else {
                "cli_debug_native"
            };
            if c.name == "code_git" {
                if self.git.is_none() {
                    let repo =
                        Repository::open_for_git(self.repo.root_path(), self.repo.limits().clone())
                            .map_err(|e| GatewayError::invalid(e.to_string()))?;
                    if repo.root_identity() != self.repo.root_identity() {
                        return Err(GatewayError::invalid("Git root changed"));
                    }
                    self.git = Some(
                        CodeDispatcher::new(
                            repo,
                            self.store
                                .connect()
                                .map_err(|_| GatewayError::invalid("Git store"))?,
                            resources::catalog()
                                .map_err(|_| GatewayError::invalid("Git catalog"))?,
                            CodeToolPolicy {
                                task_id: self.task.task_id.clone(),
                                profile_id: ctx.profile_id.clone(),
                                principal_id: "cli:operator".into(),
                                resource_id: self.task.workspace_ref.clone(),
                                authorization_ref: self.task.authorization_refs[0].clone(),
                                expires_at_ms: self.expires,
                                limits: limits(),
                                operations: BTreeMap::from([(name.into(), op.clone())]),
                                process_policy: None,
                            },
                        )
                        .map_err(|e| GatewayError::new(ErrorKind::Authorization, e.to_string()))?,
                    );
                }
                let git = self.git.as_mut().unwrap();
                git.bind_query(name, op)
                    .map_err(|e| GatewayError::new(ErrorKind::Authorization, e.to_string()))?;
                return git
                    .execute(
                        ToolCall {
                            id: c.id,
                            name: name.into(),
                            arguments,
                        },
                        ctx,
                        cancel,
                    )
                    .await;
            }
            let native = self
                .code
                .as_mut()
                .ok_or_else(|| GatewayError::invalid("SDK missing"))?;
            native
                .bind_query(name, op)
                .map_err(|e| GatewayError::new(ErrorKind::Authorization, e.to_string()))?;
            native
                .execute(
                    ToolCall {
                        id: c.id,
                        name: name.into(),
                        arguments,
                    },
                    ctx,
                    cancel,
                )
                .await
        })
    }
}

pub struct CheckedEdits {
    pub inner: Box<dyn ToolDispatcher>,
    pub repo: Arc<Repository>,
    pub store: LocalStore,
    pub task: String,
    pub checker: Option<CodeDispatcher>,
    pub project_verifier: Option<crate::project_verification::tools::Shared>,
    pub language: Language,
    pub context: Option<Arc<Mutex<melissa_skill_context::runtime::ContextRuntime>>>,
}
impl ToolDispatcher for CheckedEdits {
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
            let edit = matches!(
                c.name.as_str(),
                "fs_patch" | "fs_create" | "fs_delete" | "fs_move"
            );
            let id = c.id.clone();
            let name = c.name.clone();
            let git_snapshot = if self.context.is_some()
                && name == "code_git"
                && melissa_model_gateway::strict_json(&c.arguments)
                    .ok()
                    .is_some_and(|v| matches!(v["operation"].as_str(), Some("diff" | "log_paths")))
            {
                self.inner.validate_call(&c)?;
                self.repo.snapshot(&cancel).ok().map(|s| s.digest)
            } else {
                None
            };
            let mut out = self.inner.execute(c, ctx.clone(), cancel.clone()).await?;
            if let Some(context) = &self.context
                && !out.cancelled
                && !out.requires_reconciliation
                && !cancel.load(Ordering::Acquire)
                && !out.artifact_refs.is_empty()
                && let Some(hint) =
                    crate::repository_signals::observe(&name, &out.content, git_snapshot.as_deref())
                && (git_snapshot.is_none()
                    || (out.success
                        && self
                            .repo
                            .snapshot(&cancel)
                            .is_ok_and(|s| s.digest == hint.snapshot)))
                && let Ok(mut context) = context.lock()
            {
                let _ = context.record_repository_hints(hint.kind, hint.snapshot, hint.paths);
            }
            if !edit || !out.success || out.requires_reconciliation || out.cancelled {
                return Ok(out);
            }
            let checked = (|| -> Result<Value> {
                let snap = self.repo.snapshot(&cancel)?;
                let index =
                    melissa_repository::polyglot::PolyglotIndex::build(&self.repo, &snap, &cancel)?;
                let supported = snap
                    .files
                    .len()
                    .saturating_sub(index.unsupported_files.len());
                Ok(
                    json!({"source_digest":snap.digest,"syntax_errors":index.syntax_errors,"unsupported_files":index.unsupported_files,"level":"grammar_only","status":if supported==0{"NOT_RUN"}else if index.syntax_errors.is_empty(){"PASS"}else{"FAIL"},"passed":if supported==0{Value::Null}else{json!(index.syntax_errors.is_empty())}}),
                )
            })();
            let mut check = match checked {
                Ok(v) => v,
                Err(e) => json!({"status":"BLOCKED","passed":false,"detail":e.to_string()}),
            };
            if check["passed"] == true
                && let Some(native) = &mut self.checker
            {
                let op = CodeOperation::Procedure {
                    language: self.language,
                    operation: Operation::Check {},
                    harness: None,
                };
                let mut child = ctx.clone();
                child.invocation_id.push_str(":after-edit");
                child.attempt_id.push_str(":after-edit");
                match native
                    .execute(
                        ToolCall {
                            id: format!("{id}:after-edit"),
                            name: "code_project_check".into(),
                            arguments: serde_json::to_string(&op).unwrap(),
                        },
                        child,
                        cancel.clone(),
                    )
                    .await
                {
                    Ok(r) => {
                        check["sdk"] =
                            serde_json::from_str(&r.content).unwrap_or(json!({"raw":r.content}));
                        check["passed"] =
                            json!(r.success && !r.cancelled && !r.requires_reconciliation);
                        check["level"] = json!("grammar_and_sdk");
                        check["status"] = json!(if r.requires_reconciliation || r.cancelled {
                            "BLOCKED"
                        } else if r.success {
                            "PASS"
                        } else {
                            "FAIL"
                        });
                        out.requires_reconciliation |= r.requires_reconciliation;
                        out.cancelled |= r.cancelled;
                        out.artifact_refs.extend(r.artifact_refs);
                    }
                    Err(e) => {
                        check["sdk"] = json!({"status":"unavailable","detail":e.to_string()});
                        check["passed"] = json!(false);
                        check["status"] = json!("BLOCKED");
                    }
                }
            }
            if check["passed"] == true
                && let Some(host) = &self.project_verifier
            {
                let names = host
                    .lock()
                    .map_err(|_| GatewayError::invalid("verification host lock"))?
                    .contract
                    .verification
                    .as_ref()
                    .map(|v| v.after_edit.clone())
                    .unwrap_or_default();
                if !names.is_empty() {
                    let mut child = ctx.clone();
                    child.invocation_id.push_str(":project-after-edit");
                    child.attempt_id.push_str(":project-after-edit");
                    match crate::project_verification::tools::run_shared(
                        host.clone(),
                        crate::project_verification::runner::Request {
                            names,
                            baseline: None,
                        },
                        false,
                        child,
                        cancel.clone(),
                    )
                    .await
                    {
                        Ok(result) => {
                            check["project"] =
                                serde_json::from_str(&result.content).unwrap_or(Value::Null);
                            check["passed"] =
                                json!(result.success && !result.requires_reconciliation);
                            check["status"] = check["project"]["outcome"].clone();
                            check["level"] = json!("grammar_and_reviewed_project_checks");
                            out.requires_reconciliation |= result.requires_reconciliation;
                            out.artifact_refs.extend(result.artifact_refs);
                        }
                        Err(e) => {
                            check["status"] = json!("BLOCKED");
                            check["passed"] = json!(false);
                            check["project"] = json!({"reason":e.message});
                        }
                    }
                }
            }
            let receipt = (|| -> Result<String> {
                let artifact = format!("cli:after-edit:{id}:{}", now()?);
                crate::app::put(
                    &mut self.store,
                    &self.task,
                    &artifact,
                    "code.after-edit",
                    &crate::app::bytes(&check)?,
                )?;
                Ok(artifact)
            })();
            match receipt {
                Ok(artifact) => {
                    out.artifact_refs.push(artifact);
                    if !out.cancelled
                        && !out.requires_reconciliation
                        && !cancel.load(Ordering::Acquire)
                        && let Some(context) = &self.context
                        && let Some(hint) = crate::repository_signals::after_edit(&check)
                        && let Ok(mut context) = context.lock()
                    {
                        let _ =
                            context.record_repository_hints(hint.kind, hint.snapshot, hint.paths);
                    }
                }
                Err(_) => {
                    // The edit already happened even when its diagnostic receipt cannot be saved.
                    out.requires_reconciliation = true;
                    check["receipt_error"] = json!("post-edit receipt could not be persisted");
                    check["status"] = json!("BLOCKED");
                    check["passed"] = json!(false);
                }
            }
            out.content=json!({"mutation_applied":true,"mutation":serde_json::from_str::<Value>(&out.content).unwrap_or(json!({"raw":out.content})),"after_edit":check,"instruction":"Mutation already applied. Inspect current source and repair diagnostics; do not repeat the old mutation. Final protected acceptance is separate."}).to_string();
            out.success = check["passed"] == true || check["status"] == "NOT_RUN";
            Ok(out)
        })
    }
}
