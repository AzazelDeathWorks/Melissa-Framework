//! ast-grep generates untrusted, exact preimage-bound proposals. Application is
//! exclusively the existing scoped PatchBroker route, never a tool side effect.
use crate::{
    ByteEdit, Error, FilePatch, FileVersion, PatchProposal, Repository, Result, check_cancel,
};
use ast_grep_core::{AstGrep, Pattern};
use ast_grep_language::SupportLang;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, sync::atomic::AtomicBool};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Query {
    pub language: String,
    pub paths: Vec<String>,
    pub pattern: String,
    #[serde(default)]
    pub rewrite: Option<String>,
}
#[derive(Debug, Serialize)]
pub struct Match {
    pub before: FileVersion,
    pub start: usize,
    pub end: usize,
    pub text: String,
}
#[derive(Debug, Serialize)]
pub struct Report {
    pub schema_version: &'static str,
    pub engine: &'static str,
    pub source_digest: String,
    pub matches: Vec<Match>,
    pub proposal: Option<PatchProposal>,
    pub resolution: &'static str,
    pub complete: bool,
}
fn variables(s: &str) -> BTreeSet<&str> {
    s.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '$'))
        .filter_map(|t| t.strip_prefix('$').map(|t| t.trim_start_matches('$')))
        .filter(|t| !t.is_empty())
        .collect()
}
impl Repository {
    pub fn structural(&self, q: &Query, cancel: &AtomicBool) -> Result<Report> {
        if q.paths.is_empty()
            || q.paths.len() > self.limits().max_patch_files
            || q.pattern.is_empty()
            || q.pattern.len() > 65536
            || q.rewrite.as_ref().is_some_and(|r| r.len() > 65536)
        {
            return Err(Error::Invalid("structural query bounds"));
        }
        if !["rust", "python", "javascript", "typescript", "tsx"].contains(&q.language.as_str()) {
            return Err(Error::Unsupported("structural language"));
        }
        let grammar: SupportLang = q
            .language
            .parse()
            .map_err(|_| Error::Unsupported("structural grammar"))?;
        let pattern = Pattern::try_new(&q.pattern, grammar)
            .map_err(|_| Error::Invalid("structural pattern"))?;
        if pattern.has_error() {
            return Err(Error::Invalid("structural pattern has syntax errors"));
        }
        if let Some(rewrite) = &q.rewrite {
            let defined = pattern.defined_vars();
            if variables(rewrite).iter().any(|v| !defined.contains(v)) {
                return Err(Error::Invalid("rewrite contains an unbound metavariable"));
            }
        }
        let source = self.snapshot(cancel)?;
        let mut seen = BTreeSet::new();
        let mut matches = vec![];
        let mut patches = vec![];
        for path in &q.paths {
            check_cancel(cancel)?;
            if !seen.insert(crate::paths::key(path)) {
                return Err(Error::Invalid("duplicate structural path"));
            }
            if crate::discovery::language(path) != Some(q.language.as_str()) {
                return Err(Error::Invalid("structural path language differs"));
            }
            let (before, bytes) = self.read_cancellable(path, cancel)?;
            if !source.files.contains(&before) {
                return Err(Error::Stale("structural preimage"));
            }
            let syntax = crate::polyglot::parse_index_file(
                &before,
                &bytes,
                cancel,
                crate::polyglot::MAX_SYNTAX_NODES,
            )?
            .ok_or(Error::Unsupported("structural parser"))?;
            if syntax.syntax_error {
                return Err(Error::Invalid("structural source has syntax errors"));
            }
            let text =
                std::str::from_utf8(&bytes).map_err(|_| Error::Invalid("structural UTF-8"))?;
            let ast =
                AstGrep::try_new(text, grammar).map_err(|_| Error::Invalid("structural parse"))?;
            let root = ast.root();
            let mut edits = vec![];
            for found in root.find_all(&pattern) {
                check_cancel(cancel)?;
                if matches.len() >= self.limits().max_edits {
                    return Err(Error::Quota(
                        "structural match count; narrow paths or pattern",
                    ));
                }
                let range = found.range();
                matches.push(Match {
                    before: before.clone(),
                    start: range.start,
                    end: range.end,
                    text: text[range.clone()].into(),
                });
                if let Some(rewrite) = &q.rewrite {
                    let edit = found.replace_by(rewrite.as_str());
                    let end = edit.position + edit.deleted_length;
                    if bytes[edit.position..end] != edit.inserted_text {
                        edits.push(ByteEdit {
                            start: edit.position as u64,
                            end: end as u64,
                            expected: bytes[edit.position..end].to_vec(),
                            replacement: edit.inserted_text,
                        });
                    }
                }
            }
            edits.sort_by_key(|e| e.start);
            if !edits.is_empty() {
                patches.push(FilePatch { before, edits });
            }
        }
        let proposal = if patches.is_empty() {
            None
        } else {
            let p = PatchProposal {
                schema_version: "melissa.repository-patch/v1".into(),
                root_identity: self.root_identity().into(),
                files: patches,
            };
            p.validate(self.limits())?; // Includes overlapping matches and output admission.
            Some(p)
        };
        self.validate_snapshot(&source, cancel)?;
        Ok(Report {
            schema_version: "melissa.structural/v1",
            engine: "ast-grep-core/0.51.1",
            source_digest: source.digest,
            matches,
            proposal,
            resolution: "structural_candidates_not_semantic_rename",
            complete: true,
        })
    }
}
