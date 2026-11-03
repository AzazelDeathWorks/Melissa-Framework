//! Windows user-bound DPAPI wrapping; neither API keys nor plaintext store keys
//! are written to reports. Existing vault files are always preserved.
use crate::{Error, Result};
use melissa_local_store::StoreKey;
use std::{fs, path::Path};
use zeroize::Zeroizing;
#[cfg(windows)]
pub fn create_key(path: &Path) -> Result<StoreKey> {
    use windows_sys::Win32::{
        Foundation::LocalFree,
        Security::Cryptography::{CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData},
    };
    let mut bytes = Zeroizing::new([0u8; 32]);
    getrandom::fill(&mut *bytes).map_err(|_| Error::Denied("vault entropy"))?;
    let input = CRYPT_INTEGER_BLOB {
        cbData: 32,
        pbData: bytes.as_mut_ptr(),
    };
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };
    // The input and DPAPI output remain alive for their documented call scope.
    let ok = unsafe {
        CryptProtectData(
            &input,
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null_mut(),
            std::ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
    };
    if ok == 0 {
        return Err(Error::Denied("DPAPI key protection"));
    }
    let protected = unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize) };
    let result = (|| -> Result {
        let mut file = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(path)
            .map_err(|_| Error::Denied("preserve existing vault key"))?;
        std::io::Write::write_all(&mut file, protected)
            .map_err(|_| Error::Denied("vault key write"))?;
        file.sync_all().map_err(|_| Error::Denied("vault flush"))?;
        Ok(())
    })();
    unsafe {
        LocalFree(output.pbData.cast());
    }
    result?;
    Ok(StoreKey::from_bytes(*bytes))
}
#[cfg(windows)]
pub fn open_key(path: &Path) -> Result<StoreKey> {
    use windows_sys::Win32::{
        Foundation::LocalFree,
        Security::Cryptography::{
            CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptUnprotectData,
        },
    };
    let mut bytes = Zeroizing::new(fs::read(path).map_err(|_| Error::Denied("vault key read"))?);
    if bytes.len() > 65536 {
        return Err(Error::Invalid("vault envelope bounds"));
    }
    let input = CRYPT_INTEGER_BLOB {
        cbData: bytes.len() as u32,
        pbData: bytes.as_mut_ptr(),
    };
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };
    let ok = unsafe {
        CryptUnprotectData(
            &input,
            std::ptr::null_mut(),
            std::ptr::null(),
            std::ptr::null_mut(),
            std::ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
    };
    if ok == 0 {
        return Err(Error::Denied("DPAPI key recovery"));
    }
    let key = if output.cbData == 32 {
        let mut key = Zeroizing::new([0; 32]);
        key.copy_from_slice(unsafe { std::slice::from_raw_parts(output.pbData, 32) });
        Some(StoreKey::from_bytes(*key))
    } else {
        None
    };
    // DPAPI owns the allocation. Zero its plaintext before releasing it.
    unsafe {
        std::ptr::write_bytes(output.pbData, 0, output.cbData as usize);
        LocalFree(output.pbData.cast());
    }
    key.ok_or(Error::Invalid("vault key length"))
}
#[cfg(not(windows))]
pub fn create_key(_: &Path) -> Result<StoreKey> {
    Err(Error::Denied(
        "DPAPI vault requires Windows; supply host-managed StoreKey",
    ))
}
#[cfg(not(windows))]
pub fn open_key(_: &Path) -> Result<StoreKey> {
    Err(Error::Denied("DPAPI vault requires Windows"))
}
