//! Foreground operator sessions use the same approved task, accounting and deadline.
use super::{
    host::{Host, Operation},
    tools::Shared,
};
use crate::{
    Error, Result,
    app::{App, bytes, decode, nonce, now, put},
    control::Control,
};
use melissa_core_contracts::TaskState;
use serde_json::{Value, json};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
async fn drive(
    host: Shared,
    operation: Operation,
    control: &Control,
    signal: Arc<AtomicBool>,
) -> Result<Value> {
    let invocation = format!("operator-service:{}", nonce()?);
    let input_signal = signal.clone();
    let job = tokio::task::spawn_blocking(move || {
        let mut host = host.lock().map_err(|_| Error::Store)?;
        let sid = host.session_id().map(str::to_string);
        host.run(operation, sid.as_deref(), &invocation, input_signal)
    });
    let mut control_error = None;
    while !job.is_finished() {
        if let Err(e) = control.poll() {
            signal.store(true, Ordering::Release);
            control_error = Some(e);
        }
        if control.cancel.load(Ordering::Acquire) || control.pause.load(Ordering::Acquire) {
            signal.store(true, Ordering::Release);
        }
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    }
    let result = job
        .await
        .map_err(|_| Error::Reconciliation("Servis sahibi kesildi; intent kaydını incele."))?;
    if let Some(e) = control_error {
        return Err(e);
    }
    result
}
impl App {
    pub async fn run_services(&self, id: &str, operation: Option<Operation>) -> Result<Value> {
        if matches!(operation, Some(Operation::Start | Operation::Close)) {
            return Err(Error::Usage(
                "Tek adımda contract/migration/business/status seç; kalıcı oturum için services session.",
            ));
        }
        let mut store = self.store(id)?;
        let task = store.journal().task(id)?;
        let execution = self
            .execution(&store, id)?
            .ok_or(Error::Denied("Onaylı yürütülebilir proje görevi gerekli."))?;
        let contract = execution
            .recipe
            .project
            .as_ref()
            .ok_or(Error::Denied("Genel proje görevi gerekli."))?;
        let spec = contract
            .services
            .as_ref()
            .ok_or(Error::Denied("Görevde incelenmiş servis sözleşmesi yok."))?;
        if let Some(op) = &operation {
            op.validate(spec)?;
        }
        let resumed = task.record.state == TaskState::Running
            && self
                .live(id)
                .is_ok_and(|s| s.phase == "paused" && !s.active())
            && crate::engine::native_stop_proven(&store, id)?;
        if (task.record.state != TaskState::Queued && !resumed) || now()? >= execution.expires_at_ms
        {
            return Err(Error::Conflict(
                "Onaylı queued veya güvenli paused görev gerekli.",
            ));
        }
        let approval: Value = decode(&store.get_artifact(id, crate::app::APPROVAL)?)?;
        let decision: Value = decode(&store.get_artifact(
            id,
            &format!(
                "cli:decision:{}",
                approval["request_id"].as_str().ok_or(Error::Store)?
            ),
        )?)?;
        if decision["scope"] != "execute_recipe" {
            return Err(Error::Denied("Servis yürütmesi onaylı değil."));
        }
        let workspace = self.workspace(&store, id)?;
        let repo = self.source(&workspace)?;
        if !resumed {
            repo.validate_snapshot(&workspace.snapshot, &AtomicBool::new(false))?;
        }
        let catalog = crate::resources::catalog()?;
        let profile = execution
            .recipe
            .agents
            .iter()
            .find(|s| s.write)
            .or_else(|| {
                execution
                    .recipe
                    .agents
                    .iter()
                    .find(|s| s.profile == "melissa.code.test-engineer")
            })
            .ok_or(Error::Denied("Süreç yetkili uzman adımı gerekli."))?
            .profile
            .clone();
        if !catalog.profiles[&profile]
            .tool_ids
            .iter()
            .any(|s| s == "process.exec")
        {
            return Err(Error::Denied("Uzmanın servis süreç yetkisi yok."));
        }
        if !crate::views::image_present(&contract.environment.image_id) {
            return Err(Error::Unavailable(
                "Servis image'i yerelde yok; model/API çağrısı yapılmadı.",
            ));
        }
        if !resumed {
            let leased = store.transition_task(id, task.record.version, TaskState::Leased, None)?;
            store.transition_task(id, leased.record.version, TaskState::Running, None)?;
        }
        let signal = Arc::new(AtomicBool::new(false));
        let cancel = Arc::new(AtomicBool::new(false));
        let pause = Arc::new(AtomicBool::new(false));
        let control = Control::new(self, id, cancel.clone(), pause.clone())?;
        let host = Arc::new(Mutex::new(Host::new(
            self,
            &store,
            id,
            &profile,
            contract.clone(),
            execution.expires_at_ms,
        )?));
        control.publish(
            "agent",
            json!({"task_id":id}),
            json!({"operation":"service_session"}),
            json!({}),
            json!({}),
        )?;
        let interactive = operation.is_none();
        let result:Result<Value>=async{
   let start=drive(host.clone(),Operation::Start,&control,signal.clone()).await?;
   if start["outcome"]!="PASS"{return Ok(start)}
   let value=if let Some(op)=operation{drive(host.clone(),op,&control,signal.clone()).await?}else{
    use std::io::BufRead;
    println!("{}",start);
    let(tx,mut rx)=tokio::sync::mpsc::unbounded_channel();
    let _reader=std::thread::Builder::new().name("service-terminal-input".into()).spawn(move||{for line in std::io::stdin().lock().lines(){if tx.send(line).is_err(){break;}}})?;
    let mut last=start.clone();
    loop{
     control.poll()?;
     if cancel.load(Ordering::Acquire)||pause.load(Ordering::Acquire)||now()?>=execution.expires_at_ms{signal.store(true,Ordering::Release);break;}
     tokio::select!{
      line=rx.recv()=>{
       let Some(line)=line else{break};let line=line?;let text=line.trim();if text.is_empty(){continue}
       if ["/close","/exit","close"].contains(&text){break;}
       let op:Operation=serde_json::from_value(melissa_model_gateway::strict_json(text).map_err(|_|Error::Usage("Servis komutu JSON olmalı."))?).map_err(|_|Error::Usage("Servis komutu geçersiz."))?;
       if matches!(op,Operation::Start|Operation::Close){return Err(Error::Usage("Oturumu kapatmak için /close kullan."))}
       last=drive(host.clone(),op,&control,signal.clone()).await?;println!("{}",last);
      },
      _=tokio::time::sleep(std::time::Duration::from_millis(100))=>{}
     }
    }last
   };
   let closed=if !signal.load(Ordering::Acquire){Some(drive(host.clone(),Operation::Close,&control,signal.clone()).await?)}else{None};
   Ok(json!({"result":value,"start":start,"close":closed}))
  }.await;
        host.lock().map_err(|_| Error::Store)?.close()?;
        if !crate::engine::native_stop_proven(&store, id)? {
            return Err(Error::Reconciliation(
                "Servis native süreç duruşu kanıtlanmadı.",
            ));
        }
        let snapshot = repo.snapshot(&AtomicBool::new(false))?;
        put(
            &mut store,
            id,
            &format!("cli:paused-source:{}", control.session),
            "paused.source",
            &bytes(&snapshot)?,
        )?;
        let t = store.journal().task(id)?;
        let state = if cancel.load(Ordering::Acquire) {
            store.transition_task(id, t.record.version, TaskState::Cancelled, None)?;
            "cancelled"
        } else {
            "paused"
        };
        control.publish(
            state,
            json!({"task_id":id,"state":state}),
            json!({"operation":"service_session","interactive":interactive}),
            serde_json::to_value(store.journal().model_budget(id)?).map_err(|_| Error::Store)?,
            json!({}),
        )?;
        let body = result?;
        let outcome = if body["close"].is_object() && body["close"]["outcome"] != "PASS" {
            body["close"]["outcome"].as_str().unwrap_or("INCONCLUSIVE")
        } else {
            body["result"]["outcome"]
                .as_str()
                .or(body["outcome"].as_str())
                .unwrap_or("INCONCLUSIVE")
        };
        Ok(
            json!({"command":"services","task_id":id,"state":state,"outcome":outcome,"session":body,"model_calls":0,"task_accepted":false}),
        )
    }
}
