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
    Superseded,
    Deleted,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryKind {
    ProjectDecision,
    UserPreference,
    TransientState,
    FailureExperience,
    Unclassified,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryMetadata {
    pub kind: MemoryKind,
    pub created_at_ms: u64,
    pub task_id: String,
    pub git: melissa_repository::git_context::GitContext,
    pub expires_at_ms: Option<u64>,
}
impl MemoryMetadata {
    pub fn withheld(
        &self,
        task: &str,
        git: &melissa_repository::git_context::GitContext,
        time: u64,
    ) -> Option<&'static str> {
        if self.expires_at_ms.is_some_and(|until| time >= until) {
            return Some("expired");
        }
        if self.kind == MemoryKind::TransientState && self.task_id != task {
            return Some("different_task");
        }
        if self.kind != MemoryKind::UserPreference
            && (self.git.scope_key().is_none() || self.git.scope_key() != git.scope_key())
        {
            return Some("branch_unknown_or_changed");
        }
        None
    }
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<MemoryMetadata>,
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
    #[serde(skip)]
    dirty: BTreeSet<String>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    metadata: Option<&'a MemoryMetadata>,
}
impl Memory {
    fn operator_reviewed_preference(&self) -> bool {
        self.status == MemoryStatus::Accepted
            && self
                .metadata
                .as_ref()
                .is_some_and(|m| m.kind == MemoryKind::UserPreference)
            && self
                .review_evidence_ref
                .as_ref()
                .is_some_and(|r| r.starts_with("project-review:"))
    }

