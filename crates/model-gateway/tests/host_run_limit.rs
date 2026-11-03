use melissa_core_contracts::catalog::Catalog;
use melissa_local_store::{LocalStore, StoreConfig, StoreKey};
use melissa_model_gateway::{
    DecoderLimits, ErrorKind,
    agent_loop::{Gateway, GatewayPolicy, HostRunLimit},
};
use std::{collections::BTreeSet, path::Path};

#[test]
fn long_runs_require_an_explicit_host_grant_and_default_bounds_remain() {
    let dir = tempfile::tempdir().unwrap();
    let open = || {
        LocalStore::open(
            &dir.path().join("store"),
            StoreKey::from_bytes([181; 32]),
            StoreConfig::default(),
        )
        .unwrap()
    };
    let catalog =
        || Catalog::load_definitions(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")).unwrap();
    let policy = |wall| GatewayPolicy {
        allowed_registrations: BTreeSet::from(["owned-local-test".into()]),
        max_requests: 1,
        max_turns: 1,
        max_tools: 1,
        max_wall_time_ms: wall,
        expires_at_unix_ms: u64::MAX,
        allow_fallback: false,
        max_backoff_ms: 0,
        decoder_limits: DecoderLimits::default(),
    };
    let rejected = Gateway::new(open(), catalog(), vec![], policy(5_400_000), true);
    assert!(matches!(rejected,Err(e) if e.kind==ErrorKind::Invalid));
    assert!(HostRunLimit::new(0).is_err());
    assert!(HostRunLimit::new(86_400_001).is_ok());
    assert!(HostRunLimit::new(u64::MAX).is_err());
    let _configured = Gateway::new(open(), catalog(), vec![], policy(1_800_000), true)
        .unwrap()
        .with_host_run_limit(HostRunLimit::new(5_400_000).unwrap());
    // No task, model, credential, wire request or permission is created by this grant.
}
