//! Source-bound impact candidates. Import syntax is evidence, not a complete
//! build graph: macros, runtime loading and unresolved aliases stay explicit.
use crate::{
    Error, Repository, Result, check_cancel,
    graph::Graph,
    polyglot::{MAX_SYNTAX_NODES, parse_index_file},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::atomic::AtomicBool,
};
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Query {
    pub paths: Vec<String>,
}
#[derive(Debug, Serialize)]
pub struct Candidate {
    pub path: String,
    pub reason: &'static str,
    pub package_manifest: Option<String>,
    pub test_candidate: bool,
}
#[derive(Debug, Serialize)]
pub struct Report {
    pub schema_version: &'static str,
    pub source_digest: String,
    pub changed_paths: Vec<String>,
    pub candidates: Vec<Candidate>,
    pub unresolved_imports: Vec<(String, String)>,
    pub unsupported_files: Vec<String>,
    pub syntax_errors: Vec<String>,
    pub resolution: &'static str,
    pub complete_build_graph: bool,
}
impl Repository {
    pub fn impact_map(&self, q: &Query, cancel: &AtomicBool) -> Result<Report> {
        if q.paths.is_empty() || q.paths.len() > self.limits().max_files {
            return Err(Error::Invalid("impact seed paths"));
        }
        let source = self.snapshot(cancel)?;
        let mut facts = BTreeMap::new();
        let mut unsupported = vec![];
        let mut syntax_errors = vec![];
        let mut manifests = vec![];
        for f in &source.files {
            check_cancel(cancel)?;
            if matches!(
                f.path.rsplit('/').next(),
                Some("Cargo.toml" | "package.json" | "pyproject.toml")
            ) {
                manifests.push(f.path.clone());
            }
            if crate::discovery::language(&f.path).is_none() {
                unsupported.push(f.path.clone());
                continue;
            }
            let (v, bytes) = self.read_cancellable(&f.path, cancel)?;
            if v != *f {
                return Err(Error::Stale("impact source changed"));
            }
            if let Some(syntax) = parse_index_file(f, &bytes, cancel, MAX_SYNTAX_NODES)? {
                if syntax.syntax_error {
                    syntax_errors.push(f.path.clone());
                }
                if let Some(graph) = syntax.graph {
                    facts.insert(f.path.clone(), graph);
                }
            }
        }
        let graph = Graph::new(facts.iter().map(|(p, f)| (p.as_str(), f)).collect());
        let mut affected = BTreeSet::new();
        let seeds: BTreeSet<_> = q.paths.iter().cloned().collect();
        for path in &seeds {
            if !source.files.iter().any(|f| f.path == *path) {
                return Err(Error::Invalid("impact path outside visible source"));
            }
            affected.insert(path.clone());
        }
        let mut unresolved = vec![];
        let mut reverse: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
        for (path, imports) in &graph.imports {
            for import in imports {
                if let Some(dependency) = import.dependency {
                    reverse.entry(dependency).or_default().insert(path);
                } else {
                    unresolved.push((path.to_string(), import.import.specifier.clone()));
                }
            }
        }
        let mut pending: Vec<_> = seeds.iter().cloned().collect();
        while let Some(path) = pending.pop() {
            check_cancel(cancel)?;
            if let Some(dependents) = reverse.get(path.as_str()) {
                for &p in dependents {
                    if affected.insert(p.into()) {
                        pending.push(p.into());
                    }
                }
            }
        }
        let package = |path: &str| {
            if manifests.iter().any(|m| m == path) {
                return Some(path.to_owned());
            }
            let preferred = match crate::discovery::language(path) {
                Some("rust") => Some("Cargo.toml"),
                Some("python") => Some("pyproject.toml"),
                Some("javascript" | "typescript" | "tsx") => Some("package.json"),
                _ => None,
            };
            manifests
                .iter()
                .filter(|m| {
                    preferred.is_none_or(|name| m.rsplit('/').next() == Some(name))
                        && m.rsplit_once('/')
                            .is_none_or(|(p, _)| crate::paths::beneath(path, p))
                })
                .max_by_key(|m| m.len())
                .cloned()
        };
        let mut packages = BTreeSet::new();
        let mut candidates: Vec<_> = affected
            .iter()
            .map(|path| {
                let manifest = package(path);
                if let Some(m) = &manifest {
                    packages.insert(m.clone());
                }
                Candidate {
                    path: path.clone(),
                    reason: if seeds.contains(path) {
                        "changed_seed"
                    } else {
                        "transitive_syntactic_import"
                    },
                    package_manifest: manifest,
                    test_candidate: facts
                        .get(path)
                        .is_some_and(|f| f.definitions.iter().any(|d| d.test_candidate))
                        || path.contains("/tests/")
                        || path.starts_with("tests/")
                        || path.contains(".test.")
                        || path.contains(".spec."),
                }
            })
            .collect();
        for f in &source.files {
            if affected.contains(&f.path) {
                continue;
            }
            let manifest = package(&f.path);
            let manifest_change = manifest.as_ref().is_some_and(|m| seeds.contains(m));
            if manifest_change {
                affected.insert(f.path.clone());
                candidates.push(Candidate {
                    path: f.path.clone(),
                    reason: "changed_package_manifest_candidate",
                    package_manifest: manifest,
                    test_candidate: facts
                        .get(&f.path)
                        .is_some_and(|f| f.definitions.iter().any(|d| d.test_candidate))
                        || f.path.contains("/tests/")
                        || f.path.starts_with("tests/")
                        || f.path.contains(".test.")
                        || f.path.contains(".spec."),
                });
            }
        }
        // Package tests are deliberately broader candidates, not inferred edges.
        for (path, fact) in &facts {
            let test = fact.definitions.iter().any(|d| d.test_candidate)
                || path.contains("/tests/")
                || path.starts_with("tests/")
                || path.contains(".test.")
                || path.contains(".spec.");
            if test
                && !affected.contains(path)
                && package(path).is_some_and(|p| packages.contains(&p))
            {
                candidates.push(Candidate {
                    path: path.clone(),
                    reason: "same_package_test_candidate",
                    package_manifest: package(path),
                    test_candidate: true,
                });
            }
        }
        candidates.sort_by(|a, b| a.path.cmp(&b.path));
        self.validate_snapshot(&source, cancel)?;
        Ok(Report {
            schema_version: "melissa.impact-map/v1",
            source_digest: source.digest,
            changed_paths: seeds.into_iter().collect(),
            candidates,
            unresolved_imports: unresolved,
            unsupported_files: unsupported,
            syntax_errors,
            resolution: "syntactic_candidates_require_LSP_and_build_validation",
            complete_build_graph: false,
        })
    }
}
