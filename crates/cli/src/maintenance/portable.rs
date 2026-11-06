//! Chunked authenticated portable task archive, built on native store backup.
//! Recovery key remains separate; restore is evidence-only, never effect replay.
use super::{destination, disjoint, open_read, write_new};
use crate::{
    Error, Result,
    app::{App, absolute, bytes, decode, nonce, now, task_store_config, validate_id},
    digest,
};
use melissa_local_store::{LocalStore, StoreKey};
use melissa_repository::Repository;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs,
    io::{Read, Write},
    path::Path,
};
use zeroize::Zeroizing;
const CHUNK: u64 = 1024 * 1024;
const AAD: &[u8] = b"melissa.portable-task-manifest/v1";
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    path: String,
    bytes: u64,
    digest: String,
    cipher_digest: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    schema_version: String,
    cli_version: String,
    store_format: String,
    archive_id: String,
    task_id: String,
    wrapped_key: Vec<u8>,
    files: Vec<Entry>,
}
fn key(path: &Path) -> Result<StoreKey> {
    let path = absolute(path)?;
    let mut input = open_read(&path)?;
    if input.file.metadata()?.len() != 32 {
        return Err(Error::Denied(
            "Kurtarma anahtarı tam 32 rastgele byte olmalı; parola veya API anahtarı kullanma.",
        ));
    }
    let mut secret = Zeroizing::new([0; 32]);
    input.file.read_exact(&mut *secret)?;
    Ok(StoreKey::from_bytes(*secret))
}
pub fn keygen(out: &Path) -> Result<Value> {
    let (out, _guard) = destination(None, None, out)?;
    disjoint(&out, &std::env::current_dir()?)?;
    let mut secret = Zeroizing::new([0u8; 32]);
    getrandom::fill(&mut *secret).map_err(|_| Error::Store)?;
    write_new(&out, &*secret)?;
    Ok(
        json!({"command":"maintenance keygen","out":out,"status":"PASS","key_material":"not_printed","keep_separate_from_archive":true,"model_calls":0}),
    )
}
fn chunk_aad(archive: &str, index: usize, chunk: u64, size: u64) -> Vec<u8> {
    format!("melissa.portable-task-chunk/v1:{archive}:{index}:{chunk}:{size}").into_bytes()
}
fn key_aad(archive: &str) -> Vec<u8> {
    format!("melissa.portable-task-key/v1:{archive}").into_bytes()
}
fn safe_entry(path: &str) -> bool {
    if ["store.sqlite3", "manifest.enc"].contains(&path) {
        return true;
    }
    path.strip_prefix("cas/blobs/")
        .and_then(|p| p.strip_suffix(".blob"))
        .is_some_and(|h| {
            h.len() == 64
                && h.bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
}
fn encrypt(
    source: &Path,
    target: &Path,
    recovery: &StoreKey,
    archive: &str,
    index: usize,
) -> Result<Entry> {
    let mut input = open_read(source)?;
    let size = input.file.metadata()?.len();
    let mut output = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(target)?;
    let mut hash = blake3::Hasher::new();
    let mut cipher_hash = blake3::Hasher::new();
    let mut remaining = size;
    let mut chunk = 0;
    while remaining > 0 {
        let length = remaining.min(CHUNK) as usize;
        let mut buffer = Zeroizing::new(vec![0; length]);
        input.file.read_exact(&mut buffer)?;
        hash.update(&buffer);
        let sealed = recovery.seal(&buffer, &chunk_aad(archive, index, chunk, size))?;
        cipher_hash.update(&sealed);
        output.write_all(&sealed)?;
        remaining -= length as u64;
        chunk += 1;
    }
    if input.file.read(&mut [0u8; 1])? != 0 {
        return Err(Error::Conflict("Yedek kaynağı büyüdü."));
    }
    output.sync_all()?;
    Ok(Entry {
        path: String::new(),
        bytes: size,
        digest: format!("blake3:{}", hash.finalize()),
        cipher_digest: format!("blake3:{}", cipher_hash.finalize()),
    })
}
fn decrypt(
    source: &Path,
    target: &Path,
    recovery: &StoreKey,
    manifest: &Manifest,
    index: usize,
    entry: &Entry,
) -> Result {
    let mut input = open_read(source)?;
    let expected = entry
        .bytes
        .checked_add(
            entry
                .bytes
                .div_ceil(CHUNK)
                .checked_mul(48)
                .ok_or(Error::Store)?,
        )
        .ok_or(Error::Store)?;
    if input.file.metadata()?.len() != expected {
        return Err(Error::Store);
    }
    let mut output = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(target)?;
    let mut remaining = entry.bytes;
    let mut chunk = 0;
    let mut hash = blake3::Hasher::new();
    let mut cipher_hash = blake3::Hasher::new();
    while remaining > 0 {
        let length = remaining.min(CHUNK);
        let mut envelope = vec![0; (length + 48) as usize];
        input.file.read_exact(&mut envelope)?;
        cipher_hash.update(&envelope);
        let plain = Zeroizing::new(recovery.open(
            &envelope,
            &chunk_aad(&manifest.archive_id, index, chunk, entry.bytes),
        )?);
        if plain.len() as u64 != length {
            return Err(Error::Store);
        }
        hash.update(&plain);
        output.write_all(&plain)?;
        remaining -= length;
        chunk += 1;
    }
    output.sync_all()?;
    if entry.digest != format!("blake3:{}", hash.finalize())
        || entry.cipher_digest != format!("blake3:{}", cipher_hash.finalize())
    {
        return Err(Error::Store);
    }
    Ok(())
}
impl App {
    pub fn backup_task(&self, id: &str, out: &Path, key_file: &Path) -> Result<Value> {
        let store = self.store(id)?;
        crate::continuity::settled(self, &store, id)?;
        let (out, _parent) = destination(Some(self), None, out)?;
        disjoint(&out, Path::new(&self.workspace(&store, id)?.root))?;
        let key_file = absolute(key_file)?;
        disjoint(&key_file, &out)?;
        disjoint(&key_file, &self.home)?;
        disjoint(&key_file, Path::new(&self.workspace(&store, id)?.root))?;
        let recovery = key(&key_file)?;
        let staging = tempfile::tempdir_in(out.parent().ok_or(Error::Io)?)?;
        let native = staging.path().join("native");
        let native_manifest = store.backup(&native)?;
        fs::create_dir(&out)?;
        let _out_pin = Repository::open(&out, Default::default(), vec![])?;
        let archive = nonce()?;
        let mut paths = vec!["store.sqlite3".to_owned(), "manifest.enc".to_owned()];
        paths.extend(
            native_manifest
                .blobs
                .iter()
                .map(|b| format!("cas/blobs/{}.blob", &b.content.digest[7..])),
        );
        let mut files = vec![];
        for (index, path) in paths.iter().enumerate() {
            let mut entry = encrypt(
                &native.join(path),
                &out.join(format!("part-{index:06}.enc")),
                &recovery,
                &archive,
                index,
            )?;
            entry.path = path.clone();
            files.push(entry);
        }
        let manifest = Manifest {
            schema_version: "melissa.portable-task/v1".into(),
            cli_version: env!("CARGO_PKG_VERSION").into(),
            store_format: "melissa.local-store-backup/v1".into(),
            archive_id: archive.clone(),
            task_id: id.into(),
            wrapped_key: self.key.wrap_for_backup(&recovery, &key_aad(&archive))?,
            files,
        };
        let raw = Zeroizing::new(bytes(&manifest)?);
        if raw.len() > 16 * 1024 * 1024 {
            return Err(Error::Store);
        }
        write_new(&out.join("archive.enc"), &recovery.seal(&raw, AAD)?)?;
        Ok(
            json!({"command":"maintenance backup","task_id":id,"status":"PASS","out":out,"files":manifest.files.len(),"encrypted":"all archive files including database and metadata; separate recovery key","scope":"task store and child evidence; no project source writeback, provider settings, project memory index, control mailbox or worker state","restore_policy":"new home, evidence only, never automatic resume","model_calls":0}),
        )
    }
}
pub fn restore(source: &Path, out: &Path, key_file: &Path) -> Result<Value> {
    let source = absolute(source)?;
    let _source_pin = Repository::open(&source, Default::default(), vec![])?;
    let (out, _parent) = destination(None, None, out)?;
    disjoint(&source, &out)?;
    let key_file = absolute(key_file)?;
    disjoint(&key_file, &out)?;
    disjoint(&key_file, &source)?;
    let recovery = key(&key_file)?;
    let mut input = open_read(&source.join("archive.enc"))?;
    if input.file.metadata()?.len() > 16 * 1024 * 1024 + 48 {
        return Err(Error::Store);
    }
    let mut raw = vec![];
    input.file.read_to_end(&mut raw)?;
    let plain = Zeroizing::new(recovery.open(&raw, AAD)?);
    let manifest: Manifest = decode(&plain)?;
    if manifest.schema_version != "melissa.portable-task/v1"
        || manifest.store_format != "melissa.local-store-backup/v1"
        || manifest.cli_version != env!("CARGO_PKG_VERSION")
        || manifest.archive_id.len() != 32
        || !manifest.archive_id.bytes().all(|b| b.is_ascii_hexdigit())
        || manifest.files.len() > 100_002
    {
        return Err(Error::Conflict(
            "Yedek sürümü desteklenmiyor; kaynak yedek değişmedi.",
        ));
    }
    validate_id(&manifest.task_id)?;
    let mut paths = std::collections::BTreeSet::new();
    let mut total = 0u64;
    for entry in &manifest.files {
        if !safe_entry(&entry.path)
            || !paths.insert(entry.path.clone())
            || entry.bytes > task_store_config().max_blob_bytes + 48
        {
            return Err(Error::Store);
        }
        total = total.checked_add(entry.bytes).ok_or(Error::Store)?;
    }
    if total
        > task_store_config().max_cas_bytes
            + task_store_config().journal.max_database_bytes
            + 16 * 1024 * 1024
        || !paths.contains("store.sqlite3")
        || !paths.contains("manifest.enc")
    {
        return Err(Error::Store);
    }
    let secret =
        Zeroizing::new(recovery.open(&manifest.wrapped_key, &key_aad(&manifest.archive_id))?);
    let secret_array: &[u8; 32] = secret.as_slice().try_into().map_err(|_| Error::Store)?;
    let staging = tempfile::tempdir_in(out.parent().ok_or(Error::Io)?)?;
    let native = staging.path().join("native");
    fs::create_dir(&native)?;
    fs::create_dir_all(native.join("cas/blobs"))?;
    fs::create_dir(native.join("cas/staging"))?;
    for (index, entry) in manifest.files.iter().enumerate() {
        decrypt(
            &source.join(format!("part-{index:06}.enc")),
            &native.join(&entry.path),
            &recovery,
            &manifest,
            index,
            entry,
        )?;
    }
    // Authenticate native schema, journal and CAS in staging before publishing.
    let staged_home = staging.path().join("restored-home");
    let original_key = App::restored_home(&staged_home, secret_array)?;
    let task_dir = staged_home.join("tasks").join(&manifest.task_id);
    fs::create_dir(&task_dir)?;
    let restored = LocalStore::restore_backup(
        &native,
        &task_dir.join("store"),
        original_key.clone(),
        task_store_config(),
    )?;
    let task = restored.journal().task(&manifest.task_id)?;
    let artifacts = restored.list_artifacts(&manifest.task_id)?.len();
    let boundary = bytes(
        &json!({"schema_version":"melissa.restore-boundary/v1","task_id":manifest.task_id,"restored_at_ms":now()?,"archive_digest":digest(&raw),"automatic_resume":false,"new_task_required":true}),
    )?;
    write_new(
        &task_dir.join("restore-boundary.enc"),
        &original_key.seal(
            &boundary,
            format!("melissa.restore-boundary/v1:{}", manifest.task_id).as_bytes(),
        )?,
    )?;
    drop(restored);
    publish(&staged_home, &out)?;
    Ok(
        json!({"command":"maintenance restore","status":"PASS","task_id":manifest.task_id,"state":task.record.state,"out":out,"restored_artifacts":artifacts,"evidence_only":true,"source_archive_unchanged":true,"automatic_resume":false,"provider_configuration_restored":false,"model_calls":0}),
    )
}

fn publish(source: &Path, out: &Path) -> Result {
    #[cfg(windows)]
    unsafe {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Storage::FileSystem::{MOVEFILE_WRITE_THROUGH, MoveFileExW};
        let source = source
            .as_os_str()
            .encode_wide()
            .chain([0])
            .collect::<Vec<_>>();
        let target = out.as_os_str().encode_wide().chain([0]).collect::<Vec<_>>();
        if MoveFileExW(source.as_ptr(), target.as_ptr(), MOVEFILE_WRITE_THROUGH) == 0 {
            return Err(Error::Conflict(
                "Geri yükleme hedefi değişti veya yayınlanamadı; mevcut hedef korunur.",
            ));
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = (source, out);
        Err(Error::Unavailable(
            "Taşınabilir home geri yükleme Windows gerektirir.",
        ))
    }
}
