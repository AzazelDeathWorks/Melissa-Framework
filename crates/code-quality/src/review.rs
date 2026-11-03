use crate::{
    Error, Result, bounded, digest,
    source::{Change, FrozenSource},
};
use melissa_repository::Repository;
use serde::{Deserialize, Serialize};
use std::sync::atomic::AtomicBool;
use zeroize::Zeroizing;

pub struct Redactor {
    secrets: Zeroizing<Vec<String>>,
}
impl std::fmt::Debug for Redactor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Redactor([redacted])")
    }
}
impl Redactor {
    /// Host-registered literal secrets only. This is not a general DLP scanner.
    pub fn new(secrets: Vec<String>) -> Result<Self> {
        let mut secrets = Zeroizing::new(secrets);
        if secrets.len() > 1024 || secrets.iter().any(|s| s.len() < 8 || s.len() > 65536) {
            return Err(Error::Invalid("registered secret bounds"));
        }
        secrets.sort_by_key(|s| std::cmp::Reverse(s.len()));
        secrets.dedup();
        Ok(Self { secrets })
    }
    pub fn contains(&self, text: &str) -> bool {
        self.secrets.iter().any(|s| text.contains(s.as_str()))
    }
    pub fn text(&self, text: &str) -> String {
        self.secrets
            .iter()
            .fold(text.to_owned(), |v, s| v.replace(s.as_str(), "[REDACTED]"))
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    Before,
    After,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnchorRequest {
    pub side: Side,
    pub path: String,
    pub start: usize,
    pub end: usize,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Anchor {
    pub side: Side,
    pub path: String,
    pub start: usize,
    pub end: usize,
    pub file_digest: String,
    pub span_digest: String,
    pub start_line: usize,
    pub end_line: usize,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Critical,
    High,
    Medium,
    Low,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Finding {
    pub id: String,
    pub severity: Severity,
    pub title: String,
    pub trigger: String,
    pub impact: String,
    pub proposed_verification: String,
    pub anchor: Anchor,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Submission {
    pub source_digest: String,
    pub baseline_digest: String,
    pub findings: Vec<Finding>,
    pub limitations: Vec<String>,
}
#[derive(Debug, Serialize)]
pub struct ReviewReport {
    pub schema_version: &'static str,
    pub source_digest: String,
    pub baseline_digest: String,
    pub changes: Vec<Change>,
    pub findings: Vec<Finding>,
    pub limitations: Vec<String>,
    pub location_validation: &'static str,
    pub defect_confirmation: &'static str,
    pub protected_acceptance: bool,
    pub task_accepted: bool,
}
/// Does not own a process executor or any workspace write method.
pub struct ReviewSession {
    repo: Repository,
    baseline: FrozenSource,
    final_source: FrozenSource,
    redactor: Redactor,
}
impl ReviewSession {
    pub fn start(
        repo: Repository,
        baseline: FrozenSource,
        redactor: Redactor,
        cancel: &AtomicBool,
    ) -> Result<Self> {
        let final_source = FrozenSource::capture(&repo, cancel)?;
        final_source.changes_from(&baseline)?;
        Ok(Self {
            repo,
            baseline,
            final_source,
            redactor,
        })
    }
    pub fn source(&self) -> &FrozenSource {
        &self.final_source
    }
    pub fn baseline(&self) -> &FrozenSource {
        &self.baseline
    }
    pub fn assert_current(&self, cancel: &AtomicBool) -> Result {
        self.final_source.current(&self.repo, cancel)
    }
    pub fn scope(&self, cancel: &AtomicBool) -> Result<serde_json::Value> {
        self.assert_current(cancel)?;
        let files = self
            .final_source
            .snapshot()
            .files
            .iter()
            .filter(|f| !self.redactor.contains(&f.path))
            .collect::<Vec<_>>();
        let changed = self
            .final_source
            .changes_from(&self.baseline)?
            .into_iter()
            .filter(|f| !self.redactor.contains(&f.path))
            .collect::<Vec<_>>();
        Ok(
            serde_json::json!({"source_digest":self.final_source.snapshot().digest,
            "baseline_digest":self.baseline.snapshot().digest,"files":files,"changes":changed,
            "withheld_sensitive_paths":self.final_source.snapshot().files.len()-files.len(),
            "atomic_tree_snapshot":false,"task_accepted":false}),
        )
    }
    fn selected(&self, side: Side) -> &FrozenSource {
        match side {
            Side::Before => &self.baseline,
            Side::After => &self.final_source,
        }
    }
    fn permitted_path(&self, path: &str) -> Result {
        if self.redactor.contains(path) {
            return Err(Error::Denied("sensitive file name withheld"));
        }
        Ok(())
    }
    pub fn anchor(&self, a: &AnchorRequest, cancel: &AtomicBool) -> Result<Anchor> {
        self.assert_current(cancel)?;
        self.permitted_path(&a.path)?;
        self.anchor_captured(a)
    }
    fn anchor_captured(&self, a: &AnchorRequest) -> Result<Anchor> {
        let bytes = self.selected(a.side).bytes(&a.path)?;
        let text =
            std::str::from_utf8(bytes).map_err(|_| Error::Invalid("text anchor requires UTF-8"))?;
        if a.start >= a.end
            || a.end > bytes.len()
            || !text.is_char_boundary(a.start)
            || !text.is_char_boundary(a.end)
        {
            return Err(Error::Invalid("nonempty UTF-8 byte range required"));
        }
        Ok(Anchor {
            side: a.side,
            path: a.path.clone(),
            start: a.start,
            end: a.end,
            file_digest: digest(bytes),
            span_digest: digest(&bytes[a.start..a.end]),
            start_line: bytes[..a.start].iter().filter(|&&b| b == b'\n').count() + 1,
            end_line: bytes[..a.end - 1].iter().filter(|&&b| b == b'\n').count() + 1,
        })
    }
    pub fn read_page(
        &self,
        side: Side,
        path: &str,
        offset: usize,
        maximum: usize,
        cancel: &AtomicBool,
    ) -> Result<serde_json::Value> {
        self.assert_current(cancel)?;
        self.permitted_path(path)?;
        let bytes = self.selected(side).bytes(path)?;
        let text =
            std::str::from_utf8(bytes).map_err(|_| Error::Invalid("review text requires UTF-8"))?;
        if !(1..=65536).contains(&maximum) || offset > bytes.len() || !text.is_char_boundary(offset)
        {
            return Err(Error::Invalid("review page bounds"));
        }
        let mut end = offset.saturating_add(maximum).min(bytes.len());
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        if end == offset && offset < bytes.len() {
            return Err(Error::Invalid("page cannot split a UTF-8 character"));
        }
        // Redact the complete file before selecting a page. Replacement offsets
        // cannot be reused as source offsets: return original offsets explicitly.
        // Overlapping registered literals are handled across page boundaries.
        let mut spans = self
            .redactor
            .secrets
            .iter()
            .flat_map(|secret| {
                text.match_indices(secret.as_str())
                    .map(|(start, value)| (start, start + value.len()))
            })
            .filter(|&(start, stop)| start < end && stop > offset)
            .collect::<Vec<_>>();
        spans.sort_unstable();
        let mut merged: Vec<(usize, usize)> = Vec::new();
        for (start, stop) in spans {
            if let Some(last) = merged.last_mut().filter(|last| start <= last.1) {
                last.1 = last.1.max(stop);
            } else {
                merged.push((start, stop));
            }
        }
        let mut visible = String::new();
        let mut cursor = offset;
        for (start, stop) in merged {
            let start = start.max(offset);
            let stop = stop.min(end);
            visible.push_str(&text[cursor..start]);
            visible.push_str("[REDACTED]");
            cursor = stop;
        }
        visible.push_str(&text[cursor..end]);
        Ok(
            serde_json::json!({"path":path,"side":side,"file_digest":digest(bytes),
            "text":visible,"encoding":"utf8_with_registered_literal_redaction",
            "original_offset":offset,"next_offset":if end<bytes.len(){Some(end)}else{None},
            "original_end":end,"total_source_bytes":bytes.len(),"task_accepted":false}),
        )
    }
    pub fn submit(&self, mut input: Submission, cancel: &AtomicBool) -> Result<ReviewReport> {
        self.assert_current(cancel)?;
        if input.source_digest != self.final_source.snapshot().digest
            || input.baseline_digest != self.baseline.snapshot().digest
        {
            return Err(Error::Stale("submission source/baseline mismatch"));
        }
        if input.findings.len() > 128
            || input.limitations.is_empty()
            || input.limitations.len() > 128
        {
            return Err(Error::Invalid(
                "bounded findings and explicit limitations required",
            ));
        }
        let mut ids = std::collections::BTreeSet::new();
        for f in &mut input.findings {
            if !bounded(&f.id, 128)
                || !ids.insert(f.id.clone())
                || !bounded(&f.title, 2048)
                || !bounded(&f.trigger, 8192)
                || !bounded(&f.impact, 8192)
                || !bounded(&f.proposed_verification, 8192)
                || self.redactor.contains(&f.id)
            {
                return Err(Error::Invalid("finding identity or explanation bounds"));
            }
            self.permitted_path(&f.anchor.path)?;
            let observed = self.anchor_captured(&AnchorRequest {
                side: f.anchor.side,
                path: f.anchor.path.clone(),
                start: f.anchor.start,
                end: f.anchor.end,
            })?;
            if observed != f.anchor {
                return Err(Error::Stale("finding location or exact bytes differ"));
            }
            f.title = self.redactor.text(&f.title);
            f.trigger = self.redactor.text(&f.trigger);
            f.impact = self.redactor.text(&f.impact);
            f.proposed_verification = self.redactor.text(&f.proposed_verification);
        }
        for s in &mut input.limitations {
            if !bounded(s, 8192) {
                return Err(Error::Invalid("limitation bounds"));
            }
            *s = self.redactor.text(s);
        }
        self.assert_current(cancel)?;
        Ok(ReviewReport {
            schema_version: "melissa.located-review-candidate/v1",
            source_digest: input.source_digest,
            baseline_digest: input.baseline_digest,
            changes: self
                .final_source
                .changes_from(&self.baseline)?
                .into_iter()
                .filter(|c| !self.redactor.contains(&c.path))
                .collect(),
            findings: input.findings,
            limitations: input.limitations,
            location_validation: "exact_source_bytes_and_lines",
            defect_confirmation: "NOT_CONFIRMED_REQUIRES_INDEPENDENT_QA",
            protected_acceptance: false,
            task_accepted: false,
        })
    }
}
