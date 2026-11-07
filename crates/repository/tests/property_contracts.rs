#![cfg(windows)]
mod support;
use melissa_repository::{
    PatchBroker,
    analysis::byte_position,
    unified::{ProposedChange, UnifiedLineEndings},
};
use proptest::prelude::*;
use std::{fs, sync::atomic::AtomicBool};
proptest! {
    #[test]
    fn utf16_positions_respect_unicode_scalar_boundaries(text in prop::collection::vec(any::<char>().prop_filter("not a line separator",|c| !matches!(*c,'\r'|'\n'|'\0')),0..50)) {
        let text:String=text.into_iter().collect();let bytes=format!("{text}\r\ntail\r\n").into_bytes();let mut column=0;
        for (offset,c) in text.char_indices(){prop_assert_eq!(byte_position(&bytes,0,column).unwrap(),offset);if c.len_utf16()==2 {prop_assert!(byte_position(&bytes,0,column+1).is_err());}column+=c.len_utf16() as u32;}
        prop_assert_eq!(byte_position(&bytes,0,column).unwrap(),text.len());
    }
    #[test]
    fn unicode_crlf_patches_preserve_bytes_and_stale_user_edits(text in "[a-zA-Z0-9çğşıöü😀]{1,24}") {
        let (_dir,repo,mut store,policy)=support::fixture(&["file.txt"]);let cancel=AtomicBool::new(false);
        let before=format!("old {text}\r\nkeep\r\n");fs::write(repo.root_path().join("file.txt"),before.as_bytes()).unwrap();let snapshot=repo.snapshot(&cancel).unwrap();
        let patch=format!("--- a/file.txt\n+++ b/file.txt\n@@ -1,2 +1,2 @@\n-old {text}\n+new {text}\n keep\n");
        let changes=repo.propose_unified_with_line_endings(&snapshot,patch.as_bytes(),UnifiedLineEndings::PreserveSource,&cancel).unwrap();
        let ProposedChange::Patch(proposal)=changes.changes.into_iter().next().unwrap() else {panic!("expected file patch")};let stale=proposal.clone();
        let mut broker=PatchBroker::new(&repo,&mut store,policy).unwrap();let permit=broker.authorize("prop:apply","prop:attempt",proposal).unwrap();broker.execute(permit,&cancel).unwrap();
        prop_assert_eq!(repo.read("file.txt").unwrap().1,format!("new {text}\r\nkeep\r\n").into_bytes());
        let user=format!("user {text}\r\nkeep\r\n");fs::write(repo.root_path().join("file.txt"),user.as_bytes()).unwrap();let permit=broker.authorize("prop:stale","prop:stale-attempt",stale).unwrap();prop_assert!(broker.execute(permit,&cancel).is_err());prop_assert_eq!(repo.read("file.txt").unwrap().1,user.into_bytes());
    }
}
