//! Rebuildable repository retrieval index. SQLite lives in memory; the
//! authenticated local-store slot holds one bounded serialized generation.
use crate::{
    Error, Namespace, Result, require,
    retrieval::{Domain, Hit, SourceRef, query_terms, ranked_rows, sql},
};
use melissa_local_store::{MAX_INDEX_BYTES, RepositoryIndexCache};
use melissa_repository::{
    Repository,
    graph::{FileFacts, Graph},
    polyglot::{MAX_SYNTAX_NODES, parse_index_file},
    snapshot::IndexedFileVersion,
};
use rusqlite::{Connection, MAIN_DB};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::atomic::{AtomicBool, Ordering},
};

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    file: IndexedFileVersion,
    row: Option<i64>,
    facts: Option<FileFacts>,
    syntax_nodes: usize,
}

struct Candidate<'a> {
    entry: &'a Entry,
    lexical_rank: Option<usize>,
    score: usize,
    anchor: Option<usize>,
    line: Option<usize>,
    signals: Vec<&'static str>,
}

pub(crate) struct RepositoryIndex {
    binding: [u8; 32],
    db: Connection,
    files: BTreeMap<String, Entry>,
    dirty: bool,
}

pub(crate) struct Refresh {
    pub snapshot: String,
    pub bytes_read: u64,
    pub indexed: u64,
    pub parsed: u64,
}

pub(crate) enum Persistence {
    Unchanged,
    Saved,
    Capacity,
}

pub(crate) enum Restore {
    Loaded,
    Missing,
    Invalid,
}

pub(crate) struct SearchQuery<'a> {
    pub query: &'a str,
    pub symbols: &'a [String],
    pub links: Option<&'a BTreeMap<String, Vec<String>>>,
    pub limit: usize,
    pub namespace: &'a Namespace,
    pub now_ms: u64,
    pub expires_at_ms: u64,
    pub budget: usize,
    pub hints: &'a RankingHints,
}

#[derive(Default, Serialize)]
pub struct RankingHints {
    pub diff: BTreeMap<String, usize>,
    pub history: BTreeMap<String, usize>,
    pub diagnostics: BTreeMap<String, usize>,
}

impl RepositoryIndex {
    pub(crate) fn binding(
        repo: &Repository,
        namespace: &Namespace,
        policy_epoch: &str,
    ) -> Result<[u8; 32]> {
        let visibility = repo.source_visibility_digest().map_err(repo_error)?;
        let binding = serde_json::to_vec(&(
            "melissa.repository-index/v2",
            repo.root_path(),
            visibility,
            namespace,
            policy_epoch,
            crate::digest(include_bytes!("../../repository/src/polyglot.rs")),
            crate::digest(include_bytes!("../../repository/src/graph.rs")),
            crate::digest(include_bytes!("../../../Cargo.lock")),
        ))?;
        Ok(*blake3::hash(&binding).as_bytes())
    }

    pub(crate) fn new(binding: [u8; 32]) -> Result<Self> {
        let db = Connection::open_in_memory().map_err(sql)?;
        db.execute_batch(
            "PRAGMA temp_store=MEMORY;
             CREATE VIRTUAL TABLE corpus USING fts5(
                 body, tokenize='unicode61', content='', contentless_delete=1
             );",
        )
        .map_err(sql)?;
        Ok(Self {
            binding,
            db,
            files: BTreeMap::new(),
            dirty: false,
        })
    }

    pub(crate) fn open(cache: &RepositoryIndexCache, binding: [u8; 32]) -> Result<(Self, Restore)> {
        let state = match cache.load(&binding) {
            Ok(Some(bytes)) => match Self::restore(binding, &bytes) {
                Ok(index) => return Ok((index, Restore::Loaded)),
                Err(_) => Restore::Invalid,
            },
            Ok(None) => Restore::Missing,
            Err(_) => Restore::Invalid,
        };
        Ok((Self::new(binding)?, state))
    }

