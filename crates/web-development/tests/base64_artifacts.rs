mod support;
use base64::{Engine, engine::general_purpose::STANDARD};
use melissa_web_development::{
    plan::{App, WebPlan},
    protocol::*,
    session::WebSession,
};
use serde_json::Value;
use std::{collections::BTreeMap, sync::atomic::AtomicBool};
use support::*;

#[test]
fn explicit_v2_choice_is_source_snapshot_bound_and_does_not_raise_native_limits() {
    let f = Fixture::new(Service::Browser, None, &[]);
    let mut owned = limits();
    owned.output_bytes = 64 * 1024 * 1024;
    owned.export_bytes = 64 * 1024 * 1024;
    let plan = WebPlan::prepare(
        &f.repo,
        &f.repo.snapshot(&AtomicBool::new(false)).unwrap(),
        f.registration.clone(),
        owned,
        &f.cancelled,
    )
    .unwrap();
    let before = plan.snapshot().digest().unwrap();
    let request = plan.request().digest().unwrap();
    let legacy: Value =
        serde_json::from_slice(&plan.snapshot().files[".melissa-code10/program.json"]).unwrap();
    assert!(legacy.get("binary_encoding").is_none());
    assert_eq!(legacy["artifact_limit"], 64 * 1024 * 1024 / 3);
    let plan = plan.with_base64_binary_transport().unwrap();
    assert_ne!(before, plan.snapshot().digest().unwrap());
    assert_eq!(request, plan.request().digest().unwrap());
    plan.request().validate().unwrap();
    let v2: Value =
        serde_json::from_slice(&plan.snapshot().files[".melissa-code10/program.json"]).unwrap();
    assert_eq!(v2["binary_encoding"], "base64-v2");
    assert_eq!(v2["artifact_limit"], 42 * 1024 * 1024);
    assert!(v2["artifact_limit"].as_u64().unwrap() > 30891489);
    assert_eq!(plan.request().limits.output_bytes, 64 * 1024 * 1024);
    assert_eq!(plan.request().limits.export_bytes, 64 * 1024 * 1024);
    let digest = plan.snapshot().digest().unwrap();
    assert_eq!(
        plan.with_base64_binary_transport()
            .unwrap()
            .snapshot()
            .digest()
            .unwrap(),
        digest
    );
}

fn tar_entry<'a>(archive: &'a [u8], wanted: &str) -> Option<&'a [u8]> {
    let mut offset = 0;
    while offset + 512 <= archive.len() {
        let h = &archive[offset..offset + 512];
        if h.iter().all(|b| *b == 0) {
            break;
        }
        let end = h[..100].iter().position(|b| *b == 0).unwrap_or(100);
        let name = std::str::from_utf8(&h[..end])
            .unwrap()
            .trim_start_matches("./");
        let size = usize::from_str_radix(
            std::str::from_utf8(&h[124..136])
                .unwrap()
                .trim_matches(['\0', ' ']),
            8,
        )
        .unwrap();
        let start = offset + 512;
        assert!(start + size <= archive.len());
        if name == wanted {
            return Some(&archive[start..start + size]);
        }
        offset = start + size.div_ceil(512) * 512;
    }
    None
}

