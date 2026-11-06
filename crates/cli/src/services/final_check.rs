//! Independent host execution of the reviewed public service acceptance.
use super::{
    Required,
    host::{Host, Operation},
};
use crate::{
    Error, Result,
    app::{App, bytes, nonce, put},
    project::Contract,
};
use melissa_code_quality::source::FrozenSource;
use melissa_local_store::LocalStore;
use melissa_repository::Repository;
use serde_json::{Value, json};
use std::sync::{Arc, atomic::AtomicBool};
#[allow(clippy::too_many_arguments)]
pub fn verify(
    app: &App,
    store: &mut LocalStore,
    task: &melissa_core_contracts::wire::TaskContractV1,
    source: &FrozenSource,
    repo: &Repository,
    contract: &Contract,
    expires: u64,
    cancel: Arc<AtomicBool>,
) -> Result<Value> {
    source
        .current(repo, &cancel)
        .map_err(|_| Error::Conflict("Servis son kabul öncesi kaynak değişti."))?;
    let spec = contract.services.as_ref().unwrap();
    let mut host = Host::new(
        app,
        store,
        &task.task_id,
        "melissa.code.test-engineer",
        contract.clone(),
        expires,
    )?;
    host.final_check = true;
    let mut results = vec![];
    let mut operations = vec![Operation::Start];
    operations.extend(
        spec.business_checks
            .iter()
            .map(|name| Operation::Business { name: name.clone() }),
    );
    operations.extend(spec.acceptance.iter().map(|r| match r {
        Required::Contract { name } => Operation::Contract { name: name.clone() },
        Required::Migration { name } => Operation::Migration { name: name.clone() },
    }));
    let check = (|| -> Result {
        for operation in operations {
            source
                .current(repo, &cancel)
                .map_err(|_| Error::Conflict("Servis son kabul sırasında kaynak değişti."))?;
            let sid = host.session_id().map(str::to_string);
            let receipt = host.run(
                operation,
                sid.as_deref(),
                &format!("host-services:{}", nonce()?),
                cancel.clone(),
            )?;
            let passed =
                receipt["outcome"] == "PASS" && receipt["requires_reconciliation"] == false;
            results.push(receipt);
            if !passed {
                return Err(Error::Verification(
                    "Son servis/iş kuralı/API/migration kabulü geçmedi; kayıt korundu.",
                ));
            }
        }
        let sid = host.session_id().map(str::to_string);
        let closed = host.run(
            Operation::Close,
            sid.as_deref(),
            &format!("host-services-close:{}", nonce()?),
            cancel.clone(),
        )?;
        let passed = closed["outcome"] == "PASS"
            && closed["native_result"]["tree_confirmed_stopped"] == true;
        results.push(closed);
        if !passed {
            if results.last().unwrap()["native_result"]["tree_confirmed_stopped"] == true
                && results.last().unwrap()["requires_reconciliation"] == false
            {
                return Err(Error::Unavailable(
                    "Servis durdu fakat tam günlük kanıtı kapasiteye sığmadı.",
                ));
            }
            return Err(Error::Reconciliation("Son servis duruşu kanıtlanmadı."));
        }
        Ok(())
    })();
    host.close()?;
    check?;
    let names = contract
        .checks
        .keys()
        .filter(|name| !spec.business_checks.contains(name))
        .cloned()
        .collect::<Vec<_>>();
    let project = if names.is_empty() {
        Value::Null
    } else {
        let mut runner = crate::project_verification::runner::Runner {
            home: app.home.clone(),
            task: task.task_id.clone(),
            profile: "melissa.code.test-engineer".into(),
            contract: contract.clone(),
            expires,
            store: store.connect()?,
            final_check: true,
        };
        let receipt = runner.run(
            crate::project_verification::runner::Request {
                names,
                baseline: None,
            },
            false,
            &format!("host-project:{}", nonce()?),
            cancel.clone(),
        )?;
        if receipt["outcome"] != "PASS" {
            return Err(Error::Verification(
                "Servisten bağımsız son proje kontrolü geçmedi.",
            ));
        }
        receipt
    };
    source
        .current(repo, &cancel)
        .map_err(|_| Error::Conflict("Son servis kabulünden sonra kaynak değişti."))?;
    let result = json!({"schema_version":"melissa.project-verification/v1","acceptance_kind":"host_source_bound_project_checks","source_digest":source.snapshot().digest,"environment_digest":contract.environment.fingerprint()?,"service_checks":results,"other_project_checks":project,"all_required_checks_selected":true,"private_oracle":false,"feature_coverage":"not_proven_by_public_checks_alone","release_acceptance":false});
    put(
        store,
        &task.task_id,
        &format!("cli:project-services-verification:{}", nonce()?),
        "project.verification",
        &bytes(&result)?,
    )?;
    Ok(result)
}
