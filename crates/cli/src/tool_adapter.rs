//! Adds host provenance around the already-authorized specialist dispatcher.
use melissa_model_gateway::{
    ErrorKind, GatewayError, Result, ToolCall,
    adapter::AdapterMetadata,
    tools::{BoundTool, ToolContext, ToolDispatcher, ToolExecution},
};
use melissa_repository::Repository;
use serde_json::json;
use std::{
    future::Future,
    path::PathBuf,
    pin::Pin,
    sync::{Arc, atomic::AtomicBool},
};

pub(crate) struct TaskAdapter {
    pub inner: Box<dyn ToolDispatcher>,
    pub repo: Arc<Repository>,
    pub environment_fingerprint: String,
    pub conversation: Option<ConversationHost>,
}

pub(crate) struct ConversationHost {
    pub home: PathBuf,
    pub task: String,
    pub pause: Arc<AtomicBool>,
}
impl ConversationHost {
    fn app(&self) -> Result<crate::app::App> {
        crate::app::App::open(&self.home).map_err(|_| {
            GatewayError::new(ErrorKind::Store, "Task conversation could not be opened.")
        })
    }
}
fn question(call: &ToolCall) -> Result<String> {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Input {
        question: String,
    }
    let input: Input = serde_json::from_str(&call.arguments)
        .map_err(|_| GatewayError::invalid("ask_user requires a question"))?;
    if input.question.trim().is_empty()
        || input.question.len() > 16384
        || input.question.contains('\0')
    {
        return Err(GatewayError::invalid("invalid question"));
    }
    Ok(input.question)
}