#[test]
fn real_v2_png_and_trace_match_native_frames_cas_and_retained_original_zip() {
    let html = r#"<!doctype html><html><title>Owned v2 artifact</title><body><button id="change">Next</button><output id="generation">0</output><canvas width="1024" height="768"></canvas><script>let generation=0;function paint(){const c=document.querySelector('canvas'),x=c.getContext('2d'),d=x.createImageData(1024,768);let n=214789+generation*731;for(let i=0;i<d.data.length;i++){n=(Math.imul(1664525,n)+1013904223)>>>0;d.data[i]=(i%4===3)?255:n>>>24}x.putImageData(d,0,0);document.querySelector('#generation').textContent=String(generation)}paint();document.querySelector('#change').onclick=()=>setTimeout(()=>{generation++;paint()},180);</script></body></html>"#;
    let mut f = Fixture::new(Service::Browser, None, &[("site/index.html", html)]);
    f.registration.app = Some(App::Static {
        directory: "site".into(),
    });
    let plan = WebPlan::prepare(
        &f.repo,
        &f.repo.snapshot(&f.cancelled).unwrap(),
        f.registration.clone(),
        limits(),
        &f.cancelled,
    )
    .unwrap()
    .with_base64_binary_transport()
    .unwrap();
    let mut session = WebSession::start(
        f.repo.clone(),
        f.store.connect().unwrap(),
        f.catalog.clone(),
        f.policy.clone(),
        plan,
        f.ctx.clone(),
        f.cancelled.clone(),
        f.revoked.clone(),
    )
    .unwrap();
    body(
        &mut session,
        Command::Navigate {
            page: "main".into(),
            path: "/".into(),
        },
    );
    let screenshot = body(
        &mut session,
        Command::Screenshot {
            page: "main".into(),
        },
    );
    assert!(screenshot["binary_artifact"]["bytes"].as_u64().unwrap() > 512 * 1024);
    for generation in 1..=8 {
        body(
            &mut session,
            Command::Click {
                page: "main".into(),
                selector: "#change".into(),
            },
        );
        body(
            &mut session,
            Command::Wait {
                page: "main".into(),
                selector: format!("#generation:text-is('{generation}')"),
                state: "visible".into(),
                timeout_ms: 3000,
            },
        );
    }
    let outcome = session.finish().unwrap();
    assert!(outcome.success && outcome.cleanup_confirmed && !outcome.requires_reconciliation);
    let worker = outcome.worker.as_ref().unwrap();
    assert!(!worker.output_truncated && !worker.export_truncated && worker.exit_code == Some(0));
    let mut payloads: BTreeMap<u64, Vec<u8>> = BTreeMap::new();
    for line in worker.stdout.split(|b| *b == b'\n') {
        let Ok(frame) = serde_json::from_slice::<Value>(line) else {
            continue;
        };
        if frame["schema_version"] != "melissa.web-binary/v2" {
            continue;
        }
        assert!(frame.get("data_hex").is_none());
        let data = payloads.entry(frame["seq"].as_u64().unwrap()).or_default();
        assert_eq!(frame["offset"], data.len());
        let encoded = frame["data_base64"].as_str().unwrap();
        let bytes = STANDARD.decode(encoded).unwrap();
        assert_eq!(STANDARD.encode(&bytes), encoded);
        assert!(bytes.len() <= 24576);
        data.extend(bytes);
    }
    assert!(
        payloads.len() >= 2,
        "Both PNG and trace must use actual v2 frames"
    );
    for kind in ["png", "trace"] {
        let id = outcome
            .artifact_refs
            .iter()
            .find(|id| id.ends_with(&format!(":{kind}")))
            .unwrap();
        let bytes = f.store.get_artifact(&f.ctx.task_id, id).unwrap();
        let receipt: Value = serde_json::from_slice(
            &f.store
                .get_artifact(&f.ctx.task_id, &format!("{id}:receipt"))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(payloads[&receipt["seq"].as_u64().unwrap()], bytes);
        assert_eq!(receipt["blake3"], melissa_web_development::digest(&bytes));
        assert_eq!(receipt["descriptor"]["bytes"], bytes.len());
        if kind == "png" {
            assert!(bytes.starts_with(b"\x89PNG\r\n\x1a\n"));
        } else {
            assert!(bytes.len() > 512 * 1024 && bytes.starts_with(b"PK\x03\x04"));
            assert_eq!(
                tar_entry(&worker.archive, ".melissa-code10/trace.zip").unwrap(),
                bytes
            );
            let duplicate = format!(
                ".melissa-code10/artifacts/{}",
                receipt["descriptor"]["id"].as_str().unwrap()
            );
            assert!(
                tar_entry(&worker.archive, &duplicate).is_none(),
                "Only acknowledged duplicate is removed; original ZIP stays exact"
            );
        }
    }
    assert_eq!(
        std::fs::read(f.root().join("dirty.bin")).unwrap(),
        b"KEEP\r\nuser\0work"
    );
}
