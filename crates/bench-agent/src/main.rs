//! Benchmark bridge (Harbor / Terminal-Bench): Melissa's Gateway loop runs on
//! the host while shell and file effects are delegated to the harness, which
//! owns the task container. Protocol: one JSON object per line; requests on
//! stdout ({"op":"exec"|"read"|"write",...}), replies on stdin, and a final
//! {"op":"done",...} line. Logs go to stderr.
//!
//! MELISSA_BENCH_ENDPOINT, MELISSA_BENCH_PROTOCOL, MELISSA_BENCH_MODEL and
//! MELISSA_BENCH_API_KEY select the provider; MELISSA_BENCH_MAX_TURNS and
//! MELISSA_BENCH_WALL_MS bound the run. The instruction path is argv[1].
use melissa_core_contracts::{TaskState, catalog::Catalog, wire::TaskContractV1};
use melissa_local_store::{LocalStore, StoreConfig, StoreKey};
use melissa_model_gateway::{
    DecoderLimits, GatewayError, Protocol, RequestLimits, Result, ToolCall, ToolDefinition,
    agent_loop::{Gateway, GatewayPolicy, RunSpec},
    routing::{
        Capabilities, Measurements, ModelRegistration, PriceSource, RateCard, RouteRequirements,
    },
    tools::{BoundTool, ToolContext, ToolDispatcher, ToolExecution},
    transport::{ApiKey, EndpointTrust, ProviderEndpoint},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    future::Future,
    io::{BufRead, Write},
    path::Path,
    pin::Pin,
    sync::{Arc, Mutex, atomic::AtomicBool},
};
use tokio::sync::Semaphore;

const TASK: &str = "task:bench";
const PROFILE: &str = "melissa.code.implementer";
const RESOURCE: &str = "provider:bench";
const AUTH: &str = "authorization:provider:bench";
const OUTPUT_CHARS: usize = 16_000;
const READ_CHARS: usize = 60_000;

const PREAMBLE: &str = "You are working autonomously inside a Linux container through tools; nobody will answer questions. \
Use bash for commands. Each bash call is a fresh non-interactive shell: cd and exports do not persist, so chain with && or use absolute paths, \
and never start interactive programs or leave servers attached to the call (background them with nohup ... &). \
Use fs_read to view files, fs_replace for exact text edits and fs_write to create or overwrite whole files. \
Inspect before acting, verify the result by actually running it, and keep going until the task is fully done. \
When finished, reply with a short summary and no tool calls.\n\nTASK:\n";

fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} is required"))
}
fn env_or(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

struct Host {
    input: std::io::BufReader<std::io::Stdin>,
    output: std::io::Stdout,
}
impl Host {
    fn call(&mut self, request: Value) -> std::result::Result<Value, String> {
        writeln!(self.output, "{request}")
            .and_then(|_| self.output.flush())
            .map_err(|e| e.to_string())?;
        let mut line = String::new();
        match self.input.read_line(&mut line) {
            Ok(0) => Err("harness closed the channel".into()),
            Ok(_) => {
                let v: Value = serde_json::from_str(&line).map_err(|e| e.to_string())?;
                match v.get("error").and_then(Value::as_str) {
                    Some(e) => Err(e.to_string()),
                    None => Ok(v),
                }
            }
            Err(e) => Err(e.to_string()),
        }
    }
    fn read(&mut self, path: &str) -> std::result::Result<Vec<u8>, String> {
        let v = self.call(json!({"op":"read","path":path}))?;
        b64_decode(v["content_b64"].as_str().unwrap_or_default())
    }
    fn write(&mut self, path: &str, bytes: &[u8]) -> std::result::Result<(), String> {
        self.call(json!({"op":"write","path":path,"content_b64":b64_encode(bytes)}))
            .map(|_| ())
    }
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
fn b64_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for c in bytes.chunks(3) {
        let n = (c[0] as u32) << 16
            | (*c.get(1).unwrap_or(&0) as u32) << 8
            | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= c.len() {
                out.push(B64[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}
fn b64_decode(s: &str) -> std::result::Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    let (mut acc, mut bits) = (0u32, 0);
    for b in s.bytes().filter(|b| !b.is_ascii_whitespace() && *b != b'=') {
        let v = B64
            .iter()
            .position(|x| *x == b)
            .ok_or("invalid base64 from harness")? as u32;
        acc = acc << 6 | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Ok(out)
}

fn clip(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let head = floor(s, max / 3);
    let tail = ceil(s, s.len() - (max - head.len()));
    format!(
        "{head}\n[... {} bytes omitted ...]\n{}",
        s.len() - head.len() - (s.len() - tail),
        &s[tail..]
    )
}
fn floor(s: &str, mut i: usize) -> &str {
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    &s[..i]
}
fn ceil(s: &str, mut i: usize) -> usize {
    while !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

fn find_all(hay: &[u8], needle: &[u8]) -> usize {
    hay.windows(needle.len()).filter(|w| *w == needle).count()
}
/// Same matching rules as the repository `fs_replace`: exact bytes, LF text
/// also matches CRLF files, ambiguity rejected unless `replace_all`.
fn replace_text(
    bytes: &[u8],
    old: &str,
    new: &str,
    all: bool,
) -> std::result::Result<(Vec<u8>, usize), &'static str> {
    if old.is_empty() {
        return Err("old_text must not be empty");
    }
    let crlf = |s: &str| s.replace("\r\n", "\n").replace('\n', "\r\n");
    let (mut o, mut n) = (old.as_bytes().to_vec(), new.as_bytes().to_vec());
    let mut hits = find_all(bytes, &o);
    if hits == 0 && old.contains('\n') && bytes.windows(2).any(|w| w == b"\r\n") {
        (o, n) = (crlf(old).into_bytes(), crlf(new).into_bytes());
        hits = find_all(bytes, &o);
    }
    if hits == 0 {
        return Err("old_text not found; re-read the file and copy the exact current text");
    }
    if hits > 1 && !all {
        return Err(
            "old_text matches more than once; include more surrounding lines or set replace_all",
        );
    }
    let (mut out, mut at, mut count) = (Vec::with_capacity(bytes.len()), 0, 0);
    while let Some(i) = bytes[at..].windows(o.len()).position(|w| w == o.as_slice()) {
        out.extend_from_slice(&bytes[at..at + i]);
        out.extend_from_slice(&n);
        at += i + o.len();
        count += 1;
    }
    out.extend_from_slice(&bytes[at..]);
    Ok((out, count))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BashArg {
    command: String,
    timeout_sec: Option<u64>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadArg {
    path: String,
    start_line: Option<usize>,
    max_lines: Option<usize>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReplaceArg {
    path: String,
    old_text: String,
    new_text: String,
    #[serde(default)]
    replace_all: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WriteArg {
    path: String,
    content: String,
}

const TOOLS: [(&str, &str, &str); 4] = [
    (
        "bash",
        "process.exec",
        "Run a non-interactive bash command in the task container. Returns exit code, stdout and stderr (long output is clipped in the middle). Default timeout 180 s, maximum 1800 s.",
    ),
    (
        "fs_read",
        "fs.read",
        "Read a UTF-8 text file. Optional 1-based start_line and max_lines page large files.",
    ),
    (
        "fs_replace",
        "fs.patch",
        "Replace exact text in a file. old_text must match the current file exactly once unless replace_all is true; copy it from a fresh fs_read.",
    ),
    (
        "fs_write",
        "fs.patch",
        "Create or overwrite a whole file with content. Parent directories are created.",
    ),
];
fn schema(name: &str) -> Value {
    let (props, req) = match name {
        "bash" => (
            json!({"command":{"type":"string"},"timeout_sec":{"type":"integer","minimum":1,"maximum":1800}}),
            json!(["command"]),
        ),
        "fs_read" => (
            json!({"path":{"type":"string"},"start_line":{"type":"integer","minimum":1},"max_lines":{"type":"integer","minimum":1}}),
            json!(["path"]),
        ),
        "fs_replace" => (
            json!({"path":{"type":"string"},"old_text":{"type":"string"},"new_text":{"type":"string"},"replace_all":{"type":"boolean"}}),
            json!(["path", "old_text", "new_text"]),
        ),
        _ => (
            json!({"path":{"type":"string"},"content":{"type":"string"}}),
            json!(["path", "content"]),
        ),
    };
    json!({"type":"object","properties":props,"required":req,"additionalProperties":false})
}

struct BenchTools {
    host: Arc<Mutex<Host>>,
}
fn parse<T: for<'de> Deserialize<'de>>(call: &ToolCall) -> Result<T> {
    serde_json::from_str(&call.arguments)
        .map_err(|_| GatewayError::invalid("tool arguments do not match the schema"))
}
fn run_tool(host: &mut Host, call: &ToolCall) -> std::result::Result<String, String> {
    match call.name.as_str() {
        "bash" => {
            let a: BashArg = parse(call).map_err(|e| e.message)?;
            let t = a.timeout_sec.unwrap_or(180).clamp(1, 1800);
            let v = host.call(json!({"op":"exec","command":a.command,"timeout_sec":t}))?;
            let code = v["exit_code"].as_i64().unwrap_or(-1);
            let out = format!(
                "exit_code: {code}\nstdout:\n{}\nstderr:\n{}",
                v["stdout"].as_str().unwrap_or_default(),
                v["stderr"].as_str().unwrap_or_default()
            );
            Ok(clip(&out, OUTPUT_CHARS))
        }
        "fs_read" => {
            let a: ReadArg = parse(call).map_err(|e| e.message)?;
            let bytes = host.read(&a.path)?;
            let Ok(text) = String::from_utf8(bytes) else {
                return Err("file is not UTF-8 text; inspect it with bash".into());
            };
            let lines: Vec<&str> = text.split_inclusive('\n').collect();
            let start = a.start_line.unwrap_or(1).max(1) - 1;
            let end = lines.len().min(start + a.max_lines.unwrap_or(usize::MAX));
            let mut body = String::new();
            let mut shown = start;
            for l in lines.iter().take(end).skip(start) {
                if body.len() + l.len() > READ_CHARS {
                    break;
                }
                body.push_str(l);
                shown += 1;
            }
            if start > 0 || shown < lines.len() {
                body.push_str(&format!(
                    "\n[lines {}-{} of {}]",
                    start + 1,
                    shown,
                    lines.len()
                ));
            }
            Ok(body)
        }
        "fs_replace" => {
            let a: ReplaceArg = parse(call).map_err(|e| e.message)?;
            let bytes = host.read(&a.path)?;
            let (out, n) = replace_text(&bytes, &a.old_text, &a.new_text, a.replace_all)?;
            host.write(&a.path, &out)?;
            Ok(format!("replaced {n} occurrence(s) in {}", a.path))
        }
        _ => {
            let a: WriteArg = parse(call).map_err(|e| e.message)?;
            host.write(&a.path, a.content.as_bytes())?;
            Ok(format!("wrote {} bytes to {}", a.content.len(), a.path))
        }
    }
}
impl ToolDispatcher for BenchTools {
    fn definitions(&self) -> Vec<BoundTool> {
        TOOLS
            .iter()
            .map(|(name, canonical, description)| BoundTool {
                canonical_id: (*canonical).into(),
                definition: ToolDefinition {
                    name: (*name).into(),
                    description: (*description).into(),
                    parameters: schema(name),
                    strict: false,
                },
            })
            .collect()
    }
    fn validate_call(&self, call: &ToolCall) -> Result<()> {
        match call.name.as_str() {
            "bash" => parse::<BashArg>(call).map(|_| ()),
            "fs_read" => parse::<ReadArg>(call).map(|_| ()),
            "fs_replace" => parse::<ReplaceArg>(call).map(|_| ()),
            "fs_write" => parse::<WriteArg>(call).map(|_| ()),
            _ => Err(GatewayError::invalid("unknown tool")),
        }
    }
    fn execute<'a>(
        &'a mut self,
        call: ToolCall,
        _context: ToolContext,
        _cancelled: Arc<AtomicBool>,
    ) -> Pin<Box<dyn Future<Output = Result<ToolExecution>> + Send + 'a>> {
        let host = Arc::clone(&self.host);
        Box::pin(async move {
            let r = tokio::task::spawn_blocking(move || {
                let mut h = host
                    .lock()
                    .map_err(|_| "host channel poisoned".to_string())?;
                run_tool(&mut h, &call)
            })
            .await
            .map_err(|_| GatewayError::invalid("tool worker failed"))?;
            let (content, success) = match r {
                Ok(c) => (c, true),
                Err(e) => (json!({"error":e}).to_string(), false),
            };
            Ok(ToolExecution {
                content,
                success,
                cancelled: false,
                requires_reconciliation: false,
                artifact_refs: vec![],
            })
        })
    }
}

fn store(root: &Path, wall_ms: u64) -> LocalStore {
    let mut s =
        LocalStore::open(root, StoreKey::from_bytes([42; 32]), StoreConfig::default()).unwrap();
    let mut task: TaskContractV1 = serde_json::from_slice(include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/code01/wire/task-contract-v1.json"
    )))
    .unwrap();
    task.task_id = TASK.into();
    task.selected_profile_ids = vec![PROFILE.into()];
    task.allowed_effects = vec![
        "workspace.read".into(),
        "workspace.apply_patch_scoped".into(),
    ];
    task.allowed_resource_ids.push(RESOURCE.into());
    task.authorization_refs.push(AUTH.into());
    task.limits.max_input_tokens = 200_000_000;
    task.limits.max_output_tokens = 4_000_000;
    task.limits.max_cost_micro_usd = 1_000_000_000_000;
    task.limits.max_output_bytes = 256 * 1024 * 1024;
    task.limits.max_disk_bytes = 1024 * 1024 * 1024;
    task.limits.max_wall_time_ms = wall_ms;
    s.journal_mut().create_task(&task).unwrap();
    for state in [
        TaskState::Planned,
        TaskState::Queued,
        TaskState::Leased,
        TaskState::Running,
    ] {
        let version = s.journal().task(TASK).unwrap().record.version;
        s.transition_task(TASK, version, state, None).unwrap();
    }
    s
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() {
    let host = Arc::new(Mutex::new(Host {
        input: std::io::BufReader::new(std::io::stdin()),
        output: std::io::stdout(),
    }));
    let done = run(Arc::clone(&host))
        .await
        .unwrap_or_else(|e| json!({"status":"Error","error":e}));
    let mut h = host.lock().unwrap();
    let mut line = done;
    line["op"] = json!("done");
    let _ = writeln!(h.output, "{line}").and_then(|_| h.output.flush());
}

async fn run(host: Arc<Mutex<Host>>) -> std::result::Result<Value, String> {
    let instruction =
        std::fs::read_to_string(std::env::args().nth(1).ok_or("instruction path required")?)
            .map_err(|e| e.to_string())?;
    let wall_ms = env_or("MELISSA_BENCH_WALL_MS", 1_800_000);
    let max_turns = env_or("MELISSA_BENCH_MAX_TURNS", 200).clamp(1, 4096) as u32;
    let protocol: Protocol = serde_json::from_value(Value::String(env("MELISSA_BENCH_PROTOCOL")))
        .map_err(|e| e.to_string())?;
    let endpoint = env("MELISSA_BENCH_ENDPOINT");
    let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
    let root = std::env::var("MELISSA_BENCH_ROOT")
        .unwrap_or_else(|_| concat!(env!("CARGO_MANIFEST_DIR"), "/../..").into());
    let gw = |e: GatewayError| format!("{:?}: {}", e.kind, e.message);
    let mut g = Gateway::new(
        store(&dir.path().join("store"), wall_ms),
        Catalog::load_definitions(Path::new(&root)).map_err(|e| format!("{e:?}"))?,
        vec![ModelRegistration {
            registration_id: "bench-route".into(),
            endpoint: ProviderEndpoint::new(
                "bench-provider".into(),
                RESOURCE.into(),
                AUTH.into(),
                protocol,
                if endpoint.starts_with("http://127.0.0.1") {
                    EndpointTrust::LocalLoopback
                } else {
                    EndpointTrust::RemoteHttps
                },
                &endpoint,
                ApiKey::from_env("MELISSA_BENCH_API_KEY").map_err(gw)?,
            )
            .map_err(gw)?,
            model_id: env("MELISSA_BENCH_MODEL"),
            capabilities: Capabilities {
                tool_calling: true,
                structured_output: true,
                vision: false,
                streaming: true,
            },
            categories: BTreeSet::from(["coding".into()]),
            context_tokens: env_or("MELISSA_BENCH_CONTEXT_TOKENS", 1_000_000),
            max_output_tokens: 32_768,
            timeout_ms: 300_000,
            max_concurrency: 1,
            rate_card: Some(RateCard {
                source: PriceSource::OperatorDeclared {
                    note: "benchmark accounting only".into(),
                },
                input_micro_usd_per_million: 1_000_000,
                output_micro_usd_per_million: 4_000_000,
                cached_input_micro_usd_per_million: None,
                cache_creation_micro_usd_per_million: None,
            }),
            measurements: Measurements::default(),
            slots: Arc::new(Semaphore::new(1)),
        }],
        GatewayPolicy {
            allowed_registrations: BTreeSet::from(["bench-route".into()]),
            max_requests: (max_turns * 2).min(4096),
            max_turns,
            max_tools: (max_turns * 4).min(4096),
            max_wall_time_ms: wall_ms,
            expires_at_unix_ms: melissa_execution_broker::unix_ms()
                .map_err(|e| format!("{e:?}"))?
                + wall_ms
                + 60_000,
            allow_fallback: false,
            max_backoff_ms: 5_000,
            decoder_limits: DecoderLimits::default(),
        },
        false,
    )
    .map_err(gw)?
    .with_request_limits(RequestLimits {
        max_history_bytes: 32 * 1024 * 1024,
    })
    .map_err(gw)?;
    let spec = RunSpec {
        run_id: "bench".into(),
        task_id: TASK.into(),
        profile_id: PROFILE.into(),
        user_message: format!("{PREAMBLE}{instruction}"),
        route: RouteRequirements {
            category: "coding".into(),
            tools: true,
            structured_output: false,
            vision: false,
            minimum_success_ppm: None,
            maximum_p95_ms: None,
        },
        max_output_tokens: 16_384,
        output_schema: None,
        required_tool_names: BTreeSet::new(),
    };
    let started = std::time::Instant::now();
    let mut tools = BenchTools { host };
    let r = g
        .run(&spec, &mut tools, Arc::new(AtomicBool::new(false)))
        .await;
    let budget = g
        .store()
        .journal()
        .model_budget(TASK)
        .map_err(|e| format!("{e:?}"))?;
    let mut out = json!({
        "requests": 0, "tool_calls": 0,
        "input_tokens": budget.charged_input_tokens, "output_tokens": budget.charged_output_tokens,
        "elapsed_ms": started.elapsed().as_millis() as u64,
    });
    match r {
        Ok(r) => {
            out["status"] = json!(format!("{:?}", r.status));
            out["requests"] = json!(r.request_ids.len());
            out["tool_calls"] = json!(r.tool_calls);
            out["answer"] = json!(r.answer.map(|a| clip(&a, 4000)));
        }
        Err(e) => {
            out["status"] = json!("GatewayError");
            out["error"] = json!(gw(e));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn replace_matches_repository_rules() {
        assert_eq!(replace_text(b"a-b\n", "-", "+", false).unwrap().0, b"a+b\n");
        assert_eq!(
            replace_text(b"x\r\ny\r\n", "x\ny", "z\nw", false)
                .unwrap()
                .0,
            b"z\r\nw\r\n"
        );
        assert!(replace_text(b"aa", "a", "b", false).is_err());
        assert_eq!(
            replace_text(b"aa", "a", "b", true).unwrap(),
            (b"bb".to_vec(), 2)
        );
        assert!(replace_text(b"abc", "zz", "y", false).is_err());
        assert!(replace_text(b"abc", "", "y", false).is_err());
    }
    #[test]
    fn base64_round_trip_and_clip_keep_utf8() {
        for s in [&b""[..], b"f", b"fo", b"foo", b"\x00\xff\xfe\x80"] {
            assert_eq!(b64_decode(&b64_encode(s)).unwrap(), s);
        }
        let long = "ğ".repeat(20_000);
        let c = clip(&long, 1000);
        assert!(c.len() < 1200 && c.contains("omitted"));
    }
}
