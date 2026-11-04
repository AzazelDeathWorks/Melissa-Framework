use std::{
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicUsize, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use crate::{
    StoreError, StoreResult,
    cas::Cas,
    crypto::StoreKey,
    journal::{Journal, JournalLimits},
};

#[derive(Debug, Clone)]
pub struct StoreConfig {
    pub max_blob_bytes: u64,
    /// Physical bytes of encrypted published CAS objects, including orphans.
    /// Temporary staging, backup exports and SQLite WAL are separate resources.
    pub max_cas_bytes: u64,
    pub journal: JournalLimits,
}

impl Default for StoreConfig {
    fn default() -> Self {
        Self {
            max_blob_bytes: 64 * 1024 * 1024,
            max_cas_bytes: 1024 * 1024 * 1024,
            journal: JournalLimits::default(),
        }
    }
}

impl StoreConfig {
    pub fn validate(&self) -> StoreResult<()> {
        self.journal.validate()?;
        let maximum = crate::crypto::stored_len_for_plain(self.max_blob_bytes)?;
        if maximum >= usize::MAX as u64 || self.max_cas_bytes < maximum {
            return Err(StoreError::Invalid(
                "CAS quota must fit one maximum-sized encrypted blob".into(),
            ));
        }
        Ok(())
    }
}

pub(crate) struct Instance {
    pub root: PathBuf,
    pub key: StoreKey,
    pub config: StoreConfig,
    // Publication, GC and backup use one order: process guard, then SQLite.
    pub files: Mutex<()>,
    pub inventory_readers: AtomicUsize,
    pub publications_since_check: AtomicUsize,
    #[cfg(windows)]
    pub inventory: Mutex<Option<crate::pinned_inventory::PinnedInventory>>,
    _lease: File,
}

/// One host session owns recovery. `connect` creates independent SQLite
/// connections sharing that lease; another process cannot take over live work.
pub struct LocalStore {
    pub(crate) journal: Journal,
    pub(crate) cas: Cas,
    pub(crate) instance: Arc<Instance>,
}

impl LocalStore {
    pub fn open(root: &Path, key: StoreKey, config: StoreConfig) -> StoreResult<Self> {
        config.validate()?;
        let root = clean_absolute(root)?;
        ensure_directory(&root)?;
        let lock_path = root.join("instance.lock");
        reject_link_if_exists(&lock_path)?;
        let lease = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)?;
        reject_link_if_exists(&lock_path)?;
        lease.try_lock().map_err(|error| match error {
            std::fs::TryLockError::WouldBlock => StoreError::Busy,
            std::fs::TryLockError::Error(error) => StoreError::Io(error),
        })?;
        let instance = Arc::new(Instance {
            root,
            key,
            config,
            files: Mutex::new(()),
            inventory_readers: AtomicUsize::new(1),
            publications_since_check: AtomicUsize::new(0),
            #[cfg(windows)]
            inventory: Mutex::new(None),
            _lease: lease,
        });
        let mut store = Self::connect_instance(instance, true)?;
        store.check_artifact_integrity()?;
        // No tool is executed or retried here. Possibly dispatched work becomes
        // explicitly unknown, under the exclusive session lease.
        store.journal.recover_interrupted()?;
        store.journal.recover_interrupted_model_requests()?;
        Ok(store)
    }

    fn connect_instance(instance: Arc<Instance>, initial: bool) -> StoreResult<Self> {
        let db_path = instance.root.join("store.sqlite3");
        reject_link_if_exists(&db_path)?;
        for suffix in ["-wal", "-shm", "-journal"] {
            reject_link_if_exists(&instance.root.join(format!("store.sqlite3{suffix}")))?;
        }
        let journal = if initial {
            Journal::open(&db_path, instance.key.clone(), instance.config.journal)?
        } else {
            Journal::connect_session(&db_path, instance.key.clone(), instance.config.journal)?
        };
        let cas = Cas::open(
            &instance.root.join("cas"),
            instance.key.clone(),
            instance.config.max_blob_bytes,
        )?;
        Ok(Self {
            journal,
            cas,
            instance,
        })
    }

    pub fn connect(&self) -> StoreResult<Self> {
        Self::connect_instance(Arc::clone(&self.instance), false)
    }
    /// Authenticate once, then hold immutable Windows handles. Other processes
    /// cannot rewrite/delete cached blobs; new/orphan objects are still checked.
    /// Unsupported platforms retain full verification on every publication.
    pub fn with_pinned_cas_inventory(self) -> StoreResult<Self> {
        #[cfg(windows)]
        {
            let _guard = self.file_guard()?;
            let mut pinned = crate::pinned_inventory::PinnedInventory::default();
            pinned.list(&self.cas)?;
            *self
                .instance
                .inventory
                .lock()
                .map_err(|_| StoreError::Integrity("inventory lock poisoned".into()))? =
                Some(pinned);
        }
        Ok(self)
    }
    #[cfg(windows)]
    pub fn inventory_metrics(&self) -> StoreResult<Option<crate::InventoryMetrics>> {
        Ok(self
            .instance
            .inventory
            .lock()
            .map_err(|_| StoreError::Integrity("inventory lock poisoned".into()))?
            .as_ref()
            .map(|i| i.metrics))
    }
    pub(crate) fn publication_inventory(&self) -> StoreResult<Vec<crate::cas::BlobInfo>> {
        #[cfg(windows)]
        if let Some(pinned) = self
            .instance
            .inventory
            .lock()
            .map_err(|_| StoreError::Integrity("inventory lock poisoned".into()))?
            .as_mut()
        {
            return pinned.list(&self.cas);
        }
        self.cas
            .list_with_parallelism(self.instance.inventory_readers.load(Ordering::Acquire))
    }
    pub(crate) fn release_inventory_pins(&self) -> StoreResult<()> {
        #[cfg(windows)]
        if let Some(pinned) = self
            .instance
            .inventory
            .lock()
            .map_err(|_| StoreError::Integrity("inventory lock poisoned".into()))?
            .as_mut()
        {
            pinned.clear();
        }
        Ok(())
    }

    /// Trusted host computation setting shared with this session's connections.
    /// Defaults to one reader. This never skips authentication, changes quotas,
    /// or grants a model/tool any authority. Opening, GC and backup retain their
    /// existing full checks. Callers budget transient blob memory for readers.
    pub fn with_parallel_cas_verification(self, readers: usize) -> StoreResult<Self> {
        if !(1..=4).contains(&readers) {
            return Err(StoreError::Invalid(
                "CAS verification readers must be1..4".into(),
            ));
        }
        self.instance
            .inventory_readers
            .store(readers, Ordering::Release);
        Ok(self)
    }

    pub fn root(&self) -> &Path {
        &self.instance.root
    }

    /// Host domain persistence. Model/tool adapters must go through the future
    /// broker; possessing an ID is not a principal authorization check.
    pub fn journal(&self) -> &Journal {
        &self.journal
    }
    /// Low-level DB-only host operations. Use LocalStore's artifact-bearing
    /// methods below when a CAS object must also be authenticated.
    pub fn journal_mut(&mut self) -> &mut Journal {
        &mut self.journal
    }

    pub fn set_final_artifact(
        &mut self,
        task_id: &str,
        expected_version: u64,
        artifact_id: &str,
    ) -> StoreResult<crate::journal::StoredTask> {
        self.get_artifact(task_id, artifact_id)?;
        self.journal
            .set_final_artifact(task_id, expected_version, artifact_id)
    }

    pub fn transition_task(
        &mut self,
        task_id: &str,
        expected_version: u64,
        next: melissa_core_contracts::TaskState,
        evidence: Option<&melissa_core_contracts::CompletionEvidence>,
    ) -> StoreResult<crate::journal::StoredTask> {
        if next == melissa_core_contracts::TaskState::Completed {
            if let Some(proof) = evidence {
                for artifact in &proof.raw_artifact_refs {
                    self.get_artifact(task_id, artifact)?;
                }
            }
            let task = self.journal.task(task_id)?;
            if let Some(digest) = &task.record.final_artifact_digest {
                let artifact: String = self.journal.conn.query_row(
                    "SELECT artifact_id FROM artifacts WHERE task_id=?1 AND digest=?2 LIMIT 1",
                    rusqlite::params![task_id, digest],
                    |row| row.get(0),
                )?;
                self.get_artifact(task_id, &artifact)?;
            }
        }
        self.journal
            .transition_task(task_id, expected_version, next, evidence)
    }

    pub fn append_checkpoint(
        &mut self,
        checkpoint_id: &str,
        task_id: &str,
        expected_version: u64,
        artifact_id: &str,
        data: serde_json::Value,
    ) -> StoreResult<crate::journal::Checkpoint> {
        self.get_artifact(task_id, artifact_id)?;
        self.journal
            .append_checkpoint(checkpoint_id, task_id, expected_version, artifact_id, data)
    }

    pub fn reconcile_invocation(
        &mut self,
        expected_task_version: u64,
        expected_invocation_version: u64,
        proof: &melissa_core_contracts::ReconciliationEvidence,
    ) -> StoreResult<crate::journal::StoredTask> {
        self.get_artifact(&proof.task_id, &proof.resolution_artifact_ref)?;
        self.journal
            .reconcile_invocation(expected_task_version, expected_invocation_version, proof)
    }

    pub fn finish_attempt(
        &mut self,
        guard: &crate::journal::AttemptGuard,
        state: crate::journal::AttemptState,
        result_artifact_id: Option<&str>,
    ) -> StoreResult<crate::journal::AttemptRecord> {
        if let Some(artifact) = result_artifact_id {
            self.get_artifact(&guard.task_id, artifact)?;
        }
        self.journal
            .finish_attempt(guard, state, result_artifact_id)
    }

    pub(crate) fn file_guard(&self) -> StoreResult<MutexGuard<'_, ()>> {
        self.instance
            .files
            .lock()
            .map_err(|_| StoreError::Integrity("store file coordination lock poisoned".into()))
    }

    pub fn integrity_check(&self) -> StoreResult<()> {
        let _guard = self.file_guard()?;
        self.journal.integrity_check()?;
        self.check_artifact_integrity()
    }
}

pub(crate) fn unix_ms() -> StoreResult<u64> {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| StoreError::Invalid("clock precedes Unix epoch".into()))?
        .as_millis();
    let value = u64::try_from(millis).map_err(|_| StoreError::Invalid("clock overflow".into()))?;
    crate::journal::integer(value)?;
    Ok(value)
}

pub(crate) fn clean_absolute(path: &Path) -> StoreResult<PathBuf> {
    crate::cas::absolute_clean_path(path)
}

pub(crate) fn reject_link_if_exists(path: &Path) -> StoreResult<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            let mut linked = metadata.file_type().is_symlink();
            #[cfg(windows)]
            {
                use std::os::windows::fs::MetadataExt;
                linked |= metadata.file_attributes() & 0x400 != 0;
            }
            if linked {
                return Err(StoreError::Invalid("linked or reparse store path".into()));
            }
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

pub(crate) fn ensure_directory(path: &Path) -> StoreResult<()> {
    crate::cas::ensure_directory(path)
}
