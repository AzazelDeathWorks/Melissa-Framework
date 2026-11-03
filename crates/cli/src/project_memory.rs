//! Account-bound encrypted project memory. Proposals are never auto-accepted.
use crate::{
    Error, Result,
    app::{App, bytes, decode, now, safe_path},
    digest,
};
use melissa_core_contracts::{
    Module,
    wire::{TaskContractV1, TaskLimitsV1},
};
use melissa_local_store::{LocalStore, StoreConfig, StoreError, StoreKey};
use melissa_repository::{Limits, Repository};
use melissa_skill_context::{
    Namespace,
    retrieval::{Domain, SearchScope, Source},
    state::MemoryBook,
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
};

pub fn namespace(repo: &Repository) -> Namespace {
    Namespace {
        user: "cli:operator".into(),
        project: digest(
            format!("{}:{}", repo.root_path().display(), repo.root_identity()).as_bytes(),
        ),
        engagement: None,
    }
}
pub fn sources(repo: &Repository) -> Result<Vec<Source>> {
    let snap = repo.snapshot(&AtomicBool::new(false))?;
    let ns = namespace(repo);
    let time = now()?;
    let mut result = Vec::new();
    for f in &snap.files {
        if let Ok(content) = String::from_utf8(repo.read(&f.path)?.1) {
            result.push(Source {
                id: f.path.clone(),
                version: f.identity.clone() + ":" + &f.digest,
                namespace: ns.clone(),
                domain: Domain::Code,
                digest: digest(content.as_bytes()),
                content,
                observed_at_ms: time,
                valid_until_ms: time.saturating_add(60000),
                symbols: vec![],
                links: vec![],
            });
        }
    }
    repo.validate_snapshot(&snap, &AtomicBool::new(false))?;
    Ok(result)
}
pub struct ProjectMemory {
    store: LocalStore,
    owner: String,
    sequence: u64,
    pub book: MemoryBook,
    before: String,
    record_refs: BTreeMap<String, String>,
}
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct MemoryManifest {
    schema_version: String,
    namespace: Namespace,
    records: BTreeMap<String, String>,
}
fn review_digest(book: &MemoryBook) -> Result<String> {
    let mut value = serde_json::to_value(book).map_err(|_| Error::Store)?;
    for record in value["records"]
        .as_object_mut()
        .ok_or(Error::Store)?
        .values_mut()
    {
        for source in record["sources"].as_array_mut().ok_or(Error::Store)? {
            source["observed_at_ms"] = json!(0);
            source["valid_until_ms"] = json!(0);
        }
    }
    Ok(digest(&bytes(&value)?))
}
impl ProjectMemory {
    pub fn open(home: &Path, key: StoreKey, repo: &Repository) -> Result<Self> {
        let ns = namespace(repo);
        let id = ns.project.strip_prefix("blake3:").ok_or(Error::Store)?;
        let dir = home.join("projects").join(id);
        safe_path(&dir)?;
        std::fs::create_dir_all(&dir)?;
        let store_dir = dir.join("memory");
        // An existing incomplete store is not silently recreated.
        if store_dir.exists() && !store_dir.join("store.sqlite3").is_file() {
            return Err(Error::Store);
        }
        let mut store = LocalStore::open(&store_dir, key, StoreConfig::default())?
            .with_pinned_cas_inventory()?;
        let owner = format!("project-memory:{id}");
        match store.journal().task(&owner) {
            Ok(t) if t.contract.workspace_ref != ns.project => return Err(Error::Store),
            Ok(_) => {}
            Err(StoreError::NotFound(_)) => {
                store.journal_mut().create_task(&TaskContractV1 {
                    schema_version:"melissa.task-contract/v1".into(), task_id:owner.clone(), module:Module::Code,
                    goal:"Host-reviewed persistent project knowledge; no model execution".into(), workspace_ref:ns.project.clone(),
                    input_snapshot_digest:ns.project.clone(), acceptance_contract_digest:digest(b"host-reviewed-memory/v1"),
                    acceptance_criteria:vec!["Explicit host review; source-bound records are untrusted context, never task authority".into()],
                    selected_profile_ids:vec!["melissa.core.independent-qa".into()], allowed_resource_ids:vec![ns.project.clone()],
                    allowed_effects:vec!["workspace.read".into(),"memory.propose".into(),"evidence.append".into()],
                    authorization_refs:vec!["cli:local-memory-review".into()], authority_source:"user_task_and_rust_policy".into(),
                    limits:TaskLimitsV1 { max_wall_time_ms:60000,max_cost_micro_usd:0,max_input_tokens:1,max_output_tokens:1,max_parallel_jobs:1,max_disk_bytes:64*1024*1024,max_output_bytes:16*1024*1024 }
                })?;
            }
            Err(e) => return Err(e.into()),
        }
        let mut book = MemoryBook::default();
        let mut record_refs = BTreeMap::new();
        let (sequence, latest) = store
            .journal()
            .latest_checkpoint_in_series(&owner, "project-book:")?;
        if let Some(cp) = latest {
            let raw = store.get_artifact(&owner, &cp.artifact_id)?;
            if cp.data["digest"] != digest(&raw) || cp.data["namespace"] != json!(ns) {
                return Err(Error::Store);
            }
            let value: Value = decode(&raw)?;
            if value.get("schema_version").is_some() {
                let manifest: MemoryManifest = decode(&raw)?;
                if manifest.schema_version != "melissa.project-memory-manifest/v1"
                    || manifest.namespace != ns
                {
                    return Err(Error::Store);
                }
                let mut records = serde_json::Map::new();
                let mut total = 0usize;
                for (id, reference) in &manifest.records {
                    if !reference.starts_with("project-record:") {
                        return Err(Error::Store);
                    }
                    let raw_record = store.get_artifact(&owner, reference)?;
                    total = total.checked_add(raw_record.len()).ok_or(Error::Store)?;
                    if total > melissa_skill_context::MAX_STATE_BYTES
                        || reference
                            != &format!("project-record:{}", blake3::hash(&raw_record).to_hex())
                    {
                        return Err(Error::Store);
                    }
                    let record: Value = decode(&raw_record)?;
                    if record["id"] != *id {
                        return Err(Error::Store);
                    }
                    records.insert(id.clone(), record);
                }
                book =
                    serde_json::from_value(json!({"records":records})).map_err(|_| Error::Store)?;
                record_refs = manifest.records;
            } else {
                book = decode(&raw)?;
            }
        }
        if book.records().values().any(|r| r.namespace != ns) {
            return Err(Error::Store);
        }
        let before = review_digest(&book)?;
        book.revalidate_scoped(
            &ns,
            &sources(repo)?,
            &repo.snapshot(&AtomicBool::new(false))?.excluded_prefixes,
        );
        Ok(Self {
            store,
            owner,
            sequence,
            book,
            before,
            record_refs,
        })
    }
    pub fn save(&mut self, ns: &Namespace) -> Result {
        let review_hash = review_digest(&self.book)?;
        if review_hash == self.before {
            return Ok(());
        }
        if self.book.records().values().any(|r| &r.namespace != ns) {
            return Err(Error::Store);
        }
        // Validate the reconstructed book envelope before publishing any part.
        melissa_skill_context::encode_state(&self.book).map_err(|_| Error::Store)?;
        let mut refs = BTreeMap::new();
        for (id, record) in self.book.records() {
            let mut value = serde_json::to_value(record).map_err(|_| Error::Store)?;
            // Freshness is re-observed from current sources, not trusted from
            // persisted timestamps. Keep unchanged record objects reusable.
            for source in value["sources"].as_array_mut().ok_or(Error::Store)? {
                source["observed_at_ms"] = json!(0);
                source["valid_until_ms"] = json!(0);
            }
            let bytes = melissa_skill_context::encode_state(&value).map_err(|_| Error::Store)?;
            let reference = format!("project-record:{}", blake3::hash(&bytes).to_hex());
            if self.record_refs.get(id) != Some(&reference) {
                match self.store.artifact_metadata(&self.owner, &reference) {
                    Ok(_) => {
                        if self.store.get_artifact(&self.owner, &reference)? != bytes {
                            return Err(Error::Store);
                        }
                    }
                    Err(StoreError::NotFound(_)) => crate::app::put(
                        &mut self.store,
                        &self.owner,
                        &reference,
                        "memory.record",
                        &bytes,
                    )?,
                    Err(e) => return Err(e.into()),
                }
                self.store
                    .pin_artifact(&self.owner, &reference, "project-memory:records")?;
            }
            refs.insert(id.clone(), reference);
        }
        let raw = melissa_skill_context::encode_state(&MemoryManifest {
            schema_version: "melissa.project-memory-manifest/v1".into(),
            namespace: ns.clone(),
            records: refs.clone(),
        })
        .map_err(|_| Error::Store)?;
        let hash = digest(&raw);
        let id = format!("project-book:{}", self.sequence);
        let artifact = format!("{id}:bytes");
        crate::app::put(
            &mut self.store,
            &self.owner,
            &artifact,
            "memory.reviewed-book",
            &raw,
        )?;
        let version = self.store.journal().task(&self.owner)?.record.version;
        self.store.journal_mut().append_checkpoint(
            &id,
            &self.owner,
            version,
            &artifact,
            json!({"digest":hash,"namespace":ns}),
        )?;
        self.sequence += 1;
        self.before = review_hash;
        self.record_refs = refs;
        Ok(())
    }
}

