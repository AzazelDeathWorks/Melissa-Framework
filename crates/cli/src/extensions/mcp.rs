//! Finite MCP client: pinned HTTP/stdio tools; server annotations never grant effects.
use super::headers::{parameters as header_parameters, value as header_value};
use super::http::{Endpoint, Http};
use crate::{
    Error, Result,
    app::{App, bytes},
    project::{Command, Contract, runtime::Runtime},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "transport", rename_all = "snake_case", deny_unknown_fields)]
pub enum Transport {
    Http { endpoint: Endpoint },
    Stdio { command: Command },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Tool {
    pub description: String,
    pub input_schema: Value,
    #[serde(default)]
    pub writes: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Server {
    pub connection: Transport,
    pub protocol_version: String,
    pub tools: BTreeMap<String, Tool>,
}
impl Server {
    pub fn validate(&self) -> Result {
        if !["2025-06-18", "2025-11-25", "2026-07-28"].contains(&self.protocol_version.as_str()) {
            return Err(Error::Usage(
                "Desteklenen açık MCP protokol revizyonu gerekli.",
            ));
        }
        match &self.connection {
            Transport::Http { endpoint } => endpoint.validate()?,
            Transport::Stdio { command } => command.validate()?,
        }
        if self.tools.is_empty() {
            return Err(Error::Usage("MCP açık araç allowlist gerektirir."));
        }
        for (id, tool) in &self.tools {
            if id.is_empty()
                || id.len() > 128
                || !id
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c))
            {
                return Err(Error::Usage("MCP araç adı geçersiz."));
            }
            if tool.input_schema["type"] != "object" {
                return Err(Error::Usage("MCP argüman şeması object olmalı."));
            }
            melissa_model_gateway::schema::validate_schema(&tool.input_schema)
                .map_err(|_| Error::Usage("MCP şema profili desteklenmiyor; kontrol atlanmadı."))?;
            header_parameters(&tool.input_schema, &json!({}))?;
        }
        Ok(())
    }
}
struct Stdio {
    source_digest: String,
    runtime: Runtime,
    sid: String,
    task: String,
    cursor: u64,
    buffer: Vec<u8>,
}
pub struct Client {
    server: Server,
    http: Option<Http>,
    stdio: Option<Stdio>,
    sequence: u64,
    session: Option<String>,
    initialized: bool,
    pub identity: Value,
}
impl Client {
    pub fn new(server: Server) -> Result<Self> {
        server.validate()?;
        Ok(Self {
            server,
            http: None,
            stdio: None,
            sequence: 0,
            session: None,
            initialized: false,
            identity: Value::Null,
        })
    }
    pub async fn connect(
        &mut self,
        app: &App,
        store: melissa_local_store::LocalStore,
        task: &str,
        contract: &Contract,
        expires: u64,
        cancel: Arc<AtomicBool>,
    ) -> Result {
        if let Some(stdio) = self.stdio.as_mut() {
            let repo = app.source(&app.workspace(&store, task)?)?;
            if repo.snapshot(&AtomicBool::new(false))?.digest != stdio.source_digest {
                stdio.runtime.close()?;
                self.stdio = None;
                self.initialized = false;
            }
        }
        if self.initialized {
            return Ok(());
        }
        match &self.server.connection {
            Transport::Http { endpoint } => self.http = Some(Http::new(endpoint.clone())?),
            Transport::Stdio { command } => {
                let repo = app.source(&app.workspace(&store, task)?)?;
                let mut runtime = Runtime::new(
                    app,
                    task,
                    "melissa.code.test-engineer",
                    contract.clone(),
                    expires,
                )?
                .with_private_input();
                let sid = runtime.start(
                    app,
                    &store,
                    &repo,
                    command.clone(),
                    "",
                    cancel.clone(),
                    false,
                    false,
                )?;
                self.stdio = Some(Stdio {
                    source_digest: repo.snapshot(&AtomicBool::new(false))?.digest,
                    runtime,
                    sid,
                    task: task.into(),
                    cursor: 0,
                    buffer: vec![],
                });
            }
        }
        if self.server.protocol_version == "2026-07-28" {
            let response = self
                .rpc(app, "server/discover", json!({}), cancel.clone())
                .await?;
            self.identity = response;
        } else {
            let response=self.rpc(app,"initialize",json!({"protocolVersion":self.server.protocol_version,"capabilities":{},"clientInfo":{"name":"melissa","version":env!("CARGO_PKG_VERSION")}}),cancel.clone()).await?;
            if response["protocolVersion"] != self.server.protocol_version {
                return Err(Error::Denied(
                    "MCP sürüm yanıtı onaylı revizyonla eşleşmiyor.",
                ));
            }
            self.identity = response;
            self.notify(app, "notifications/initialized", json!({}), cancel.clone())
                .await?;
        }
        // The remote may advertise extra tools, but only reviewed names and
        // exact schemas become callable. Duplicate/cyclic pages are rejected.
        let mut cursor = Value::Null;
        let mut cursors = BTreeSet::new();
        let mut listed = BTreeMap::new();
        loop {
            let params = if cursor.is_null() {
                json!({})
            } else {
                json!({"cursor":cursor})
            };
            let response = self.rpc(app, "tools/list", params, cancel.clone()).await?;
            for tool in response["tools"]
                .as_array()
                .ok_or(Error::Unavailable("MCP tools/list yanıtı geçersiz."))?
            {
                let name = tool["name"]
                    .as_str()
                    .ok_or(Error::Unavailable("MCP araç adı yok."))?;
                if listed
                    .insert(name.to_owned(), tool["inputSchema"].clone())
                    .is_some()
                    || listed.len() > 4096
                {
                    return Err(Error::Unavailable(
                        "MCP araç kataloğu tekrarlı veya kapasiteyi aşıyor.",
                    ));
                }
            }
            cursor = response["nextCursor"].clone();
            if cursor.is_null() {
                break;
            }
            if !cursor.is_string() || !cursors.insert(cursor.to_string()) || cursors.len() > 128 {
                return Err(Error::Unavailable("MCP katalog imleci geçersiz/tekrarlı."));
            }
        }
        for (name, tool) in &self.server.tools {
            if listed.get(name) != Some(&tool.input_schema) {
                self.initialized = false;
                return Err(Error::Denied(
                    "MCP araç şeması değişti veya araç kayıp; yeniden konfigürasyon/onay gerekli.",
                ));
            }
        }
        self.initialized = true;
        Ok(())
    }
    pub async fn call(
        &mut self,
        app: &App,
        name: &str,
        args: Value,
        writes: bool,
        cancel: Arc<AtomicBool>,
    ) -> Result<Value> {
        if !self.initialized {
            return Err(Error::Denied("MCP bağlantısı doğrulanmadı."));
        }
        let tool = self
            .server
            .tools
            .get(name)
            .ok_or(Error::Denied("MCP aracı allowlist dışında."))?;
        if tool.writes && !writes {
            return Err(Error::Denied(
                "Salt okunur uzman uzak yazma aracını çağıramaz.",
            ));
        }
        melissa_model_gateway::schema::validate_output(&tool.input_schema, &args.to_string())
            .map_err(|_| Error::Usage("MCP argümanları onaylı şemaya uymuyor."))?;
        let result = self
            .rpc(
                app,
                "tools/call",
                json!({"name":name,"arguments":args}),
                cancel,
            )
            .await
            .map_err(|_| {
                Error::Reconciliation(
                    "MCP tools/call tamamlanma yanıtı doğrulanmadı; otomatik tekrar yok.",
                )
            })?;
        if result["resultType"]
            .as_str()
            .is_some_and(|v| v != "complete")
        {
            return Err(Error::Unavailable(
                "MCP ek giriş/uzak görev sonucu desteklenmiyor; nested istek yürütülmedi.",
            ));
        }
        if !result["content"].is_array() || result.get("isError").is_some_and(|v| !v.is_boolean()) {
            return Err(Error::Reconciliation(
                "MCP araç sonuç zarfı geçersiz; uzak tamamlanma başarı sayılmadı.",
            ));
        }
        Ok(result)
    }
    fn message(&mut self, method: &str, mut params: Value, notification: bool) -> Value {
        if self.server.protocol_version == "2026-07-28" {
            params["_meta"] = json!({"io.modelcontextprotocol/protocolVersion":self.server.protocol_version,"io.modelcontextprotocol/clientInfo":{"name":"melissa","version":env!("CARGO_PKG_VERSION")},"io.modelcontextprotocol/clientCapabilities":{}});
        }
        self.sequence += 1;
        if notification {
            json!({"jsonrpc":"2.0","method":method,"params":params})
        } else {
            json!({"jsonrpc":"2.0","id":self.sequence,"method":method,"params":params})
        }
    }
    async fn notify(
        &mut self,
        app: &App,
        method: &str,
        params: Value,
        cancel: Arc<AtomicBool>,
    ) -> Result {
        let message = self.message(method, params, true);
        self.exchange(app, &message, true, cancel).await.map(|_| ())
    }
    async fn rpc(
        &mut self,
        app: &App,
        method: &str,
        params: Value,
        cancel: Arc<AtomicBool>,
    ) -> Result<Value> {
        let message = self.message(method, params, false);
        let response = self.exchange(app, &message, false, cancel).await?;
        if response["jsonrpc"] != "2.0"
            || response["id"] != message["id"]
            || response.get("method").is_some()
        {
            return Err(Error::Unavailable("MCP yanıt kimliği/yönü geçersiz."));
        }
        if response.get("error").is_some() {
            return Err(Error::Unavailable(
                "MCP JSON-RPC hata yanıtı; otomatik tekrar yok.",
            ));
        }
        response
            .get("result")
            .cloned()
            .ok_or(Error::Unavailable("MCP result eksik."))
    }
    async fn exchange(
        &mut self,
        app: &App,
        message: &Value,
        notification: bool,
        cancel: Arc<AtomicBool>,
    ) -> Result<Value> {
        if let Some(http) = &self.http {
            let Transport::Http { endpoint } = &self.server.connection else {
                return Err(Error::Store);
            };
            let mut request = http
                .request(reqwest::Method::POST, &endpoint.url)
                .header("Accept", "application/json, text/event-stream")
                .header("MCP-Protocol-Version", &self.server.protocol_version)
                .json(message);
            if self.server.protocol_version == "2026-07-28" {
                request = request.header(
                    "Mcp-Method",
                    message["method"].as_str().ok_or(Error::Store)?,
                );
                if let Some(name) = message["params"]["name"].as_str() {
                    request = request.header("Mcp-Name", header_value(name));
                    if let Some(tool) = self.server.tools.get(name) {
                        for (header, value) in
                            header_parameters(&tool.input_schema, &message["params"]["arguments"])?
                        {
                            request = request.header(header, value);
                        }
                    }
                }
            }
            if let Some(session) = &self.session {
                request = request.header("Mcp-Session-Id", session);
            }
            let (status, headers, raw) = http
                .send_rpc(
                    request,
                    cancel,
                    if notification {
                        None
                    } else {
                        Some(message["id"].clone())
                    },
                )
                .await?;
            if let Some(session) = headers.get("Mcp-Session-Id") {
                if self.server.protocol_version == "2026-07-28" {
                    return Err(Error::Unavailable(
                        "Modern MCP yanıtında beklenmeyen session header.",
                    ));
                }
                let session = session
                    .to_str()
                    .map_err(|_| Error::Unavailable("MCP oturum kimliği geçersiz."))?;
                if session.len() > 1024
                    || session.is_empty()
                    || !session.bytes().all(|v| v.is_ascii_graphic())
                {
                    return Err(Error::Unavailable("MCP oturum kimliği geçersiz."));
                }
                if self.session.as_ref().is_some_and(|old| old != session) {
                    return Err(Error::Unavailable("MCP oturumu beklenmeden değişti."));
                }
                self.session = Some(session.into());
            }
            if notification {
                return if status == 202 {
                    Ok(Value::Null)
                } else {
                    Err(Error::Unavailable("MCP bildirim kabul edilmedi."))
                };
            }
            if status != 200 {
                return Err(Error::Unavailable(
                    "MCP HTTP reddi; kimlik/izin/protokol yeniden incelenmeli.",
                ));
            }
            let content = headers
                .get("Content-Type")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            return if content.starts_with("text/event-stream") {
                sse_response(&raw, &message["id"])
            } else if content.starts_with("application/json") {
                melissa_model_gateway::strict_json(
                    std::str::from_utf8(&raw)
                        .map_err(|_| Error::Unavailable("MCP UTF-8 geçersiz."))?,
                )
                .map_err(|_| Error::Unavailable("MCP JSON geçersiz."))
            } else {
                Err(Error::Unavailable("MCP Content-Type desteklenmiyor."))
            };
        }
        let stdio = self.stdio.as_mut().ok_or(Error::Store)?;
        let mut raw = bytes(message)?;
        raw.push(b'\n');
        // Chunk the transport frames, not the logical MCP message. Only this
        // adapter can write protocol stdin; generic project_process cannot
        // bypass the reviewed tools/list and tools/call allowlist.
        for (index, chunk) in raw.chunks(30000).enumerate() {
            let hex = chunk.iter().map(|b| format!("{b:02x}")).collect::<String>();
            app.project_private_input(
                &stdio.task,
                &stdio.sid,
                &format!("mcp-{}-{index}", self.sequence),
                "stdin",
                &hex,
            )?;
        }
        if notification {
            return Ok(Value::Null);
        }
        let until = tokio::time::Instant::now() + Duration::from_secs(300);
        loop {
            if cancel.load(Ordering::Acquire) || tokio::time::Instant::now() >= until {
                app.project_input(&stdio.task, &stdio.sid, "mcp-cancel", "cancel", "")?;
                return Err(Error::Reconciliation(
                    "MCP stdio iptal/zaman aşımı; çağrı tekrar edilmedi.",
                ));
            }
            let page = app.project_poll(&stdio.task, &stdio.sid, stdio.cursor, 256 * 1024)?;
            for event in page["events"].as_array().ok_or(Error::Store)? {
                let v = &event["value"];
                if v["event"] == "stdout" && v["stage"] == "command" {
                    let hex = v["hex"].as_str().ok_or(Error::Store)?;
                    for i in (0..hex.len()).step_by(2) {
                        stdio.buffer.push(
                            u8::from_str_radix(&hex[i..i + 2], 16).map_err(|_| Error::Store)?,
                        );
                    }
                }
            }
            stdio.cursor = page["next_cursor"].as_u64().ok_or(Error::Store)?;
            if stdio.buffer.len() > 8 * 1024 * 1024 {
                return Err(Error::Unavailable("MCP stdio yanıt kapasitesi aşıldı."));
            }
            while let Some(end) = stdio.buffer.iter().position(|b| *b == b'\n') {
                let raw = stdio.buffer.drain(..=end).collect::<Vec<_>>();
                let response = melissa_model_gateway::strict_json(
                    std::str::from_utf8(&raw)
                        .map_err(|_| Error::Unavailable("MCP stdio UTF-8 geçersiz."))?,
                )
                .map_err(|_| Error::Unavailable("MCP stdio JSON geçersiz."))?;
                if response.get("id").is_some() {
                    if response["id"] != message["id"] {
                        return Err(Error::Unavailable("MCP stdio yanıt kimliği beklenmiyor."));
                    }
                    return Ok(response);
                }
                if response.get("method").is_none() {
                    return Err(Error::Unavailable("MCP stdio bildirim geçersiz."));
                }
            }
            if page["owner_lost"] == true
                || !matches!(
                    page["state"]["phase"].as_str(),
                    Some("preparing" | "running")
                )
            {
                return Err(Error::Reconciliation(
                    "MCP stdio yanıtı gelmeden süreç durdu.",
                ));
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
    pub async fn close(&mut self, cancel: Arc<AtomicBool>) -> Result {
        self.initialized = false;
        if let Some(stdio) = self.stdio.as_mut() {
            stdio.runtime.close()?;
        }
        if let (Some(http), Some(session), Transport::Http { endpoint }) =
            (&self.http, &self.session, &self.server.connection)
        {
            let (status, _, _) = tokio::time::timeout(
                Duration::from_secs(5),
                http.send(
                    http.request(reqwest::Method::DELETE, &endpoint.url)
                        .header("Mcp-Session-Id", session)
                        .header("MCP-Protocol-Version", &self.server.protocol_version),
                    cancel,
                ),
            )
            .await
            .map_err(|_| {
                Error::Reconciliation(
                    "MCP oturum kapatma süresi doldu; uzak kapanış iddia edilmedi.",
                )
            })??;
            if !matches!(status, 200 | 202 | 204 | 404 | 405) {
                return Err(Error::Unavailable(
                    "MCP session kapatma yanıtı doğrulanmadı.",
                ));
            }
        }
        self.session = None;
        Ok(())
    }
}
pub fn sse_response(raw: &[u8], id: &Value) -> Result<Value> {
    let text = std::str::from_utf8(raw)
        .map_err(|_| Error::Unavailable("MCP SSE UTF-8 geçersiz."))?
        .replace("\r\n", "\n");
    let mut found = None;
    let Some(end) = text.rfind("\n\n") else {
        return Err(Error::Unavailable("MCP SSE tamamlanmış frame yok."));
    };
    for block in text[..end].split("\n\n") {
        let data = block
            .lines()
            .filter_map(|line| {
                line.strip_prefix("data:")
                    .map(|s| s.strip_prefix(' ').unwrap_or(s))
            })
            .collect::<Vec<_>>()
            .join("\n");
        if data.is_empty() {
            continue;
        }
        let v = melissa_model_gateway::strict_json(&data)
            .map_err(|_| Error::Unavailable("MCP SSE JSON geçersiz."))?;
        if v.get("id").is_some() {
            if v["id"] != *id || found.is_some() || v.get("method").is_some() {
                return Err(Error::Unavailable("MCP SSE yanıt kimliği/yönü geçersiz."));
            }
            found = Some(v);
        }
    }
    found.ok_or(Error::Unavailable("MCP SSE son yanıtı yok."))
}
