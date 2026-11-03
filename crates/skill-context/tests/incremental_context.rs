mod common;
use common::*;
use melissa_skill_context::{digest, retrieval::*, state::MemoryBook};

#[test]
fn memory_stores_provenance_and_reads_verified_legacy_books() {
    let s = source("src/a.rs", &"needle ü漢字\n".repeat(20000));
    let mut book = MemoryBook::default();
    book.propose("m", namespace(), "key", "claim", vec![s.clone()])
        .unwrap();
    let raw = serde_json::to_vec(&book).unwrap();
    assert!(raw.len() < 2048);
    let mut legacy = serde_json::to_value(&book).unwrap();
    legacy["records"]["m"]["sources"][0] = serde_json::to_value(&s).unwrap();
    let restored: MemoryBook = serde_json::from_value(legacy.clone()).unwrap();
    assert_eq!(
        serde_json::to_value(restored).unwrap(),
        serde_json::to_value(book).unwrap()
    );
    legacy["records"]["m"]["sources"][0]["content"] = serde_json::json!("tampered");
    assert!(serde_json::from_value::<MemoryBook>(legacy).is_err());
}

#[test]
fn warm_search_reuses_index_changes_only_modified_files_and_returns_bounded_ranges() {
    let mut sources = vec![
        source(
            "a",
            &format!(
                "{}needle\n{}",
                "ü漢字\n".repeat(2000),
                "suffix".repeat(2000)
            ),
        ),
        source("b", "needle other"),
    ];
    let mut index = SearchIndex::new().unwrap();
    let first = index
        .search(&sources, &scope(&sources), "needle", &[], 10)
        .unwrap();
    assert_eq!(index.updated_documents, 2);
    assert_eq!(first.len(), 2);
    for hit in &first {
        let original = sources.iter().find(|s| s.id == hit.source.id).unwrap();
        assert_eq!(hit.excerpt, original.content[hit.start_byte..hit.end_byte]);
        assert_eq!(hit.source.digest, digest(original.content.as_bytes()));
        assert_eq!(hit.excerpt_digest, digest(hit.excerpt.as_bytes()));
        assert!(hit.excerpt.len() <= 4096);
    }
    index
        .search(&sources, &scope(&sources), "other", &[], 10)
        .unwrap();
    assert_eq!(index.updated_documents, 2);
    sources[0] = source("a", "changed content");
    let hits = index
        .search(&sources, &scope(&sources), "needle", &[], 10)
        .unwrap();
    assert_eq!(index.updated_documents, 3);
    assert_eq!(hits.len(), 1);
    sources.pop();
    assert!(
        index
            .search(&sources, &scope(&sources), "needle", &[], 10)
            .unwrap()
            .is_empty()
    );
    let mut restricted = scope(&sources);
    restricted.namespace.project = "other-project".into();
    assert!(
        index
            .search(&sources, &restricted, "changed", &[], 10)
            .unwrap()
            .is_empty()
    );
}
