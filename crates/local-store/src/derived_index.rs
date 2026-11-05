//! Bounded, replaceable derived search state, separate from durable evidence.
use std::{
    fs::{self, File, OpenOptions, TryLockError},
    io::{Read, Write},
    path::{Path, PathBuf},
};

use crate::{LocalStore, StoreError, StoreKey, StoreResult, crypto::ENVELOPE_OVERHEAD, service};

pub const MAX_INDEX_BYTES: usize = 64 * 1024 * 1024;
// Single entry, replaced atomically. Each refresh still rechecks actual
// source identities; age is only a storage reuse bound.
const TTL_MS: u64 = 24 * 60 * 60 * 1000;
const TIMED_HEADER: &[u8; 8] = b"MLSIDX\x02\0";
const SLOT: &str = "repository-index.v1.enc";

#[derive(Clone)]
pub struct RepositoryIndexCache {
    root: PathBuf,
    key: StoreKey,
}

impl LocalStore {
    pub fn repository_index_cache(&self) -> RepositoryIndexCache {
        RepositoryIndexCache {
            root: self.instance.root.clone(),
            key: self.instance.key.clone(),
        }
    }
}

impl RepositoryIndexCache {
    /// The trusted host chooses a managed cache directory outside source.
    pub fn open(root: &Path, key: StoreKey) -> StoreResult<Self> {
        let root = service::clean_absolute(root)?;
        service::ensure_directory(&root)?;
        Ok(Self { root, key })
    }

    // Multiple tasks can share one project cache. A writer holds the lease
    // only for replacement or removal. Readers can see either complete image.
    fn writer(&self) -> StoreResult<File> {
        let path = self.root.join("repository-index.v1.lock");
        service::reject_link_if_exists(&path)?;
        let lease = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)?;
        service::reject_link_if_exists(&path)?;
        lease.try_lock().map_err(|error| match error {
            TryLockError::WouldBlock => StoreError::Busy,
            TryLockError::Error(error) => StoreError::Io(error),
        })?;
        Ok(lease)
    }

    pub fn load(&self, binding: &[u8; 32]) -> StoreResult<Option<Vec<u8>>> {
        let path = self.root.join(SLOT);
        service::reject_link_if_exists(&path)?;
        let mut file = match File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let mut scope = [0; 32];
        file.read_exact(&mut scope)?;
        if &scope != binding {
            return Ok(None);
        }
        let mut header = [0; 8];
        file.read_exact(&mut header)?;
        if &header != TIMED_HEADER {
            // v1 held no authenticated creation time. Rebuild derived state.
            return Ok(None);
        }
        let mut created = [0; 8];
        file.read_exact(&mut created)?;
        let mut aad = [0; 48];
        aad[..32].copy_from_slice(binding);
        aad[32..40].copy_from_slice(TIMED_HEADER);
        aad[40..].copy_from_slice(&created);
        let mut envelope = Vec::new();
        let limit = MAX_INDEX_BYTES as u64 + ENVELOPE_OVERHEAD;
        file.take(limit + 1).read_to_end(&mut envelope)?;
        if envelope.len() as u64 > limit {
            return Err(StoreError::Quota("derived repository index".into()));
        }
        let bytes = self.key.open(&envelope, &aad)?;
        let created = u64::from_le_bytes(created);
        let now = service::unix_ms()?;
        Ok((created <= now && now - created < TTL_MS).then_some(bytes))
    }

    pub fn save(&self, binding: &[u8; 32], bytes: &[u8]) -> StoreResult<()> {
        if bytes.len() > MAX_INDEX_BYTES {
            return Err(StoreError::Quota("derived repository index".into()));
        }
        let path = self.root.join(SLOT);
        let created = service::unix_ms()?.to_le_bytes();
        let mut aad = [0; 48];
        aad[..32].copy_from_slice(binding);
        aad[32..40].copy_from_slice(TIMED_HEADER);
        aad[40..].copy_from_slice(&created);
        let envelope = self.key.seal(bytes, &aad)?;
        let mut staged = tempfile::Builder::new()
            .prefix(".repository-index-")
            .tempfile_in(&self.root)?;
        staged.write_all(&aad)?;
        staged.write_all(&envelope)?;
        staged.as_file().sync_all()?;
        let _writer = self.writer()?;
        service::reject_link_if_exists(&path)?;
        staged.persist(path).map_err(|error| error.error)?;
        Ok(())
    }

    pub fn clear(&self, binding: &[u8; 32]) -> StoreResult<()> {
        let _writer = self.writer()?;
        let path = self.root.join(SLOT);
        service::reject_link_if_exists(&path)?;
        let mut file = match File::open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        let mut scope = [0; 32];
        file.read_exact(&mut scope)?;
        drop(file);
        if &scope == binding {
            fs::remove_file(path)?;
        }
        Ok(())
    }
}
