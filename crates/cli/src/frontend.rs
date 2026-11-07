//! Versioned local presentation API shared by the terminal and desktop.
//! No request bypasses Session/App approval, source or execution policy.
use crate::{Error, Result, app::App, session::FrontendSession};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, VecDeque},
    path::Path,
};
use tokio::sync::{mpsc, oneshot};

#[cfg(windows)]
pub(crate) mod paste;
#[cfg(windows)]
pub mod pipe;
mod presentation;
#[cfg(windows)]
pub mod terminal;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Action {
    Snapshot,
    Read {
        view: String,
    },
    Process {
        session_id: String,
        operation: String,
        after: u64,
        hex: String,
        expected_revision: u64,
    },
    Input {
        text: String,
        expected_revision: u64,
        approval_digest: Option<String>,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub version: u32,
    pub request_id: String,
    #[serde(default)]
    pub after: u64,
    pub action: Action,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Event {
    pub seq: u64,
    pub kind: String,
    pub data: Value,
    #[serde(skip)]
    bytes: usize,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Response {
    pub version: u32,
    pub epoch: String,
    pub request_id: String,
    pub ok: bool,
    pub state: Value,
    pub events: Vec<Event>,
    pub cursor: u64,
    pub replay_reset: bool,
    pub value: Value,
    pub error: Option<Value>,
}
struct Job {
    request: Request,
    reply: oneshot::Sender<Response>,
}
#[derive(Clone)]
pub struct Host {
    sender: mpsc::Sender<Job>,
}
impl Host {
    pub fn start(home: &Path, workspace: &Path) -> Result<Self> {
        let mut session = FrontendSession::new(home, workspace)?;
        let epoch = crate::app::nonce()?;
        let (sender, mut receiver) = mpsc::channel::<Job>(64);
        // A dedicated runtime owns the session and continues polling execution
        // while other clients read progress or send pause/cancel/steering.
        std::thread::Builder::new().name("melissa-ui-host".into()).spawn(move||{
            let runtime=match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                Ok(r)=>r,Err(_)=>return,
            };
            runtime.block_on(async move {
                let mut log=EventLog::default();
                let mut cache:BTreeMap<String,(String,Response)>=BTreeMap::new();
                let mut running:Option<tokio::task::JoinHandle<Result<Value>>>=None;
                let mut ticker=tokio::time::interval(std::time::Duration::from_millis(250));
                let mut previous=Value::Null;
                let mut visible:Option<VisibleText>=None;
                let (text_sender,mut text_receiver)=mpsc::unbounded_channel::<melissa_model_gateway::visible_stream::VisibleDelta>();
                let text_observer:melissa_model_gateway::visible_stream::Observer=std::sync::Arc::new(move|delta|{let _=text_sender.send(delta);});
                loop {
                    tokio::select! {
                        biased;
                        result=async {running.as_mut().unwrap().await},if running.is_some()=>{
                            while let Ok(delta)=text_receiver.try_recv(){absorb(&mut log,&mut visible,delta);}
                            finish_text(&mut log,&mut visible);
                            running=None;
                            session.revision+=1;
                            match result {
                                Ok(Ok(v))=>log.push("result",v),
                                Ok(Err(e))=>log.push("error",error(&e)),
                                Err(_)=>log.push("error",json!({"kind":"requires_reconciliation","message":"Çalıştırıcı beklenmedik kapandı; durumu inceleyin."})),
                            }
                        }
                        job=receiver.recv()=>{
                            let Some(job)=job else {
                                if running.is_none(){break;}
                                // Dropping every frontend must not drop an
                                // execution future with unknown side effects.
                                if let Some(id)=session.task() && let Ok(app)=App::open(session.home()) {
                                    let _=app.request_control(&id,"pause");
                                }
                                if let Some(run)=running.take(){let _=run.await;}
                                break;
                            };
                            let req=job.request;
                            let fingerprint=crate::digest(&serde_json::to_vec(&(req.version,&req.action)).unwrap_or_default());
                            if let Some((prior,response))=cache.get(&req.request_id) {
                                let mut response=response.clone();
                                if prior!=&fingerprint {response.ok=false;response.error=Some(error(&Error::Conflict("İstek kimliği farklı içerikle yeniden kullanıldı.")));}
                                response.state=state(&session,running.is_some(),&visible);
                                response.events=log.after(req.after);
                                response.cursor=log.seq;
                                response.replay_reset=log.expired(req.after);
                                let _=job.reply.send(response);continue;
                            }
                            let checked=validate(&req);
                            let mut value=Value::Null;
                            let result=if let Err(e)=checked {Err(e)} else {
                                match &req.action {
                                    Action::Snapshot=>Ok(()),
                                    Action::Read{view}=>session.read(view).map(|v|value=v),
                                    Action::Process{session_id,operation,after,hex,expected_revision}=>{
                                        if operation!="poll" {session.check_revision(*expected_revision).and_then(|()|session.process(session_id,operation,*after,hex,&req.request_id)).map(|v|{value=v;session.revision+=1;})}
                                        else {session.process(session_id,operation,*after,hex,&req.request_id).map(|v|value=v)}
                                    },
                                    Action::Input{text,expected_revision,approval_digest}=>{
                                        match session.check_revision(*expected_revision) {
                                            Err(e)=>Err(e),
                                            Ok(()) if matches!(text.trim(),"/run"|"/resume")=>{
                                                if running.is_some(){Err(Error::Conflict("Görev zaten çalışıyor."))}
                                                else {
                                                    session.start_run(text.trim()=="/resume").map(|(home,id,resume)|{
                                                        let observer=text_observer.clone();
                                                        running=Some(tokio::spawn(async move {melissa_model_gateway::visible_stream::observe(observer,async{App::open(&home)?.run_task(&id,resume).await}).await}));
                                                        log.push("execution_started",json!({"task_id":session.task(),"resume":resume}));
                                                    })
                                                }
                                            }
                                            Ok(()) if ["/exec","/preflight","/environment-sync"].contains(&text.split_whitespace().next().unwrap_or(""))=>{
                                                if running.is_some(){Err(Error::Conflict("Etkin görev durmadan yeni süreç başlatılamaz."))}
                                                else {session.start_project(text).map(|(home,id,operation,command)|{
                                                    running=Some(tokio::spawn(async move{App::open(&home)?.run_project_operation(&id,&operation,command).await}));
                                                    log.push("execution_started",json!({"task_id":session.task(),"operation":"project_process"}));
                                                })}
                                            }
                                            Ok(())=>session.command(text,approval_digest.as_deref(),running.is_some()).await,
                                        }
                                    }
                                }
                            };
                            for e in session.drain(){log.push(e["kind"].as_str().unwrap_or("result"),e["data"].clone());}
                            let response=Response{version:1,epoch:epoch.clone(),request_id:req.request_id.clone(),ok:result.is_ok(),
                                state:state(&session,running.is_some(),&visible),events:log.after(req.after),cursor:log.seq,
                                replay_reset:log.expired(req.after),value,error:result.err().map(|e|error(&e))};
                            if matches!(&req.action,Action::Input{..}) || matches!(&req.action,Action::Process{operation,..} if operation!="poll") {
                                let mut receipt=response.clone();receipt.events.clear();receipt.state=Value::Null;
                                cache.insert(req.request_id,(fingerprint,receipt));
                            }
                            let _=job.reply.send(response);
                        }
                        _=ticker.tick(),if running.is_some()=>{
                            let state=session.snapshot(true);
                            if state["progress"]!=previous {previous=state["progress"].clone();log.push("progress",previous.clone());}
                        }
                        Some(delta)=text_receiver.recv()=>{
                            absorb(&mut log,&mut visible,delta);
                        }
                    }
                }
            });
        })?;
        Ok(Self { sender })
    }
    pub async fn request(&self, request: Request) -> Result<Response> {
        let (reply, receive) = oneshot::channel();
        self.sender
            .send(Job { request, reply })
            .await
            .map_err(|_| Error::Unavailable("Arayüz görev bağlantısı kapandı."))?;
        receive
            .await
            .map_err(|_| Error::Unavailable("Arayüz görev yanıtı alınamadı."))
    }
}
struct VisibleText {
    id: u64,
    text: String,
}
fn state(session: &FrontendSession, busy: bool, visible: &Option<VisibleText>) -> Value {
    let mut state = session.snapshot(busy);
    state["visible_text"] = visible
        .as_ref()
        .map(|v| json!({"stream_id":v.id,"text":v.text,"tentative":true}))
        .unwrap_or(Value::Null);
    state
}
fn absorb(
    log: &mut EventLog,
    visible: &mut Option<VisibleText>,
    delta: melissa_model_gateway::visible_stream::VisibleDelta,
) {
    if visible.as_ref().is_some_and(|v| v.id != delta.stream_id) {
        finish_text(log, visible);
    }
    visible
        .get_or_insert_with(|| VisibleText {
            id: delta.stream_id,
            text: String::new(),
        })
        .text
        .push_str(&delta.text);
}
fn finish_text(log: &mut EventLog, visible: &mut Option<VisibleText>) {
    if let Some(v) = visible.take() {
        log.push(
            "assistant_text",
            json!({"stream_id":v.id,"text":v.text,"tentative":true}),
        );
    }
}
fn validate(r: &Request) -> Result {
    if r.version != 1
        || r.request_id.is_empty()
        || r.request_id.len() > 80
        || !r
            .request_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err(Error::Usage(
            "Arayüz protokolü veya istek kimliği geçersiz.",
        ));
    }
    if let Action::Input { text, .. } = &r.action {
        if text.trim().is_empty() || text.len() > 1024 * 1024 {
            return Err(Error::Usage(
                "Boş veya tek mesaj aktarım sınırını aşan girdi.",
            ));
        }
        if text.trim() == "/exit" {
            return Err(Error::Usage(
                "Bağlantıyı kapatmak için arayüzün çıkış işlemini kullan.",
            ));
        }
    }
    Ok(())
}
fn error(e: &Error) -> Value {
    json!({"kind":e.kind(),"message":e.to_string()})
}
#[derive(Default)]
struct EventLog {
    seq: u64,
    events: VecDeque<Event>,
    bytes: usize,
}
impl EventLog {
    fn push(&mut self, kind: &str, data: Value) {
        self.seq += 1;
        let bytes = serde_json::to_vec(&data).map_or(0, |v| v.len());
        self.bytes = self.bytes.saturating_add(bytes);
        self.events.push_back(Event {
            seq: self.seq,
            kind: kind.into(),
            data,
            bytes,
        });
        // This is a reconnect window, not an execution/tool/token budget.
        // Authoritative raw task evidence remains in the encrypted journal.
        while self.events.len() > 512 || self.bytes > 16 * 1024 * 1024 {
            if let Some(event) = self.events.pop_front() {
                self.bytes = self.bytes.saturating_sub(event.bytes);
            }
        }
    }
    fn after(&self, after: u64) -> Vec<Event> {
        self.events
            .iter()
            .filter(|e| e.seq > after)
            .cloned()
            .collect()
    }
    fn expired(&self, after: u64) -> bool {
        after > self.seq
            || (self.events.is_empty() && after < self.seq)
            || self
                .events
                .front()
                .is_some_and(|e| after.saturating_add(1) < e.seq)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reconnect_window_bounds_memory_and_reports_missing_history() {
        let mut log = EventLog::default();
        for _ in 0..20 {
            log.push("large", json!({"text":"x".repeat(1024*1024)}));
        }
        assert!(log.bytes <= 16 * 1024 * 1024);
        assert!(log.expired(0));
        assert!(!log.after(0).is_empty());
        assert_eq!(log.seq, 20);
        log.push("oversized", json!({"text":"x".repeat(17*1024*1024)}));
        assert!(log.expired(20));
        assert!(log.events.is_empty());
    }
}
