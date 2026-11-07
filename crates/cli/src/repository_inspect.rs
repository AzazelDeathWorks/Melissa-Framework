//! Operator-only, read-only repository helpers. No model, credentials or task
//! mutation. Normal source visibility applies to every queried source path.
use crate::{Error, Result, app::App};
use std::{io::Read, path::Path, sync::atomic::AtomicBool};
pub fn run(_app: &App, action: &str, workspace: &Path, query: &Path) -> Result<serde_json::Value> {
    let repo = melissa_repository::Repository::open(
        &crate::app::absolute(workspace)?,
        Default::default(),
        vec![],
    )?;
    let mut raw = vec![];
    std::fs::File::open(query)?
        .take(1024 * 1024 + 1)
        .read_to_end(&mut raw)?;
    if raw.len() > 1024 * 1024 {
        return Err(Error::Usage("Sorgu dosyası çok büyük."));
    }
    let v = melissa_model_gateway::strict_json(
        std::str::from_utf8(&raw).map_err(|_| Error::Usage("Sorgu UTF-8 olmalı."))?,
    )
    .map_err(|_| Error::Usage("Sorgu JSON geçersiz."))?;
    let cancel = AtomicBool::new(false);
    match action {
        "impact-map" => {
            let q =
                serde_json::from_value(v).map_err(|_| Error::Usage("Etki sorgusu geçersiz."))?;
            serde_json::to_value(repo.impact_map(&q, &cancel)?).map_err(|_| Error::Store)
        }
        "structural-patch" => {
            let q =
                serde_json::from_value(v).map_err(|_| Error::Usage("Yapısal sorgu geçersiz."))?;
            serde_json::to_value(repo.structural(&q, &cancel)?).map_err(|_| Error::Store)
        }
        _ => Err(Error::Usage("Bilinmeyen repository işlemi.")),
    }
}
