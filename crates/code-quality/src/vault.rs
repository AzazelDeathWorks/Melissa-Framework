//! Windows user-bound DPAPI wrapping (owner-only key file on Unix); neither API keys nor plaintext store keys
//! are written to reports. Existing vault files are always preserved.
use crate::{Error, Result};
use melissa_local_store::StoreKey;
use std::{fs, path::Path};
use zeroize::Zeroizing;
#[cfg(windows)]
pub fn create_key(path: &Path) -> Result<StoreKey> {
    let mut bytes = Zeroizing::new([0u8; 32]);
    getrandom::fill(&mut *bytes).map_err(|_| Error::Denied("vault entropy"))?;
    import_key(path, &bytes)
}
/// Trusted-host restore, after archive authentication. Destination must be new.
#[cfg(windows)]
pub fn import_key(path: &Path, secret: &[u8; 32]) -> Result<StoreKey> {
    use windows_sys::Win32::{
        Foundation::LocalFree,
        Security::Cryptography::{CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData},
    };
    let mut bytes = Zeroizing::new(*secret);
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
/// Unix: a 0600 owner-only key file; the user account boundary replaces DPAPI.
#[cfg(unix)]
pub fn create_key(path: &Path) -> Result<StoreKey> {
    let mut bytes = Zeroizing::new([0u8; 32]);
    getrandom::fill(&mut *bytes).map_err(|_| Error::Denied("vault entropy"))?;
    import_key(path, &bytes)
}
#[cfg(unix)]
pub fn import_key(path: &Path, secret: &[u8; 32]) -> Result<StoreKey> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(path)
        .map_err(|_| Error::Denied("preserve existing vault key"))?;
    std::io::Write::write_all(&mut file, secret).map_err(|_| Error::Denied("vault key write"))?;
    file.sync_all().map_err(|_| Error::Denied("vault flush"))?;
    Ok(StoreKey::from_bytes(*secret))
}
#[cfg(unix)]
pub fn open_key(path: &Path) -> Result<StoreKey> {
    use std::{io::Read, os::unix::fs::MetadataExt};
    let link = fs::symlink_metadata(path).map_err(|_| Error::Denied("vault key read"))?;
    let mut file = fs::File::open(path).map_err(|_| Error::Denied("vault key read"))?;
    let meta = file
        .metadata()
        .map_err(|_| Error::Denied("vault key read"))?;
    if !link.file_type().is_file()
        || link.dev() != meta.dev()
        || link.ino() != meta.ino()
        || meta.nlink() != 1
        || meta.uid() != rustix::process::geteuid().as_raw()
        || meta.mode() & 0o077 != 0
    {
        return Err(Error::Denied(
            "vault key must be an owner-only (0600) regular file",
        ));
    }
    if meta.len() != 32 {
        return Err(Error::Invalid("vault key length"));
    }
    let mut key = Zeroizing::new([0u8; 32]);
    file.read_exact(&mut *key)
        .map_err(|_| Error::Denied("vault key read"))?;
    Ok(StoreKey::from_bytes(*key))
}
