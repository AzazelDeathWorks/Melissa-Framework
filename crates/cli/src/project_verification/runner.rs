use super::{check, outcome};
use crate::{
    Error, Result,
    app::{App, bytes, decode, now, put},
    digest,
    project::{Command, Contract, runtime::Runtime},
};
use melissa_local_store::LocalStore;
use melissa_verification::{Outcome, aggregate, combine};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    #[serde(default)]
    pub names: Vec<String>,
    #[serde(default)]
    pub baseline: Option<String>,
}
pub struct Runner {
    pub home: PathBuf,
    pub task: String,
    pub profile: String,
    pub contract: Contract,
    pub expires: u64,
    pub store: LocalStore,
    pub final_check: bool,
}
impl Runner {
    pub fn active(&self) -> Result {
        let task = self.store.journal().task(&self.task)?;
        if !(task.record.state == melissa_core_contracts::TaskState::Running
            || (self.final_check
                && task.record.state == melissa_core_contracts::TaskState::Verifying))
            || task.record.unknown_invocation_id.is_some()
            || !task.contract.selected_profile_ids.contains(&self.profile)
            || now()? >= self.expires
        {
            return Err(Error::Denied("Doğrulama görev/profil yetkisi etkin değil."));
        }
        Ok(())
    }
    pub fn validate(&self, request: &Request, reproduce: bool) -> Result<Vec<String>> {
        let names: Vec<_> = if request.names.is_empty() {
            self.contract.checks.keys().cloned().collect()
        } else {
            request.names.clone()
        };
        if names.is_empty() {
            return Err(Error::Usage("Doğrulama için onaylı komut yok."));
        }
        let mut unique = BTreeSet::new();
        for name in &names {
            if !self.contract.checks.contains_key(name) || !unique.insert(name) {
                return Err(Error::Denied("Bilinmeyen/tekrarlı doğrulama komutu."));
            }
            if reproduce
                && self
                    .contract
                    .verification
                    .as_ref()
                    .is_some_and(|v| !v.reproduce.is_empty() && !v.reproduce.contains(name))
            {
                return Err(Error::Denied("Komut onaylı reproduction seçiminde yok."));
            }
        }
        Ok(names)
    }
    pub fn run(
        &mut self,
        request: Request,
        reproduce: bool,
        invocation: &str,
        cancel: Arc<AtomicBool>,
    ) -> Result<Value> {
        let names = self.validate(&request, reproduce)?;
        let app = App::open(&self.home)?;
        let task = self.store.journal().task(&self.task)?;
        if !(task.record.state == melissa_core_contracts::TaskState::Running
            || (self.final_check
                && task.record.state == melissa_core_contracts::TaskState::Verifying))
            || task.record.unknown_invocation_id.is_some()
            || !task.contract.selected_profile_ids.contains(&self.profile)
            || task.contract.acceptance_contract_digest != digest(&bytes(&self.contract)?)
        {
            return Err(Error::Denied(
                "Doğrulama görev/profil/sözleşme yetkisi etkin değil.",
            ));
        }
        let repo = app.source(&app.workspace(&self.store, &self.task)?)?;
        let source = repo.snapshot(&cancel)?;
        let input_digest = digest(&bytes(
            &json!({"request":request,"reproduce":reproduce,"profile":self.profile}),
        )?);
        let id = format!("verification:{}", blake3::hash(invocation.as_bytes()));
        if let Ok(raw) = self.store.get_artifact(&self.task, &id) {
            let old: Value = decode(&raw)?;
            if old["input_digest"] != input_digest || old["source_digest"] != source.digest {
                return Err(Error::Conflict(
                    "Eski doğrulama invocation'ı değişmiş kaynak/girdi için tekrar kullanılamaz.",
                ));
            }
            return Ok(old);
        }
        let intent = format!("{id}:intent");
        if self.store.artifact_metadata(&self.task, &intent).is_ok() {
            return Err(Error::Reconciliation(
                "Önceki doğrulamanın sonucu belirsiz; otomatik tekrar başlatılmadı.",
            ));
        }
        let baseline = if let Some(b) = &request.baseline {
            let m = self.store.artifact_metadata(&self.task, b)?;
            if m.logical_type != "cli.verification.receipt" {
                return Err(Error::Denied(
                    "Baseline aynı görevin doğrulama kaydı olmalı.",
                ));
            }
            Some(decode::<Value>(&self.store.get_artifact(&self.task, b)?)?)
        } else {
            None
        };
        put(
            &mut self.store,
            &self.task,
            &intent,
            "verification.intent",
            &bytes(
                &json!({"input_digest":input_digest,"source_digest":source.digest,"mode":if reproduce{"reproduce"}else{"verify"}}),
            )?,
        )?;
        let mut runtime = Runtime::new(
            &app,
            &self.task,
            &self.profile,
            self.contract.clone(),
            self.expires,
        )?;
        let mut checks = vec![];
        let mut all_status = vec![];
        let mut requires_reconciliation = false;
        for name in &names {
            let command = self.contract.checks[name].clone();
            let options = check(&self.contract, name);
            let mut attempts = vec![];
            let mut statuses = vec![];
            for index in 0..options.repetitions {
                if cancel.load(Ordering::Acquire) || now()? >= self.expires {
                    statuses.push(Outcome::NotRun);
                    break;
                }
                if repo.snapshot(&cancel)?.digest != source.digest {
                    statuses.push(Outcome::Blocked);
                    break;
                }
                let files = BTreeMap::from([
                    (
                        ".melissa-extension/verification/worker.py".into(),
                        include_bytes!("worker.py").to_vec(),
                    ),
                    (
                        ".melissa-extension/verification/input.json".into(),
                        bytes(
                            &json!({"command":command,"check":options,"max_bytes":self.contract.environment.limits.output_bytes,"environment":self.contract.environment}),
                        )?,
                    ),
                ]);
                let launcher = Command {
                    executable: "/usr/bin/python3".into(),
                    argv: vec![
                        "-I".into(),
                        "-u".into(),
                        ".melissa-extension/verification/worker.py".into(),
                    ],
                    cwd: ".".into(),
                };
                let native_invocation = format!(
                    "verify:{}",
                    blake3::hash(format!("{invocation}\0{name}\0{index}").as_bytes())
                );
                let started = runtime.start_with_files(
                    &app,
                    &self.store,
                    &repo,
                    launcher,
                    &native_invocation,
                    cancel.clone(),
                    false,
                    false,
                    files,
                );
                let sid = match started {
                    Ok(s) => s,
                    Err(e) => {
                        let prelaunch = matches!(
                            e,
                            Error::Usage(_)
                                | Error::Denied(_)
                                | Error::Conflict(_)
                                | Error::Unavailable(_)
                        );
                        let status = if prelaunch {
                            Outcome::Blocked
                        } else {
                            Outcome::Inconclusive
                        };
                        requires_reconciliation |= !prelaunch;
                        attempts.push(json!({"index":index,"outcome":status.as_str(),"reason":e.to_string(),"command_started":if prelaunch{Some(false)}else{None}}));
                        statuses.push(status);
                        break;
                    }
                };
                let native = Runtime::wait(&app, &self.task, &sid, &cancel)?;
                requires_reconciliation |= native["requires_reconciliation"] == true;
                let mut raw = vec![];
                let mut cursor = 0;
                loop {
                    let page = app.project_poll(&self.task, &sid, cursor, 256 * 1024)?;
                    for event in page["events"].as_array().ok_or(Error::Store)? {
                        let v = &event["value"];
                        if v["event"] == "stdout"
                            && let Some(hex) = v["hex"].as_str()
                        {
                            for b in (0..hex.len()).step_by(2) {
                                raw.push(
                                    u8::from_str_radix(&hex[b..b + 2], 16)
                                        .map_err(|_| Error::Store)?,
                                );
                            }
                        }
                    }
                    if page["has_more"] == false {
                        break;
                    }
                    cursor = page["next_cursor"].as_u64().ok_or(Error::Store)?;
                }
                let mut result = if native["status"] == "PASS" {
                    serde_json::from_slice::<Value>(&raw).unwrap_or(
                        json!({"outcome":"INCONCLUSIVE","reason":"collector_result_missing"}),
                    )
                } else {
                    json!({"outcome":if native["status"]=="BLOCKED"{"BLOCKED"}else{"INCONCLUSIVE"},"reason":"native_process_did_not_complete"})
                };
                if native["status"] == "PASS"
                    && (native["source_digest"] != source.digest
                        || native["environment_digest"]
                            != self.contract.environment.fingerprint()?)
                {
                    result["outcome"] = json!("INCONCLUSIVE");
                    result["reason"] = json!("source_or_environment_binding_mismatch");
                }
                result["native_result"] = native.clone();
                let detail_id = format!("{id}:{name}:{index}");
                put(
                    &mut self.store,
                    &self.task,
                    &detail_id,
                    "verification.detail",
                    &bytes(&result)?,
                )?;
                statuses.push(outcome(&result));
                attempts.push(json!({"index":index,"session_id":sid,"outcome":result["outcome"],"command_exit_code":result["command_exit_code"],"reason":result["reason"],"test_counts":result["test_counts"],"coverage":result["coverage"],"mutation_counts":result["mutation_counts"],"native_status":native["status"],"detail_artifact":detail_id}));
            }
            let status = aggregate(&statuses, usize::from(options.repetitions));
            all_status.push(status);
            checks.push(json!({"name":name,"kind":options.kind,"command_digest":digest(&bytes(&command)?),"outcome":status.as_str(),"requested_repetitions":options.repetitions,"attempts":attempts}));
            if matches!(
                status,
                Outcome::Blocked | Outcome::Inconclusive | Outcome::NotRun
            ) {
                break;
            }
        }
        runtime.close()?;
        let current = repo.snapshot(&AtomicBool::new(false))?.digest == source.digest;
        let status = if current {
            combine(&all_status, names.len())
        } else {
            Outcome::Blocked
        };
        let regression = baseline.as_ref().is_some_and(|b| {
            b["outcome"] == "FAIL"
                && b["environment_digest"]
                    == self.contract.environment.fingerprint().unwrap_or_default()
                && b["source_digest"] != source.digest
                && status == Outcome::Pass
                && b["checks"].as_array().is_some_and(|old| {
                    old.len() == checks.len()
                        && old.iter().zip(&checks).all(|(a, b)| {
                            a["name"] == b["name"] && a["command_digest"] == b["command_digest"]
                        })
                })
        });
        let failure_reproduced = reproduce && status == Outcome::Fail;
        let result = json!({"schema_version":"melissa.project-verification/v1","receipt_id":id,"mode":if reproduce{"reproduce"}else{"verify"},"task_id":self.task,"source_digest":source.digest,"source_current":current,"environment_digest":self.contract.environment.fingerprint()?,"input_digest":input_digest,"outcome":status.as_str(),"operation_status":if reproduce {if failure_reproduced{"PASS"}else if status==Outcome::Pass{"FAIL"}else{status.as_str()}}else{status.as_str()},"failure_reproduced":failure_reproduced,"regression_confirmed":regression,"baseline":request.baseline,"selected_checks":names,"all_required_checks_selected":names.len()==self.contract.checks.len(),"checks":checks,"evidence_class":"public_project_validation","protected_acceptance":false,"task_accepted":false,"model_calls":0,"requires_reconciliation":requires_reconciliation,"replay_contains_raw_logs_or_environment":false});
        put(
            &mut self.store,
            &self.task,
            &id,
            "verification.receipt",
            &bytes(&result)?,
        )?;
        Ok(result)
    }
}