    pub(crate) fn restore(binding: [u8; 32], bytes: &[u8]) -> Result<Self> {
        let prefix: [u8; 4] = bytes
            .get(..4)
            .ok_or_else(|| Error("short repository index header".into()))?
            .try_into()
            .expect("exact header length");
        let end = 4usize
            .checked_add(u32::from_le_bytes(prefix) as usize)
            .ok_or_else(|| Error("repository index header overflow".into()))?;
        require(
            end < bytes.len() && bytes.len() <= MAX_INDEX_BYTES,
            "repository index size",
        )?;
        let files = serde_json::from_slice(&bytes[4..end])?;
        let mut db = Connection::open_in_memory().map_err(sql)?;
        db.deserialize_read_exact(MAIN_DB, &bytes[end..], bytes.len() - end, false)
            .map_err(sql)?;
        db.execute_batch("PRAGMA temp_store=MEMORY;").map_err(sql)?;
        db.prepare("SELECT rowid FROM corpus LIMIT 0")
            .map_err(sql)?;
        Ok(Self {
            binding,
            db,
            files,
            dirty: false,
        })
    }

    pub(crate) fn persist(&mut self, cache: &RepositoryIndexCache) -> Result<Persistence> {
        if !self.dirty {
            return Ok(Persistence::Unchanged);
        }
        let meta = serde_json::to_vec(&self.files)?;
        let page_count: usize = self
            .db
            .pragma_query_value(None, "page_count", |r| r.get(0))
            .map_err(sql)?;
        let page_size: usize = self
            .db
            .pragma_query_value(None, "page_size", |r| r.get(0))
            .map_err(sql)?;
        let size = 4usize
            .saturating_add(meta.len())
            .saturating_add(page_count.saturating_mul(page_size));
        if size > MAX_INDEX_BYTES {
            self.dirty = false;
            return Ok(Persistence::Capacity);
        }
        let image = self.db.serialize(MAIN_DB).map_err(sql)?;
        let mut bytes = Vec::with_capacity(size);
        bytes.extend_from_slice(&(meta.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&meta);
        bytes.extend_from_slice(&image);
        Self::restore(self.binding, &bytes)?;
        cache.save(&self.binding, &bytes)?;
        self.dirty = false;
        Ok(Persistence::Saved)
    }

    pub(crate) fn refresh(&mut self, repo: &Repository, cancel: &AtomicBool) -> Result<Refresh> {
        let prior = self
            .files
            .iter()
            .map(|(p, e)| (p.clone(), e.file.clone()))
            .collect();
        let mut scan = repo.scan_index(&prior, cancel).map_err(repo_error)?;
        let mut report = Refresh {
            snapshot: scan.snapshot.digest,
            bytes_read: scan.bytes_read,
            indexed: 0,
            parsed: 0,
        };
        if scan.changed_bytes.is_empty() && scan.files == prior {
            return Ok(report);
        }
        let tx = self.db.transaction().map_err(sql)?;
        for (path, entry) in &self.files {
            if let Some(row) = entry.row
                && (!scan.files.contains_key(path) || scan.changed_bytes.contains_key(path))
            {
                tx.execute("DELETE FROM corpus WHERE rowid=?1", [row])
                    .map_err(sql)?;
            }
        }
        let mut nodes = self
            .files
            .iter()
            .filter(|(path, _)| {
                scan.files.contains_key(*path) && !scan.changed_bytes.contains_key(*path)
            })
            .map(|(_, entry)| entry.syntax_nodes)
            .sum::<usize>();
        require(nodes <= MAX_SYNTAX_NODES, "repository syntax node budget")?;
        let mut updates = BTreeMap::new();
        let mut next_row = self
            .files
            .values()
            .filter_map(|entry| entry.row)
            .max()
            .unwrap_or(0)
            + 1;
        for (path, bytes) in std::mem::take(&mut scan.changed_bytes) {
            require(
                !cancel.load(Ordering::Acquire),
                "repository index cancelled",
            )?;
            let file = scan
                .files
                .get(&path)
                .ok_or_else(|| Error("missing indexed version".into()))?;
            let parsed = parse_index_file(&file.version, &bytes, cancel, MAX_SYNTAX_NODES - nodes)
                .map_err(repo_error)?;
            let mut syntax_nodes = 0;
            let facts = if let Some(parsed) = parsed {
                syntax_nodes = parsed.nodes;
                nodes += syntax_nodes;
                report.parsed += 1;
                parsed.graph
            } else {
                None
            };
            let row = if let Ok(body) = std::str::from_utf8(&bytes) {
                let row = self
                    .files
                    .get(&path)
                    .and_then(|entry| entry.row)
                    .unwrap_or_else(|| {
                        let row = next_row;
                        next_row += 1;
                        row
                    });
                tx.execute(
                    "INSERT INTO corpus(rowid,body) VALUES(?1,?2)",
                    rusqlite::params![row, body],
                )
                .map_err(sql)?;
                report.indexed += 1;
                Some(row)
            } else {
                None
            };
            updates.insert(
                path,
                Entry {
                    file: file.clone(),
                    row,
                    facts,
                    syntax_nodes,
                },
            );
        }
        require(
            !cancel.load(Ordering::Acquire),
            "repository index cancelled",
        )?;
        tx.commit().map_err(sql)?;
        self.files.retain(|path, _| scan.files.contains_key(path));
        for (path, file) in scan.files {
            if let Some(entry) = updates.remove(&path) {
                self.files.insert(path, entry);
            } else if let Some(entry) = self.files.get_mut(&path) {
                entry.file = file;
            }
        }
        self.dirty = true;
        Ok(report)
    }

    pub(crate) fn validate_hit(
        &self,
        repo: &Repository,
        hit: &Hit,
        cancel: &AtomicBool,
    ) -> Result<()> {
        let version = self
            .files
            .get(&hit.source.id)
            .map(|entry| &entry.file.version)
            .ok_or_else(|| Error("cached hit outside current repository index".into()))?;
        require(
            hit.source.digest == version.digest
                && hit.source.version == version_id(version)
                && repo
                    .read_cancellable(&version.path, cancel)
                    .map_err(repo_error)?
                    .0
                    == *version,
            "cached hit no longer matches indexed source",
        )
    }

    pub(crate) fn graph(&self) -> Graph<'_> {
        Graph::new(
            self.files
                .iter()
                .filter_map(|(path, entry)| {
                    entry.facts.as_ref().map(|facts| (path.as_str(), facts))
                })
                .collect(),
        )
    }

