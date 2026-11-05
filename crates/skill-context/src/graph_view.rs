use crate::{Error, Result, repository_index::RepositoryIndex, require};
use melissa_repository::{
    PatchProposal, Repository,
    graph::SymbolId,
    polyglot::{MAX_SYNTAX_NODES, parse_index_file},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Query {
    path: Option<String>,
    symbol: Option<String>,
    #[serde(default = "page_limit")]
    limit: usize,
    #[serde(default)]
    node_offset: usize,
    #[serde(default)]
    edge_offset: usize,
    patch: Option<PatchProposal>,
}

fn page_limit() -> usize {
    64
}

impl Query {
    pub(crate) fn parse(input: &Value, repo: &Repository) -> Result<Self> {
        let query: Self = serde_json::from_value(input.clone())?;
        require(
            (1..=128).contains(&query.limit)
                && query.node_offset <= 100_000
                && query.edge_offset <= 4_000_000
                && query
                    .symbol
                    .as_ref()
                    .is_none_or(|s| !s.is_empty() && s.len() <= 256),
            "graph query bounds",
        )?;
        if let Some(path) = &query.path {
            repo.source_visibility()
                .check(path)
                .map_err(|e| Error(e.to_string()))?;
        }
        if let Some(patch) = &query.patch {
            patch
                .validate(repo.limits())
                .map_err(|e| Error(e.to_string()))?;
            require(
                patch.root_identity == repo.root_identity(),
                "foreign graph patch root",
            )?;
            for file in &patch.files {
                repo.source_visibility()
                    .check(&file.before.path)
                    .map_err(|e| Error(e.to_string()))?;
            }
        }
        Ok(query)
    }

    fn matches(&self, path: &str, name: Option<&str>) -> bool {
        self.path.as_deref().is_none_or(|p| p == path)
            && self.symbol.as_deref().is_none_or(|s| Some(s) == name)
    }
}

struct Page {
    rows: Vec<Value>,
    offset: usize,
    visited: usize,
    limit: usize,
    bytes: usize,
    next: Option<usize>,
}

impl Page {
    fn new(offset: usize, limit: usize) -> Self {
        Self {
            rows: vec![],
            offset,
            visited: 0,
            limit,
            bytes: 2,
            next: None,
        }
    }

    fn add(&mut self, row: impl FnOnce() -> Value) -> Result<()> {
        let index = self.visited;
        self.visited += 1;
        if index < self.offset || self.next.is_some() {
            return Ok(());
        }
        if self.rows.len() == self.limit {
            self.next = Some(index);
            return Ok(());
        }
        let row = row();
        let len = serde_json::to_vec(&row)?.len() + 1;
        if self.bytes + len > 80 * 1024 {
            self.next = Some(index);
        } else {
            self.bytes += len;
            self.rows.push(row);
        }
        Ok(())
    }
}

pub(crate) fn render(
    index: &RepositoryIndex,
    repo: &Repository,
    snapshot: &str,
    query: &Query,
    cancel: &AtomicBool,
) -> Result<Value> {
    let graph = index.graph();
    let mut nodes = Page::new(query.node_offset, query.limit);
    let mut edges = Page::new(query.edge_offset, query.limit);
    let versions: std::collections::BTreeMap<_, _> =
        index.versions().map(|f| (f.path.as_str(), f)).collect();
    let describe = |id: SymbolId<'_>| {
        let definition = graph.definition(id);
        json!({"id":id,"name":definition.name,"kind":definition.kind,
            "name_range":definition.name_range,"declaration":definition.declaration,
            "test_candidate":definition.test_candidate,"digest":versions[id.path].digest})
    };
    for (&path, version) in &versions {
        require(!cancel.load(Ordering::Acquire), "graph cancelled")?;
        if query.path.as_deref().is_some_and(|p| p != path) {
            continue;
        }
        let facts = graph.files.get(path).copied();
        let defs = facts.map(|f| f.definitions.as_slice()).unwrap_or_default();
        let mut selected = defs
            .iter()
            .enumerate()
            .filter(|(_, d)| query.matches(path, Some(&d.name)))
            .peekable();
        if query.symbol.is_some() && selected.peek().is_none() {
            continue;
        }
        nodes.add(|| {
            let shown: Vec<_> = selected.by_ref().take(24).map(|(i, _)| describe(SymbolId { path, index:i })).collect();
            json!({"id":path,"digest":version.digest,"language":facts.map(|f|f.language.as_str()),
                "status":match facts {None=>"unsupported",Some(f) if f.syntax_error=>"syntax_partial",Some(_)=>"parsed"},
                "symbols":shown.iter().map(|s|&s["name"]).collect::<Vec<_>>(),
                "definitions":shown,"definitions_truncated":selected.next().is_some(),
                "facts_truncated":facts.is_some_and(|f|f.truncated)})
        })?;
    }
    if let Some(patch) = &query.patch {
        let mut nodes_left = MAX_SYNTAX_NODES;
        for file in &patch.files {
            require(!cancel.load(Ordering::Acquire), "graph patch cancelled")?;
            let (current, content) = repo
                .read_cancellable(&file.before.path, cancel)
                .map_err(|e| Error(e.to_string()))?;
            require(
                versions.get(current.path.as_str()) == Some(&&current) && current == file.before,
                "stale graph patch preimage",
            )?;
            for edit in &file.edits {
                require(
                    content.get(edit.start as usize..edit.end as usize)
                        == Some(edit.expected.as_slice()),
                    "graph patch expected bytes mismatch",
                )?;
            }
            if let Some((&path, facts)) = graph.files.get_key_value(current.path.as_str()) {
                for (i, def) in facts.definitions.iter().enumerate() {
                    if query.matches(path, Some(&def.name))
                        && file
                            .edits
                            .iter()
                            .any(|edit| def.declaration.intersects(edit.start, edit.end))
                    {
                        edges.add(|| json!({"kind":"affected_symbol","source":describe(SymbolId {path,index:i}),"preimage_only":true}))?;
                    }
                }
                if edges.next.is_none() {
                    let mut preview = Vec::new();
                    let mut ranges = Vec::new();
                    let mut copied = 0;
                    for edit in &file.edits {
                        require(!cancel.load(Ordering::Acquire), "graph preview cancelled")?;
                        preview.extend_from_slice(&content[copied..edit.start as usize]);
                        let start = preview.len() as u64;
                        preview.extend_from_slice(&edit.replacement);
                        ranges.push((start, preview.len() as u64));
                        copied = edit.end as usize;
                    }
                    preview.extend_from_slice(&content[copied..]);
                    if std::str::from_utf8(&preview).is_err() {
                        edges.add(|| json!({"kind":"patch_preview_unavailable","path":path,"reason":"non_utf8_postimage"}))?;
                        continue;
                    }
                    let mut after = current;
                    after.bytes = preview.len() as u64;
                    after.digest = crate::digest(&preview);
                    if let Some(parsed) = parse_index_file(&after, &preview, cancel, nodes_left)
                        .map_err(|e| Error(e.to_string()))?
                    {
                        nodes_left -= parsed.nodes;
                        if let Some(facts) = parsed.graph {
                            for def in &facts.definitions {
                                if query.matches(path, Some(&def.name))
                                    && ranges
                                        .iter()
                                        .any(|&(start, end)| def.declaration.intersects(start, end))
                                {
                                    edges.add(|| json!({"kind":"affected_symbol_preview","path":path,
                                        "definition":def,"preview_digest":after.digest,"syntax_partial":facts.syntax_error}))?;
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    for (&path, facts) in &graph.files {
        require(!cancel.load(Ordering::Acquire), "graph cancelled")?;
        if edges.next.is_some() {
            break;
        }
        if query.symbol.is_none() {
            for import in &graph.imports[path] {
                if query.matches(path, None)
                    || import.dependency.is_some_and(|d| query.matches(d, None))
                {
                    edges.add(|| json!({"kind":"import","source":import,
                        "digest":versions[path].digest,"dependency_status":if import.dependency.is_some(){"path_candidate"}else{"unresolved"}}))?;
                }
            }
        }
        for reference in &facts.references {
            require(!cancel.load(Ordering::Acquire), "graph cancelled")?;
            if edges.next.is_some() {
                break;
            }
            let owner = reference.owner.map(|i| SymbolId { path, index: i });
            let owner_name = owner.map(|id| graph.definition(id).name.as_str());
            if query
                .symbol
                .as_deref()
                .is_some_and(|name| name != reference.name && Some(name) != owner_name)
            {
                continue;
            }
            if let Some(target) = graph.target(path, reference) {
                if !query.matches(path, Some(&reference.name))
                    && !query.matches(target.path, Some(&reference.name))
                    && !query.matches(path, owner_name)
                {
                    continue;
                }
                edges.add(|| json!({"kind":if reference.call{"call_candidate"}else{"reference_candidate"},
                    "path":path,"digest":versions[path].digest,"range":reference.range,
                    "caller":owner.map(describe),"target":describe(target),
                    "related_test":owner.filter(|id|graph.definition(*id).test_candidate).map(describe),
                    "compiler_resolved":false}))?;
            }
        }
    }
    Ok(
        json!({"source_digest":snapshot,"nodes":nodes.rows,"edges":edges.rows,
        "next_node_offset":nodes.next,"next_edge_offset":edges.next,
        "coverage":{"languages":["rust","python","javascript","typescript","tsx"],
            "resolution":"syntax candidates; use native semantic tools for resolved references",
            "dynamic_dispatch_and_external_modules":"unresolved",
            "patch":"affected declarations in authenticated preimages and read-only proposed postimages",
            "visible_files":versions.len(),"grammar_files":graph.files.len()},
        "authority":"untrusted_data","protected_acceptance":false}),
    )
}

pub(crate) fn input_schema() -> Value {
    json!({"type":"object","properties":{
        "path":{"type":"string"},"symbol":{"type":"string","maxLength":256},
        "limit":{"type":"integer","minimum":1,"maximum":128},
        "node_offset":{"type":"integer","minimum":0,"maximum":100000},
        "edge_offset":{"type":"integer","minimum":0,"maximum":4000000},
        "patch":{"type":"object","description":"Optional current melissa.repository-patch/v1 preimage; read-only affected declaration analysis."}
    },"additionalProperties":false})
}
