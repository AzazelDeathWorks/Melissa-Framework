#![cfg(windows)]
use melissa_repository::{
    Limits, Repository,
    polyglot::{ParseCache, PolyglotIndex},
};
use std::{fs, sync::atomic::AtomicBool};
#[test]
fn parser_reuses_unchanged_files_but_reobserves_changes_deletions_and_stale_snapshots() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("a.rs"), "fn alpha() {}\n").unwrap();
    fs::write(dir.path().join("b.py"), "def beta():\n    pass\n").unwrap();
    let repo = Repository::open(dir.path(), Limits::default(), vec![]).unwrap();
    let cancel = AtomicBool::new(false);
    let mut cache = ParseCache::default();
    for _ in 0..2 {
        let snap = repo.snapshot(&cancel).unwrap();
        assert_eq!(
            PolyglotIndex::build_incremental(&repo, &snap, &cancel, &mut cache)
                .unwrap()
                .symbols
                .len(),
            2
        );
    }
    assert_eq!((cache.parsed_files, cache.reused_files), (2, 2));
    let old = repo.snapshot(&cancel).unwrap();
    fs::write(dir.path().join("a.rs"), "fn gamma() {}\n").unwrap();
    assert!(PolyglotIndex::build_incremental(&repo, &old, &cancel, &mut cache).is_err());
    let current = repo.snapshot(&cancel).unwrap();
    let next = PolyglotIndex::build_incremental(&repo, &current, &cancel, &mut cache).unwrap();
    assert_eq!(cache.parsed_files, 3);
    assert!(next.symbols.iter().any(|s| s.name == "gamma"));
    fs::remove_file(dir.path().join("b.py")).unwrap();
    let current = repo.snapshot(&cancel).unwrap();
    assert_eq!(
        PolyglotIndex::build_incremental(&repo, &current, &cancel, &mut cache)
            .unwrap()
            .symbols
            .len(),
        1
    );
}
