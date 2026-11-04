//! Read-only NTFS change receipts. An unavailable or unknown journal never
//! substitutes timestamps for a content read.
use std::{fs::File, mem::size_of, os::windows::io::AsRawHandle};
use windows_sys::Win32::System::{
    IO::DeviceIoControl,
    Ioctl::{
        FSCTL_QUERY_USN_JOURNAL, FSCTL_READ_FILE_USN_DATA, READ_FILE_USN_DATA, USN_JOURNAL_DATA_V0,
        USN_JOURNAL_DATA_V2,
    },
};

pub(crate) struct FileJournal<'a> {
    root: &'a File,
    id: u64,
    lowest_valid: i64,
}

impl<'a> FileJournal<'a> {
    pub(crate) fn open(root: &'a File) -> Option<Self> {
        // The existing pinned directory supplies volume identity without a
        // raw volume handle or extra privileges.
        let data = Self::query(root)?;
        Some(Self {
            root,
            id: data.UsnJournalID,
            lowest_valid: data.LowestValidUsn,
        })
    }

    fn query(root: &File) -> Option<USN_JOURNAL_DATA_V2> {
        let mut data = USN_JOURNAL_DATA_V2::default();
        let mut returned = 0;
        let ok = unsafe {
            DeviceIoControl(
                root.as_raw_handle(),
                FSCTL_QUERY_USN_JOURNAL,
                std::ptr::null(),
                0,
                (&mut data as *mut USN_JOURNAL_DATA_V2).cast(),
                size_of::<USN_JOURNAL_DATA_V2>() as u32,
                &mut returned,
                std::ptr::null_mut(),
            )
        };
        (ok != 0 && returned as usize >= size_of::<USN_JOURNAL_DATA_V0>()).then_some(data)
    }

    pub(crate) fn stamp(&self, file: &File) -> Option<(u64, i64)> {
        let input = READ_FILE_USN_DATA {
            MinMajorVersion: 2,
            MaxMajorVersion: 3,
        };
        let mut record = [0u64; 128];
        let mut returned = 0;
        let ok = unsafe {
            DeviceIoControl(
                file.as_raw_handle(),
                FSCTL_READ_FILE_USN_DATA,
                (&input as *const READ_FILE_USN_DATA).cast(),
                size_of::<READ_FILE_USN_DATA>() as u32,
                record.as_mut_ptr().cast(),
                size_of::<[u64; 128]>() as u32,
                &mut returned,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 || returned < 8 {
            return None;
        }
        let header = record[0].to_le_bytes();
        let len = u32::from_le_bytes(header[..4].try_into().ok()?);
        let major = u16::from_le_bytes(header[4..6].try_into().ok()?);
        let offset = match major {
            2 => 24,
            3 => 40,
            _ => return None,
        };
        if len < offset + 8 || len > returned {
            return None;
        }
        let usn = i64::from_le_bytes(record[(offset / 8) as usize].to_le_bytes());
        (usn > 0 && usn >= self.lowest_valid).then_some((self.id, usn))
    }

    pub(crate) fn unchanged(&self) -> bool {
        Self::query(self.root).is_some_and(|data| {
            data.UsnJournalID == self.id && data.LowestValidUsn == self.lowest_valid
        })
    }
}
