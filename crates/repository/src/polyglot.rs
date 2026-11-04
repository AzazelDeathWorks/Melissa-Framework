//! Real grammar-backed syntax index for Rust, Python, JS, TS and TSX.
use crate::{Error, Repository, RepositorySnapshot, Result, check_cancel, discovery::language};
use serde::Serialize;
use std::{
    collections::BTreeMap,
    ops::ControlFlow,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};
#[derive(Default)]
pub struct ParseCache {
    entries: BTreeMap<String, (crate::FileVersion, Vec<SyntaxSymbol>, bool, bool, usize)>,
    pub parsed_files: u64,
    pub reused_files: u64,
}
#[derive(Debug, Clone, Serialize)]
pub struct SyntaxSymbol {
    pub path: String,
    pub file_digest: String,
    pub language: String,
    pub name: String,
    pub kind: String,
    pub start_byte: usize,
    pub end_byte: usize,
    pub line: usize,
    pub utf16_column: usize,
}
#[derive(Debug, Clone, Serialize)]
pub struct PolyglotIndex {
    pub snapshot_digest: String,
    pub symbols: Vec<SyntaxSymbol>,
    pub syntax_errors: Vec<String>,
    pub unsupported_files: Vec<String>,
    pub resolution: String,
    pub grammar_versions: Vec<String>,
}
impl PolyglotIndex {
    pub fn build(
        repo: &Repository,
        snapshot: &RepositorySnapshot,
        cancel: &AtomicBool,
    ) -> Result<Self> {
        Self::build_incremental(repo, snapshot, cancel, &mut ParseCache::default())
    }
    pub fn build_incremental(
        repo: &Repository,
        snapshot: &RepositorySnapshot,
        cancel: &AtomicBool,
        cache: &mut ParseCache,
    ) -> Result<Self> {
        repo.validate_snapshot(snapshot, cancel)?;
        let current: std::collections::BTreeSet<_> =
            snapshot.files.iter().map(|f| f.path.as_str()).collect();
        cache
            .entries
            .retain(|path, _| current.contains(path.as_str()));
        let mut out = Self {
            snapshot_digest: snapshot.digest.clone(),
            symbols: vec![],
            syntax_errors: vec![],
            unsupported_files: vec![],
            resolution: "syntactic; use LSP for semantic references".into(),
            grammar_versions: vec![
                "rust=0.24.2".into(),
                "python=0.25.0".into(),
                "javascript=0.25.0".into(),
                "typescript/tsx=0.23.2".into(),
            ],
        };
        let mut nodes = 0;
        for file in &snapshot.files {
            check_cancel(cancel)?;
            if let Some((version, symbols, error, unsupported, count)) =
                cache.entries.get(&file.path)
                && version == file
            {
                nodes += count;
                if nodes > MAX_SYNTAX_NODES {
                    return Err(Error::Quota("syntax nodes"));
                }
                out.symbols.extend(symbols.iter().cloned());
                if *error {
                    out.syntax_errors.push(file.path.clone());
                }
                if *unsupported {
                    out.unsupported_files.push(file.path.clone());
                }
                cache.reused_files += 1;
                continue;
            }
            if language(&file.path).is_none() {
                out.unsupported_files.push(file.path.clone());
                cache
                    .entries
                    .insert(file.path.clone(), (file.clone(), vec![], false, true, 0));
                continue;
            }
            let (version, bytes) = repo.read(&file.path)?;
            if version != *file {
                return Err(Error::Stale("polyglot source"));
            }
            let parsed = parse_file(file, &bytes, cancel, MAX_SYNTAX_NODES - nodes)?
                .ok_or(Error::Unsupported("language grammar"))?;
            nodes += parsed.nodes;
            if parsed.syntax_error {
                out.syntax_errors.push(file.path.clone());
            }
            out.symbols.extend(parsed.symbols.iter().cloned());
            cache.entries.insert(
                file.path.clone(),
                (
                    file.clone(),
                    parsed.symbols,
                    parsed.syntax_error,
                    false,
                    parsed.nodes,
                ),
            );
            cache.parsed_files += 1;
        }
        repo.validate_snapshot(snapshot, cancel)?;
        Ok(out)
    }
}

/// Grammar output for one already authenticated file read.
pub struct FileSyntax {
    pub symbols: Vec<SyntaxSymbol>,
    pub syntax_error: bool,
    pub nodes: usize,
}

pub const MAX_SYNTAX_NODES: usize = 1_000_000;

pub fn parse_file(
    file: &crate::FileVersion,
    bytes: &[u8],
    cancel: &AtomicBool,
    node_budget: usize,
) -> Result<Option<FileSyntax>> {
    let Some(lang) = language(&file.path) else {
        return Ok(None);
    };
    std::str::from_utf8(bytes).map_err(|_| Error::Invalid("source UTF-8"))?;
    let grammar = match lang {
        "rust" => tree_sitter_rust::LANGUAGE,
        "python" => tree_sitter_python::LANGUAGE,
        "javascript" => tree_sitter_javascript::LANGUAGE,
        "typescript" => tree_sitter_typescript::LANGUAGE_TYPESCRIPT,
        "tsx" => tree_sitter_typescript::LANGUAGE_TSX,
        _ => unreachable!(),
    };
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&grammar.into())
        .map_err(|_| Error::Unsupported("grammar ABI"))?;
    let start = Instant::now();
    let mut progress = |_: &tree_sitter::ParseState| {
        if cancel.load(Ordering::Acquire) || start.elapsed() > Duration::from_secs(60) {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    };
    let mut input = |offset: usize, _| bytes.get(offset..).unwrap_or_default();
    let tree = parser
        .parse_with_options(
            &mut input,
            None,
            Some(tree_sitter::ParseOptions::new().progress_callback(&mut progress)),
        )
        .ok_or(Error::Quota("syntax deadline"))?;
    let mut nodes = 0;
    let mut symbols = Vec::new();
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        check_cancel(cancel)?;
        nodes += 1;
        if nodes > node_budget {
            return Err(Error::Quota("syntax nodes"));
        }
        let k = node.kind();
        let declaration = matches!(
            k,
            "function_item"
                | "struct_item"
                | "enum_item"
                | "trait_item"
                | "mod_item"
                | "function_definition"
                | "class_definition"
                | "function_declaration"
                | "class_declaration"
                | "interface_declaration"
                | "type_alias_declaration"
                | "method_definition"
                | "variable_declarator"
        );
        if declaration && let Some(name) = node.child_by_field_name("name") {
            let text = name
                .utf8_text(bytes)
                .map_err(|_| Error::Invalid("symbol UTF-8"))?;
            let pos = name.start_position();
            let line_start = name.start_byte().saturating_sub(pos.column);
            let col = std::str::from_utf8(&bytes[line_start..name.start_byte()])
                .map_err(|_| Error::Invalid("symbol column"))?
                .encode_utf16()
                .count();
            symbols.push(SyntaxSymbol {
                path: file.path.clone(),
                file_digest: file.digest.clone(),
                language: lang.into(),
                name: text.into(),
                kind: k.into(),
                start_byte: name.start_byte(),
                end_byte: name.end_byte(),
                line: pos.row,
                utf16_column: col,
            });
        }
        for i in (0..node.named_child_count()).rev() {
            if let Some(child) = node.named_child(i as u32) {
                stack.push(child);
            }
        }
    }
    Ok(Some(FileSyntax {
        symbols,
        syntax_error: tree.root_node().has_error(),
        nodes,
    }))
}
