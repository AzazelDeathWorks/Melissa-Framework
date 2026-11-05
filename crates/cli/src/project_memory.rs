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
use melissa_skill_context::{Namespace, memory_sources::ObservedSources, state::MemoryBook};
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
#[cfg(test)]
pub fn sources(repo: &Repository) -> Result<Vec<melissa_skill_context::retrieval::Source>> {
    use melissa_skill_context::retrieval::{Domain, Source};
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
    depth: u8,
    record_refs: BTreeMap<String, String>,
}
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct MemoryManifest {
    schema_version: String,
    namespace: Namespace,
    records: BTreeMap<String, String>,
    #[serde(default)]
    base: Option<String>,
    #[serde(default)]
    depth: u8,
}
const SERIES: &str = "project-book:";
const MANIFEST_V1: &str = "melissa.project-memory-manifest/v1";
const MANIFEST_V2: &str = "melissa.project-memory-manifest/v2";
const MAX_DEPTH: u8 = 31;

fn observed(
    repo: &Repository,
    ns: &Namespace,
    book: &MemoryBook,
    time: u64,
) -> Result<ObservedSources> {
    ObservedSources::observe(
        repo,
        ns,
        book.source_ids(ns),
        time,
        time.saturating_add(60_000),
        &AtomicBool::new(false),
    )
    .map_err(|_| Error::Store)
}

fn checked_checkpoint(
    store: &LocalStore,
    owner: &str,
    ns: &Namespace,
    cp: &melissa_local_store::journal::Checkpoint,
) -> Result<Vec<u8>> {
    let raw = store.get_artifact(owner, &cp.artifact_id)?;
    if cp.data["digest"] != digest(&raw) || cp.data["namespace"] != json!(ns) {
        return Err(Error::Store);
    }
    Ok(raw)
}