    fn view(&self) -> MemoryView<'_, &[crate::retrieval::SourceRef]> {
        MemoryView {
            id: &self.id,
            namespace: &self.namespace,
            key: &self.key,
            claim: &self.claim,
            sources: &self.sources,
            status: self.status,
            review_evidence_ref: self.review_evidence_ref.as_deref(),
            metadata: self.metadata.as_ref(),
        }
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
                    view.status = MemoryStatus::Superseded;
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
        let references: Vec<_> = current.iter().map(Source::reference).collect();
        self.revalidate_references(namespace, &references, excluded);
    }

    pub fn dirty_ids(&self) -> &BTreeSet<String> {
        &self.dirty
    }

    pub fn mark_persisted(&mut self) {
        self.dirty.clear();
    }

    pub fn source_ids<'a>(&'a self, namespace: &'a Namespace) -> impl Iterator<Item = &'a str> {
        self.records
            .values()
            .filter(move |record| {
                &record.namespace == namespace
                    && matches!(
                        record.status,
                        MemoryStatus::Accepted | MemoryStatus::Proposed
                    )
            })
            .flat_map(|record| record.sources.iter().map(|source| source.id.as_str()))
    }

    pub fn revalidate_references<'a>(
        &mut self,
        namespace: &Namespace,
        current: impl IntoIterator<Item = &'a crate::retrieval::SourceRef>,
        excluded: &[String],
    ) {
        let visibility = melissa_repository::SourceVisibilityPolicy::new(excluded.to_vec());
        let current: BTreeMap<_, _> = current
            .into_iter()
            .filter(|s| &s.namespace == namespace)
            .map(|s| (s.id.as_str(), s))
            .collect();
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
            if record.operator_reviewed_preference() {
                continue;
            }
            if record.sources.iter().all(|old| {
                current.get(old.id.as_str()).is_some_and(|s| {
                    s.version == old.version && s.digest == old.digest && s.domain == old.domain
                })
            }) {
                for old in &mut record.sources {
                    let observed = current[old.id.as_str()];
                    old.observed_at_ms = observed.observed_at_ms;
                    old.valid_until_ms = observed.valid_until_ms;
                }
            } else {
                record.status = MemoryStatus::Stale;
                self.dirty.insert(record.id.clone());
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
        validate_memory_sources(sources.iter().copied())?;
        let references = sources
            .iter()
            .map(|s| {
                s.validate()?;
                Ok((s.reference(), s.content.len() as u64))
            })
            .collect::<Result<Vec<_>>>()?;
        let selected: Vec<_> = references.iter().map(|(r, n)| (r, *n)).collect();
        self.propose_references(id, namespace, key, claim, &selected)
    }

    pub fn propose_references(
        &mut self,
        id: &str,
        namespace: Namespace,
        key: &str,
        claim: &str,
        sources: &[(&crate::retrieval::SourceRef, u64)],
    ) -> Result<String> {
        namespace.validate()?;
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
        let mut seen = BTreeSet::new();
        let mut source_bytes = 0u64;
        for &(s, bytes) in sources {
            source_bytes = source_bytes.saturating_add(bytes);
            require(
                s.namespace == namespace
                    && !s.id.trim().is_empty()
                    && s.id.len() <= 4096
                    && !s.version.trim().is_empty()
                    && s.version.len() <= 4096
                    && s.digest.strip_prefix("blake3:").is_some_and(|hash| {
                        hash.len() == 64
                            && hash
                                .bytes()
                                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
                    })
                    && s.observed_at_ms < s.valid_until_ms
                    && seen.insert(s.id.as_str())
                    && seen.len() <= MAX_MEMORY_SOURCES
                    && source_bytes <= MAX_MEMORY_SOURCE_BYTES as u64,
                "invalid memory source metadata or size",
            )?;
        }
        let references: Vec<_> = sources.iter().map(|(reference, _)| *reference).collect();
        let view = MemoryView {
            id,
            namespace: &namespace,
            key,
            claim,
            sources: &references,
            status: MemoryStatus::Proposed,
            review_evidence_ref: None,
            metadata: None,
        };
        self.preflight(id, &view, &BTreeSet::new())?;
        let record = Memory {
            id: id.into(),
            namespace,
            key: key.into(),
            claim: claim.into(),
            sources: references.into_iter().cloned().collect(),
            status: MemoryStatus::Proposed,
            review_evidence_ref: None,
            metadata: None,
        };
        let hash = memory_review_digest(&record)?;
        self.records.insert(id.into(), record);
        self.dirty.insert(id.into());
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
                    .status = MemoryStatus::Superseded;
                self.dirty.insert(prior.clone());
            }
        }
        let r = self.records.get_mut(id).expect("checked");
        r.status = if accept {
            MemoryStatus::Accepted
        } else {
            MemoryStatus::Rejected
        };
        r.review_evidence_ref = Some(evidence_ref.into());
        self.dirty.insert(id.into());
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
    /// Host-observed provenance decorates only an unreviewed proposal and is
    /// admitted through the same aggregate byte accounting as all memory.
    pub fn annotate(&mut self, id: &str, metadata: MemoryMetadata) -> Result<String> {
        let record = self
            .records
            .get(id)
            .ok_or_else(|| crate::Error("memory not found".into()))?;
        require(
            record.status == MemoryStatus::Proposed
                && record.metadata.is_none()
                && metadata.created_at_ms > 0
                && !metadata.task_id.is_empty()
                && metadata.task_id.len() <= 256
                && metadata
                    .expires_at_ms
                    .is_none_or(|t| t > metadata.created_at_ms)
                && (metadata.kind != MemoryKind::TransientState
                    || metadata.expires_at_ms.is_some()),
            "invalid memory provenance",
        )?;
        let mut view = record.view();
        view.metadata = Some(&metadata);
        self.preflight(id, &view, &BTreeSet::new())?;
        let record = self.records.get_mut(id).expect("checked");
        record.metadata = Some(metadata);
        let hash = memory_review_digest(record)?;
        self.dirty.insert(id.into());
        Ok(hash)
    }
    /// Operator-reviewed preferences are project-scoped statements, not claims
    /// about the current source bytes. Their evidence remains visible-policy
    /// checked even when those bytes change. All other kinds require freshness.
    pub fn search_context(
        &self,
        scope: &SearchScope,
        query: &str,
        task: &str,
        git: &melissa_repository::git_context::GitContext,
        time: u64,
        visibility: &melissa_repository::SourceVisibilityPolicy,
    ) -> Vec<&Memory> {
        self.records
            .values()
            .filter(|record| {
                record.namespace == scope.namespace
                    && record.status == MemoryStatus::Accepted
                    && (record.key.contains(query) || record.claim.contains(query))
                    && record.sources.iter().all(|source| {
                        source.namespace == scope.namespace
                            && source.domain == scope.domain
                            && visibility.allows(&source.id)
                    })
                    && record
                        .metadata
                        .as_ref()
                        .is_none_or(|m| m.withheld(task, git, time).is_none())
                    && (record.operator_reviewed_preference()
                        || record.sources.iter().all(|source| scope.allows_ref(source)))
            })
            .collect()
    }
    pub fn delete(&mut self, scope: &Namespace, id: &str) -> Result<()> {
        let r = self
            .records
            .get_mut(id)
            .ok_or_else(|| crate::Error("memory not found".into()))?;
        require(&r.namespace == scope, "memory deletion scope mismatch")?;
        if r.status == MemoryStatus::Deleted {
            return Ok(());
        }
        r.status = MemoryStatus::Deleted;
        self.dirty.insert(id.into());
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
                self.dirty.insert(r.id.clone());
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
const CACHE_MAX_ENTRIES: usize = 256;
const CACHE_MAX_TOTAL_BYTES: usize = 4 * 1024 * 1024;
const CACHE_MAX_ENTRY_BYTES: usize = 512 * 1024;
const CACHE_TTL_MS: u64 = 5 * 60 * 1000;

impl CacheEntry {
    fn cost(&self) -> Result<usize> {
        Ok(serde_json::to_vec(&self.key)?
            .len()
            .saturating_add(self.bytes.len().saturating_mul(4))
            .saturating_add(512))
    }
}

impl Cache {
    /// Disposable legacy entries are normalized on restore. All surviving
    /// entries must still authenticate and match their exact namespace/key.
    pub(crate) fn restore(&mut self, namespace: &Namespace, now: u64) -> Result<()> {
        self.entries
            .retain(|_, entry| &entry.key.namespace == namespace);
        self.trim(now)?;
        for (identity, entry) in &self.entries {
            require(
                &entry.key.digest()? == identity && digest(&entry.bytes) == entry.digest,
                "restored cache integrity mismatch",
            )?;
        }
        Ok(())
    }

    fn trim(&mut self, now: u64) -> Result<()> {
        self.entries.retain(|_, entry| {
            entry.expires_ms = entry
                .expires_ms
                .min(entry.created_ms.saturating_add(CACHE_TTL_MS));
            entry.created_ms <= now
                && now < entry.expires_ms
                && entry.bytes.len() <= CACHE_MAX_ENTRY_BYTES
        });
        let mut sizes = self
            .entries
            .iter()
            .map(|(id, e)| Ok((e.created_ms, id.clone(), e.cost()?)))
            .collect::<Result<Vec<_>>>()?;
        sizes.sort_unstable();
        let mut used = sizes.iter().map(|(_, _, bytes)| *bytes).sum::<usize>();
        for (_, id, cost) in sizes {
            if self.entries.len() <= CACHE_MAX_ENTRIES && used <= CACHE_MAX_TOTAL_BYTES {
                break;
            }
            self.entries.remove(&id);
            used -= cost;
        }
        Ok(())
    }

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
        let identity = key.digest()?;
        if bytes.len() <= CACHE_MAX_ENTRY_BYTES {
            self.entries.insert(
                identity,
                CacheEntry {
                    key,
                    created_ms: now,
                    expires_ms: expires,
                    digest: digest(&bytes),
                    bytes,
                },
            );
        }
        self.trim(now)
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
