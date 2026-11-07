#![cfg(windows)]
mod support;
use melissa_model_gateway::{
    ToolCall,
    tools::{ToolContext, ToolDispatcher},
};
use melissa_repository::{
    FileMutation, MutationProposal,
    tools::{RepositoryDispatcher, RepositoryToolPolicy},
};
use serde_json::{Value, json};
use std::{
    fs,
    sync::{Arc, atomic::AtomicBool},
};

async fn invoke(
    d: &mut RepositoryDispatcher,
    name: &str,
    args: Value,
    id: &str,
) -> melissa_model_gateway::tools::ToolExecution {
    let call = ToolCall {
        id: id.into(),
        name: name.into(),
        arguments: args.to_string(),
    };
    d.validate_call(&call).unwrap();
    d.execute(
        call,
        ToolContext {
            task_id: "task:code05-closeout".into(),
            profile_id: "melissa.code.implementer".into(),
            run_id: "website-flow".into(),
            request_id: format!("request:{id}"),
            invocation_id: format!("inv:{id}"),
            attempt_id: format!("attempt:{id}"),
            generation: 1,
        },
        Arc::new(AtomicBool::new(false)),
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn denied_git_probe_discloses_nothing_then_authorized_website_files_are_created() {
    for git_exists in [false, true] {
        let (_temp, repo, store, policy) =
            support::fixture(&["index.html", "styles.css", "app.js"]);
        let root = repo.root_path().to_owned();
        let identity = repo.root_identity().to_owned();
        if git_exists {
            fs::create_dir(root.join(".git")).unwrap();
            fs::write(root.join(".git/private"), "must-not-leak").unwrap();
        }
        let mut d = RepositoryDispatcher::new(
            repo,
            store,
            RepositoryToolPolicy {
                files: policy,
                read_repository: true,
                enabled_tools: ["fs_absent", "fs_create", "fs_read"]
                    .map(String::from)
                    .into(),
            },
        )
        .unwrap()
        .with_denied_read_observations()
        .unwrap();
        let denied = invoke(&mut d, "fs_absent", json!({"path":".git"}), "denied").await;
        assert!(!denied.success && !denied.requires_reconciliation);
        let result: Value = serde_json::from_str(&denied.content).unwrap();
        assert_eq!(result["status"], "BLOCKED");
        assert_eq!(result["automatic_retry_allowed"], false);
        assert!(!denied.content.contains("must-not-leak"));
        assert!(!denied.content.contains("parent_identity"));
        for (i, (name, source)) in [("index.html", "<!doctype html><html lang=\"tr\"><link rel=\"stylesheet\" href=\"styles.css\"><h1>Merhaba</h1><script src=\"app.js\"></script></html>"), ("styles.css", "body{font-family:system-ui;max-width:72rem;margin:auto}"), ("app.js", "document.documentElement.dataset.ready='true';")].iter().enumerate() {
            let absence = invoke(&mut d, "fs_absent", json!({"path":name}), &format!("absent-{i}")).await;
            let destination = serde_json::from_value(serde_json::from_str::<Value>(&absence.content).unwrap()["result"].clone()).unwrap();
            let proposal = MutationProposal { schema_version: "melissa.repository-mutation/v1".into(), root_identity: identity.clone(), mutation: FileMutation::Create { destination, bytes: source.as_bytes().to_vec() } };
            assert!(invoke(&mut d, "fs_create", json!(proposal), &format!("create-{i}")).await.success);
            assert_eq!(fs::read_to_string(root.join(name)).unwrap(), *source);
        }
        assert!(
            invoke(
                &mut d,
                "fs_read",
                json!({"path":"index.html"}),
                "verify-site"
            )
            .await
            .success
        );
    }
}