    pub(crate) fn versions(&self) -> impl Iterator<Item = &melissa_repository::FileVersion> {
        self.files.values().map(|entry| &entry.file.version)
    }

    fn rank(
        &self,
        request: &SearchQuery<'_>,
        terms: &[&str],
        words: &BTreeSet<String>,
        cancel: &AtomicBool,
    ) -> Result<Vec<Candidate<'_>>> {
        let lexical: BTreeMap<_, _> = ranked_rows(&self.db, terms)?
            .into_iter()
            .enumerate()
            .map(|(rank, row)| (row, rank))
            .collect();
        let graph = self.graph();
        let requested: BTreeSet<_> = request.symbols.iter().map(|s| s.as_str()).collect();
        let mut anchors = BTreeMap::new();
        let mut seeds = BTreeSet::new();
        let mut wanted = BTreeSet::new();
        let path_query = request.query.trim().replace('\\', "/").to_lowercase();
        for (&path, facts) in &graph.files {
            if path_matches(path, &path_query) {
                seeds.insert(path);
            }
            for definition in &facts.definitions {
                let explicit = requested.contains(definition.name.as_str());
                if explicit || words.contains(&definition.name.to_lowercase()) {
                    let score = if explicit { 1200 } else { 700 };
                    let entry = anchors
                        .entry(path)
                        .or_insert((score, definition.name_range.start_byte));
                    if score > entry.0 {
                        *entry = (score, definition.name_range.start_byte);
                    }
                    seeds.insert(path);
                    wanted.insert(definition.name.as_str());
                }
            }
        }
        let neighbors = graph.neighbors(&seeds);
        let mut related = BTreeMap::new();
        for (&path, facts) in &graph.files {
            require(
                !cancel.load(Ordering::Acquire),
                "repository ranking cancelled",
            )?;
            for reference in &facts.references {
                if wanted.contains(reference.name.as_str())
                    && let Some(target) = graph.target(path, reference)
                    && seeds.contains(target.path)
                    && (target.path != path
                        || reference.owner.is_some_and(|owner| owner != target.index))
                {
                    let test = reference
                        .owner
                        .is_some_and(|i| facts.definitions[i].test_candidate);
                    let score = if test {
                        390
                    } else if reference.call {
                        310
                    } else {
                        240
                    };
                    let reason = if test {
                        "related_test"
                    } else if reference.call {
                        "caller"
                    } else {
                        "reference"
                    };
                    let entry =
                        related
                            .entry(path)
                            .or_insert((score, reference.range.start_byte, reason));
                    if score > entry.0 {
                        *entry = (score, reference.range.start_byte, reason);
                    }
                }
            }
        }
        let mut candidates = Vec::new();
        for (path, entry) in &self.files {
            require(
                !cancel.load(Ordering::Acquire),
                "repository ranking cancelled",
            )?;
            let Some(row) = entry.row else { continue };
            let rank = lexical.get(&row).copied();
            let mut signals = Vec::new();
            let mut score = 0;
            let mut anchor = None;
            if let Some(rank) = rank {
                score += 400 * 16 / (16 + rank);
                signals.push("bm25");
            }
            let lower = path.to_lowercase();
            if path_matches(&lower, &path_query) {
                score += 850;
                signals.push("path_exact");
            } else if lower
                .split(|c: char| !c.is_alphanumeric())
                .any(|part| words.contains(part))
            {
                score += 110;
                signals.push("path_terms");
            }
            if let Some(&(strength, at)) = anchors.get(path.as_str()) {
                score += strength;
                signals.push("symbol_definition");
                anchor = Some(at);
            }
            if let Some(&(strength, at, reason)) = related.get(path.as_str()) {
                if anchor.is_some_and(|definition: usize| definition.abs_diff(at) > 512) {
                    candidates.push(Candidate {
                        entry,
                        lexical_rank: rank,
                        score: strength + 100,
                        anchor: Some(at),
                        line: None,
                        signals: vec![reason],
                    });
                }
                score += strength;
                signals.push(reason);
                anchor = anchor.or(Some(at));
            }
            if neighbors.contains(path.as_str())
                || request
                    .links
                    .and_then(|links| links.get(path))
                    .is_some_and(|paths| paths.iter().any(|p| seeds.contains(p.as_str())))
            {
                score += 170;
                signals.push("dependency");
            }
            if score == 0 {
                continue;
            }
            let mut line = None;
            if let Some(&at) = request.hints.diagnostics.get(path) {
                score += 160;
                signals.push("diagnostics");
                line = Some(at);
            }
            if let Some(&at) = request.hints.diff.get(path) {
                score += 120;
                signals.push("git_diff");
                line = line.or(Some(at));
            }
            if request.hints.history.contains_key(path) {
                score += 60;
                signals.push("git_history");
            }
            candidates.push(Candidate {
                entry,
                lexical_rank: rank,
                score,
                anchor,
                line,
                signals,
            });
        }
        candidates.sort_unstable_by(|a, b| {
            b.score
                .cmp(&a.score)
                .then(
                    a.lexical_rank
                        .unwrap_or(usize::MAX)
                        .cmp(&b.lexical_rank.unwrap_or(usize::MAX)),
                )
                .then(a.entry.file.version.path.cmp(&b.entry.file.version.path))
        });
        Ok(candidates)
    }

    pub(crate) fn search(
        &self,
        repo: &Repository,
        request: &SearchQuery<'_>,
        cancel: &AtomicBool,
    ) -> Result<Vec<Hit>> {
        let terms = query_terms(request.query);
        let words: BTreeSet<_> = terms.iter().map(|s| s.to_lowercase()).collect();
        let candidates = self.rank(request, &terms, &words, cancel)?;
        let mut seen = BTreeMap::new();
        let mut output: Vec<Hit> = Vec::new();
        let mut bytes = 2usize;
        for Candidate {
            entry,
            lexical_rank,
            anchor,
            line,
            signals,
            ..
        } in candidates.into_iter().take(request.limit.saturating_mul(4))
        {
            if output.len() == request.limit || bytes.saturating_add(128) > request.budget {
                break;
            }
            require(
                !cancel.load(Ordering::Acquire),
                "repository search cancelled",
            )?;
            let file = &entry.file.version;
            if seen
                .get(&file.digest)
                .is_some_and(|path| *path != &file.path)
            {
                continue;
            }
            let reference = SourceRef {
                id: file.path.clone(),
                version: version_id(file),
                namespace: request.namespace.clone(),
                domain: Domain::Code,
                digest: file.digest.clone(),
                observed_at_ms: request.now_ms,
                valid_until_ms: request
                    .expires_at_ms
                    .min(request.now_ms.saturating_add(60_000)),
                symbols: vec![],
                links: vec![],
            };
            if bytes.saturating_add(serde_json::to_vec(&reference)?.len()) >= request.budget {
                continue;
            }
            let (current, content) = repo
                .read_cancellable(&file.path, cancel)
                .map_err(repo_error)?;
            require(
                current == *file,
                "indexed source changed while selecting results",
            )?;
            let content =
                std::str::from_utf8(&content).map_err(|_| Error("indexed source UTF-8".into()))?;
            let at = match anchor.or_else(|| line.and_then(|line| line_offset(content, line))) {
                Some(at) => at,
                None => term_offset(content, &words, cancel)?.unwrap_or(0),
            };
            if output.iter().any(|hit| {
                hit.source.id == file.path && (hit.start_byte..hit.end_byte).contains(&at)
            }) {
                continue;
            }
            let graph_match = signals.iter().any(|signal| {
                matches!(
                    *signal,
                    "symbol_definition" | "related_test" | "caller" | "reference" | "dependency"
                )
            });
            let mut span_bytes = (request.budget.saturating_sub(bytes)
                / (request.limit - output.len()).max(1))
            .clamp(32, 4096);
            let signals: Vec<_> = signals.into_iter().map(str::to_owned).collect();
            let hit = loop {
                let (start, end, start_line, end_line) = window(content, at, span_bytes);
                let excerpt = &content[start..end];
                let hit = Hit {
                    source: reference.clone(),
                    excerpt: excerpt.into(),
                    excerpt_digest: crate::digest(excerpt.as_bytes()),
                    start_byte: start,
                    end_byte: end,
                    start_line,
                    end_line: Some(end_line),
                    lexical_rank,
                    graph_match,
                    authority: "untrusted_data".into(),
                    signals: signals.clone(),
                };
                let size = serde_json::to_vec(&hit)?.len() + usize::from(!output.is_empty());
                if bytes.saturating_add(size) <= request.budget {
                    bytes += size;
                    break Some(hit);
                }
                if span_bytes <= 32 {
                    break None;
                }
                span_bytes = (span_bytes / 2).max(32);
            };
            let Some(hit) = hit else { continue };
            seen.insert(&file.digest, &file.path);
            output.push(hit);
        }
        Ok(output)
    }
}