impl App {
    pub fn memory(
        &self,
        workspace: &Path,
        action: &str,
        id: Option<&str>,
        expected: Option<&str>,
    ) -> Result<Value> {
        let repo = Repository::open(workspace, Limits::default(), vec![])?;
        let ns = namespace(&repo);
        let mut project = ProjectMemory::open(&self.home, self.key.clone(), &repo)?;
        if action == "accept" || action == "reject" {
            let id = id.ok_or(Error::Usage("Bellek kimliği gerekli."))?;
            let source = sources(&repo)?;
            let scope = SearchScope {
                namespace: ns.clone(),
                domain: Domain::Code,
                now_ms: now()?,
                current: source
                    .iter()
                    .map(|s| (s.id.clone(), (s.version.clone(), s.digest.clone())))
                    .collect(),
            };
            let record = project
                .book
                .records()
                .get(id)
                .ok_or(Error::Usage("Bellek bulunamadı."))?;
            let supersedes = project
                .book
                .records()
                .values()
                .filter(|r| {
                    r.key == record.key
                        && r.id != id
                        && r.status == melissa_skill_context::state::MemoryStatus::Accepted
                })
                .map(|r| r.id.clone())
                .collect::<BTreeSet<_>>();
            // Accepting a replacement needs the digest covering the entire book,
            // including the superseded facts shown by `memory list`.
            if expected != Some(review_digest(&project.book)?.as_str()) {
                return Err(Error::Conflict(
                    "Bellek listesi değişti; güncel review_digest gerekli.",
                ));
            }
            let record_digest = digest(&bytes(record)?);
            project
                .book
                .review(
                    id,
                    &record_digest,
                    &scope,
                    action == "accept",
                    "cli:explicit-user-review",
                    &supersedes,
                )
                .map_err(|_| Error::Denied("Bellek kaynak veya inceleme sınırı."))?;
        } else if action == "forget" {
            if expected != Some(review_digest(&project.book)?.as_str()) {
                return Err(Error::Conflict("Güncel review_digest gerekli."));
            }
            project
                .book
                .delete(&ns, id.ok_or(Error::Usage("Bellek kimliği gerekli."))?)
                .map_err(|_| Error::Store)?;
        } else if action != "list" {
            return Err(Error::Usage("Bilinmeyen bellek işlemi."));
        }
        project.save(&ns)?;
        let review_digest = review_digest(&project.book)?;
        // Source bytes are kept encrypted, but never dumped by the listing.
        let records=project.book.records().values().map(|r|json!({"id":r.id,"key":r.key,"claim":r.claim,"status":r.status,"sources":r.sources.iter().map(|s|json!({"path":s.id,"digest":s.digest})).collect::<Vec<_>>()})).collect::<Vec<_>>();
        Ok(
            json!({"command":"memory","action":action,"project":ns.project,"records":records,"review_digest":review_digest,"model_calls":0,"forget_semantics":"excluded from retrieval; encrypted historical audit is retained"}),
        )
    }
}

