//! Exact reviewed destinations, no redirects/proxy inheritance or automatic replay.
use crate::{Error, Result};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use url::Url;
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Endpoint {
    pub url: String,
    #[serde(default)]
    pub auth_env: Option<String>,
    #[serde(default)]
    pub loopback: bool,
    pub timeout_ms: u64,
    pub max_bytes: usize,
}
impl Endpoint {
    pub fn validate(&self) -> Result {
        let u = Url::parse(&self.url).map_err(|_| Error::Usage("Eklenti URL geçersiz."))?;
        let local = matches!(u.host_str(), Some("127.0.0.1" | "[::1]" | "::1"));
        if !u.username().is_empty()
            || u.password().is_some()
            || u.query().is_some()
            || u.fragment().is_some()
            || u.host_str().is_none()
            || if self.loopback {
                !local || !matches!(u.scheme(), "http" | "https")
            } else {
                u.scheme() != "https" || local
            }
        {
            return Err(Error::Denied(
                "HTTPS veya açık loopback fixture, secretsiz sabit URL gerekli.",
            ));
        }
        if self.timeout_ms == 0
            || self.timeout_ms > 86_400_000
            || self.max_bytes == 0
            || self.max_bytes > 8 * 1024 * 1024
        {
            return Err(Error::Usage(
                "HTTP fiziksel süre/yanıt kapasitesi geçersiz.",
            ));
        }
        if self.auth_env.as_ref().is_some_and(|v| {
            v.is_empty() || !v.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
        }) {
            return Err(Error::Usage("Credential ortam değişkeni geçersiz."));
        }
        Ok(())
    }
}
pub struct Http {
    client: reqwest::Client,
    endpoint: Endpoint,
    secret: Option<zeroize::Zeroizing<String>>,
}
impl Http {
    pub fn new(endpoint: Endpoint) -> Result<Self> {
        endpoint.validate()?;
        let secret = endpoint
            .auth_env
            .as_ref()
            .map(|name| {
                std::env::var(name)
                    .map(zeroize::Zeroizing::new)
                    .map_err(|_| Error::Unavailable("Eklenti credential değişkeni bulunamadı."))
            })
            .transpose()?;
        if secret.as_ref().is_some_and(|v| {
            v.is_empty() || v.len() > 8192 || !v.bytes().all(|c| c.is_ascii_graphic())
        }) {
            return Err(Error::Denied("Eklenti credential biçimi geçersiz."));
        }
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_millis(endpoint.timeout_ms))
            .build()
            .map_err(|_| Error::Unavailable("HTTP istemcisi hazırlanamadı."))?;
        Ok(Self {
            client,
            endpoint,
            secret,
        })
    }
    pub fn request(&self, method: reqwest::Method, url: &str) -> reqwest::RequestBuilder {
        let mut request = self.client.request(method, url);
        if let Some(secret) = &self.secret {
            request = request.bearer_auth(secret.as_str());
        }
        request
    }
    pub async fn send(
        &self,
        request: reqwest::RequestBuilder,
        cancel: Arc<AtomicBool>,
    ) -> Result<(u16, reqwest::header::HeaderMap, Vec<u8>)> {
        self.send_rpc(request, cancel, None).await
    }
    pub async fn send_rpc(
        &self,
        request: reqwest::RequestBuilder,
        cancel: Arc<AtomicBool>,
        response_id: Option<serde_json::Value>,
    ) -> Result<(u16, reqwest::header::HeaderMap, Vec<u8>)> {
        let work = async {
            let response = request.send().await.map_err(|_| {
                Error::Reconciliation("HTTP gönderimi/yanıtı belirsiz; otomatik tekrar yok.")
            })?;
            let status = response.status().as_u16();
            let headers = response.headers().clone();
            if response
                .content_length()
                .is_some_and(|n| n > self.endpoint.max_bytes as u64)
            {
                return Err(Error::Unavailable("HTTP yanıt kapasitesi aşıldı."));
            }
            let mut stream = response.bytes_stream();
            let mut data = vec![];
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.map_err(|_| {
                    Error::Reconciliation("HTTP yanıt akışı yarım; tekrar yapılmadı.")
                })?;
                if data.len() + chunk.len() > self.endpoint.max_bytes {
                    return Err(Error::Unavailable(
                        "HTTP yanıt kapasitesi aşıldı; kırpılmış sonuç başarı değildir.",
                    ));
                }
                data.extend_from_slice(&chunk);
                if let Some(id) = &response_id
                    && headers
                        .get("Content-Type")
                        .and_then(|v| v.to_str().ok())
                        .is_some_and(|v| v.starts_with("text/event-stream"))
                    && super::mcp::sse_response(&data, id).is_ok()
                {
                    break;
                }
            }
            // A server may reflect a bearer value: never persist or show it.
            if let Some(secret) = &self.secret {
                if let Ok(text) = std::str::from_utf8(&data) {
                    data = text.replace(secret.as_str(), "[REDACTED]").into_bytes();
                } else {
                    return Err(Error::Denied(
                        "Credential kullanılan ikili HTTP yanıtı yayımlanmaz.",
                    ));
                }
            }
            Ok((status, headers, data))
        };
        tokio::pin!(work);
        loop {
            if cancel.load(Ordering::Acquire) {
                return Err(Error::Reconciliation(
                    "HTTP çağrısı iptal edildi; uzak etki doğrulanmadı.",
                ));
            }
            tokio::select! {r=&mut work=>return r,_=tokio::time::sleep(Duration::from_millis(20))=>{}}
        }
    }
}
