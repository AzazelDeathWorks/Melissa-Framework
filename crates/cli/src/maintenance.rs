//! Operator-only local maintenance; no automatic retention deletion or replay.
pub mod portable;
#[cfg(all(test, windows))]
mod tests;
use crate::{
    Error, Result,
    app::{App, absolute, bytes, now},
    digest,
};
use melissa_repository::Repository;
use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};
fn canonical(path: &Path) -> Result<PathBuf> {
    let mut missing = vec![];
    let mut parent = path;
    while !parent.exists() {
        missing.push(parent.file_name().ok_or(Error::Io)?.to_owned());
        parent = parent.parent().ok_or(Error::Io)?;
    }
    let mut result = fs::canonicalize(parent)?;
    for part in missing.into_iter().rev() {
        result.push(part);
    }
    Ok(result)
}
pub(crate) fn disjoint(left: &Path, right: &Path) -> Result {
    let l = canonical(left)?
        .to_string_lossy()
        .replace('/', "\\")
        .to_uppercase();
    let r = canonical(right)?
        .to_string_lossy()
        .replace('/', "\\")
        .to_uppercase();
    if l == r || l.starts_with(&(r.clone() + "\\")) || r.starts_with(&(l + "\\")) {
        return Err(Error::Denied(
            "Çıktı, veri ve kaynak klasörleri tamamen ayrı olmalı.",
        ));
    }
    Ok(())
}
pub(crate) fn destination(
    app: Option<&App>,
    task: Option<&str>,
    path: &Path,
) -> Result<(PathBuf, Repository)> {
    let path = absolute(path)?;
    if fs::symlink_metadata(&path).is_ok() {
        return Err(Error::Conflict("Çıktı önceden var; üzerine yazılmadı."));
    }
    let parent = Repository::open(path.parent().ok_or(Error::Io)?, Default::default(), vec![])?;
    if let Some(app) = app {
        disjoint(&path, &app.home)?;
        if let Some(task) = task {
            let store = app.store(task)?;
            disjoint(&path, Path::new(&app.workspace(&store, task)?.root))?;
        }
    }
    Ok((path, parent))
}
pub(crate) fn write_new(path: &Path, data: &[u8]) -> Result {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.write_all(data)?;
    file.sync_all()?;
    Ok(())
}
fn tree_size(path: &Path) -> Result<(u64, u64)> {
    let mut stack = vec![path.to_owned()];
    let (mut bytes, mut count) = (0u64, 0u64);
    while let Some(path) = stack.pop() {
        crate::app::safe_path(&path)?;
        let meta = fs::symlink_metadata(&path)?;
        count += 1;
        if count > 1_000_000 {
            return Err(Error::Unavailable(
                "Dosya envanteri çok büyük; kısmi ölçüm tam diye sunulmadı.",
            ));
        }
        if meta.is_dir() {
            for entry in fs::read_dir(path)? {
                stack.push(entry?.path());
            }
        } else if meta.is_file() {
            bytes = bytes.checked_add(meta.len()).ok_or(Error::Store)?;
        } else {
            return Err(Error::Denied("Ölçümde desteklenmeyen dosya türü."));
        }
    }
    Ok((bytes, count))
}
impl App {
    pub fn disk(&self, id: &str) -> Result<Value> {
        let store = self.store(id)?;
        let entries = store.retention_preview(id, now()?)?;
        let mut unique = std::collections::BTreeMap::new();
        for entry in &entries {
            unique.insert(&entry.artifact.digest, entry.artifact.stored_bytes);
        }
        let totals = tree_size(&self.home.join("tasks").join(id))?;
        Ok(
            json!({"command":"maintenance disk","task_id":id,"disk":{"task_directory_logical_bytes":totals.0,"filesystem_entries":totals.1,"task_artifact_count":entries.len(),"unique_task_ciphertext_bytes":unique.values().sum::<u64>(),"eligible_artifacts":entries.iter().filter(|e|e.eligible).count(),"physical_allocated_bytes":null,"basis":"logical file lengths; CAS deduplicated by digest; task directory includes WAL/worker copies, not physical disk allocation"},"model_calls":0}),
        )
    }
    pub fn retention_preview(&self, id: &str) -> Result<Value> {
        let store = self.store(id)?;
        let state = store.journal().task(id)?;
        let entries = store.retention_preview(id, now()?)?;
        let view = json!({"task_id":id,"state":state.record.state,"version":state.record.version,"entries":entries,"deletion_performed":false,"preview_only":true,"active_evidence_protected":true,"scope":"Owner pins, legal holds, invocation/checkpoint/final references and active task state prevent reclamation; no pin/hold released."});
        Ok(
            json!({"command":"maintenance retention-preview","review_digest":digest(&bytes(&view)?),"retention":view,"model_calls":0}),
        )
    }
}

pub(crate) struct ReadFile {
    pub file: fs::File,
    _parent: Repository,
}
pub(crate) fn open_read(path: &Path) -> Result<ReadFile> {
    crate::app::safe_path(path)?;
    let parent = Repository::open(path.parent().ok_or(Error::Io)?, Default::default(), vec![])?;
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ,
        };
        options
            .share_mode(FILE_SHARE_READ)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = options.open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(Error::Denied("Düzenli dosya gerekli."));
    }
    #[cfg(windows)]
    unsafe {
        use std::os::windows::{fs::MetadataExt, io::AsRawHandle};
        use windows_sys::Win32::Storage::FileSystem::{
            BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
        };
        let mut info: BY_HANDLE_FILE_INFORMATION = std::mem::zeroed();
        if meta.file_attributes() & 0x400 != 0
            || GetFileInformationByHandle(file.as_raw_handle(), &mut info) == 0
            || info.nNumberOfLinks != 1
        {
            return Err(Error::Denied("Bağlantı/hardlink dosyası okunmadı."));
        }
    }
    Ok(ReadFile {
        file,
        _parent: parent,
    })
}