fn path_matches(path: &str, query: &str) -> bool {
    if query.is_empty() {
        return false;
    }
    let path = path.to_lowercase();
    let basename = path.rsplit('/').next().unwrap_or(&path);
    let stem = basename
        .rsplit_once('.')
        .map(|(stem, _)| stem)
        .unwrap_or(basename);
    path == query || basename == query || stem == query
}

fn line_offset(content: &str, line: usize) -> Option<usize> {
    if line <= 1 {
        return Some(0);
    }
    content
        .match_indices('\n')
        .nth(line - 2)
        .map(|(i, _)| i + 1)
}

fn term_offset(
    content: &str,
    words: &BTreeSet<String>,
    cancel: &AtomicBool,
) -> Result<Option<usize>> {
    if words.is_empty() {
        return Ok(None);
    }
    let mut start = 0;
    let mut next_check = 0;
    for (end, ch) in content.char_indices() {
        if end >= next_check {
            require(
                !cancel.load(Ordering::Acquire),
                "repository excerpt cancelled",
            )?;
            next_check = end + 16 * 1024;
        }
        if !ch.is_alphanumeric() {
            if end > start && words.contains(&content[start..end].to_lowercase()) {
                return Ok(Some(start));
            }
            start = end + ch.len_utf8();
        }
    }
    Ok(
        (start < content.len() && words.contains(&content[start..].to_lowercase()))
            .then_some(start),
    )
}

