//! Windows-only authenticated inventory held immutable by sharing modes.
//! A cached size is used only while a live handle denies writes/deletion and
//! a newly opened path has the same volume/file identity. No mtime trust.
use crate::{
    StoreError, StoreResult,
    cas::{BlobInfo, Cas},
};
use std::{
    collections::BTreeMap,
    fs::{File, OpenOptions},
    os::windows::{fs::OpenOptionsExt, io::AsRawHandle},
};
use windows_sys::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, FILE_FLAG_OPEN_REPARSE_POINT, FILE_ID_INFO, FILE_SHARE_READ,
    FileIdInfo, GetFileInformationByHandle, GetFileInformationByHandleEx,
};

#[derive(Debug, Clone, Copy, Default, serde::Serialize)]
pub struct InventoryMetrics {
    pub authenticated_blobs: u64,
    pub reused_blobs: u64,
}
#[derive(Default)]
pub(crate) struct PinnedInventory {
    files: BTreeMap<String, Pinned>,
    pub metrics: InventoryMetrics,
}
struct Pinned {
    _file: File,
    identity: (u64, [u8; 16]),
    info: BlobInfo,
}
fn identity(file: &File) -> StoreResult<(u64, [u8; 16])> {
    let mut info = unsafe { std::mem::zeroed::<BY_HANDLE_FILE_INFORMATION>() };
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } == 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    if info.dwFileAttributes & 0x400 != 0 || info.nNumberOfLinks != 1 {
        return Err(StoreError::Integrity(
            "linked pinned inventory entry".into(),
        ));
    }
    // ReFS does not guarantee uniqueness of the older 64-bit file index.
    let mut id = unsafe { std::mem::zeroed::<FILE_ID_INFO>() };
    if unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            FileIdInfo,
            (&mut id as *mut FILE_ID_INFO).cast(),
            std::mem::size_of::<FILE_ID_INFO>() as u32,
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error().into());
    }
    if id.FileId.Identifier == [0; 16] || id.FileId.Identifier == [255; 16] {
        return Err(StoreError::Integrity(
            "filesystem did not provide a stable file identity".into(),
        ));
    }
    Ok((id.VolumeSerialNumber, id.FileId.Identifier))
}
impl PinnedInventory {
    pub fn clear(&mut self) {
        self.files.clear();
    }
    pub fn list(&mut self, cas: &Cas) -> StoreResult<Vec<BlobInfo>> {
        let paths = cas.inventory_paths()?;
        if self.files.keys().any(|d| !paths.contains_key(d)) {
            return Err(StoreError::Integrity(
                "pinned CAS inventory disappeared".into(),
            ));
        }
        let mut result = Vec::with_capacity(paths.len());
        for (digest, path) in paths {
            let file = OpenOptions::new()
                .read(true)
                .share_mode(FILE_SHARE_READ)
                .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
                .open(path)?;
            let id = identity(&file)?;
            if let Some(pinned) = self.files.get(&digest) {
                if pinned.identity != id {
                    return Err(StoreError::Integrity(
                        "pinned CAS object was replaced".into(),
                    ));
                }
                self.metrics.reused_blobs += 1;
                result.push(pinned.info.clone());
            } else {
                let info = cas.verify_file(&digest, &file)?;
                self.metrics.authenticated_blobs += 1;
                result.push(info.clone());
                // Bound open handles. Larger inventories retain full validation
                // for overflow objects instead of silently trusting their metadata.
                if self.files.len() < 4096 {
                    self.files.insert(
                        digest,
                        Pinned {
                            _file: file,
                            identity: id,
                            info,
                        },
                    );
                }
            }
        }
        Ok(result)
    }
}