#[derive(Clone)]
pub struct Location {
    pub home: PathBuf,
    pub key: StoreKey,
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn manifest_reuses_unchanged_records_and_reopens_latest_book_without_auto_acceptance() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("source");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("main.py"), "value = 1\n").unwrap();
        let repo = Repository::open(&root, Limits::default(), vec![]).unwrap();
        let home = temp.path().join("home");
        let key = StoreKey::from_bytes([213; 32]);
        let ns = namespace(&repo);
        let mut memory = ProjectMemory::open(&home, key.clone(), &repo).unwrap();
        let current = sources(&repo).unwrap();
        memory
            .book
            .propose("first", ns.clone(), "value", "one", current.clone())
            .unwrap();
        memory.save(&ns).unwrap();
        let original_ref = memory.record_refs["first"].clone();
        let sequence = memory.sequence;
        memory.save(&ns).unwrap();
        assert_eq!(memory.sequence, sequence);
        memory
            .book
            .propose("second", ns.clone(), "format", "python", current)
            .unwrap();
        memory.save(&ns).unwrap();
        assert_eq!(memory.record_refs["first"], original_ref);
        let manifest_count = memory.sequence;
        drop(memory);
        let mut memory = ProjectMemory::open(&home, key.clone(), &repo).unwrap();
        assert_eq!(memory.sequence, manifest_count);
        assert_eq!(memory.book.records().len(), 2);
        assert!(
            memory
                .book
                .records()
                .values()
                .all(|r| r.status == melissa_skill_context::state::MemoryStatus::Proposed)
        );
        memory.save(&ns).unwrap();
        assert_eq!(memory.sequence, manifest_count);
        memory.book.delete(&ns, "second").unwrap();
        memory.save(&ns).unwrap();
        assert_eq!(memory.record_refs["first"], original_ref);
        drop(memory);
        let memory = ProjectMemory::open(&home, key, &repo).unwrap();
        assert_eq!(memory.book.records()["first"].claim, "one");
        assert_eq!(
            memory.book.records()["second"].status,
            melissa_skill_context::state::MemoryStatus::Deleted
        );
        assert!(memory.book.records()["second"].claim.is_empty());
        assert_eq!(memory.record_refs["first"], original_ref);
    }
}
