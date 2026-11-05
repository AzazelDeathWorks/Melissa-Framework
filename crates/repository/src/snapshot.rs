use crate::{
    Error, Result, check_cancel, digest,
    paths::{self, Pinned},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{Read, Seek, SeekFrom},
    path::Path,
    sync::atomic::AtomicBool,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    pub max_files: usize,
    pub max_file_bytes: u64,
    pub max_total_bytes: u64,
    pub max_patch_files: usize,
    pub max_edits: usize,
    pub max_search_hits: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            max_files: 10_000,
            max_file_bytes: 8 * 1024 * 1024,
            max_total_bytes: 128 * 1024 * 1024,
            max_patch_files: 128,
            max_edits: 4096,
            max_search_hits: 1000,
        }
    }
}
impl Limits {
    pub fn validate(&self) -> Result {
        if self.max_files == 0
            || self.max_files > 100_000
            || self.max_file_bytes == 0
            || self.max_file_bytes > 64 * 1024 * 1024
            || self.max_total_bytes < self.max_file_bytes
            || self.max_total_bytes > 1024 * 1024 * 1024
            || self.max_patch_files == 0
            || self.max_patch_files > 1024
            || self.max_edits == 0
            || self.max_edits > 100_000
            || self.max_search_hits == 0
            || self.max_search_hits > 100_000
        {
            return Err(Error::Invalid("invalid host resource envelope"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileVersion {
    pub path: String,
    pub identity: String,
    pub digest: String,
    pub bytes: u64,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepositorySnapshot {
    pub schema_version: String,
    pub root_identity: String,
    pub files: Vec<FileVersion>,
    pub digest: String,
    pub total_bytes: u64,
    pub atomic_tree_snapshot: bool,
    pub excluded_prefixes: Vec<String>,
}

/// A source digest paired with an optional host-observed NTFS journal receipt.
/// Only authenticated, workspace-bound index state may supply a previous value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexedFileVersion {
    pub version: FileVersion,
    journal_stamp: Option<(u64, i64)>,
}

pub struct IndexScan {
    pub snapshot: RepositorySnapshot,
    pub files: BTreeMap<String, IndexedFileVersion>,
    pub changed_bytes: BTreeMap<String, Vec<u8>>,
    pub bytes_read: u64,
}
#[derive(Debug, Clone, Serialize)]
pub struct SearchHit {
    pub path: String,
    pub digest: String,
    pub byte_offset: usize,
    pub line: usize,
    pub end_line: usize,
    pub match_excerpt: Option<String>,
    pub excerpt_start_byte: Option<usize>,
    pub excerpt_end_byte: Option<usize>,
}
#[derive(Debug, Clone, Serialize)]
pub struct SearchResult {
    pub hits: Vec<SearchHit>,
    pub truncated: bool,
    pub snapshot_digest: String,
}
#[derive(Debug, Clone, Serialize)]
pub struct WorkerInput {
    pub source_snapshot_digest: String,
    pub snapshot: melissa_container_executor::Snapshot,
}

pub struct Repository {
    pub(crate) root: Pinned,
    pub(crate) limits: Limits,
    visibility: crate::SourceVisibilityPolicy,
    package_capture: bool,
}
impl Repository {
    /// Host-selected workspace. Does not run Git, build scripts, hooks or tools.
    pub fn open(root: &Path, limits: Limits, excluded_prefixes: Vec<String>) -> Result<Self> {
        limits.validate()?;
        let mut excluded = vec![
            ".git".into(),
            "target".into(),
            "node_modules".into(),
            ".venv".into(),
        ];
        excluded.extend(excluded_prefixes);
        Ok(Self {
            root: paths::pin_root(root)?,
            limits,
            visibility: crate::SourceVisibilityPolicy::source(excluded, vec![])?,
            package_capture: false,
        })
    }
    pub fn root_path(&self) -> &Path {
        &self.root.canonical
    }
    /// Host-selected source policy; exceptions must be reviewed with the task.
    pub fn open_with_source_policy(
        root: &Path,
        limits: Limits,
        policy: crate::SourceVisibilityPolicy,
    ) -> Result<Self> {
        limits.validate()?;
        if !policy.protects_sensitive_paths() {
            return Err(Error::Invalid(
                "source policy must enable sensitive defaults",
            ));
        }
        Ok(Self {
            root: paths::pin_root(root)?,
            limits,
            visibility: policy,
            package_capture: false,
        })
    }
    /// Independent handle with the exact same source visibility and envelope.
    /// A domain worker must not accidentally re-include operator exclusions.
    pub fn reopen(&self) -> Result<Self> {
        let root = paths::pin(&self.root.canonical, true, false)?;
        if root.identity != self.root.identity {
            return Err(Error::Stale("repository root identity changed"));
        }
        Ok(Self {
            root,
            limits: self.limits.clone(),
            visibility: self.visibility.clone(),
            package_capture: self.package_capture,
        })
    }
    /// Exact bounded worktree capture for Git: no generated/ignored exclusions.
    /// A repository that exceeds the selected envelope fails instead of
    /// reporting missing tracked files as deleted.
    pub fn open_for_git(root: &Path, limits: Limits) -> Result<Self> {
        limits.validate()?;
        Ok(Self {
            root: paths::pin_root(root)?,
            limits,
            visibility: crate::SourceVisibilityPolicy::new(vec![".git".into()])?,
            package_capture: false,
        })
    }
    /// Complete package file capture: generated/runtime directories are part
    /// of the artifact. Git metadata and sensitive paths are rejected instead
    /// of silently omitting files and misrepresenting package integrity.
    /// Existing source/Git capture policies retain their original behavior.
    pub fn open_for_package(root: &Path, limits: Limits) -> Result<Self> {
        limits.validate()?;
        Ok(Self {
            root: paths::pin_root(root)?,
            limits,
            visibility: crate::SourceVisibilityPolicy::new(vec![])?,
            package_capture: true,
        })
    }
    pub fn root_identity(&self) -> &str {
        &self.root.identity
    }
    pub fn limits(&self) -> &Limits {
        &self.limits
    }
    pub(crate) fn check_path(&self, path: &str) -> Result {
        self.visibility.check(path)
    }
    pub fn source_visibility(&self) -> &crate::SourceVisibilityPolicy {
        &self.visibility
    }
    /// Binds historical model-visible results to the same root and policy.
    pub fn source_visibility_digest(&self) -> Result<String> {
        Ok(digest(&serde_json::to_vec(&(
            "melissa.source-visibility/v2",
            self.root_identity(),
            self.visibility.policy_digest_material(),
        ))?))
    }
    pub(crate) fn lock(&self, path: &str, write: bool) -> Result<Pinned> {
        self.check_path(path)?;
        paths::pin(
            &self.root.canonical.join(path.replace('/', "\\")),
            false,
            write,
        )
    }
    pub(crate) fn read_locked(
        &self,
        pin: &mut Pinned,
        path: &str,
    ) -> Result<(FileVersion, Vec<u8>)> {
        self.read_locked_with_cancel(pin, path, None)
    }

    fn read_locked_with_cancel(
        &self,
        pin: &mut Pinned,
        path: &str,
        cancel: Option<&AtomicBool>,
    ) -> Result<(FileVersion, Vec<u8>)> {
        self.check_path(path)?;
        if pin.file.metadata()?.len() > self.limits.max_file_bytes {
            return Err(Error::Quota("file bytes"));
        }
        pin.file.seek(SeekFrom::Start(0))?;
        let mut bytes = Vec::new();
        let mut bounded = pin.file.by_ref().take(self.limits.max_file_bytes + 1);
        let content_digest = if let Some(cancel) = cancel {
            let mut hash = blake3::Hasher::new();
            loop {
                check_cancel(cancel)?;
                let start = bytes.len();
                let count = bounded.by_ref().take(64 * 1024).read_to_end(&mut bytes)?;
                if count == 0 {
                    break;
                }
                hash.update(&bytes[start..]);
            }
            format!("blake3:{}", hash.finalize().to_hex())
        } else {
            bounded.read_to_end(&mut bytes)?;
            digest(&bytes)
        };
        if bytes.len() as u64 > self.limits.max_file_bytes {
            return Err(Error::Quota("file bytes"));
        }
        let version = FileVersion {
            path: path.into(),
            identity: pin.identity.clone(),
            digest: content_digest,
            bytes: bytes.len() as u64,
        };
        Ok((version, bytes))
    }
    pub fn read(&self, path: &str) -> Result<(FileVersion, Vec<u8>)> {
        let mut file = self.lock(path, false)?;
        self.read_locked(&mut file, path)
    }

    pub fn read_cancellable(
        &self,
        path: &str,
        cancel: &AtomicBool,
    ) -> Result<(FileVersion, Vec<u8>)> {
        check_cancel(cancel)?;
        self.read_locked_with_cancel(&mut self.lock(path, false)?, path, Some(cancel))
    }
    /// Read a source by the exact path that a repository snapshot would emit.
    /// A case alias must not become a second durable source identity.
    pub fn read_source_cancellable(
        &self,
        path: &str,
        cancel: &AtomicBool,
    ) -> Result<Option<(FileVersion, Vec<u8>)>> {
        check_cancel(cancel)?;
        let mut pin = match self.lock(path, false) {
            Ok(pin) => pin,
            Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(None);
            }
            Err(error) => return Err(error),
        };
        let expected = self.root.canonical.join(path.replace('/', "\\"));
        if pin.canonical.as_os_str() != expected.as_os_str() {
            return Ok(None);
        }
        self.read_locked_with_cancel(&mut pin, path, Some(cancel))
            .map(Some)
    }
    pub fn snapshot(&self, cancel: &AtomicBool) -> Result<RepositorySnapshot> {
        self.capture(cancel, |path, pin| {
            self.read_locked_with_cancel(pin, path, Some(cancel))
                .map(|(version, _)| version)
        })
    }

    /// Recheck the visible tree using the same pinned path admission as a
    /// snapshot. Read bytes whenever the OS cannot prove the cached version
    /// unchanged. Index receipts are for retrieval, never write admission.
    pub fn scan_index(
        &self,
        previous: &BTreeMap<String, IndexedFileVersion>,
        cancel: &AtomicBool,
    ) -> Result<IndexScan> {
        #[cfg(windows)]
        let journal = crate::file_journal::FileJournal::open(&self.root.file);
        let stamp = |pin: &Pinned| {
            #[cfg(windows)]
            {
                journal.as_ref().and_then(|j| j.stamp(&pin.file))
            }
            #[cfg(not(windows))]
            {
                let _ = pin;
                None
            }
        };
        let mut files = BTreeMap::new();
        let mut changed_bytes = BTreeMap::new();
        let mut bytes_read = 0u64;
        let snapshot = self.capture(cancel, |path, pin| {
            let old = previous.get(path);
            let (file, content) = self.observe_index_file(path, pin, stamp(pin), old, cancel)?;
            if let Some(content) = content {
                bytes_read += file.version.bytes;
                if old.is_none_or(|old| old.version.digest != file.version.digest) {
                    changed_bytes.insert(path.to_owned(), content);
                }
            }
            let version = file.version.clone();
            files.insert(path.to_owned(), file);
            Ok(version)
        })?;
        #[cfg(windows)]
        let stable = journal
            .as_ref()
            .map(|j| j.verify())
            .transpose()?
            .unwrap_or(false);
        #[cfg(not(windows))]
        let stable = false;
        if !stable {
            // A changed volume cursor could be unrelated activity. Confirm the
            // observed tree, reusing the fresh file receipts where possible.
            let confirmed = self.capture(cancel, |path, pin| {
                let (file, content) =
                    self.observe_index_file(path, pin, stamp(pin), files.get(path), cancel)?;
                if content.is_some() {
                    bytes_read += file.version.bytes;
                }
                Ok(file.version)
            })?;
            #[cfg(windows)]
            if let Some(journal) = &journal {
                journal.verify()?;
            }
            if confirmed != snapshot {
                return Err(Error::Stale("repository changed during index scan"));
            }
        }
        Ok(IndexScan {
            snapshot,
            files,
            changed_bytes,
            bytes_read,
        })
    }

    fn observe_index_file(
        &self,
        path: &str,
        pin: &mut Pinned,
        journal_stamp: Option<(u64, i64)>,
        old: Option<&IndexedFileVersion>,
        cancel: &AtomicBool,
    ) -> Result<(IndexedFileVersion, Option<Vec<u8>>)> {
        let bytes = pin.file.metadata()?.len();
        if bytes > self.limits.max_file_bytes {
            return Err(Error::Quota("file bytes"));
        }
        if let Some(old) = old
            && journal_stamp.is_some()
            && journal_stamp == old.journal_stamp
            && old.version.path == path
            && old.version.identity == pin.identity
            && old.version.bytes == bytes
        {
            return Ok((old.clone(), None));
        }
        let (version, content) = self.read_locked_with_cancel(pin, path, Some(cancel))?;
        Ok((
            IndexedFileVersion {
                version,
                journal_stamp,
            },
            Some(content),
        ))
    }

    fn capture(
        &self,
        cancel: &AtomicBool,
        mut read_version: impl FnMut(&str, &mut Pinned) -> Result<FileVersion>,
    ) -> Result<RepositorySnapshot> {
        let mut files = Vec::new();
        let mut total = 0u64;
        let mut seen = BTreeSet::new();
        let mut entries = 0;
        self.visit(
            &self.root,
            "",
            &mut seen,
            &mut entries,
            cancel,
            &mut |path, pin| {
                let version = read_version(path, pin)?;
                total = total
                    .checked_add(version.bytes)
                    .ok_or(Error::Quota("byte overflow"))?;
                if total > self.limits.max_total_bytes {
                    return Err(Error::Quota("snapshot bytes"));
                }
                files.push(version);
                Ok(())
            },
        )?;
        files.sort_by(|a, b| a.path.cmp(&b.path));
        let seal = digest(&serde_json::to_vec(&(
            self.root_identity(),
            &files,
            self.visibility.excluded_prefixes(),
        ))?);
        Ok(RepositorySnapshot {
            schema_version: "melissa.repository-snapshot/v1".into(),
            root_identity: self.root.identity.clone(),
            files,
            digest: seal,
            total_bytes: total,
            atomic_tree_snapshot: false,
            excluded_prefixes: self.visibility.excluded_prefixes().to_vec(),
        })
    }
    fn visit(
        &self,
        parent: &Pinned,
        relative: &str,
        seen: &mut BTreeSet<String>,
        entries: &mut usize,
        cancel: &AtomicBool,
        file: &mut impl FnMut(&str, &mut Pinned) -> Result,
    ) -> Result {
        check_cancel(cancel)?;
        for entry in fs::read_dir(&parent.canonical)? {
            check_cancel(cancel)?;
            *entries += 1;
            if *entries > self.limits.max_files {
                return Err(Error::Quota("directory and file entry count"));
            }
            let entry = entry?;
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| Error::Invalid("non-Unicode entry"))?;
            let child = if relative.is_empty() {
                name.clone()
            } else {
                format!("{relative}/{name}")
            };
            paths::relative(&child)?;
            if self.package_capture && crate::SourceVisibilityPolicy::sensitive_path(&child) {
                return Err(Error::Denied("package contains a sensitive path"));
            }
            if child.split('/').any(|s| s.eq_ignore_ascii_case(".git")) {
                if self.package_capture {
                    return Err(Error::Denied("package contains Git metadata"));
                }
                continue;
            }
            let kind = entry.file_type()?;
            if !self.visibility.allows(&child)
                && !(kind.is_dir() && self.visibility.permits_traversal(&child))
            {
                continue;
            }
            if !seen.insert(paths::key(&child)) {
                return Err(Error::Denied("case-colliding paths"));
            }
            paths::with_read_child(parent, &name, kind.is_dir(), |pin| {
                if kind.is_dir() {
                    self.visit(pin, &child, seen, entries, cancel, file)
                } else {
                    file(&child, pin)
                }
            })?;
        }
        Ok(())
    }
    pub fn validate_snapshot(&self, snapshot: &RepositorySnapshot, cancel: &AtomicBool) -> Result {
        let current = self.snapshot(cancel)?;
        if current != *snapshot {
            return Err(Error::Stale(
                "repository snapshot changed or its envelope was altered",
            ));
        }
        Ok(())
    }
    pub fn worker_input(
        &self,
        source: &RepositorySnapshot,
        cancel: &AtomicBool,
    ) -> Result<WorkerInput> {
        self.worker_input_with_budget(source, cancel, self.limits.max_total_bytes)
    }

    pub fn worker_input_with_budget(
        &self,
        source: &RepositorySnapshot,
        cancel: &AtomicBool,
        disk_bytes: u64,
    ) -> Result<WorkerInput> {
        check_cancel(cancel)?;
        let bytes_limit = disk_bytes.min(self.limits.max_total_bytes);
        melissa_container_executor::Snapshot::preflight(
            source.files.iter().map(|f| (f.path.as_str(), f.bytes)), bytes_limit,
        ).map_err(|_| Error::Quota("worker transfer preflight: file count, bytes, reserved path or ustar path capacity"))?;
        let mut files = std::collections::BTreeMap::new();
        for f in &source.files {
            let (current, bytes) = self.read_cancellable(&f.path, cancel)?;
            if current != *f {
                return Err(Error::Stale("worker snapshot source changed"));
            }
            files.insert(f.path.clone(), bytes);
        }
        let snapshot = melissa_container_executor::Snapshot { files };
        snapshot
            .validate(bytes_limit)
            .map_err(|_| Error::Quota("worker input format or bytes"))?;
        self.validate_snapshot(source, cancel)?;
        Ok(WorkerInput {
            source_snapshot_digest: source.digest.clone(),
            snapshot,
        })
    }
    /// Byte-literal search; matches carry the exact file and snapshot digest.
    pub fn search(
        &self,
        snapshot: &RepositorySnapshot,
        needle: &[u8],
        cancel: &AtomicBool,
    ) -> Result<SearchResult> {
        if needle.is_empty() || needle.len() > 64 * 1024 {
            return Err(Error::Invalid("invalid search needle"));
        }
        self.validate_snapshot(snapshot, cancel)?;
        let mut hits = Vec::new();
        let mut truncated = false;
        // Linear-time substring search; comparing every window cost
        // file size x needle length (up to 64 KiB) with no cancel point.
        let finder = memchr::memmem::Finder::new(needle);
        let prefix = overlap_table(needle);
        let periodic = needle.len() >= 64 && prefix[needle.len() - 1] >= needle.len() / 2;
        let match_lines = memchr::memchr_iter(b'\n', needle).count();
        for observed in &snapshot.files {
            check_cancel(cancel)?;
            let (version, bytes) = self.read_cancellable(&observed.path, cancel)?;
            if version != *observed {
                return Err(Error::Stale("search file changed"));
            }
            let text = std::str::from_utf8(&bytes).ok();
            let (mut start, mut line, mut counted) = (0, 1, 0);
            let mut overlap = OverlapCursor::default();
            // Restart one byte past each hit so overlapping matches are kept.
            while let Some(pos) = if periodic {
                overlap.find(&bytes, needle, &prefix, cancel)?
            } else {
                find_chunked(&bytes, start, &finder, needle.len(), cancel)?
            } {
                check_cancel(cancel)?;
                if hits.len() == self.limits.max_search_hits {
                    truncated = true;
                    break;
                }
                line += memchr::memchr_iter(b'\n', &bytes[counted..pos]).count();
                counted = pos;
                let excerpt = text.map(|text| {
                    let mut from = pos.saturating_sub(96);
                    while !text.is_char_boundary(from) {
                        from += 1;
                    }
                    let mut end = (from + 512).min(text.len());
                    while !text.is_char_boundary(end) {
                        end -= 1;
                    }
                    (from, end, text[from..end].to_owned())
                });
                hits.push(SearchHit {
                    path: version.path.clone(),
                    digest: version.digest.clone(),
                    byte_offset: pos,
                    line,
                    end_line: line + match_lines,
                    excerpt_start_byte: excerpt.as_ref().map(|e| e.0),
                    excerpt_end_byte: excerpt.as_ref().map(|e| e.1),
                    match_excerpt: excerpt.map(|e| e.2),
                });
                start = pos + 1;
            }
            if truncated {
                break;
            }
        }
        Ok(SearchResult {
            hits,
            truncated,
            snapshot_digest: snapshot.digest.clone(),
        })
    }
}

fn overlap_table(needle: &[u8]) -> Vec<usize> {
    let mut table = vec![0; needle.len()];
    let mut count = 0;
    for i in 1..needle.len() {
        count = overlap_step(needle, &table, count, needle[i]);
        table[i] = count;
    }
    table
}

fn overlap_step(needle: &[u8], table: &[usize], mut count: usize, byte: u8) -> usize {
    while count > 0 && needle[count] != byte {
        count = table[count - 1];
    }
    count + usize::from(needle[count] == byte)
}

#[derive(Default)]
struct OverlapCursor {
    next: usize,
    matched: usize,
}

impl OverlapCursor {
    fn find(
        &mut self,
        bytes: &[u8],
        needle: &[u8],
        table: &[usize],
        cancel: &AtomicBool,
    ) -> Result<Option<usize>> {
        check_cancel(cancel)?;
        while let Some(&byte) = bytes.get(self.next) {
            if self.next.is_multiple_of(16 * 1024) {
                check_cancel(cancel)?;
            }
            self.matched = overlap_step(needle, table, self.matched, byte);
            self.next += 1;
            if self.matched == needle.len() {
                self.matched = table[self.matched - 1];
                return Ok(Some(self.next - needle.len()));
            }
        }
        Ok(None)
    }
}

fn find_chunked(
    bytes: &[u8],
    start: usize,
    finder: &memchr::memmem::Finder<'_>,
    needle_len: usize,
    cancel: &AtomicBool,
) -> Result<Option<usize>> {
    let mut cursor = start;
    while cursor < bytes.len() {
        check_cancel(cancel)?;
        let end = bytes
            .len()
            .min(cursor.saturating_add(256 * 1024 + needle_len - 1));
        if let Some(found) = finder.find(&bytes[cursor..end]) {
            return Ok(Some(cursor + found));
        }
        if end == bytes.len() {
            break;
        }
        cursor += 256 * 1024;
    }
    Ok(None)
}

#[cfg(test)]
mod search_tests {
    use super::*;
    #[test]
    fn periodic_overlap_cursor_matches_literal_positions_without_losing_cancellation() {
        let needle = b"ab".repeat(100);
        let bytes = b"x"
            .iter()
            .copied()
            .chain(b"ab".repeat(4000))
            .collect::<Vec<_>>();
        let expected: Vec<_> = bytes
            .windows(needle.len())
            .enumerate()
            .filter_map(|(i, chunk)| (chunk == needle).then_some(i))
            .collect();
        let cancel = AtomicBool::new(false);
        let prefix = overlap_table(&needle);
        let mut cursor = OverlapCursor::default();
        let mut actual = Vec::new();
        while let Some(pos) = cursor.find(&bytes, &needle, &prefix, &cancel).unwrap() {
            actual.push(pos);
        }
        assert_eq!(actual, expected);
        assert!(matches!(
            cursor.find(&bytes, &needle, &prefix, &AtomicBool::new(true)),
            Err(Error::Cancelled)
        ));
    }
    #[test]
    fn chunk_boundaries_overlaps_and_cancellation_preserve_literal_search() {
        let cancel = AtomicBool::new(false);
        let finder = memchr::memmem::Finder::new(b"aaa");
        assert_eq!(
            find_chunked(b"aaaa", 0, &finder, 3, &cancel).unwrap(),
            Some(0)
        );
        assert_eq!(
            find_chunked(b"aaaa", 1, &finder, 3, &cancel).unwrap(),
            Some(1)
        );
        let mut bytes = vec![b'x'; 600_000];
        bytes[262_143..262_146].copy_from_slice(b"aaa");
        assert_eq!(
            find_chunked(&bytes, 0, &finder, 3, &cancel).unwrap(),
            Some(262_143)
        );
        assert_eq!(
            find_chunked(&bytes, 262_144, &finder, 3, &cancel).unwrap(),
            None
        );
        cancel.store(true, std::sync::atomic::Ordering::Release);
        assert!(matches!(
            find_chunked(&bytes, 0, &finder, 3, &cancel),
            Err(Error::Cancelled)
        ));
    }
}

#[cfg(all(test, windows))]
mod worker_preflight_tests {
    use super::*;

    #[test]
    fn oversized_worker_transfer_is_rejected_before_source_materialization() {
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("source.bin");
        std::fs::write(&file, vec![0xa5; 64 * 1024]).unwrap();
        let repo = Repository::open(directory.path(), Limits::default(), vec![]).unwrap();
        let cancel = AtomicBool::new(false);
        let snapshot = repo.snapshot(&cancel).unwrap();
        std::fs::remove_file(file).unwrap();
        assert!(matches!(
            repo.worker_input_with_budget(&snapshot, &cancel, 4096),
            Err(Error::Quota(_))
        ));
    }
}