fn put_immutable(
    store: &mut LocalStore,
    owner: &str,
    reference: &str,
    kind: &str,
    bytes: &[u8],
) -> Result {
    match store.artifact_metadata(owner, reference) {
        Ok(metadata) => {
            if metadata.logical_type != format!("cli.{kind}")
                || store.get_artifact(owner, reference)? != bytes
            {
                return Err(Error::Store);
            }
        }
        Err(StoreError::NotFound(_)) => crate::app::put(store, owner, reference, kind, bytes)?,
        Err(e) => return Err(e.into()),
    }
    Ok(())
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
        let mut depth = MAX_DEPTH;
        let (sequence, latest) = store
            .journal()
            .latest_checkpoint_in_series(&owner, SERIES)?;
        if let Some(cp) = latest {
            let raw = checked_checkpoint(&store, &owner, &ns, &cp)?;
            let value: Value = decode(&raw)?;
            if value.get("schema_version").is_none() {
                book = decode(&raw)?;
            } else {
                let mut manifest: MemoryManifest = decode(&raw)?;
                depth = if manifest.schema_version == MANIFEST_V1 {
                    MAX_DEPTH
                } else {
                    manifest.depth
                };
                let mut index = sequence.checked_sub(1).ok_or(Error::Store)?;
                loop {
                    if manifest.namespace != ns
                        || manifest.depth > MAX_DEPTH
                        || !matches!(manifest.schema_version.as_str(), MANIFEST_V1 | MANIFEST_V2)
                        || (manifest.schema_version == MANIFEST_V1 && manifest.depth != 0)
                        || manifest.base.is_some() != (manifest.depth > 0)
                    {
                        return Err(Error::Store);
                    }
                    for (id, reference) in manifest.records {
                        record_refs.entry(id).or_insert(reference);
                    }
                    if record_refs.len() > melissa_skill_context::state::MAX_MEMORY_RECORDS {
                        return Err(Error::Store);
                    }
                    let Some(base) = manifest.base else { break };
                    index = index.checked_sub(1).ok_or(Error::Store)?;
                    if base != format!("{SERIES}{index}") {
                        return Err(Error::Store);
                    }
                    let cp = store.journal().checkpoint(&base, &owner)?;
                    let raw = checked_checkpoint(&store, &owner, &ns, &cp)?;
                    let prior: MemoryManifest = decode(&raw)?;
                    if prior.schema_version != MANIFEST_V2
                        || prior.depth.checked_add(1) != Some(manifest.depth)
                    {
                        return Err(Error::Store);
                    }
                    manifest = prior;
                }
                let mut records = serde_json::Map::new();
                let mut total = 0usize;
                for (id, reference) in &record_refs {
                    let raw = store.get_artifact(&owner, reference)?;
                    total = total.checked_add(raw.len()).ok_or(Error::Store)?;
                    if total > melissa_skill_context::MAX_STATE_BYTES
                        || reference != &format!("project-record:{}", blake3::hash(&raw).to_hex())
                    {
                        return Err(Error::Store);
                    }
                    let record: Value = decode(&raw)?;
                    if record["id"] != *id {
                        return Err(Error::Store);
                    }
                    if let Some(reference) = record["review_evidence_ref"]
                        .as_str()
                        .filter(|r| r.starts_with("project-review:"))
                    {
                        let proof = store.get_artifact(&owner, reference)?;
                        let proof_value: Value = decode(&proof)?;
                        if reference != format!("project-review:{}", blake3::hash(&proof).to_hex())
                            || proof_value["id"] != *id
                            || proof_value["namespace"] != json!(ns)
                            || proof_value["schema_version"] != "melissa.project-memory-review/v1"
                        {
                            return Err(Error::Store);
                        }
                    }
                    records.insert(id.clone(), record);
                }
                book =
                    serde_json::from_value(json!({"records":records})).map_err(|_| Error::Store)?;
            }
        }
        if book.records().values().any(|r| r.namespace != ns) {
            return Err(Error::Store);
        }
        let time = now()?;
        let current = observed(repo, &ns, &book, time)?;
        book.revalidate_references(
            &ns,
            current.references(),
            repo.source_visibility().excluded_prefixes(),
        );
        Ok(Self {
            store,
            owner,
            sequence,
            book,
            depth,
            record_refs,
        })
    }

    pub fn save(&mut self, ns: &Namespace) -> Result {
        let mut book = std::mem::take(&mut self.book);
        let result = self.save_book(&mut book, ns);
        self.book = book;
        result
    }

    pub fn save_book(&mut self, book: &mut MemoryBook, ns: &Namespace) -> Result {
        if book.dirty_ids().is_empty() {
            return Ok(());
        }
        let compact = self.sequence == 0 || self.depth >= MAX_DEPTH;
        let mut changed = BTreeMap::new();
        for (id, record) in book.records() {
            if !book.dirty_ids().contains(id) && self.record_refs.contains_key(id) {
                continue;
            }
            if &record.namespace != ns {
                return Err(Error::Store);
            }
            let mut value = serde_json::to_value(record).map_err(|_| Error::Store)?;
            // Freshness is re-observed. Time-only renewals do not write data.
            for source in value["sources"].as_array_mut().ok_or(Error::Store)? {
                source["observed_at_ms"] = json!(0);
                source["valid_until_ms"] = json!(0);
            }
            let raw = melissa_skill_context::encode_state(&value).map_err(|_| Error::Store)?;
            let restored: melissa_skill_context::state::Memory = decode(&raw)?;
            if &restored.id != id || &restored.namespace != ns {
                return Err(Error::Store);
            }
            let reference = format!("project-record:{}", blake3::hash(&raw).to_hex());
            if self.record_refs.get(id) != Some(&reference) {
                changed.insert(id.clone(), (reference, raw));
            }
        }
        if changed.is_empty() {
            book.mark_persisted();
            return Ok(());
        }
        let depth = if compact { 0 } else { self.depth + 1 };
        let mut records = if compact {
            self.record_refs.clone()
        } else {
            BTreeMap::new()
        };
        records.extend(
            changed
                .iter()
                .map(|(id, (reference, _))| (id.clone(), reference.clone())),
        );
        let raw = melissa_skill_context::encode_state(&MemoryManifest {
            schema_version: MANIFEST_V2.into(),
            namespace: ns.clone(),
            records,
            base: (!compact).then(|| format!("{SERIES}{}", self.sequence - 1)),
            depth,
        })
        .map_err(|_| Error::Store)?;
        let _: MemoryManifest = decode(&raw)?;
        for (reference, raw) in changed.values() {
            put_immutable(
                &mut self.store,
                &self.owner,
                reference,
                "memory.record",
                raw,
            )?;
            self.store
                .pin_artifact(&self.owner, reference, "project-memory:records")?;
        }
        let hash = digest(&raw);
        let artifact = format!("{SERIES}{}:{}", self.sequence, blake3::hash(&raw).to_hex());
        put_immutable(
            &mut self.store,
            &self.owner,
            &artifact,
            "memory.reviewed-book",
            &raw,
        )?;
        let version = self.store.journal().task(&self.owner)?.record.version;
        self.store.journal_mut().append_series_checkpoint(
            (SERIES, self.sequence),
            &self.owner,
            version,
            &artifact,
            json!({"digest":hash,"namespace":ns}),
        )?;
        self.sequence += 1;
        self.depth = depth;
        self.record_refs.extend(
            changed
                .into_iter()
                .map(|(id, (reference, _))| (id, reference)),
        );
        book.mark_persisted();
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
            let time = now()?;
            let scope = observed(&repo, &ns, &project.book, time)?.scope(&ns, time);
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
            let proof = bytes(&json!({
                "schema_version":"melissa.project-memory-review/v1",
                "namespace":ns,"id":id,"action":action,"record_digest":record_digest,
                "review_digest":expected,"supersedes":supersedes,"operator":"cli:operator"
            }))?;
            let reference = format!("project-review:{}", blake3::hash(&proof).to_hex());
            project
                .book
                .review(
                    id,
                    &record_digest,
                    &scope,
                    action == "accept",
                    &reference,
                    &supersedes,
                )
                .map_err(|_| Error::Denied("Bellek kaynak veya inceleme sınırı."))?;
            put_immutable(
                &mut project.store,
                &project.owner,
                &reference,
                "memory.review",
                &proof,
            )?;
            project
                .store
                .pin_artifact(&project.owner, &reference, "project-memory:reviews")?;
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
        // The listing exposes source identities and digests only.
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

    #[test]
    fn legacy_memory_migrates_to_bounded_deltas_and_only_reads_referenced_files() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("source");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("main.py"), "value = 1\n").unwrap();
        for n in 0..256 {
            std::fs::write(
                root.join(format!("unrelated{n}.txt")),
                vec![b'x'; 64 * 1024],
            )
            .unwrap();
        }
        let repo = Repository::open(&root, Limits::default(), vec![]).unwrap();
        let home = temp.path().join("home");
        let key = StoreKey::from_bytes([214; 32]);
        let ns = namespace(&repo);
        let mut project = ProjectMemory::open(&home, key.clone(), &repo).unwrap();
        let time = now().unwrap();
        let current = ObservedSources::observe(
            &repo,
            &ns,
            ["main.py"],
            time,
            time + 60_000,
            &AtomicBool::new(false),
        )
        .unwrap();
        let selected = current.select(["main.py"]).unwrap();
        project
            .book
            .propose_references("legacy", ns.clone(), "legacy", "review me", &selected)
            .unwrap();
        let old = bytes(&project.book).unwrap();
        put_immutable(
            &mut project.store,
            &project.owner,
            "project-book:0:bytes",
            "memory.reviewed-book",
            &old,
        )
        .unwrap();
        let version = project
            .store
            .journal()
            .task(&project.owner)
            .unwrap()
            .record
            .version;
        project
            .store
            .journal_mut()
            .append_checkpoint(
                "project-book:0",
                &project.owner,
                version,
                "project-book:0:bytes",
                json!({"digest":digest(&old),"namespace":ns}),
            )
            .unwrap();
        drop(project);
        let mut project = ProjectMemory::open(&home, key.clone(), &repo).unwrap();
        assert_eq!(project.book.records()["legacy"].claim, "review me");
        let report = observed(&repo, &ns, &project.book, time).unwrap();
        assert_eq!((report.files_read, report.bytes_read), (1, 10));
        eprintln!(
            "PROJECT_MEMORY_SOURCE_FILES repo_files=257 referenced_files={} bytes_read={}",
            report.files_read, report.bytes_read
        );
        for n in 0..34 {
            project
                .book
                .propose_references(
                    &format!("fact:{n}"),
                    ns.clone(),
                    "new",
                    "proposal",
                    &selected,
                )
                .unwrap();
            project.save(&ns).unwrap();
        }
        assert_eq!(project.depth, 1);
        let (_, cp) = project
            .store
            .journal()
            .latest_checkpoint_in_series(&project.owner, SERIES)
            .unwrap();
        let manifest: MemoryManifest =
            decode(&checked_checkpoint(&project.store, &project.owner, &ns, &cp.unwrap()).unwrap())
                .unwrap();
        assert_eq!(manifest.records.len(), 1);
        assert!(manifest.records.contains_key("fact:33"));
        drop(project);
        let hidden = Repository::open(&root, Limits::default(), vec!["main.py".into()]).unwrap();
        let hidden_book = ProjectMemory::open(&home, key.clone(), &hidden).unwrap();
        assert_eq!(
            hidden_book.book.records()["legacy"].status,
            melissa_skill_context::state::MemoryStatus::Proposed
        );
        drop(hidden_book);
        std::fs::rename(root.join("main.py"), root.join("old.py")).unwrap();
        std::fs::write(root.join("main.py"), "value = 1\n").unwrap();
        let reopened = ProjectMemory::open(&home, key, &repo).unwrap();
        assert_eq!(reopened.book.records().len(), 35);
        assert!(
            reopened
                .book
                .records()
                .values()
                .all(|record| record.status == melissa_skill_context::state::MemoryStatus::Stale)
        );
    }
}
