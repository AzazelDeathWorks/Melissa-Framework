//! Loopback regressions for the HTTP429 evidence lost in the actual CODE10 run.
use melissa_model_gateway::transport::{
    ApiKey, Delivery, EndpointTrust, HttpTransport, ProviderEndpoint,
};
use melissa_model_gateway::{DecoderLimits, ErrorKind, Message, Protocol, TurnRequest};
use serde_json::json;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::oneshot,
    task::JoinHandle,
};

struct Fixture {
    endpoint: ProviderEndpoint,
    headers_sent: oneshot::Receiver<()>,
    task: JoinHandle<()>,
}

async fn fixture(protocol: Protocol, body: Vec<u8>, chunked: bool, delay: Duration) -> Fixture {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (sent, headers_sent) = oneshot::channel();
    let task = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let mut buffer = [0u8; 4096];
        let header_end = loop {
            let n = socket.read(&mut buffer).await.unwrap();
            if n == 0 {
                return;
            }
            request.extend_from_slice(&buffer[..n]);
            assert!(request.len() < 65536);
            if let Some(i) = request.windows(4).position(|v| v == b"\r\n\r\n") {
                break i + 4;
            }
        };
        let headers = std::str::from_utf8(&request[..header_end]).unwrap();
        let length = headers
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().unwrap())
            })
            .unwrap();
        assert!(length < 65536);
        while request.len() - header_end < length {
            let n = socket.read(&mut buffer).await.unwrap();
            if n == 0 {
                return;
            }
            request.extend_from_slice(&buffer[..n]);
        }
        let framing = if chunked {
            "Transfer-Encoding: chunked\r\n".to_owned()
        } else {
            format!("Content-Length: {}\r\n", body.len())
        };
        let head = format!(
            "HTTP/1.1 429 Throttled\r\nContent-Type: Application/JSON; charset=utf-8\r\nConnection: close\r\nx-request-id: fixture-throttle-id\r\nRetry-After: 77\r\n{framing}\r\n"
        );
        if socket.write_all(head.as_bytes()).await.is_err() {
            return;
        }
        let _ = sent.send(());
        tokio::time::sleep(delay).await;
        for part in body.chunks(11) {
            if chunked
                && socket
                    .write_all(format!("{:x}\r\n", part.len()).as_bytes())
                    .await
                    .is_err()
            {
                return;
            }
            if socket.write_all(part).await.is_err() {
                return;
            }
            if chunked && socket.write_all(b"\r\n").await.is_err() {
                return;
            }
            tokio::task::yield_now().await;
        }
        if chunked {
            let _ = socket.write_all(b"0\r\n\r\n").await;
        }
        let _ = socket.shutdown().await;
    });
    let endpoint = ProviderEndpoint::new(
        "fixture-provider".into(),
        "fixture-resource".into(),
        "fixture-authorization".into(),
        protocol,
        EndpointTrust::LoopbackFixture,
        &format!("http://{address}/v1/responses"),
        ApiKey::new("LOCAL_THROTTLE_FIXTURE_CREDENTIAL".into()).unwrap(),
    )
    .unwrap();
    Fixture {
        endpoint,
        headers_sent,
        task,
    }
}

fn request() -> TurnRequest {
    TurnRequest {
        model: "fixture-model".into(),
        system_prompt: "owned loopback fixture".into(),
        messages: vec![Message::User {
            text: "local diagnostic".into(),
        }],
        tools: vec![],
        max_output_tokens: 128,
        output_schema: None,
    }
}

#[tokio::test]
async fn http429_json_retains_exact_evidence_without_becoming_success_or_refusal() {
    for protocol in [Protocol::OpenAiResponses, Protocol::AnthropicMessages] {
        for code in ["unavailable_route", "insufficient_quota"] {
            let body = serde_json::to_vec(
                &json!({"error":{"code":code,"message":"PRIVATE_THROTTLE_DETAIL ş"}}),
            )
            .unwrap();
            let http = fixture(protocol, body.clone(), true, Duration::ZERO).await;
            let outcome = HttpTransport::new(true)
                .unwrap()
                .generate(
                    &http.endpoint,
                    &request(),
                    5000,
                    DecoderLimits::default(),
                    Arc::new(AtomicBool::new(false)),
                )
                .await;
            assert_eq!(outcome.http_status, Some(429));
            assert_eq!(outcome.delivery, Delivery::ResponseReceived);
            assert_eq!(outcome.error.as_ref().unwrap().kind, ErrorKind::RateLimited);
            assert!(outcome.response.is_none());
            assert_eq!(
                outcome.provider_request_id.as_deref(),
                Some("fixture-throttle-id")
            );
            assert_eq!(outcome.retry_after_ms, Some(30000));
            assert_eq!(outcome.raw_sse, body);
            assert_eq!(outcome.response_bytes, body.len() as u64);
            assert!(!format!("{outcome:?}").contains("PRIVATE_THROTTLE_DETAIL"));
            assert!(!format!("{outcome:?}").contains("LOCAL_THROTTLE_FIXTURE_CREDENTIAL"));
            http.task.await.unwrap();
        }
    }
}

#[tokio::test]
async fn http429_body_capture_keeps_existing_quota_deadline_and_cancellation_guards() {
    for chunked in [false, true] {
        let http = fixture(
            Protocol::OpenAiResponses,
            vec![b'x'; 65537],
            chunked,
            Duration::ZERO,
        )
        .await;
        let outcome = HttpTransport::new(true)
            .unwrap()
            .generate(
                &http.endpoint,
                &request(),
                5000,
                DecoderLimits::default(),
                Arc::new(AtomicBool::new(false)),
            )
            .await;
        assert_eq!(outcome.http_status, Some(429));
        assert_eq!(outcome.error.as_ref().unwrap().kind, ErrorKind::Protocol);
        assert!(outcome.response.is_none() && outcome.raw_sse.len() <= 65536);
        http.task.await.unwrap();
    }
    for cancel in [false, true] {
        let http = fixture(
            Protocol::OpenAiResponses,
            br#"{"error":{"code":"unavailable_route"}}"#.to_vec(),
            false,
            Duration::from_secs(5),
        )
        .await;
        let flag = Arc::new(AtomicBool::new(false));
        let signal = flag.clone();
        let canceller = tokio::spawn(async move {
            http.headers_sent.await.unwrap();
            if cancel {
                tokio::time::sleep(Duration::from_millis(50)).await;
                signal.store(true, Ordering::Release);
            }
        });
        let outcome = HttpTransport::new(true)
            .unwrap()
            .generate(
                &http.endpoint,
                &request(),
                if cancel { 2000 } else { 200 },
                DecoderLimits::default(),
                flag,
            )
            .await;
        assert_eq!(outcome.http_status, Some(429));
        assert_eq!(outcome.delivery, Delivery::ResponseReceived);
        assert_eq!(
            outcome.error.as_ref().unwrap().kind,
            if cancel {
                ErrorKind::Cancelled
            } else {
                ErrorKind::Timeout
            }
        );
        assert!(outcome.response.is_none() && outcome.raw_sse.is_empty());
        canceller.await.unwrap();
        http.task.abort();
        let _ = http.task.await;
    }
}