impl ToolDispatcher for TaskAdapter {
    fn definitions(&self) -> Vec<BoundTool> {
        let mut tools = self.inner.definitions();
        if self.conversation.is_some() {
            tools.push(BoundTool{canonical_id:"evidence.attach".into(),definition:melissa_model_gateway::ToolDefinition{name:"ask_user".into(),description:"Ask one necessary, targeted clarification and wait for an explicit operator answer. No API calls while waiting. This cannot grant tools, budget, new paths or acceptance. Do not ask for secrets or repeat an existing authorization.".into(),parameters:json!({"type":"object","properties":{"question":{"type":"string"}},"required":["question"],"additionalProperties":false}),strict:false}});
        }
        tools
    }
    fn validate_call(&self, call: &ToolCall) -> Result<()> {
        if self.conversation.is_some() && call.name == "ask_user" {
            question(call)?;
            return Ok(());
        }
        self.inner.validate_call(call)
    }
    fn turn_context(&self, cancelled: &AtomicBool) -> Result<Option<String>> {
        self.conversation
            .as_ref()
            .map(|host| {
                host.app()?
                    .conversation_context(&host.task, &self.repo, cancelled)
                    .map_err(|_| {
                        GatewayError::new(
                            ErrorKind::Store,
                            "Project guidance or task steering could not be verified.",
                        )
                    })
            })
            .transpose()
    }
    fn adapter_metadata(&self, cancelled: &AtomicBool) -> Result<AdapterMetadata> {
        let source = self.repo.snapshot(cancelled).map_err(|error| {
            GatewayError::new(
                if matches!(error, melissa_repository::Error::Cancelled) {
                    ErrorKind::Cancelled
                } else {
                    ErrorKind::Tool
                },
                "Host could not capture tool preflight source; no tool dispatched.",
            )
        })?;
        let visibility = self
            .repo
            .source_visibility_digest()
            .map_err(|_| GatewayError::new(ErrorKind::Tool, "Source visibility unavailable."))?;
        Ok(AdapterMetadata {
            adapter_id: Some("melissa.cli.specialist".into()),
            adapter_version: Some(env!("CARGO_PKG_VERSION").into()),
            environment_fingerprint: Some(self.environment_fingerprint.clone()),
            source_before: Some(source.digest),
            visibility_digest: Some(visibility),
        })
    }
    fn execute<'a>(
        &'a mut self,
        call: ToolCall,
        context: ToolContext,
        cancelled: Arc<AtomicBool>,
    ) -> Pin<Box<dyn Future<Output = Result<ToolExecution>> + Send + 'a>> {
        if call.name == "ask_user"
            && let Some(host) = &self.conversation
        {
            return Box::pin(async move {
                if context.task_id != host.task {
                    return Err(GatewayError::new(
                        ErrorKind::Authorization,
                        "Question task mismatch.",
                    ));
                }
                let app = host.app()?;
                let created = app
                    .ask_question(&host.task, &question(&call)?)
                    .map_err(|_| {
                        GatewayError::new(ErrorKind::Store, "Question was not recorded.")
                    })?;
                let id = created["brief"]["questions"]
                    .as_array()
                    .and_then(|v| v.last())
                    .and_then(|v| v["id"].as_str())
                    .ok_or_else(|| {
                        GatewayError::new(ErrorKind::Store, "Question identity unavailable.")
                    })?
                    .to_owned();
                loop {
                    if host.pause.load(std::sync::atomic::Ordering::Acquire) {
                        return Ok(ToolExecution {
                            content:
                                json!({"question_id":id,"status":"pending_pause","answer":null})
                                    .to_string(),
                            success: false,
                            cancelled: false,
                            requires_reconciliation: false,
                            artifact_refs: vec![],
                        });
                    }
                    if cancelled.load(std::sync::atomic::Ordering::Acquire) {
                        return Ok(ToolExecution {
                            content: json!({"question_id":id,"status":"cancelled","answer":null})
                                .to_string(),
                            success: false,
                            cancelled: true,
                            requires_reconciliation: false,
                            artifact_refs: vec![],
                        });
                    }
                    let brief = app
                        .conversation(&host.task)
                        .map_err(|_| {
                            GatewayError::new(
                                ErrorKind::Store,
                                "Question answer record unavailable.",
                            )
                        })?
                        .ok_or_else(|| {
                            GatewayError::new(ErrorKind::Store, "Question record missing.")
                        })?;
                    if let Some(answer) = brief
                        .questions
                        .iter()
                        .find(|q| q.id == id)
                        .and_then(|q| q.answer.as_ref())
                    {
                        return Ok(ToolExecution{content:json!({"question_id":id,"answer":answer,"runtime_authority_changed":false}).to_string(),success:true,cancelled:false,requires_reconciliation:false,artifact_refs:vec![]});
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }
            });
        }
        self.inner.execute(call, context, cancelled)
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    struct NoEffects;
    impl ToolDispatcher for NoEffects {
        fn definitions(&self) -> Vec<BoundTool> {
            vec![]
        }
        fn validate_call(&self, _: &ToolCall) -> Result<()> {
            Err(GatewayError::new(
                ErrorKind::Authorization,
                "No tools authorized.",
            ))
        }
        fn execute<'a>(
            &'a mut self,
            _: ToolCall,
            _: ToolContext,
            _: Arc<AtomicBool>,
        ) -> Pin<Box<dyn Future<Output = Result<ToolExecution>> + Send + 'a>> {
            Box::pin(async { panic!("preflight must not execute a tool") })
        }
    }
    #[test]
    fn host_metadata_tracks_current_source_and_preserves_authority_and_cancellation() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("main.rs"), b"fn main() {}\n").unwrap();
        let repo = Arc::new(Repository::open(root.path(), Default::default(), vec![]).unwrap());
        let adapter = TaskAdapter {
            inner: Box::new(NoEffects),
            repo: repo.clone(),
            environment_fingerprint: "reviewed-environment".into(),
            conversation: None,
        };
        let cancel = AtomicBool::new(false);
        let first = adapter.adapter_metadata(&cancel).unwrap();
        assert_eq!(
            first.source_before.as_deref(),
            Some(repo.snapshot(&cancel).unwrap().digest.as_str())
        );
        std::fs::write(
            root.path().join("main.rs"),
            b"fn main() {println!(\"changed\");}\n",
        )
        .unwrap();
        let second = adapter.adapter_metadata(&cancel).unwrap();
        assert_ne!(first.source_before, second.source_before);
        assert_eq!(
            first.environment_fingerprint,
            second.environment_fingerprint
        );
        assert!(
            adapter
                .validate_call(&ToolCall {
                    id: "call".into(),
                    name: "unbound".into(),
                    arguments: "{}".into()
                })
                .is_err()
        );
        cancel.store(true, std::sync::atomic::Ordering::Release);
        assert_eq!(
            adapter.adapter_metadata(&cancel).unwrap_err().kind,
            ErrorKind::Cancelled
        );
    }

    #[tokio::test]
    async fn clarification_waits_for_real_answer_and_pause_or_cancel_never_fabricates_one() {
        for stop in ["answer", "pause", "cancel"] {
            let t = tempfile::tempdir().unwrap();
            let home = t.path().join("home");
            let root = t.path().join("project");
            std::fs::create_dir(&root).unwrap();
            std::fs::write(root.join("main.py"), "pass\n").unwrap();
            crate::app::App::init(&home).unwrap();
            let app = crate::app::App::open(&home).unwrap();
            let value = app
                .create_conversation(
                    &root,
                    "inspect",
                    crate::conversation::Mode::Develop,
                    json!({}),
                )
                .unwrap();
            let id = value["task_id"].as_str().unwrap().to_owned();
            let pause = Arc::new(AtomicBool::new(false));
            let cancel = Arc::new(AtomicBool::new(false));
            let mut adapter = TaskAdapter {
                inner: Box::new(NoEffects),
                repo: Arc::new(Repository::open(&root, Default::default(), vec![]).unwrap()),
                environment_fingerprint: "offline".into(),
                conversation: Some(ConversationHost {
                    home: home.clone(),
                    task: id.clone(),
                    pause: pause.clone(),
                }),
            };
            let call = ToolCall {
                id: "question-call".into(),
                name: "ask_user".into(),
                arguments: json!({"question":"Keep the existing public API?"}).to_string(),
            };
            adapter.validate_call(&call).unwrap();
            let context = ToolContext {
                task_id: id.clone(),
                profile_id: "melissa.code.debugger".into(),
                run_id: "run".into(),
                request_id: "request".into(),
                invocation_id: "invocation".into(),
                attempt_id: "attempt".into(),
                generation: 1,
            };
            let run = adapter.execute(call, context, cancel.clone());
            let operator = async {
                loop {
                    if let Some(brief) = app.conversation(&id).unwrap()
                        && let Some(q) = brief.questions.last()
                    {
                        assert!(q.answer.is_none());
                        match stop {
                            "answer" => {
                                app.answer_question(&id, &q.id, "Yes, retain compatibility.")
                                    .unwrap();
                            }
                            "pause" => pause.store(true, std::sync::atomic::Ordering::Release),
                            _ => cancel.store(true, std::sync::atomic::Ordering::Release),
                        }
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            };
            let (result, ()) = tokio::time::timeout(std::time::Duration::from_secs(3), async {
                tokio::join!(run, operator)
            })
            .await
            .unwrap();
            let result = result.unwrap();
            assert!(!result.requires_reconciliation);
            if stop == "answer" {
                assert!(result.success);
                assert!(result.content.contains("retain compatibility"));
            } else {
                assert!(!result.success);
                assert!(result.content.contains("\"answer\":null"));
                assert_eq!(result.cancelled, stop == "cancel");
            }
            assert!(
                adapter
                    .turn_context(&AtomicBool::new(false))
                    .unwrap()
                    .unwrap()
                    .contains("host_scoped_project_context")
            );
        }
    }
}
