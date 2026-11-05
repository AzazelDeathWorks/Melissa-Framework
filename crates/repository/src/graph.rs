//! Bounded grammar observations and conservative, source-visible relations.
//! A name or module match remains a candidate until a language server resolves it.
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use tree_sitter::Node;

const FACTS_PER_FILE: usize = 4096;

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Span {
    pub start_byte: usize,
    pub end_byte: usize,
    pub start_line: usize,
    pub end_line: usize,
}

impl Span {
    fn from_node(node: Node<'_>) -> Self {
        Self {
            start_byte: node.start_byte(),
            end_byte: node.end_byte(),
            start_line: node.start_position().row + 1,
            end_line: node.end_position().row + 1
                - usize::from(
                    node.end_position().column == 0
                        && node.end_position().row > node.start_position().row,
                ),
        }
    }

    pub fn intersects(self, start: u64, end: u64) -> bool {
        if start == end {
            (self.start_byte as u64..=self.end_byte as u64).contains(&start)
        } else {
            start < self.end_byte as u64 && end > self.start_byte as u64
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Definition {
    pub name: String,
    pub kind: String,
    pub name_range: Span,
    pub declaration: Span,
    pub test_candidate: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Reference {
    pub name: String,
    pub range: Span,
    pub owner: Option<usize>,
    pub call: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Import {
    pub specifier: String,
    pub range: Span,
    pub rust_module: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct FileFacts {
    pub language: String,
    pub syntax_error: bool,
    pub truncated: bool,
    pub definitions: Vec<Definition>,
    pub references: Vec<Reference>,
    pub imports: Vec<Import>,
}

impl FileFacts {
    pub(crate) fn new(lang: &str, syntax_error: bool) -> Self {
        Self {
            language: lang.into(),
            syntax_error,
            truncated: false,
            definitions: vec![],
            references: vec![],
            imports: vec![],
        }
    }

    pub(crate) fn definition(
        &mut self,
        file: &crate::FileVersion,
        node: Node<'_>,
        name: Node<'_>,
        text: &str,
        bytes: &[u8],
    ) -> Option<usize> {
        if self.definitions.len() >= FACTS_PER_FILE || text.len() > 256 {
            self.truncated = true;
            return None;
        }
        let test = text.starts_with("test_")
            || text.ends_with("_test")
            || file
                .path
                .split('/')
                .any(|part| part == "tests" || part == "__tests__")
            || file.path.rsplit('/').next().is_some_and(|name| {
                name.starts_with("test_") || name.contains(".test.") || name.contains(".spec.")
            })
            || node.prev_named_sibling().is_some_and(|attribute| {
                attribute.kind() == "attribute_item"
                    && attribute
                        .utf8_text(bytes)
                        .is_ok_and(|s| s == "#[test]" || s == "#[tokio::test]")
            });
        let index = self.definitions.len();
        self.definitions.push(Definition {
            name: text.into(),
            kind: node.kind().into(),
            name_range: Span::from_node(name),
            declaration: Span::from_node(node),
            test_candidate: test,
        });
        matches!(
            node.kind(),
            "function_item" | "function_definition" | "function_declaration" | "method_definition"
        )
        .then_some(index)
    }

    fn import(&mut self, node: Node<'_>, bytes: &[u8], rust_module: bool) {
        let Ok(text) = node.utf8_text(bytes) else {
            return;
        };
        let specifier = if matches!(self.language.as_str(), "javascript" | "typescript" | "tsx") {
            let Some(quote @ ('\'' | '"')) = text.chars().next() else {
                return;
            };
            let Some(specifier) = text.strip_prefix(quote).and_then(|s| s.strip_suffix(quote))
            else {
                return;
            };
            specifier
        } else {
            text
        };
        if self.imports.len() >= FACTS_PER_FILE || specifier.len() > 1024 {
            self.truncated = true;
            return;
        }
        self.imports.push(Import {
            specifier: specifier.into(),
            range: Span::from_node(node),
            rust_module,
        });
    }

    pub(crate) fn observe(&mut self, node: Node<'_>, owner: Option<usize>, bytes: &[u8]) {
        match (self.language.as_str(), node.kind()) {
            ("python", "import_from_statement") => {
                if let Some(module) = node.child_by_field_name("module_name") {
                    self.import(module, bytes, false);
                }
            }
            ("python", "import_statement") => {
                let mut cursor = node.walk();
                for child in node.named_children(&mut cursor) {
                    if child.kind() == "aliased_import" {
                        if let Some(name) = child.child_by_field_name("name") {
                            self.import(name, bytes, false);
                        }
                    } else if child.kind() == "dotted_name" {
                        self.import(child, bytes, false);
                    }
                }
            }
            ("rust", "mod_item") if node.child_by_field_name("body").is_none() => {
                if let Some(name) = node.child_by_field_name("name") {
                    self.import(name, bytes, true);
                }
            }
            ("rust", "use_declaration") => {
                if let Some(arg) = node.child_by_field_name("argument") {
                    self.import(arg, bytes, false);
                }
            }
            ("javascript" | "typescript" | "tsx", "import_statement" | "export_statement") => {
                if let Some(source) = node.child_by_field_name("source") {
                    self.import(source, bytes, false);
                }
            }
            _ => {}
        }
        if !matches!(node.kind(), "identifier" | "type_identifier") {
            return;
        }
        let Some(parent) = node.parent() else {
            return;
        };
        if matches!(
            parent.kind(),
            "function_item"
                | "function_definition"
                | "function_declaration"
                | "class_definition"
                | "class_declaration"
                | "method_definition"
                | "struct_item"
                | "enum_item"
                | "trait_item"
                | "mod_item"
                | "variable_declarator"
                | "type_alias_declaration"
                | "interface_declaration"
        ) && parent.child_by_field_name("name") == Some(node)
        {
            return;
        }
        let Ok(name) = node.utf8_text(bytes) else {
            return;
        };
        if self.references.len() >= FACTS_PER_FILE || name.len() > 256 {
            self.truncated = true;
            return;
        }
        let callable = if parent.kind() == "scoped_identifier"
            && parent.child_by_field_name("name") == Some(node)
        {
            parent
        } else {
            node
        };
        let call = callable.parent().is_some_and(|call| {
            matches!(call.kind(), "call_expression" | "call")
                && call.child_by_field_name("function") == Some(callable)
        });
        self.references.push(Reference {
            name: name.into(),
            range: Span::from_node(node),
            owner,
            call,
        });
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Ord, PartialOrd, Serialize)]
pub struct SymbolId<'a> {
    pub path: &'a str,
    pub index: usize,
}

#[derive(Serialize)]
pub struct ModuleImport<'a> {
    pub path: &'a str,
    pub import: &'a Import,
    pub dependency: Option<&'a str>,
}

pub struct Graph<'a> {
    pub files: BTreeMap<&'a str, &'a FileFacts>,
    names: BTreeMap<&'a str, BTreeMap<&'a str, Option<usize>>>,
    pub imports: BTreeMap<&'a str, Vec<ModuleImport<'a>>>,
    dependencies: BTreeMap<&'a str, BTreeSet<&'a str>>,
}

impl<'a> Graph<'a> {
    pub fn new(files: BTreeMap<&'a str, &'a FileFacts>) -> Self {
        let mut names = BTreeMap::<&str, BTreeMap<&str, Option<usize>>>::new();
        let mut imports = BTreeMap::new();
        let mut dependencies = BTreeMap::<&str, BTreeSet<_>>::new();
        for (&path, facts) in &files {
            for (index, definition) in facts.definitions.iter().enumerate() {
                names
                    .entry(&definition.name)
                    .or_default()
                    .entry(path)
                    .and_modify(|entry| *entry = None)
                    .or_insert(Some(index));
            }
            let observed: Vec<_> = facts
                .imports
                .iter()
                .map(|import| {
                    let dependency = module_target(path, &facts.language, import, &files);
                    if let Some(dep) = dependency {
                        dependencies.entry(path).or_default().insert(dep);
                    }
                    ModuleImport {
                        path,
                        import,
                        dependency,
                    }
                })
                .collect();
            imports.insert(path, observed);
        }
        Self {
            files,
            names,
            imports,
            dependencies,
        }
    }

    pub fn definition(&self, id: SymbolId<'a>) -> &'a Definition {
        &self.files[id.path].definitions[id.index]
    }

    pub fn target(&self, path: &str, reference: &Reference) -> Option<SymbolId<'a>> {
        let definitions = self.names.get(reference.name.as_str())?;
        if let Some((&path, &index)) = definitions.get_key_value(path) {
            return index.map(|index| SymbolId { path, index });
        }
        let deps = self.dependencies.get(path)?;
        if deps.len() < definitions.len() {
            unique_import(
                deps.iter()
                    .filter_map(|path| definitions.get_key_value(*path)),
            )
        } else {
            unique_import(definitions.iter().filter(|(path, _)| deps.contains(*path)))
        }
    }

    pub fn neighbors(&self, seeds: &BTreeSet<&str>) -> BTreeSet<&'a str> {
        let mut result = BTreeSet::new();
        for (&path, deps) in &self.dependencies {
            if seeds.contains(path) {
                result.extend(deps.iter().copied());
            }
            if deps.iter().any(|dep| seeds.contains(dep)) {
                result.insert(path);
            }
        }
        result
    }
}

fn unique_import<'a, 'b>(
    mut candidates: impl Iterator<Item = (&'b &'a str, &'b Option<usize>)>,
) -> Option<SymbolId<'a>>
where
    'a: 'b,
{
    let (&path, &index) = candidates.next()?;
    if candidates.next().is_some() {
        return None;
    }
    index.map(|index| SymbolId { path, index })
}

fn join_module(parent: &str, module: &str) -> Option<String> {
    let mut components: Vec<_> = parent.split('/').filter(|p| !p.is_empty()).collect();
    for part in module.split('/') {
        match part {
            ".." => {
                components.pop()?;
            }
            "." => {}
            "" => return None,
            value => components.push(value),
        }
    }
    (!components.is_empty()).then(|| components.join("/"))
}

fn module_target<'a>(
    path: &str,
    lang: &str,
    import: &Import,
    files: &BTreeMap<&'a str, &'a FileFacts>,
) -> Option<&'a str> {
    let (parent, file) = path.rsplit_once('/').unwrap_or(("", path));
    let spec = import.specifier.as_str();
    let mut choices = BTreeSet::new();
    match lang {
        "python" => {
            let dots = spec.bytes().take_while(|c| *c == b'.').count();
            let module = &spec[dots..];
            let mut bases = Vec::new();
            if dots == 0 {
                bases.push(module.replace('.', "/"));
                bases.push(format!("src/{}", module.replace('.', "/")));
            } else {
                let mut dir = parent;
                for _ in 1..dots {
                    dir = dir
                        .rsplit_once('/')
                        .map(|(p, _)| p)
                        .or_else(|| (!dir.is_empty()).then_some(""))?;
                }
                if module.is_empty() {
                    bases.push(dir.into());
                } else {
                    bases.push(join_module(dir, &module.replace('.', "/"))?);
                }
            }
            for base in bases {
                if base.is_empty() {
                    choices.insert("__init__.py".to_owned());
                } else {
                    choices.insert(format!("{base}.py"));
                    choices.insert(format!("{base}/__init__.py"));
                }
            }
        }
        "javascript" | "typescript" | "tsx"
            if spec.starts_with("./") || spec.starts_with("../") =>
        {
            let base = join_module(parent, spec)?;
            choices.insert(base.clone());
            for suffix in ["js", "jsx", "ts", "tsx", "mjs", "cjs"] {
                choices.insert(format!("{base}.{suffix}"));
                choices.insert(format!("{base}/index.{suffix}"));
            }
        }
        "rust" => {
            let module_dir = if matches!(file, "lib.rs" | "main.rs" | "mod.rs") {
                parent.to_owned()
            } else {
                join_module(parent, file.strip_suffix(".rs")?)?
            };
            let bases = if import.rust_module {
                vec![join_module(&module_dir, spec)?]
            } else {
                let (head, tail) = spec.split_once("::")?;
                if !tail.split("::").all(|name| {
                    !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_')
                }) {
                    return None;
                }
                let root = match head {
                    "crate" => path
                        .rfind("/src/")
                        .map(|i| &path[..i + 4])
                        .or_else(|| path.starts_with("src/").then_some("src"))?,
                    "self" => &module_dir,
                    "super" => module_dir.rsplit_once('/').map(|(p, _)| p).unwrap_or(""),
                    _ => return None,
                };
                let module = tail.replace("::", "/");
                let mut bases = vec![join_module(root, &module)?];
                if let Some((prefix, _)) = module.rsplit_once('/') {
                    bases.push(join_module(root, prefix)?);
                }
                bases
            };
            for base in bases {
                choices.insert(format!("{base}.rs"));
                choices.insert(format!("{base}/mod.rs"));
            }
        }
        _ => return None,
    }
    let mut visible = choices
        .iter()
        .filter_map(|p| files.get_key_value(p.as_str()).map(|(&p, _)| p))
        .filter(|p| *p != path);
    let candidate = visible.next()?;
    visible.next().is_none().then_some(candidate)
}
