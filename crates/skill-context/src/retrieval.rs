//! Ephemeral SQLite FTS5 index. Persistent source bytes live only in encrypted
//! host storage; the index can always be rebuilt from authenticated sources.
use crate::{Error, Namespace, Result, digest, require};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Domain {
    Documents,
    Code,
    Science,
    Cyber,
    Game,
    Skills,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    pub id: String,
    pub version: String,
    pub namespace: Namespace,
    pub domain: Domain,
    pub content: String,
    pub digest: String,
    pub observed_at_ms: u64,
    pub valid_until_ms: u64,
    pub symbols: Vec<String>,
    pub links: Vec<String>,
}
impl Source {
    pub fn reference(&self) -> SourceRef {
        SourceRef {
            id: self.id.clone(),
            version: self.version.clone(),
            namespace: self.namespace.clone(),
            domain: self.domain,
            digest: self.digest.clone(),
            observed_at_ms: self.observed_at_ms,
            valid_until_ms: self.valid_until_ms,
            symbols: vec![],
            links: vec![],
        }
    }
    pub fn validate(&self) -> Result<()> {
        self.namespace.validate()?;
        require(
            !self.id.trim().is_empty()
                && self.id.len() <= 4096
                && !self.version.trim().is_empty()
                && self.version.len() <= 4096
                && digest(self.content.as_bytes()) == self.digest
                && self.observed_at_ms < self.valid_until_ms,
            "invalid source identity, bytes or freshness",
        )
    }
}
/// Content-addressed provenance; full source bytes stay in repository/artifacts.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceRef {
    pub id: String,
    pub version: String,
    pub namespace: Namespace,
    pub domain: Domain,
    pub digest: String,
    pub observed_at_ms: u64,
    pub valid_until_ms: u64,
    pub symbols: Vec<String>,
    pub links: Vec<String>,
}
/// A current-source inventory supplied by the host after source revalidation.
/// Neither a search result nor model memory can populate this automatically.
#[derive(Debug, Clone)]
pub struct SearchScope {
    pub namespace: Namespace,
    pub domain: Domain,
    pub now_ms: u64,
    pub current: BTreeMap<String, (String, String)>,
}
impl SearchScope {
    pub fn allows(&self, s: &Source) -> bool {
        s.namespace == self.namespace
            && s.domain == self.domain
            && s.observed_at_ms <= self.now_ms
            && self.now_ms < s.valid_until_ms
            && self
                .current
                .get(&s.id)
                .is_some_and(|(v, d)| v == &s.version && d == &s.digest)
    }
    pub fn allows_ref(&self, s: &SourceRef) -> bool {
        s.namespace == self.namespace
            && s.domain == self.domain
            && s.observed_at_ms <= self.now_ms
            && self.now_ms < s.valid_until_ms
            && self
                .current
                .get(&s.id)
                .is_some_and(|(v, d)| v == &s.version && d == &s.digest)
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hit {
    pub source: SourceRef,
    pub excerpt: String,
    pub excerpt_digest: String,
    pub start_byte: usize,
    pub end_byte: usize,
    pub start_line: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_line: Option<usize>,
    pub lexical_rank: Option<usize>,
    pub graph_match: bool,
    pub authority: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub signals: Vec<String>,
}

pub(crate) fn query_terms(query: &str) -> Vec<&str> {
    query
        .split(|c: char| !c.is_alphanumeric())
        .filter(|s| !s.is_empty())
        .collect()
}

pub(crate) fn ranked_rows(db: &Connection, terms: &[&str]) -> Result<Vec<i64>> {
    if terms.is_empty() {
        return Ok(vec![]);
    }
    // Literal terms never become column names, operators or SQL.
    let escaped = terms
        .iter()
        .map(|t| format!("\"{t}\""))
        .collect::<Vec<_>>()
        .join(" OR ");
    let mut stmt = db
        .prepare("SELECT rowid FROM corpus WHERE corpus MATCH ?1 ORDER BY bm25(corpus),rowid")
        .map_err(sql)?;
    stmt.query_map([escaped], |row| row.get::<_, i64>(0))
        .map_err(sql)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(sql)
}

pub(crate) fn make_hit(
    source: SourceRef,
    content: &str,
    terms: &[&str],
    limit: usize,
    lexical_rank: Option<usize>,
    graph_match: bool,
) -> Hit {
    let excerpt_bytes = (128 * 1024 / limit).min(4096);
    let at = terms
        .iter()
        .filter_map(|term| content.find(term))
        .min()
        .unwrap_or(0);
    let mut start = at.saturating_sub(256);
    while !content.is_char_boundary(start) {
        start += 1;
    }
    let mut end = (start + excerpt_bytes).min(content.len());
    while !content.is_char_boundary(end) {
        end -= 1;
    }
    let excerpt = content[start..end].to_owned();
    Hit {
        source,
        excerpt_digest: digest(excerpt.as_bytes()),
        excerpt,
        start_byte: start,
        end_byte: end,
        start_line: content.as_bytes()[..start]
            .iter()
            .filter(|&&b| b == b'\n')
            .count()
            + 1,
        end_line: None,
        lexical_rank,
        graph_match,
        authority: "untrusted_data".into(),
        signals: vec![],
    }
}

pub fn search(
    sources: &[Source],
    scope: &SearchScope,
    query: &str,
    symbols: &[String],
    limit: usize,
) -> Result<Vec<Hit>> {
    SearchIndex::new()?.search(sources, scope, query, symbols, limit)
}

/// Session-local derived FTS state. No plaintext index is persisted to disk.
pub struct SearchIndex {
    db: Connection,
    indexed: BTreeMap<String, (String, String, i64)>,
    next_row: i64,
    pub updated_documents: u64,
}
impl SearchIndex {
    pub fn new() -> Result<Self> {
        let db = Connection::open_in_memory().map_err(sql)?;
        db.execute_batch("CREATE VIRTUAL TABLE corpus USING fts5(body, tokenize='unicode61');")
            .map_err(sql)?;
        Ok(Self {
            db,
            indexed: BTreeMap::new(),
            next_row: 1,
            updated_documents: 0,
        })
    }
    pub fn search(
        &mut self,
        sources: &[Source],
        scope: &SearchScope,
        query: &str,
        symbols: &[String],
        limit: usize,
    ) -> Result<Vec<Hit>> {
        scope.namespace.validate()?;
        require(
            (1..=256).contains(&limit) && query.len() <= 65536,
            "search page or query limit",
        )?;
        let mut ids = BTreeSet::new();
        let eligible: Vec<_> = sources.iter().filter(|s| scope.allows(s)).collect();
        for s in &eligible {
            s.validate()?;
            require(
                ids.insert(s.id.clone()),
                "duplicate current source identity",
            )?;
        }
        let tx = self.db.transaction().map_err(sql)?;
        for (id, (_, _, row)) in &self.indexed {
            if !ids.contains(id) {
                tx.execute("DELETE FROM corpus WHERE rowid=?1", [row])
                    .map_err(sql)?;
            }
        }
        let mut next = self.indexed.clone();
        next.retain(|id, _| ids.contains(id));
        let mut row_to_index = BTreeMap::new();
        let mut updates = 0;
        for (i, s) in eligible.iter().enumerate() {
            let previous = next.get(&s.id);
            let row = previous.map(|(_, _, r)| *r).unwrap_or_else(|| {
                let r = self.next_row;
                self.next_row += 1;
                r
            });
            if previous.is_none_or(|(v, d, _)| v != &s.version || d != &s.digest) {
                tx.execute("DELETE FROM corpus WHERE rowid=?1", [row])
                    .map_err(sql)?;
                tx.execute(
                    "INSERT INTO corpus(rowid,body) VALUES(?1,?2)",
                    params![row, s.content],
                )
                .map_err(sql)?;
                next.insert(s.id.clone(), (s.version.clone(), s.digest.clone(), row));
                updates += 1;
            }
            row_to_index.insert(row, i);
        }
        tx.commit().map_err(sql)?;
        self.indexed = next;
        self.updated_documents += updates;
        let terms = query_terms(query);
        let mut lexical = BTreeMap::new();
        for (rank, row) in ranked_rows(&self.db, &terms)?.into_iter().enumerate() {
            if let Some(i) = row_to_index.get(&row) {
                lexical.insert(*i, rank);
            }
        }
        let seed_ids: BTreeSet<_> = eligible
            .iter()
            .filter(|s| s.symbols.iter().any(|x| symbols.contains(x)))
            .map(|s| s.id.as_str())
            .collect();
        let mut hits = Vec::new();
        for (i, s) in eligible.iter().enumerate() {
            let graph = seed_ids.contains(s.id.as_str())
                || s.links.iter().any(|id| seed_ids.contains(id.as_str()));
            if lexical.contains_key(&i) || graph {
                hits.push((i, graph, lexical.get(&i).copied()));
            }
        }
        hits.sort_by(|a, b| {
            b.1.cmp(&a.1)
                .then(a.2.unwrap_or(usize::MAX).cmp(&b.2.unwrap_or(usize::MAX)))
                .then(eligible[a.0].id.cmp(&eligible[b.0].id))
        });
        let mut seen = BTreeSet::new();
        hits.retain(|h| seen.insert(&eligible[h.0].digest));
        // Rank and deduplicate references before allocating bounded excerpts.
        let bounded = hits
            .into_iter()
            .take(limit)
            .map(|(i, graph_match, lexical_rank)| {
                let s = eligible[i];
                make_hit(
                    s.reference(),
                    &s.content,
                    &terms,
                    limit,
                    lexical_rank,
                    graph_match,
                )
            });
        let mut output = Vec::new();
        let mut bytes = 2usize;
        for hit in bounded {
            let size = serde_json::to_vec(&hit)?.len() + 1;
            if bytes.saturating_add(size) > 256 * 1024 {
                break;
            }
            bytes += size;
            output.push(hit);
        }
        Ok(output)
    }
}
pub(crate) fn sql(_: rusqlite::Error) -> Error {
    Error("CODE-07 SQLite FTS operation failed".into())
}
