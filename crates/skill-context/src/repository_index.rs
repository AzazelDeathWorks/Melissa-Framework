//! Rebuildable repository retrieval index. SQLite lives in memory; the
//! authenticated local-store slot holds one bounded serialized generation.
use crate::{
    Error, Namespace, Result, require,
    retrieval::{Domain, Hit, SourceRef, make_hit, query_terms, ranked_rows, sql},
};
use melissa_local_store::{LocalStore, MAX_INDEX_BYTES};
use melissa_repository::{
    Repository,
    polyglot::{MAX_SYNTAX_NODES, parse_file},
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
    symbols: Vec<String>,
    syntax_nodes: usize,
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

pub(crate) struct SearchQuery<'a> {
    pub query: &'a str,
    pub symbols: &'a [String],
    pub links: Option<&'a BTreeMap<String, Vec<String>>>,
    pub limit: usize,
    pub namespace: &'a Namespace,
    pub now_ms: u64,
    pub expires_at_ms: u64,
}

impl RepositoryIndex {
    pub(crate) fn binding(
        repo: &Repository,
        namespace: &Namespace,
        policy_epoch: &str,
    ) -> Result<[u8; 32]> {
        let visibility = repo.source_visibility_digest().map_err(repo_error)?;
        let binding = serde_json::to_vec(&(
            "melissa.repository-index/v1",
            repo.root_path(),
            visibility,
            namespace,
            policy_epoch,
            crate::digest(include_bytes!("../../repository/src/polyglot.rs")),
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

    pub(crate) fn restore(binding: [u8; 32], bytes: &[u8]) -> Result<Self> {
        let prefix: [u8; 4] = bytes
            .get(..4)
            .ok_or_else(|| Error("short repository index header".into()))?
            .try_into()
            .expect("exact header length");
        let end = 4 + u32::from_le_bytes(prefix) as usize;
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

    pub(crate) fn persist(&mut self, store: &LocalStore) -> Result<()> {
        if !self.dirty {
            return Ok(());
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
        require(
            size <= MAX_INDEX_BYTES,
            "encrypted repository index capacity",
        )?;
        let image = self.db.serialize(MAIN_DB).map_err(sql)?;
        let mut bytes = Vec::with_capacity(size);
        bytes.extend_from_slice(&(meta.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&meta);
        bytes.extend_from_slice(&image);
        store.save_repository_index(&self.binding, &bytes)?;
        self.dirty = false;
        Ok(())
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
            let parsed = parse_file(&file.version, &bytes, cancel, MAX_SYNTAX_NODES - nodes)
                .map_err(repo_error)?;
            let mut symbols = Vec::new();
            let mut syntax_nodes = 0;
            if let Some(parsed) = parsed {
                syntax_nodes = parsed.nodes;
                nodes += syntax_nodes;
                symbols.extend(parsed.symbols.into_iter().map(|symbol| symbol.name));
                report.parsed += 1;
            }
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
                    symbols,
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

    pub(crate) fn validate_hit(&self, repo: &Repository, hit: &Hit) -> Result<()> {
        let version = self
            .files
            .get(&hit.source.id)
            .map(|entry| &entry.file.version)
            .ok_or_else(|| Error("cached hit outside current repository index".into()))?;
        require(
            hit.source.digest == version.digest
                && hit.source.version == version_id(version)
                && repo.read(&version.path).map_err(repo_error)?.0 == *version,
            "cached hit no longer matches indexed source",
        )
    }

    pub(crate) fn search(
        &self,
        repo: &Repository,
        request: &SearchQuery<'_>,
        cancel: &AtomicBool,
    ) -> Result<Vec<Hit>> {
        let terms = query_terms(request.query);
        let lexical: BTreeMap<_, _> = ranked_rows(&self.db, &terms)?
            .into_iter()
            .enumerate()
            .map(|(rank, row)| (row, rank))
            .collect();
        let seeds: BTreeSet<_> = self
            .files
            .iter()
            .filter(|(_, entry)| entry.symbols.iter().any(|s| request.symbols.contains(s)))
            .map(|(path, _)| path)
            .collect();
        let mut candidates: Vec<_> = self
            .files
            .iter()
            .filter_map(|(path, entry)| {
                let rank = entry.row.and_then(|row| lexical.get(&row)).copied();
                let graph = seeds.contains(path)
                    || request
                        .links
                        .and_then(|links| links.get(path))
                        .is_some_and(|ids| ids.iter().any(|id| seeds.contains(id)));
                (entry.row.is_some() && (rank.is_some() || graph)).then_some((entry, rank, graph))
            })
            .collect();
        candidates.sort_unstable_by(|(a, ar, ag), (b, br, bg)| {
            bg.cmp(ag)
                .then(ar.unwrap_or(usize::MAX).cmp(&br.unwrap_or(usize::MAX)))
                .then(a.file.version.path.cmp(&b.file.version.path))
        });
        let mut seen = BTreeSet::new();
        let mut output = Vec::new();
        let mut bytes = 2usize;
        for (entry, rank, graph) in candidates
            .into_iter()
            .filter(|(entry, _, _)| seen.insert(&entry.file.version.digest))
            .take(request.limit)
        {
            require(
                !cancel.load(Ordering::Acquire),
                "repository search cancelled",
            )?;
            let file = &entry.file.version;
            let (current, content) = repo.read(&file.path).map_err(repo_error)?;
            require(
                current == *file,
                "indexed source changed while selecting results",
            )?;
            let content =
                std::str::from_utf8(&content).map_err(|_| Error("indexed source UTF-8".into()))?;
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
            let hit = make_hit(reference, content, &terms, request.limit, rank, graph);
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

fn version_id(file: &melissa_repository::FileVersion) -> String {
    format!("{}:{}", file.identity, file.digest)
}

fn repo_error(error: melissa_repository::Error) -> Error {
    Error(error.to_string())
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use melissa_local_store::{StoreConfig, StoreKey};
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
            for file in 0..32 {
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
        let store = LocalStore::open(&store_path, key.clone(), StoreConfig::default()).unwrap();
        let cancel = AtomicBool::new(false);
        let mut index = RepositoryIndex::new(binding).unwrap();
        let start = Instant::now();
        let cold = index.refresh(&repo, &cancel).unwrap();
        let cold_us = start.elapsed().as_micros();
        assert_eq!((cold.indexed, cold.parsed), (514, 2));
        assert!(!index.files.contains_key("private/blocked.py"));
        assert_eq!(cold.snapshot, repo.snapshot(&cancel).unwrap().digest);
        index.persist(&store).unwrap();
        drop(index);
        drop(store);

        let store = LocalStore::open(&store_path, key, StoreConfig::default()).unwrap();
        let start = Instant::now();
        let mut index = RepositoryIndex::restore(
            binding,
            &store.load_repository_index(&binding).unwrap().unwrap(),
        )
        .unwrap();
        let warm = index.refresh(&repo, &cancel).unwrap();
        let warm_us = start.elapsed().as_micros();
        assert_eq!((warm.indexed, warm.parsed), (0, 0));
        assert_eq!(warm.snapshot, cold.snapshot);
        assert!(warm.bytes_read <= cold.bytes_read);
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
                    },
                    &cancel,
                )
                .unwrap()
        };
        let hits = search(&index, "needleold", &[]);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].source.id, "calc.py");
        assert!(search(&index, "hiddenmarker", &[]).is_empty());
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
        index.persist(&store).unwrap();
        let other = Repository::open(&root, Limits::default(), vec!["calc.py".into()]).unwrap();
        let other_binding = RepositoryIndex::binding(&other, &ns, "test").unwrap();
        assert!(
            store
                .load_repository_index(&other_binding)
                .unwrap()
                .is_none()
        );
        let blob = fs::read(store_path.join("repository-index.v1.enc")).unwrap();
        assert!(
            !blob
                .windows(b"needlenew".len())
                .any(|window| window == b"needlenew")
        );
        eprintln!(
            "REPOSITORY_INDEX cold_bytes={} warm_bytes={} delta_bytes={} cold_us={cold_us} reopen_us={warm_us} delta_us={delta_us} encrypted_index_bytes={}",
            cold.bytes_read,
            warm.bytes_read,
            delta.bytes_read,
            blob.len()
        );
    }
}
