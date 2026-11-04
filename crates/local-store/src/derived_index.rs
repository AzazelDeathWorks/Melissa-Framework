//! Bounded, replaceable derived search state, separate from durable evidence.
use std::{
    fs::{self, File},
    io::{Read, Write},
};

use crate::{LocalStore, StoreError, StoreResult, crypto::ENVELOPE_OVERHEAD, service};

pub const MAX_INDEX_BYTES: usize = 64 * 1024 * 1024;
const SLOT: &str = "repository-index.v1.enc";

impl LocalStore {
    pub fn load_repository_index(&self, binding: &[u8; 32]) -> StoreResult<Option<Vec<u8>>> {
        let _guard = self.file_guard()?;
        let path = self.instance.root.join(SLOT);
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
        let mut envelope = Vec::new();
        let limit = MAX_INDEX_BYTES as u64 + ENVELOPE_OVERHEAD;
        file.take(limit + 1).read_to_end(&mut envelope)?;
        if envelope.len() as u64 > limit {
            return Err(StoreError::Quota("derived repository index".into()));
        }
        self.instance.key.open(&envelope, binding).map(Some)
    }

    pub fn save_repository_index(&self, binding: &[u8; 32], bytes: &[u8]) -> StoreResult<()> {
        if bytes.len() > MAX_INDEX_BYTES {
            return Err(StoreError::Quota("derived repository index".into()));
        }
        let _guard = self.file_guard()?;
        let path = self.instance.root.join(SLOT);
        service::reject_link_if_exists(&path)?;
        let envelope = self.instance.key.seal(bytes, binding)?;
        let mut staged = tempfile::Builder::new()
            .prefix(".repository-index-")
            .tempfile_in(&self.instance.root)?;
        staged.write_all(binding)?;
        staged.write_all(&envelope)?;
        staged.as_file().sync_all()?;
        staged.persist(path).map_err(|error| error.error)?;
        Ok(())
    }

    pub fn clear_repository_index(&self, binding: &[u8; 32]) -> StoreResult<()> {
        let _guard = self.file_guard()?;
        let path = self.instance.root.join(SLOT);
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
