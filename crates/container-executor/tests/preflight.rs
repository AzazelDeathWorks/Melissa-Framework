use melissa_container_executor::{MAX_SNAPSHOT_BYTES, MAX_SNAPSHOT_FILES, Snapshot};
#[test]
fn metadata_preflight_rejects_transfer_limits_without_allocating_payloads() {
    assert!(Snapshot::preflight([("a", MAX_SNAPSHOT_BYTES)], MAX_SNAPSHOT_BYTES).is_ok());
    assert!(Snapshot::preflight([("a", MAX_SNAPSHOT_BYTES + 1)], u64::MAX).is_err());
    assert!(Snapshot::preflight([("a", 1), ("a/b", 0)], 10).is_err());
    assert!(Snapshot::preflight([("a", 0), ("a", 0)], 10).is_err());
    assert!(Snapshot::preflight([(".melissa-runtime/x", 0)], 10).is_err());
    let names = (0..=MAX_SNAPSHOT_FILES)
        .map(|i| format!("file{i}"))
        .collect::<Vec<_>>();
    assert!(
        Snapshot::preflight(
            names[..MAX_SNAPSHOT_FILES].iter().map(|n| (n.as_str(), 0)),
            1
        )
        .is_ok()
    );
    assert!(Snapshot::preflight(names.iter().map(|n| (n.as_str(), 0)), 1).is_err());
}
