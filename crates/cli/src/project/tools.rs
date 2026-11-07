use super::{Command, Contract, runtime::Runtime};
use crate::{Result, app::App};
use melissa_local_store::LocalStore;
use melissa_model_gateway::{
    ErrorKind, GatewayError, ToolCall, ToolDefinition,
    tools::{BoundTool, ToolContext, ToolDispatcher, ToolExecution},
};
use melissa_repository::Repository;
use serde::Deserialize;
use serde_json::json;
use std::{
    future::Future,
    path::PathBuf,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
pub struct ProjectTools {
    pub runtime: Arc<Mutex<Runtime>>,
    home: PathBuf,
    task: String,
    repo: Arc<Repository>,
    store: LocalStore,
    contract: Contract,
    write: bool,
}
fn err(_: crate::Error) -> GatewayError {
    GatewayError::new(
        ErrorKind::Tool,
        "Project process operation failed; inspect its durable session/receipt before retry.",
    )
}
#[derive(Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
enum Input {
    Start {
        command: Command,
    },
    Check {
        name: String,
    },
    Preflight,
    Sync,
    Poll {
        session_id: String,
        #[serde(default)]
        after: u64,
    },
    Wait {
        session_id: String,
        #[serde(default = "wait_default")]
        wait_ms: u64,
        #[serde(default)]
        after: u64,
    },
    Stdin {
        session_id: String,
        hex: String,
    },
    Eof {
        session_id: String,
    },
    Cancel {
        session_id: String,
    },
}
fn wait_default() -> u64 {
    1000
}
impl ProjectTools {
    #[allow(clippy::too_many_arguments)] // Exact node authority and environment binding, never model-supplied grants.
    pub fn new(
        app: &App,
        store: &LocalStore,
        task: &str,
        profile: &str,
        repo: Arc<Repository>,
        contract: Contract,
        expires: u64,
        write: bool,
    ) -> Result<Self> {
        Ok(Self {
            runtime: Arc::new(Mutex::new(Runtime::new(
                app,
                task,
                profile,
                contract.clone(),
                expires,
            )?)),
            home: app.home.clone(),
            task: task.into(),
            repo,
            store: store.connect()?,
            contract,
            write,
        })
    }
    fn parse(&self, call: &ToolCall) -> melissa_model_gateway::Result<Input> {
        let v = melissa_model_gateway::strict_json(&call.arguments)?;
        let input: Input = serde_json::from_value(v)
            .map_err(|_| GatewayError::invalid("project_process input"))?;
        if let Input::Start { command } = &input {
            if !self.write {
                return Err(GatewayError::new(
                    ErrorKind::Authorization,
                    "Raw process commands need the writing role; use an approved named check.",
                ));
            }
            command
                .validate()
                .map_err(|_| GatewayError::invalid("project command"))?;
        }
        if let Input::Check { name } = &input
            && !self.contract.checks.contains_key(name)
        {
            return Err(GatewayError::invalid("Unknown approved check name."));
        }
        Ok(input)
    }
}
impl ToolDispatcher for ProjectTools {
    fn definitions(&self) -> Vec<BoundTool> {
        vec![BoundTool {
            canonical_id: if self.write {
                "process.exec"
            } else {
                "code.tests"
            }
            .into(),
            definition: ToolDefinition {
                name: "project_process".into(),
                description: format!(
                    "Owned isolated project sessions. start(command executable/argv/cwd) requires a writing role. check(name) uses an approved check; names: {:?}. preflight probes versions/capacity; sync prepares dependencies inside a disposable copy. poll/wait return paged stdout/stderr hex and native outcome. stdin(hex), eof, cancel operate only this task's session_id. No host mounts, host writes, network, background host shell or model-selected image/resources. Keep session IDs, poll to completion before submit_candidate. Generated files stay in worker evidence; use scoped repository edits for host changes.",
                    self.contract.checks.keys().collect::<Vec<_>>()
                ),
                parameters: json!({"type":"object","properties":{"operation":{"type":"string","enum":["start","check","preflight","sync","poll","wait","stdin","eof","cancel"]},"command":{"type":"object","properties":{"executable":{"type":"string"},"argv":{"type":"array","items":{"type":"string"}},"cwd":{"type":"string"}},"required":["executable","argv","cwd"],"additionalProperties":false},"name":{"type":"string"},"session_id":{"type":"string"},"after":{"type":"integer","minimum":0},"wait_ms":{"type":"integer","minimum":0,"maximum":60000},"hex":{"type":"string"}},"required":["operation"],"additionalProperties":false}),
                strict: false,
            },
        }]
    }
    fn validate_call(&self, c: &ToolCall) -> melissa_model_gateway::Result<()> {
        if c.name != "project_process" {
            return Err(GatewayError::invalid("project tool"));
        }
        self.parse(c).map(|_| ())
    }
    fn execute<'a>(
        &'a mut self,
        call: ToolCall,
        ctx: ToolContext,
        cancel: Arc<AtomicBool>,
    ) -> Pin<Box<dyn Future<Output = melissa_model_gateway::Result<ToolExecution>> + Send + 'a>>
    {
        Box::pin(async move {
            if ctx.task_id != self.task {
                return Err(GatewayError::new(
                    ErrorKind::Authorization,
                    "Project task mismatch.",
                ));
            }
            let app = App::open(&self.home).map_err(err)?;
            let input = self.parse(&call)?;
            let value = match input {
                Input::Start { command } => {
                    let sid = self
                        .runtime
                        .lock()
                        .map_err(|_| GatewayError::invalid("process lock"))?
                        .start(
                            &app,
                            &self.store,
                            &self.repo,
                            command,
                            &ctx.invocation_id,
                            cancel,
                            false,
                            false,
                        )
                        .map_err(err)?;
                    json!({"session_id":sid,"execution_status":"NOT_RUN","phase":"preparing","command_completed":false})
                }
                Input::Check { name } => {
                    let command = self.contract.checks[&name].clone();
                    let sid = self
                        .runtime
                        .lock()
                        .map_err(|_| GatewayError::invalid("process lock"))?
                        .start(
                            &app,
                            &self.store,
                            &self.repo,
                            command,
                            &ctx.invocation_id,
                            cancel,
                            false,
                            false,
                        )
                        .map_err(err)?;
                    json!({"session_id":sid,"check":name,"execution_status":"NOT_RUN","phase":"preparing"})
                }
                Input::Preflight | Input::Sync => {
                    let preflight = matches!(input, Input::Preflight);
                    let sid = self
                        .runtime
                        .lock()
                        .map_err(|_| GatewayError::invalid("process lock"))?
                        .start(
                            &app,
                            &self.store,
                            &self.repo,
                            Command {
                                executable: "/bin/true".into(),
                                argv: vec![],
                                cwd: ".".into(),
                            },
                            &ctx.invocation_id,
                            cancel,
                            preflight,
                            !preflight,
                        )
                        .map_err(err)?;
                    json!({"session_id":sid,"execution_status":"NOT_RUN","phase":"preparing"})
                }
                Input::Poll { session_id, after } => app
                    .project_poll(&self.task, &session_id, after, 32 * 1024)
                    .map_err(err)?,
                Input::Wait {
                    session_id,
                    after,
                    wait_ms,
                } => {
                    if wait_ms > 60000 {
                        return Err(GatewayError::invalid(
                            "wait_ms exceeds one polling request; call wait again for longer jobs",
                        ));
                    }
                    let until = tokio::time::Instant::now() + Duration::from_millis(wait_ms);
                    loop {
                        let v = app
                            .project_poll(&self.task, &session_id, after, 32 * 1024)
                            .map_err(err)?;
                        if v["owner_lost"] == true
                            || !matches!(
                                v["state"]["phase"].as_str(),
                                Some("preparing" | "running")
                            )
                            || tokio::time::Instant::now() >= until
                        {
                            break v;
                        }
                        if cancel.load(Ordering::Acquire) {
                            let _ = app.project_input(
                                &self.task,
                                &session_id,
                                &ctx.invocation_id,
                                "cancel",
                                "",
                            );
                        }
                        tokio::time::sleep(Duration::from_millis(30)).await;
                    }
                }
                Input::Stdin { session_id, hex } => app
                    .project_input(&self.task, &session_id, &ctx.invocation_id, "stdin", &hex)
                    .map_err(err)?,
                Input::Eof { session_id } => app
                    .project_input(&self.task, &session_id, &ctx.invocation_id, "eof", "")
                    .map_err(err)?,
                Input::Cancel { session_id } => app
                    .project_input(&self.task, &session_id, &ctx.invocation_id, "cancel", "")
                    .map_err(err)?,
            };
            let failed = value["state"]["result"]["status"]
                .as_str()
                .is_some_and(|s| s != "PASS");
            Ok(ToolExecution {
                content: value.to_string(),
                success: !failed,
                cancelled: false,
                requires_reconciliation: value["requires_reconciliation"] == true
                    || value["state"]["result"]["requires_reconciliation"] == true,
                artifact_refs: vec![],
            })
        })
    }
}

