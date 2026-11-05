use melissa_repository::{FileVersion, graph::Graph, polyglot::parse_index_file};
use std::{collections::BTreeMap, sync::atomic::AtomicBool};

#[test]
fn supported_grammars_produce_source_ranges_calls_and_scoped_import_candidates() {
    let cases = [
        (
            "src/lib.rs",
            "mod calc; use crate::calc::choose; fn caller() { choose(); }",
            "src/calc.rs",
            "pub fn choose() {}",
        ),
        (
            "runner.py",
            "from calc import choose\ndef caller():\n    choose()\n",
            "calc.py",
            "def choose():\n    pass\n",
        ),
        (
            "runner.js",
            "import {choose} from './calc.js'; function caller() { choose(); }",
            "calc.js",
            "export function choose() {}",
        ),
        (
            "runner.ts",
            "import {choose} from './calc'; function caller(): void { choose(); }",
            "calc.ts",
            "export function choose(): void {}",
        ),
        (
            "runner.tsx",
            "import {choose} from './calc'; function caller() { choose(); return <div/>; }",
            "calc.tsx",
            "export function choose() {}",
        ),
    ];
    let cancel = AtomicBool::new(false);
    for (path, body, dependency, definition) in cases {
        let parse = |path: &str, body: &str| {
            let file = FileVersion {
                path: path.into(),
                identity: "fixture".into(),
                digest: "fixture".into(),
                bytes: body.len() as u64,
            };
            parse_index_file(&file, body.as_bytes(), &cancel, 10_000)
                .unwrap()
                .unwrap()
                .graph
                .unwrap()
        };
        let caller = parse(path, body);
        let callee = parse(dependency, definition);
        assert!(!caller.syntax_error && !callee.syntax_error, "{path}");
        let graph = Graph::new(BTreeMap::from([(path, &caller), (dependency, &callee)]));
        assert!(
            graph.imports[path]
                .iter()
                .any(|i| i.dependency == Some(dependency)),
            "{path}"
        );
        let call = caller
            .references
            .iter()
            .find(|r| r.call && r.name == "choose")
            .unwrap_or_else(|| panic!("{path}"));
        assert_eq!(&body[call.range.start_byte..call.range.end_byte], "choose");
        assert_eq!(caller.definitions[call.owner.unwrap()].name, "caller");
        let id = graph.target(path, call).unwrap();
        assert_eq!(id.path, dependency);
        let chosen = graph.definition(id);
        assert_eq!(
            &definition[chosen.name_range.start_byte..chosen.name_range.end_byte],
            "choose"
        );
        let narrower = Graph::new(BTreeMap::from([(path, &caller)]));
        assert!(narrower.target(path, call).is_none(), "{path}");
    }
}
