use crate::{
    Error, Result,
    app::{App, bytes, put},
    project::{Command, Contract, runtime::Runtime},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{Arc, atomic::AtomicBool},
};
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Helper {
    pub command: Command,
    #[serde(default)]
    pub skill: Option<String>,
}
impl Helper {
    pub fn validate(&self, skills: &BTreeMap<String, super::skills::Package>) -> Result {
        self.command.validate()?;
        if let Some(id) = &self.skill {
            let package = skills
                .get(id)
                .ok_or(Error::Usage("Helper skill pini bulunamadı."))?;
            for arg in &self.command.argv {
                if let Some(path) = arg.strip_prefix("@skill/")
                    && !package.files.contains_key(path)
                {
                    return Err(Error::Usage("Helper skill dosyası pin dışında."));
                }
            }
        } else if self.command.argv.iter().any(|a| a.starts_with("@skill/")) {
            return Err(Error::Usage("@skill için açık skill pini gerekli."));
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Event {
    BeforeTool,
    AfterTool,
    AfterMutation,
    TaskEnd,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Hook {
    pub id: String,
    pub event: Event,
    #[serde(default)]
    pub tools: Vec<String>,
    pub helper: Helper,
}
pub struct Runner {
    pub home: std::path::PathBuf,
    pub task: String,
    pub contract: Contract,
    pub expires: u64,
    pub store: melissa_local_store::LocalStore,
}
impl Runner {
    pub async fn helper(
        &mut self,
        id: &str,
        helper: &Helper,
        input: Value,
        cancel: Arc<AtomicBool>,
    ) -> Result<Value> {
        let app = App::open(&self.home)?;
        let mut store = self.store.connect()?;
        if store
            .journal()
            .task(&self.task)?
            .contract
            .acceptance_contract_digest
            != crate::digest(&bytes(&self.contract)?)
        {
            return Err(Error::Denied(
                "Helper ortamı/paketi onaylı görev sözleşmesine uymuyor.",
            ));
        }
        let bundle = self
            .contract
            .extensions
            .as_ref()
            .ok_or(Error::Denied("Eklenti onayı yok."))?;
        helper.validate(&bundle.skills)?;
        let repo = app.source(&app.workspace(&store, &self.task)?)?;
        let source_digest = repo.snapshot(&AtomicBool::new(false))?.digest;
        let input_digest = crate::digest(&bytes(&input)?);
        let helper_digest = crate::digest(&bytes(helper)?);
        let receipt_id = format!("extension:helper:{}", blake3::hash(id.as_bytes()));
        if let Ok(raw) = store.get_artifact(&self.task, &receipt_id) {
            let old: Value = crate::app::decode(&raw)?;
            if old["input_digest"] != input_digest
                || old["helper_digest"] != helper_digest
                || old["source_digest"] != source_digest
                || old["extension_digest"] != bundle.fingerprint()?
            {
                return Err(Error::Conflict(
                    "Helper eski/güncellenmiş girdiye ait; aynı invocation tekrarlanmadı.",
                ));
            }
            return Ok(old);
        }
        let intent = format!("{receipt_id}:intent");
        if store.artifact_metadata(&self.task, &intent).is_ok() {
            return Err(Error::Reconciliation(
                "Eklenti helper önceki sonucu belirsiz; otomatik yeniden çalıştırılmadı.",
            ));
        }
        if cancel.load(std::sync::atomic::Ordering::Acquire) || crate::app::now()? >= self.expires {
            let record = json!({"status":"NOT_RUN","reason":"task cancelled or deadline elapsed before dispatch","extension_digest":bundle.fingerprint()?,"source_digest":source_digest,"input_digest":input_digest,"helper_digest":helper_digest,"authority_granted":false});
            put(
                &mut store,
                &self.task,
                &receipt_id,
                "extension.helper.receipt",
                &bytes(&record)?,
            )?;
            return Ok(record);
        }
        put(
            &mut store,
            &self.task,
            &intent,
            "extension.helper.intent",
            &bytes(
                &json!({"id":id,"helper":helper,"extension_digest":bundle.fingerprint()?,"input_digest":crate::digest(&bytes(&input)?)}),
            )?,
        )?;
        let mut files = BTreeMap::from([(".melissa-extension/input.json".into(), bytes(&input)?)]);
        let mut command = helper.command.clone();
        if let Some(skill) = &helper.skill {
            for (path, raw) in &bundle.skills[skill].files {
                files.insert(
                    format!(".melissa-extension/skills/{skill}/{path}"),
                    raw.clone(),
                );
            }
            for arg in &mut command.argv {
                if let Some(path) = arg.strip_prefix("@skill/") {
                    *arg = format!(".melissa-extension/skills/{skill}/{path}");
                }
            }
        }
        let mut runtime = Runtime::new(
            &app,
            &self.task,
            "melissa.code.test-engineer",
            self.contract.clone(),
            self.expires,
        )?;
        let sid = runtime.start_with_files(
            &app,
            &store,
            &repo,
            command,
            "",
            cancel.clone(),
            false,
            false,
            files,
        )?;
        let result = loop {
            let page = app.project_poll(&self.task, &sid, 0, 1)?;
            if page["owner_lost"] == true {
                return Err(Error::Reconciliation("Eklenti helper sahibi kayıp."));
            }
            if !matches!(
                page["state"]["phase"].as_str(),
                Some("preparing" | "running")
            ) {
                break page["state"]["result"].clone();
            }
            if cancel.load(std::sync::atomic::Ordering::Acquire) {
                let _ = app.project_input(&self.task, &sid, "helper-cancel", "cancel", "");
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        };
        runtime.close()?;
        let mut cursor = 0;
        let mut stdout = vec![];
        let mut stderr = vec![];
        loop {
            let page = app.project_poll(&self.task, &sid, cursor, 256 * 1024)?;
            for event in page["events"].as_array().ok_or(Error::Store)? {
                let v = &event["value"];
                let target = match v["event"].as_str() {
                    Some("stdout") => &mut stdout,
                    Some("stderr") => &mut stderr,
                    _ => continue,
                };
                if v["stage"] != "command" {
                    continue;
                }
                let hex = v["hex"].as_str().ok_or(Error::Store)?;
                for i in (0..hex.len()).step_by(2) {
                    target.push(u8::from_str_radix(&hex[i..i + 2], 16).map_err(|_| Error::Store)?);
                }
            }
            cursor = page["next_cursor"].as_u64().ok_or(Error::Store)?;
            if page["has_more"] == false {
                break;
            }
        }
        let record = json!({"status":result["status"],"session_id":sid,"native":result,"stdout_hex":stdout.iter().map(|b|format!("{b:02x}")).collect::<String>(),"stderr_hex":stderr.iter().map(|b|format!("{b:02x}")).collect::<String>(),"stdout_text":String::from_utf8(stdout).ok(),"stderr_text":String::from_utf8(stderr).ok(),"extension_digest":bundle.fingerprint()?,"source_digest":source_digest,"input_digest":input_digest,"helper_digest":helper_digest,"authority_granted":false});
        put(
            &mut store,
            &self.task,
            &receipt_id,
            "extension.helper.receipt",
            &bytes(&record)?,
        )?;
        Ok(record)
    }
    pub async fn event(
        &mut self,
        event: Event,
        tool: &str,
        key: &str,
        input: Value,
        cancel: Arc<AtomicBool>,
    ) -> Result<Vec<Value>> {
        let Some(bundle) = &self.contract.extensions else {
            return Ok(vec![]);
        };
        let mut receipts = vec![];
        let hooks = bundle.config.hooks.clone();
        for hook in &hooks {
            if hook.event != event
                || (!hook.tools.is_empty() && !hook.tools.iter().any(|t| t == tool))
            {
                continue;
            }
            let receipt = self
                .helper(
                    &format!("hook:{key}:{event:?}:{}", hook.id),
                    &hook.helper,
                    json!({"event":event,"tool":tool,"context":input,"hook_id":hook.id}),
                    cancel.clone(),
                )
                .await?;
            receipts.push(json!({"hook":hook.id,"receipt":receipt}));
            if receipt["status"] != "PASS" {
                return Err(if receipt["status"] == "INCONCLUSIVE" {
                    Error::Reconciliation("Hook etkisi belirsiz; tekrar yapılmadı.")
                } else {
                    Error::Verification(
                        "Hook başarısız; kaynak işlemi/kabul sonucu ayrı kaydedildi.",
                    )
                });
            }
        }
        Ok(receipts)
    }
}
pub struct Dispatcher {
    pub inner: Box<dyn melissa_model_gateway::tools::ToolDispatcher>,
    pub runner: Runner,
}
impl melissa_model_gateway::tools::ToolDispatcher for Dispatcher {
    fn definitions(&self) -> Vec<melissa_model_gateway::tools::BoundTool> {
        self.inner.definitions()
    }
    fn validate_call(
        &self,
        c: &melissa_model_gateway::ToolCall,
    ) -> melissa_model_gateway::Result<()> {
        self.inner.validate_call(c)
    }
    fn execute<'a>(
        &'a mut self,
        c: melissa_model_gateway::ToolCall,
        ctx: melissa_model_gateway::tools::ToolContext,
        cancel: Arc<AtomicBool>,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = melissa_model_gateway::Result<
                        melissa_model_gateway::tools::ToolExecution,
                    >,
                > + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            self.validate_call(&c)?;
            let context = json!({"task_id":ctx.task_id,"invocation_id":ctx.invocation_id,"arguments_digest":crate::digest(c.arguments.as_bytes())});
            self.runner
                .event(
                    Event::BeforeTool,
                    &c.name,
                    &ctx.invocation_id,
                    context.clone(),
                    cancel.clone(),
                )
                .await
                .map_err(super::tools::gateway)?;
            let outcome = self
                .inner
                .execute(c.clone(), ctx.clone(), cancel.clone())
                .await;
            let mutated = outcome.as_ref().is_ok_and(|v| {
                v.success
                    || serde_json::from_str::<Value>(&v.content)
                        .is_ok_and(|c| c["mutation_applied"] == true)
            }) && matches!(
                c.name.as_str(),
                "fs_patch" | "fs_create" | "fs_delete" | "fs_move"
            );
            let result_context = json!({"request":context,"tool_returned":outcome.is_ok(),"mutation_applied":mutated,"success":outcome.as_ref().is_ok_and(|v|v.success)});
            let after = self
                .runner
                .event(
                    Event::AfterTool,
                    &c.name,
                    &ctx.invocation_id,
                    result_context.clone(),
                    cancel.clone(),
                )
                .await;
            let mutation = if mutated {
                self.runner
                    .event(
                        Event::AfterMutation,
                        &c.name,
                        &ctx.invocation_id,
                        result_context,
                        cancel.clone(),
                    )
                    .await
            } else {
                Ok(vec![])
            };
            if let Err(error) = after.and(mutation) {
                return Ok(melissa_model_gateway::tools::ToolExecution{content:json!({"status":"FAIL","hook_error":error.to_string(),"mutation_applied":mutated,"original_observation":outcome.as_ref().ok().map(|v|&v.content),"original_error":outcome.as_ref().err().map(|e|&e.message),"retry_mutation":false}).to_string(),success:false,cancelled:false,requires_reconciliation:matches!(error,Error::Reconciliation(_))||outcome.is_err()||outcome.as_ref().is_ok_and(|v|v.requires_reconciliation),artifact_refs:outcome.as_ref().map(|v|v.artifact_refs.clone()).unwrap_or_default()});
            }
            outcome
        })
    }
}
