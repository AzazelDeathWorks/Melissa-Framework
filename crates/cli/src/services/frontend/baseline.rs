use crate::{
    Error, Result,
    app::{App, bytes, decode, now, safe_path},
};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    path::Path,
};
pub fn sha(raw: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(raw))
}
pub fn unhex(raw: &str) -> Result<Vec<u8>> {
    if !raw.len().is_multiple_of(2) {
        return Err(Error::Store);
    }
    (0..raw.len())
        .step_by(2)
        .map(|i| {
            raw.get(i..i + 2)
                .and_then(|s| u8::from_str_radix(s, 16).ok())
                .ok_or(Error::Store)
        })
        .collect()
}
pub fn valid_id(id: &str) -> bool {
    id.strip_prefix("visual-")
        .is_some_and(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()))
}
pub fn binding(repo: &melissa_repository::Repository) -> String {
    crate::digest(repo.root_identity().as_bytes())
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Accept {
    pub artifact_id: String,
    pub name: String,
    pub digest: String,
}
pub fn accept(app: &App, task: &str, input: Accept) -> Result<Value> {
    if !super::super::name(&input.name) {
        return Err(Error::Usage("Görsel referans adı geçersiz."));
    }
    let store = app.store(task)?;
    let meta_id = format!("{}:metadata", input.artifact_id);
    if store
        .artifact_metadata(task, &input.artifact_id)?
        .logical_type
        != "cli.frontend.binary"
        || store.artifact_metadata(task, &meta_id)?.logical_type != "cli.frontend.metadata"
    {
        return Err(Error::Denied(
            "Yalnız görevin kayıtlı ekran kanıtı kabul edilir.",
        ));
    }
    let metadata: Value = decode(&store.get_artifact(task, &meta_id)?)?;
    if metadata["review_digest"] != input.digest
        || metadata["kind"] != "png"
        || !metadata["capture"].is_object()
        || metadata["worker_source_current"] != true
        || metadata["source_current"] != true
    {
        return Err(Error::Denied(
            "Güncel kaynaklı checkpoint ve tam inceleme digest'i gerekli.",
        ));
    }
    let raw = store.get_artifact(task, &input.artifact_id)?;
    if !raw.starts_with(b"\x89PNG\r\n\x1a\n") || sha(&raw) != metadata["sha256"] {
        return Err(Error::Store);
    }
    let repo = app.source(&app.workspace(&store, task)?)?;
    if metadata["workspace_binding"] != binding(&repo) {
        return Err(Error::Denied("Referans başka projeye ait."));
    }
    let record = json!({"schema_version":"melissa.visual-baseline/v1","accepted_by":"operator_explicit_cli","accepted_at_ms":now()?,"name":input.name,"workspace_binding":binding(&repo),"origin_task":task,"artifact_id":input.artifact_id,"metadata":metadata,"capture":metadata["capture"],"png_hex":raw.iter().map(|b|format!("{b:02x}")).collect::<String>()});
    let plain = bytes(&record)?;
    let id = format!("visual-{}", blake3::hash(&plain));
    let directory = app.home.join("visual-baselines");
    safe_path(&directory)?;
    std::fs::create_dir_all(&directory)?;
    let path = directory.join(format!("{id}.enc"));
    safe_path(&path)?;
    let encrypted = app.key.seal(
        &plain,
        format!("melissa.visual-baseline/v1:{id}").as_bytes(),
    )?;
    let mut file = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&path)?;
    file.write_all(&encrypted)?;
    file.sync_all()?;
    Ok(
        json!({"command":"services baseline-accept","baseline_id":id,"name":input.name,"sha256":metadata["sha256"],"source_digest":metadata["source_digest"],"applies_to":"new_reviewed_tasks_only","automatic_update":false,"model_calls":0}),
    )
}
pub fn load(
    app: &App,
    repo: &melissa_repository::Repository,
    spec: &super::Spec,
) -> Result<BTreeMap<String, Value>> {
    let mut result = BTreeMap::new();
    for visual in spec.checks.values().flat_map(|c| &c.visual) {
        let id = &visual.baseline_id;
        if result.contains_key(id) {
            continue;
        }
        if !valid_id(id) {
            return Err(Error::Usage("Referans kimliği geçersiz."));
        }
        let path = app.home.join("visual-baselines").join(format!("{id}.enc"));
        safe_path(&path)?;
        let mut encrypted = vec![];
        std::fs::File::open(path)?
            .take(64 * 1024 * 1024 + 1)
            .read_to_end(&mut encrypted)?;
        if encrypted.len() > 64 * 1024 * 1024 {
            return Err(Error::Unavailable("Referans aktarım kapasitesini aşıyor."));
        }
        let plain = app.key.open(
            &encrypted,
            format!("melissa.visual-baseline/v1:{id}").as_bytes(),
        )?;
        if id != &format!("visual-{}", blake3::hash(&plain)) {
            return Err(Error::Store);
        }
        let record: Value = decode(&plain)?;
        if record["workspace_binding"] != binding(repo)
            || record["accepted_by"] != "operator_explicit_cli"
        {
            return Err(Error::Denied(
                "Kullanıcı referansı proje kimliğiyle eşleşmiyor.",
            ));
        }
        result.insert(id.clone(), record);
    }
    Ok(result)
}
pub fn has_baselines(app: &App, workspace: &Path, spec: &super::Spec) -> Result {
    let repo = melissa_repository::Repository::open(
        &crate::app::absolute(workspace)?,
        Default::default(),
        vec![],
    )?;
    load(app, &repo, spec).map(|_| ())
}
