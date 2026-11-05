//! Adds host provenance around the already-authorized specialist dispatcher.
use melissa_model_gateway::{
    ErrorKind, GatewayError, Result, ToolCall,
    adapter::AdapterMetadata,
    tools::{BoundTool, ToolContext, ToolDispatcher, ToolExecution},
};
use melissa_repository::Repository;
use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, atomic::AtomicBool},
};

pub(crate) struct TaskAdapter {
    pub inner: Box<dyn ToolDispatcher>,
    pub repo: Arc<Repository>,
    pub environment_fingerprint: String,
}

impl ToolDispatcher for TaskAdapter {
    fn definitions(&self) -> Vec<BoundTool> {
        self.inner.definitions()
    }
    fn validate_call(&self, call: &ToolCall) -> Result<()> {
        self.inner.validate_call(call)
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
}
