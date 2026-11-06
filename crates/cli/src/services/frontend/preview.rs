//! Foreground, token-protected loopback HTTP preview. Docker ports stay closed.
//! Requests traverse the exact task-owned stdin/evidence path. No filesystem
//! serving, daemon socket, external URL or broad reverse proxy is exposed.
use super::super::{host::Operation, tools::Shared};
use crate::{
    Error, Result,
    app::{bytes, decode, nonce},
};
use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};
pub struct Preview {
    pub url: String,
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}
impl Preview {
    pub fn start(host: Shared, service: String) -> Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        listener.set_nonblocking(true)?;
        let address = listener.local_addr()?.to_string();
        let token = nonce()?;
        let url = format!("http://{address}/__melissa/{token}");
        let stop = Arc::new(AtomicBool::new(false));
        let signal = stop.clone();
        let join = thread::Builder::new()
            .name("owned-preview-http".into())
            .spawn(move || {
                while !signal.load(Ordering::Acquire) {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                            let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
                            if let Err(error) =
                                serve(&mut stream, &address, &token, &service, &host, &signal)
                            {
                                let _ = reply(
                                    &mut stream,
                                    502,
                                    "text/plain",
                                    error.to_string().as_bytes(),
                                    "",
                                );
                            }
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(30))
                        }
                        Err(_) => break,
                    }
                }
            })?;
        Ok(Self {
            url,
            stop,
            join: Some(join),
        })
    }
}
impl Drop for Preview {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}
fn reply(stream: &mut TcpStream, status: u16, kind: &str, body: &[u8], extra: &str) -> Result {
    if kind.contains(['\r', '\n']) {
        return Err(Error::Store);
    }
    write!(
        stream,
        "HTTP/1.1 {status} Response\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\nReferrer-Policy: no-referrer\r\nX-Content-Type-Options: nosniff\r\n{extra}\r\n",
        body.len()
    )?;
    stream.write_all(body)?;
    Ok(())
}
fn serve(
    stream: &mut TcpStream,
    address: &str,
    token: &str,
    service: &str,
    host: &Shared,
    signal: &Arc<AtomicBool>,
) -> Result {
    let mut raw = vec![];
    let mut block = [0; 4096];
    let end = loop {
        let n = stream.read(&mut block)?;
        if n == 0 {
            return Err(Error::Usage("Eksik HTTP isteği."));
        }
        raw.extend_from_slice(&block[..n]);
        if let Some(i) = raw.windows(4).position(|b| b == b"\r\n\r\n") {
            break i + 4;
        }
        if raw.len() > 32768 {
            return reply(stream, 431, "text/plain", b"Header capacity", "");
        }
    };
    if end > 32768 {
        return reply(stream, 431, "text/plain", b"Header capacity", "");
    }
    let header =
        std::str::from_utf8(&raw[..end]).map_err(|_| Error::Usage("HTTP başlığı geçersiz."))?;
    let mut lines = header.split("\r\n");
    let request: Vec<_> = lines.next().unwrap().split(' ').collect();
    if request.len() != 3 || request[2] != "HTTP/1.1" {
        return reply(stream, 400, "text/plain", b"HTTP/1.1 required", "");
    }
    let method = request[0].to_owned();
    let path = request[1].to_owned();
    let mut fields = std::collections::BTreeMap::new();
    for line in lines.filter(|s| !s.is_empty()) {
        let (k, v) = line
            .split_once(':')
            .ok_or(Error::Usage("HTTP başlığı geçersiz."))?;
        if fields
            .insert(k.to_ascii_lowercase(), v.trim().to_string())
            .is_some()
        {
            return reply(stream, 400, "text/plain", b"Duplicate header", "");
        }
    }
    if fields.get("host").map(String::as_str) != Some(address)
        || fields.contains_key("transfer-encoding")
        || fields.contains_key("upgrade")
    {
        return reply(
            stream,
            403,
            "text/plain",
            b"Host, transfer or upgrade denied",
            "",
        );
    }
    let origin = format!("http://{address}");
    if fields.get("origin").is_some_and(|s| s != &origin)
        || fields
            .get("sec-fetch-site")
            .is_some_and(|s| !["same-origin", "none"].contains(&s.as_str()))
    {
        return reply(stream, 403, "text/plain", b"Cross-origin denied", "");
    }
    if method == "GET" && path == format!("/__melissa/{token}") {
        return reply(
            stream,
            303,
            "text/plain",
            b"Open preview",
            &format!(
                "Location: /\r\nSet-Cookie: melissa_preview={token}; HttpOnly; SameSite=Strict; Path=/\r\n"
            ),
        );
    }
    if !fields.get("cookie").is_some_and(|s| {
        s.split(';')
            .any(|c| c.trim() == format!("melissa_preview={token}"))
    }) {
        return reply(
            stream,
            403,
            "text/plain",
            b"Preview capability required",
            "",
        );
    }
    if !["GET", "HEAD", "OPTIONS"].contains(&method.as_str())
        && fields.get("origin") != Some(&origin)
    {
        return reply(stream, 403, "text/plain", b"Same-origin write required", "");
    }
    let length = fields
        .get("content-length")
        .map(|s| s.parse::<usize>())
        .transpose()
        .map_err(|_| Error::Usage("HTTP boyutu geçersiz."))?
        .unwrap_or(0);
    if length > 1024 * 1024 {
        return reply(stream, 413, "text/plain", b"Body capacity", "");
    }
    while raw.len() < end + length {
        let n = stream.read(&mut block)?;
        if n == 0 {
            return Err(Error::Usage("Eksik HTTP gövdesi."));
        }
        raw.extend_from_slice(&block[..n]);
    }
    let body_hex = raw[end..end + length]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let mut owner = host.lock().map_err(|_| Error::Store)?;
    let sid = owner.session_id().map(str::to_string);
    let receipt = match owner.run(
        Operation::Preview {
            service: service.into(),
            method: method.clone(),
            path,
            content_type: fields.get("content-type").cloned(),
            body_hex,
        },
        sid.as_deref(),
        &format!("operator-preview:{}", nonce()?),
        signal.clone(),
    ) {
        Ok(value) => value,
        Err(Error::Conflict(_) | Error::Denied(_)) => {
            return reply(
                stream,
                409,
                "application/json",
                b"{\"outcome\":\"BLOCKED\",\"reason\":\"source_or_task_not_current\"}",
                "",
            );
        }
        Err(error) => return Err(error),
    };
    if receipt["outcome"] != "PASS" {
        return reply(
            stream,
            409,
            "application/json",
            &bytes(
                &json!({"outcome":receipt["outcome"],"receipt_id":receipt["receipt_id"],"reason":"preview_source_or_service_not_current; no retry"}),
            )?,
            "",
        );
    }
    let detail: Value = decode(&owner.store.get_artifact(
        &owner.task,
        receipt["detail_artifact"].as_str().ok_or(Error::Store)?,
    )?)?;
    let content = super::baseline::unhex(detail["body"]["data_hex"].as_str().ok_or(Error::Store)?)?;
    reply(
        stream,
        detail["body"]["status"].as_u64().ok_or(Error::Store)? as u16,
        detail["body"]["content_type"]
            .as_str()
            .ok_or(Error::Store)?,
        if method == "HEAD" { &[] } else { &content },
        "",
    )
}