fn window(content: &str, anchor: usize, maximum: usize) -> (usize, usize, usize, usize) {
    let mut anchor = anchor.min(content.len());
    while !content.is_char_boundary(anchor) {
        anchor -= 1;
    }
    let mut start = anchor.saturating_sub(maximum / 4);
    while !content.is_char_boundary(start) {
        start += 1;
    }
    if let Some(newline) = content[start..anchor].find('\n') {
        start += newline + 1;
    }
    let mut end = (start + maximum).min(content.len());
    while !content.is_char_boundary(end) {
        end -= 1;
    }
    if end < content.len()
        && let Some(newline) = content[anchor..end].rfind('\n')
    {
        end = anchor + newline + 1;
    }
    let start_line = content.as_bytes()[..start]
        .iter()
        .filter(|&&b| b == b'\n')
        .count()
        + 1;
    let excerpt = &content[start..end];
    let end_line = start_line
        + excerpt
            .strip_suffix('\n')
            .unwrap_or(excerpt)
            .bytes()
            .filter(|&b| b == b'\n')
            .count();
    (start, end, start_line, end_line)
}

fn version_id(file: &melissa_repository::FileVersion) -> String {
    format!("{}:{}", file.identity, file.digest)
}

fn repo_error(error: melissa_repository::Error) -> Error {
    Error(error.to_string())
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use melissa_local_store::StoreKey;
    use melissa_repository::Limits;
    use std::{
        fs::{self, FileTimes, OpenOptions},
        time::Instant,
    };

    #[test]
    fn persistent_index_reuses_fts_and_invalidates_changed_and_excluded_sources() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().join("workspace");
        let store_path = fixture.path().join("store");
        fs::create_dir_all(root.join("private")).unwrap();
        fs::write(
            root.join("private/blocked.py"),
            "def hiddenmarker(): pass\n",
        )
        .unwrap();
        let edited = root.join("calc.py");
        fs::write(&edited, "def needleold(): return 1\n").unwrap();
        fs::write(root.join("remove.py"), "def oldfixture(): pass\n").unwrap();
        let body = "padding scoped corpus example\n".repeat(256);
        for dir in 0..16 {
            let folder = root.join(format!("src{dir}"));
            fs::create_dir(&folder).unwrap();
            for file in 0..128 {
                fs::write(folder.join(format!("{file}.txt")), &body).unwrap();
            }
        }
        let ns = Namespace {
            user: "fixture".into(),
            project: "project".into(),
            engagement: None,
        };
        let repo = Repository::open(&root, Limits::default(), vec!["private".into()]).unwrap();
        let key = StoreKey::generate().unwrap();
        let binding = RepositoryIndex::binding(&repo, &ns, "test").unwrap();
        let cache = RepositoryIndexCache::open(&store_path, key.clone()).unwrap();
        let cancel = AtomicBool::new(false);
        let mut index = RepositoryIndex::new(binding).unwrap();
        let start = Instant::now();
        let cold = index.refresh(&repo, &cancel).unwrap();
        let cold_us = start.elapsed().as_micros();
        assert_eq!((cold.indexed, cold.parsed), (2050, 2));
        assert!(!index.files.contains_key("private/blocked.py"));
        assert_eq!(cold.snapshot, repo.snapshot(&cancel).unwrap().digest);
        let start = Instant::now();
        assert!(matches!(index.persist(&cache).unwrap(), Persistence::Saved));
        let cold_persist_us = start.elapsed().as_micros();
        drop(index);
        drop(cache);

        let cache = RepositoryIndexCache::open(&store_path, key).unwrap();
        let start = Instant::now();
        let (mut index, restored) = RepositoryIndex::open(&cache, binding).unwrap();
        assert!(matches!(restored, Restore::Loaded));
        let warm = index.refresh(&repo, &cancel).unwrap();
        let warm_us = start.elapsed().as_micros();
        assert_eq!((warm.indexed, warm.parsed), (0, 0));
        assert_eq!(warm.snapshot, cold.snapshot);
        assert!(warm.bytes_read <= cold.bytes_read);
        let start = Instant::now();
        let repeat = index.refresh(&repo, &cancel).unwrap();
        let repeat_us = start.elapsed().as_micros();
        assert_eq!((repeat.indexed, repeat.parsed), (0, 0));
        assert_eq!(repeat.snapshot, cold.snapshot);
        assert!(matches!(
            index.persist(&cache).unwrap(),
            Persistence::Unchanged
        ));
        let search = |index: &RepositoryIndex, query: &str, symbols: &[String]| {
            index
                .search(
                    &repo,
                    &SearchQuery {
                        query,
                        symbols,
                        links: None,
                        limit: 10,
                        namespace: &ns,
                        now_ms: 10,
                        expires_at_ms: 1000,
                        budget: 256 * 1024,
                        hints: &RankingHints::default(),
                    },
                    &cancel,
                )
                .unwrap()
        };
        let hits = search(&index, "needleold", &[]);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].source.id, "calc.py");
        assert!(search(&index, "hiddenmarker", &[]).is_empty());
        // Expected paths are fixed by the fixture before observing either ranker.
        let relevance_cases = [
            ("needleold", "calc.py"),
            ("oldfixture", "remove.py"),
            ("calc.py", "calc.py"),
            ("remove.py", "remove.py"),
        ];
        let mut baseline_top1 = 0;
        let mut hybrid_top1 = 0;
        let mut hybrid_us = Vec::new();
        for (query, expected) in relevance_cases {
            let rows = ranked_rows(&index.db, &query_terms(query)).unwrap();
            baseline_top1 += usize::from(
                rows.first()
                    .is_some_and(|row| index.files[expected].row == Some(*row)),
            );
            let start = Instant::now();
            let hits = search(&index, query, &[]);
            hybrid_us.push(start.elapsed().as_micros());
            hybrid_top1 += usize::from(hits.first().is_some_and(|hit| hit.source.id == expected));
        }
        assert_eq!(hybrid_top1, relevance_cases.len());
        eprintln!(
            "REPOSITORY_RELEVANCE cases={} lexical_top1={baseline_top1} hybrid_top1={hybrid_top1} hybrid_query_us={hybrid_us:?}",
            relevance_cases.len()
        );
        let modified = fs::metadata(&edited).unwrap().modified().unwrap();
        fs::write(&edited, "def needlenew(): return 2\n").unwrap();
        OpenOptions::new()
            .write(true)
            .open(&edited)
            .unwrap()
            .set_times(FileTimes::new().set_modified(modified))
            .unwrap();
        fs::remove_file(root.join("remove.py")).unwrap();
        let start = Instant::now();
        let delta = index.refresh(&repo, &cancel).unwrap();
        let delta_us = start.elapsed().as_micros();
        assert_eq!((delta.indexed, delta.parsed), (1, 1));
        assert_ne!(delta.snapshot, cold.snapshot);
        assert_eq!(delta.snapshot, repo.snapshot(&cancel).unwrap().digest);
        assert!(search(&index, "needleold", &[]).is_empty());
        assert!(search(&index, "oldfixture", &[]).is_empty());
        assert_eq!(
            search(&index, "", &["needlenew".into()])[0].source.id,
            "calc.py"
        );
        assert!(matches!(index.persist(&cache).unwrap(), Persistence::Saved));
        let other = Repository::open(&root, Limits::default(), vec!["calc.py".into()]).unwrap();
        let other_binding = RepositoryIndex::binding(&other, &ns, "test").unwrap();
        assert!(cache.load(&other_binding).unwrap().is_none());
        cache.clear(&other_binding).unwrap();
        let path = store_path.join("repository-index.v1.enc");
        let blob = fs::read(&path).unwrap();
        assert!(
            !blob
                .windows(b"needlenew".len())
                .any(|window| window == b"needlenew")
        );
        eprintln!(
            "REPOSITORY_INDEX cold_bytes={} warm_bytes={} repeat_bytes={} delta_bytes={} cold_refresh_us={cold_us} cold_persist_us={cold_persist_us} reopen_us={warm_us} repeat_us={repeat_us} delta_us={delta_us} encrypted_index_bytes={}",
            cold.bytes_read,
            warm.bytes_read,
            repeat.bytes_read,
            delta.bytes_read,
            blob.len()
        );
        let mut tampered = blob;
        *tampered.last_mut().unwrap() ^= 1;
        fs::write(path, tampered).unwrap();
        let (mut rebuilt, restored) = RepositoryIndex::open(&cache, binding).unwrap();
        assert!(matches!(restored, Restore::Invalid));
        assert!(rebuilt.refresh(&repo, &AtomicBool::new(true)).is_err());
        let recovered = rebuilt.refresh(&repo, &cancel).unwrap();
        assert_eq!((recovered.indexed, recovered.parsed), (2049, 1));
        assert_eq!(recovered.snapshot, delta.snapshot);
        assert!(search(&rebuilt, "needleold", &[]).is_empty());
        assert_eq!(search(&rebuilt, "needlenew", &[])[0].source.id, "calc.py");
        assert!(matches!(
            rebuilt.persist(&cache).unwrap(),
            Persistence::Saved
        ));
        let (_, restored) = RepositoryIndex::open(&cache, binding).unwrap();
        assert!(matches!(restored, Restore::Loaded));
    }
}
