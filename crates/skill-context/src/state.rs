//! Authenticated immutable checkpoints, project memory proposals and cache keys.
//! All methods are trusted-host APIs. Stored references cannot renew authority.
use crate::{
    Namespace, Result,
    context::Pack,
    digest, json,
    package::{Registry, SkillPolicy},
    require,
    retrieval::{SearchScope, Source},
};
use melissa_core_contracts::design::SkillPin;
use melissa_local_store::cas::Cas;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub fn save<T: Serialize>(cas: &Cas, record: &T) -> Result<String> {
    let bytes = crate::encode_state(record)?;
    let blob = cas.put(&bytes)?;
    Ok(format!("blake3:{}", blob.digest))
}
pub fn restore<T: serde::de::DeserializeOwned>(cas: &Cas, reference: &str) -> Result<T> {
    let raw = reference
        .strip_prefix("blake3:")
        .ok_or_else(|| crate::Error("invalid checkpoint digest".into()))?;
    json(&cas.get(raw)?)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Checkpoint {
    pub schema_version: String,
    pub pack: Pack,
    pub authorization_ref: String,
    pub cancellation_ref: String,
    pub remaining_cost_micro_usd: u64,
    pub consumed_model_requests: u64,
    pub open_effect_refs: Vec<String>,
    pub rejected_hypotheses: Vec<String>,
    pub open_issue_refs: Vec<String>,
    pub evidence_refs: Vec<String>,
}
/// Independently refreshed from the host task, policy and durable budget ledger.
pub struct ResumeState {
    pub namespace: Namespace,
    pub task_id: String,
    pub task_contract_digest: String,
    pub workspace_digest: String,
    pub authorization_ref: String,
    pub cancellation_ref: String,
    pub cancelled: bool,
    pub authorization_active: bool,
    pub remaining_cost_micro_usd: u64,
    pub consumed_model_requests: u64,
    pub skills: Vec<SkillPin>,
}
impl Checkpoint {
    /// Returns a candidate for the existing gateway/broker, never starts work
    /// or replays tool calls. Native provider continuation remains CODE-04's job.
    pub fn validate_resume(
        &self,
        current: &ResumeState,
        registry: &Registry,
        policy: &SkillPolicy,
    ) -> Result<()> {
        require(
            self.schema_version == "melissa.context-checkpoint/v1"
                && self.pack.schema_version == "melissa.context-pack/v1"
                && !current.cancelled
                && current.authorization_active
                && self.open_effect_refs.is_empty(),
            "checkpoint cancelled, unauthorized or needs reconciliation",
        )?;
        require(
            current.namespace == self.pack.namespace
                && current.task_id == self.pack.task_id
                && current.task_contract_digest == self.pack.task_contract_digest
                && current.workspace_digest == self.pack.workspace_digest
                && current.authorization_ref == self.authorization_ref
                && !self.authorization_ref.trim().is_empty()
                && current.cancellation_ref == self.cancellation_ref
                && !self.cancellation_ref.trim().is_empty(),
            "checkpoint scope, source or authority changed; re-observe",
        )?;
        require(
            self.pack.policy_epoch == policy.epoch
                && current.remaining_cost_micro_usd <= self.remaining_cost_micro_usd
                && current.consumed_model_requests >= self.consumed_model_requests
                && current.skills == self.pack.skills,
            "checkpoint policy/skill changed or budget reset",
        )?;
        for pin in &current.skills {
            registry.load(pin, policy)?;
        }
        for i in &self.pack.items {
            require(
                digest(&i.payload) == i.payload_digest,
                "checkpoint payload changed",
            )?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryStatus {
    Proposed,
    Accepted,
    Rejected,
    Stale,
    Deleted,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Memory {
    pub id: String,
    pub namespace: Namespace,
    pub key: String,
    pub claim: String,
    #[serde(deserialize_with = "read_memory_sources")]
    pub sources: Vec<crate::retrieval::SourceRef>,
    pub status: MemoryStatus,
    pub review_evidence_ref: Option<String>,
}
fn memory_review_digest(record: &Memory) -> Result<String> {
    // Re-observation renews freshness without changing the proposed fact.
    // Identity, version, content digest, status and claim remain hash-bound;
    // review separately requires every source to be current and unexpired.
    let mut stable = record.clone();
    for source in &mut stable.sources {
        source.observed_at_ms = 0;
        source.valid_until_ms = 0;
    }
    Ok(digest(&serde_json::to_vec(&stable)?))
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryBook {
    #[serde(deserialize_with = "read_memory_records")]
    records: BTreeMap<String, Memory>,
}
fn read_memory_sources<'de, D: serde::Deserializer<'de>>(
    de: D,
) -> std::result::Result<Vec<crate::retrieval::SourceRef>, D::Error> {
    use serde::de::Error as _;
    struct Sources;
    impl<'de> serde::de::Visitor<'de> for Sources {
        type Value = Vec<crate::retrieval::SourceRef>;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("bounded unique memory sources")
        }
        fn visit_seq<A: serde::de::SeqAccess<'de>>(
            self,
            mut seq: A,
        ) -> std::result::Result<Self::Value, A::Error> {
            let mut result = Vec::new();
            let mut ids = BTreeSet::new();
            let mut legacy_bytes = 0usize;
            while let Some(value) = seq.next_element::<serde_json::Value>()? {
                if result.len() >= MAX_MEMORY_SOURCES {
                    return Err(A::Error::custom("too many memory sources"));
                }
                // Verify legacy embedded content once, then keep references only.
                let reference: crate::retrieval::SourceRef = if value.get("content").is_some() {
                    let old: Source = serde_json::from_value(value).map_err(A::Error::custom)?;
                    legacy_bytes = legacy_bytes
                        .checked_add(old.content.len())
                        .ok_or_else(|| A::Error::custom("memory source bytes overflow"))?;
                    if legacy_bytes > MAX_MEMORY_SOURCE_BYTES {
                        return Err(A::Error::custom("memory source bytes exceed envelope"));
                    }
                    old.validate().map_err(A::Error::custom)?;
                    old.reference()
                } else {
                    serde_json::from_value(value).map_err(A::Error::custom)?
                };
                if !ids.insert(reference.id.clone()) {
                    return Err(A::Error::custom("duplicate memory source"));
                }
                result.push(reference);
            }
            Ok(result)
        }
    }
    de.deserialize_seq(Sources)
}
/// Host allocation bounds, checked on borrowed sources before cloning content.
pub const MAX_MEMORY_SOURCES: usize = 128;
pub const MAX_MEMORY_SOURCE_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_MEMORY_RECORDS: usize = 4096;
pub const MAX_MEMORY_RECORD_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_MEMORY_REVIEW_REF_BYTES: usize = 4096;

#[derive(Serialize)]
struct MemoryView<'a, S: Serialize> {
    id: &'a str,
    namespace: &'a Namespace,
    key: &'a str,
    claim: &'a str,
    sources: S,
    status: MemoryStatus,
    review_evidence_ref: Option<&'a str>,
}
impl Memory {
    fn view(&self) -> MemoryView<'_, &[crate::retrieval::SourceRef]> {
        MemoryView {
            id: &self.id,
            namespace: &self.namespace,
            key: &self.key,
            claim: &self.claim,
            sources: &self.sources,
            status: self.status,
            review_evidence_ref: self.review_evidence_ref.as_deref(),
        }
    }
}
struct SourceReferences<'a>(&'a [&'a Source]);
impl Serialize for SourceReferences<'_> {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        use serde::ser::SerializeSeq;
        let mut seq = serializer.serialize_seq(Some(self.0.len()))?;
        for source in self.0 {
            seq.serialize_element(&source.reference_view())?;
        }
        seq.end()
    }
}
fn read_memory_records<'de, D: serde::Deserializer<'de>>(
    de: D,
) -> std::result::Result<BTreeMap<String, Memory>, D::Error> {
    use serde::de::Error as _;
    struct Records;
    impl<'de> serde::de::Visitor<'de> for Records {
        type Value = BTreeMap<String, Memory>;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("bounded unique memory records")
        }
        fn visit_map<A: serde::de::MapAccess<'de>>(
            self,
            mut map: A,
        ) -> std::result::Result<Self::Value, A::Error> {
            let mut records = BTreeMap::new();
            let mut bytes = b"{\"records\":{}}".len();
            while let Some(id) = map.next_key::<String>()? {
                if records.len() >= MAX_MEMORY_RECORDS || records.contains_key(&id) {
                    return Err(A::Error::custom("duplicate or too many memory records"));
                }
                let record: Memory = map.next_value()?;
                if record.id != id || id.is_empty() || id.len() > 512 {
                    return Err(A::Error::custom("invalid memory identity"));
                }
                let size = crate::encoded_len(&record, MAX_MEMORY_RECORD_BYTES)
                    .map_err(A::Error::custom)?;
                bytes += size
                    + crate::encoded_len(&id, 4096).map_err(A::Error::custom)?
                    + 1
                    + usize::from(!records.is_empty());
                if bytes > crate::MAX_STATE_BYTES {
                    return Err(A::Error::custom("memory book exceeds envelope"));
                }
                records.insert(id, record);
            }
            Ok(records)
        }
    }
    de.deserialize_map(Records)
}
pub fn validate_memory_sources<'a>(sources: impl IntoIterator<Item = &'a Source>) -> Result<()> {
    let mut ids = BTreeSet::new();
    let mut total = 0usize;
    for source in sources {
        require(ids.len() < MAX_MEMORY_SOURCES, "too many memory sources")?;
        require(ids.insert(&source.id), "duplicate memory source")?;
        total = total
            .checked_add(source.content.len())
            .ok_or_else(|| crate::Error("memory source size overflow".into()))?;
        require(
            total <= MAX_MEMORY_SOURCE_BYTES,
            "memory source bytes exceed host envelope",
        )?;
    }
    require(!ids.is_empty(), "memory sources required")
}
impl MemoryBook {
    pub fn records(&self) -> &BTreeMap<String, Memory> {
        &self.records
    }
    /// Size the resulting book using borrowed views, before any mutation or
    /// source-reference cloning. JSON escaping counts toward the same bound
    /// used by durable save/restore.
    fn preflight<T: Serialize>(
        &self,
        id: &str,
        replacement: &T,
        supersedes: &BTreeSet<String>,
    ) -> Result<()> {
        let adding = !self.records.contains_key(id);
        require(
            self.records.len() + usize::from(adding) <= MAX_MEMORY_RECORDS,
            "too many memory records",
        )?;
        let replacement_bytes = crate::encoded_len(replacement, MAX_MEMORY_RECORD_BYTES)?;
        let mut total = b"{\"records\":{}}".len();
        let mut count = 0usize;
        let mut add = |key: &str, size: usize| -> Result<()> {
            total += crate::encoded_len(&key, 4096)? + 1 + size + usize::from(count != 0);
            count += 1;
            require(
                total <= crate::MAX_STATE_BYTES,
                "memory book exceeds host envelope",
            )
        };
        for (key, record) in &self.records {
            let size = if key == id {
                replacement_bytes
            } else {
                let mut view = record.view();
                if supersedes.contains(key) {
                    view.status = MemoryStatus::Stale;
                }
                crate::encoded_len(&view, MAX_MEMORY_RECORD_BYTES)?
            };
            add(key, size)?;
        }
        if adding {
            add(id, replacement_bytes)?;
        }
        Ok(())
    }
    /// Re-observe authenticated current source bytes at a later task/session.
    /// Expired observation time alone does not erase a fact; changed source does.
    /// This never promotes proposals, rejected records or tombstones.
    pub fn revalidate(&mut self, namespace: &Namespace, current: &[Source]) {
        self.revalidate_scoped(namespace, current, &[]);
    }
    /// Missing sources outside this task's read scope are withheld, not
    /// globally declared stale. Retrieval still checks the current scope.
    pub fn revalidate_scoped(
        &mut self,
        namespace: &Namespace,
        current: &[Source],
        excluded: &[String],
    ) {
        let visibility = melissa_repository::SourceVisibilityPolicy::new(excluded.to_vec());
        for record in self
            .records
            .values_mut()
            .filter(|r| &r.namespace == namespace)
        {
            if !matches!(
                record.status,
                MemoryStatus::Accepted | MemoryStatus::Proposed
            ) {
                continue;
            }
            if record
                .sources
                .iter()
                .any(|s| !visibility.as_ref().is_ok_and(|p| !p.excludes(&s.id)))
            {
                continue;
            }
            let refreshed: Option<Vec<_>> = record
                .sources
                .iter()
                .map(|old| {
                    current
                        .iter()
                        .find(|s| {
                            &s.namespace == namespace
                                && s.id == old.id
                                && s.version == old.version
                                && s.digest == old.digest
                        })
                        .map(Source::reference)
                })
                .collect();
            if let Some(sources) = refreshed {
                record.sources = sources;
            } else {
                record.status = MemoryStatus::Stale;
            }
        }
    }
    pub fn propose(
        &mut self,
        id: &str,
        namespace: Namespace,
        key: &str,
        claim: &str,
        sources: Vec<Source>,
    ) -> Result<String> {
        self.propose_borrowed(
            id,
            namespace,
            key,
            claim,
            &sources.iter().collect::<Vec<_>>(),
        )
    }
    pub fn propose_borrowed(
        &mut self,
        id: &str,
        namespace: Namespace,
        key: &str,
        claim: &str,
        sources: &[&Source],
    ) -> Result<String> {
        namespace.validate()?;
        validate_memory_sources(sources.iter().copied())?;
        require(
            !id.trim().is_empty()
                && id.len() <= 512
                && !key.trim().is_empty()
                && key.len() <= 1024
                && !claim.trim().is_empty()
                && claim.len() <= 65536
                && !self.records.contains_key(id)
                && !sources.is_empty(),
            "invalid or duplicate memory proposal",
        )?;
        for s in sources {
            s.validate()?;
            require(s.namespace == namespace, "memory source namespace mismatch")?;
        }
        let view = MemoryView {
            id,
            namespace: &namespace,
            key,
            claim,
            sources: SourceReferences(sources),
            status: MemoryStatus::Proposed,
            review_evidence_ref: None,
        };
        self.preflight(id, &view, &BTreeSet::new())?;
        let record = Memory {
            id: id.into(),
            namespace,
            key: key.into(),
            claim: claim.into(),
            sources: sources.iter().map(|s| s.reference()).collect(),
            status: MemoryStatus::Proposed,
            review_evidence_ref: None,
        };
        let hash = memory_review_digest(&record)?;
        self.records.insert(id.into(), record);
        Ok(hash)
    }
    /// Host review decision only; semantic truth is the reviewer's responsibility.
    /// A model proposal cannot invoke this transition through a model tool.
    pub fn review(
        &mut self,
        id: &str,
        expected: &str,
        scope: &SearchScope,
        accept: bool,
        evidence_ref: &str,
        supersedes: &BTreeSet<String>,
    ) -> Result<()> {
        let record = self
            .records
            .get(id)
            .ok_or_else(|| crate::Error("memory not found".into()))?;
        require(
            record.status == MemoryStatus::Proposed
                && (memory_review_digest(record)? == expected
                    || digest(&serde_json::to_vec(record)?) == expected)
                && record.namespace == scope.namespace
                && !evidence_ref.trim().is_empty()
                && evidence_ref.len() <= MAX_MEMORY_REVIEW_REF_BYTES
                && record.sources.iter().all(|s| scope.allows_ref(s)),
            "stale/unreviewable memory proposal",
        )?;
        let conflicts: BTreeSet<_> = self
            .records
            .values()
            .filter(|r| {
                r.id != id
                    && r.namespace == record.namespace
                    && r.key == record.key
                    && r.status == MemoryStatus::Accepted
            })
            .map(|r| r.id.clone())
            .collect();
        require(
            !accept || &conflicts == supersedes,
            "memory conflict requires explicit full supersession",
        )?;
        let mut reviewed = record.view();
        reviewed.status = if accept {
            MemoryStatus::Accepted
        } else {
            MemoryStatus::Rejected
        };
        reviewed.review_evidence_ref = Some(evidence_ref);
        let none = BTreeSet::new();
        self.preflight(id, &reviewed, if accept { supersedes } else { &none })?;
        if accept {
            for prior in supersedes {
                self.records
                    .get_mut(prior)
                    .expect("conflict set checked")
                    .status = MemoryStatus::Stale;
            }
        }
        let r = self.records.get_mut(id).expect("checked");
        r.status = if accept {
            MemoryStatus::Accepted
        } else {
            MemoryStatus::Rejected
        };
        r.review_evidence_ref = Some(evidence_ref.into());
        Ok(())
    }
    pub fn search(&self, scope: &SearchScope, query: &str) -> Vec<&Memory> {
        self.records
            .values()
            .filter(|r| {
                r.namespace == scope.namespace
                    && r.status == MemoryStatus::Accepted
                    && r.sources.iter().all(|s| scope.allows_ref(s))
                    && (r.key.contains(query) || r.claim.contains(query))
            })
            .collect()
    }
    pub fn delete(&mut self, scope: &Namespace, id: &str) -> Result<()> {
        let r = self
            .records
            .get_mut(id)
            .ok_or_else(|| crate::Error("memory not found".into()))?;
        require(&r.namespace == scope, "memory deletion scope mismatch")?;
        r.status = MemoryStatus::Deleted;
        r.claim.clear();
        r.sources.clear();
        r.review_evidence_ref = None;
        Ok(())
    }
    pub fn invalidate_source(&mut self, scope: &Namespace, source_id: &str) {
        for r in self
            .records
            .values_mut()
            .filter(|r| &r.namespace == scope && r.sources.iter().any(|s| s.id == source_id))
        {
            if matches!(r.status, MemoryStatus::Accepted | MemoryStatus::Proposed) {
                r.status = MemoryStatus::Stale;
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheKind {
    Prompt,
    Retrieval,
    Parse,
    Build,
    ToolResult,
    Artifact,
    SkillEvaluation,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CacheKey {
    pub kind: CacheKind,
    pub namespace: Namespace,
    pub input_digest: String,
    pub source_digest: String,
    pub skill_digest: String,
    pub provider_model: String,
    pub prompt_digest: String,
    pub tool_version: String,
    pub verifier_version: String,
    pub environment_digest: String,
    pub policy_epoch: String,
}
impl CacheKey {
    fn validate(&self) -> Result<()> {
        self.namespace.validate()?;
        require(
            [
                &self.input_digest,
                &self.source_digest,
                &self.skill_digest,
                &self.provider_model,
                &self.prompt_digest,
                &self.tool_version,
                &self.verifier_version,
                &self.environment_digest,
                &self.policy_epoch,
            ]
            .iter()
            .all(|s| !s.trim().is_empty()),
            "cache dependency missing",
        )
    }
    pub fn digest(&self) -> Result<String> {
        self.validate()?;
        Ok(digest(&serde_json::to_vec(self)?))
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CacheEntry {
    key: CacheKey,
    created_ms: u64,
    expires_ms: u64,
    bytes: Vec<u8>,
    digest: String,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Cache {
    entries: BTreeMap<String, CacheEntry>,
}
/// Fresh host decision, deliberately not serialized with the cached data.
pub struct CacheAdmission {
    pub authorized_now: bool,
    pub deterministic_readonly: bool,
    pub cancelled: bool,
}
impl Cache {
    pub fn put(
        &mut self,
        key: CacheKey,
        bytes: Vec<u8>,
        now: u64,
        expires: u64,
        admission: &CacheAdmission,
    ) -> Result<()> {
        require(
            admission.authorized_now
                && admission.deterministic_readonly
                && !admission.cancelled
                && expires > now,
            "unsafe cache insertion",
        )?;
        // Derived cache is disposable. Bound its serialized footprint so a
        // large result cannot make a previously readable checkpoint unwritable.
        const MAX_CACHE_BYTES: usize = 4 * 1024 * 1024;
        const MAX_CACHE_ENTRIES: usize = 256;
        let identity = key.digest()?;
        self.entries
            .retain(|id, e| id != &identity && e.expires_ms > now);
        let cost = serde_json::to_vec(&key)?
            .len()
            .saturating_add(bytes.len().saturating_mul(4))
            .saturating_add(512);
        if cost > MAX_CACHE_BYTES {
            return Ok(());
        }
        while self.entries.len() >= MAX_CACHE_ENTRIES
            || self
                .entries
                .values()
                .map(|e| {
                    e.bytes
                        .len()
                        .saturating_mul(4)
                        .saturating_add(serde_json::to_vec(&e.key).map_or(0, |b| b.len()))
                        .saturating_add(512)
                })
                .sum::<usize>()
                .saturating_add(cost)
                > MAX_CACHE_BYTES
        {
            let Some(oldest) = self
                .entries
                .iter()
                .min_by_key(|(_, e)| e.created_ms)
                .map(|(id, _)| id.clone())
            else {
                break;
            };
            self.entries.remove(&oldest);
        }
        let d = digest(&bytes);
        self.entries.insert(
            identity,
            CacheEntry {
                key,
                created_ms: now,
                expires_ms: expires,
                bytes,
                digest: d,
            },
        );
        Ok(())
    }
    pub fn get(
        &self,
        key: &CacheKey,
        now: u64,
        admission: &CacheAdmission,
    ) -> Result<Option<&[u8]>> {
        require(
            admission.authorized_now && admission.deterministic_readonly && !admission.cancelled,
            "cache access denied",
        )?;
        let Some(e) = self.entries.get(&key.digest()?) else {
            return Ok(None);
        };
        require(
            e.key == *key && digest(&e.bytes) == e.digest,
            "cache integrity mismatch",
        )?;
        Ok((e.created_ms <= now && now < e.expires_ms).then_some(e.bytes.as_slice()))
    }
    pub fn invalidate_source(&mut self, scope: &Namespace, digest: &str) {
        self.entries
            .retain(|_, e| &e.key.namespace != scope || e.key.source_digest != digest);
    }
}
