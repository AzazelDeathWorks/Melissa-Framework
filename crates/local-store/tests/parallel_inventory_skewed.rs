use melissa_local_store::{StoreKey, cas::Cas};
use std::fs;

#[test]
fn skewed_inventory_matches_serial_and_reauthenticates_every_size() {
    let directory = tempfile::tempdir().unwrap();
    let cas = Cas::open(directory.path(), StoreKey::from_bytes([187; 32]), 4 << 20).unwrap();
    let sizes = [0, 1, 31, 1024, 65536, 131072, 1 << 20, 4 << 20];
    for (index, size) in sizes.into_iter().enumerate() {
        cas.put(&vec![(index + 1) as u8; size]).unwrap();
    }
    let expected = cas.list().unwrap();
    assert_eq!(expected.len(), sizes.len());
    assert_eq!(
        expected.iter().map(|blob| blob.plain_bytes).sum::<u64>(),
        sizes.iter().map(|&size| size as u64).sum::<u64>()
    );
    assert_eq!(cas.list_with_parallelism(4).unwrap(), expected);
    for blob in &expected {
        let path = directory
            .path()
            .join("blobs")
            .join(format!("{}.blob", &blob.digest[7..]));
        let original = fs::read(&path).unwrap();
        let mut damaged = original.clone();
        *damaged.last_mut().unwrap() ^= 1;
        fs::write(&path, damaged).unwrap();
        assert!(cas.list_with_parallelism(4).is_err());
        fs::write(&path, original).unwrap();
    }
    assert_eq!(cas.list_with_parallelism(4).unwrap(), expected);
    assert_eq!(cas.list().unwrap(), expected);
}

#[test]
fn orphan_size_is_not_trusted_after_an_earlier_successful_inventory() {
    let directory = tempfile::tempdir().unwrap();
    let cas = Cas::open(directory.path(), StoreKey::from_bytes([188; 32]), 4096).unwrap();
    cas.put(b"retained valid artifact").unwrap();
    let expected = cas.list_with_parallelism(4).unwrap();
    let orphan = directory
        .path()
        .join("blobs")
        .join(format!("{}.blob", "0".repeat(64)));
    assert!(!orphan.exists());
    for invalid in [Vec::new(), vec![0_u8; 5000]] {
        fs::write(&orphan, invalid).unwrap();
        assert!(cas.list_with_parallelism(4).is_err());
        assert!(cas.list().is_err());
        fs::remove_file(&orphan).unwrap();
        assert_eq!(cas.list_with_parallelism(4).unwrap(), expected);
    }
}
