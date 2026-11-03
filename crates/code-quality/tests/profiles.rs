#![cfg(windows)]
use melissa_code_quality::profiles::ReviewProfiles;
use melissa_repository::{Limits, Repository};
use std::{fs, path::Path};

#[test]
fn definitions_work_without_historical_evidence_but_reject_role_substitution_and_duplicate_json() {
    let directory = tempfile::tempdir().unwrap();
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let paths = [
        "code/reviewer.json",
        "code/security-reviewer.json",
        "core/independent-qa.json",
    ];
    for path in paths {
        let target = directory.path().join("catalog/agents").join(path);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::copy(root.join("catalog/agents").join(path), target).unwrap();
    }
    let repository = Repository::open(directory.path(), Limits::default(), vec![]).unwrap();
    let profiles = ReviewProfiles::load_from_repository(&repository).unwrap();
    assert!(profiles.profile("melissa.code.reviewer").is_some());
    assert!(profiles.profile("melissa.code.implementer").is_none());
    assert!(!directory.path().join("reports").exists());
    let target = directory.path().join("catalog/agents/code/reviewer.json");
    let original = fs::read_to_string(&target).unwrap();
    fs::write(
        &target,
        original.replace("melissa.code.reviewer", "melissa.code.security-reviewer"),
    )
    .unwrap();
    assert!(ReviewProfiles::load_from_repository(&repository).is_err());
    // The already captured host definitions cannot be changed through the file.
    assert_eq!(
        profiles.profile("melissa.code.reviewer").unwrap().id,
        "melissa.code.reviewer"
    );
    fs::write(
        &target,
        original.replacen('{', "{\"id\":\"melissa.code.reviewer\",", 1),
    )
    .unwrap();
    assert!(ReviewProfiles::load_from_repository(&repository).is_err());
}