/// Project commands use their selected environment. Keep the older fixed-SDK
/// backend only for explicitly separate navigation/evidence observations.
pub struct NavigationOnly(pub Box<dyn ToolDispatcher>);
impl NavigationOnly {
    fn allowed(name: &str) -> bool {
        matches!(
            name,
            "code_semantic"
                | "code_git"
                | "code_result_page"
                | "code_debug_start"
                | "code_debug_command"
        )
    }
}
impl ToolDispatcher for NavigationOnly {
    fn definitions(&self) -> Vec<BoundTool> {
        self.0
            .definitions()
            .into_iter()
            .filter(|t| Self::allowed(&t.definition.name))
            .collect()
    }
    fn validate_call(&self, call: &ToolCall) -> melissa_model_gateway::Result<()> {
        if !Self::allowed(&call.name) {
            return Err(GatewayError::new(
                ErrorKind::Authorization,
                "Fixed-SDK build/debug procedures do not use this project's selected environment; use project_process.",
            ));
        }
        self.0.validate_call(call)
    }
    fn execute<'a>(
        &'a mut self,
        call: ToolCall,
        context: ToolContext,
        cancel: Arc<AtomicBool>,
    ) -> Pin<Box<dyn Future<Output = melissa_model_gateway::Result<ToolExecution>> + Send + 'a>>
    {
        if let Err(error) = self.validate_call(&call) {
            return Box::pin(async move { Err(error) });
        }
        self.0.execute(call, context, cancel)
    }
}
