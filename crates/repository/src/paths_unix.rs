//! Unix backend: every component is opened relative to its held parent with
//! O_NOFOLLOW, so symlinked ancestors or targets are denied without pathname
//! prechecks. Unix cannot share-lock names; deletes and moves re-confirm the
//! held device/inode at the name and verify it again after renaming.
use super::{Pinned, relative};
use crate::{Error, Result};
use rustix::{
    fs::{self as rfs, AtFlags, FileType, Mode, OFlags},
    io::Errno,
};
use std::{
    ffi::{OsStr, OsString},
    fs::File,
    os::fd::{AsFd, BorrowedFd, OwnedFd},
    path::{Component, Path, PathBuf},
};

fn errno(e: Errno) -> Error {
    if e == Errno::LOOP || e == Errno::NOTDIR || e == Errno::MLINK {
        Error::Denied("reparse or wrong-kind file")
    } else {
        std::io::Error::from(e).into()
    }
}

fn absolute(path: &Path) -> Result {
    if !path.is_absolute() {
        return Err(Error::Invalid("absolute local path required"));
    }
    for c in path.components() {
        match c {
            Component::RootDir => (),
            Component::Normal(s) => {
                relative(s.to_str().ok_or(Error::Invalid("non-Unicode path"))?)?
            }
            _ => return Err(Error::Denied("device, network or traversal root")),
        }
    }
    Ok(())
}

fn name(path: &Path) -> Result<&OsStr> {
    path.file_name()
        .ok_or(Error::Invalid("filesystem root is not a workspace"))
}

fn open_dir(parent: BorrowedFd<'_>, name: &OsStr) -> Result<OwnedFd> {
    rfs::openat(
        parent,
        name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(errno)
}

/// Pins "/" and every directory of `dir`, returning the guards; the last
/// guard is `dir` itself.
fn walk(dir: &Path) -> Result<Vec<File>> {
    let root = rfs::open(
        "/",
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(errno)?;
    let mut guards = vec![File::from(root)];
    for c in dir.components() {
        if let Component::Normal(part) = c {
            let next = open_dir(guards.last().expect("root").as_fd(), part)?;
            guards.push(File::from(next));
        }
    }
    Ok(guards)
}

fn open_at(
    parent: BorrowedFd<'_>,
    name: &OsStr,
    dir: bool,
    write: bool,
    create: bool,
) -> Result<File> {
    let mut flags = OFlags::NOFOLLOW | OFlags::CLOEXEC;
    if dir {
        flags |= OFlags::RDONLY | OFlags::DIRECTORY;
    } else {
        // NONBLOCK keeps a FIFO from hanging the open; regular files ignore it.
        flags |= OFlags::NONBLOCK | if write { OFlags::RDWR } else { OFlags::RDONLY };
    }
    if create {
        flags |= OFlags::CREATE | OFlags::EXCL;
    }
    let fd = rfs::openat(parent, name, flags, Mode::from_raw_mode(0o644)).map_err(errno)?;
    Ok(File::from(fd))
}

fn same(a: &rfs::Stat, b: &rfs::Stat) -> bool {
    a.st_dev == b.st_dev && a.st_ino == b.st_ino
}

fn inspect(file: File, canonical: PathBuf, dir: bool) -> Result<(File, String, PathBuf)> {
    let st = rfs::fstat(&file).map_err(errno)?;
    let kind = FileType::from_raw_mode(st.st_mode);
    if (dir && kind != FileType::Directory) || (!dir && kind != FileType::RegularFile) {
        return Err(Error::Denied("reparse or wrong-kind file"));
    }
    if !dir && st.st_nlink != 1 {
        return Err(Error::Denied("hard-linked files are not supported"));
    }
    let identity = format!("{:016x}:{:016x}", st.st_dev as u64, st.st_ino as u64);
    Ok((file, identity, canonical))
}

fn parent_of(pin: &Pinned) -> Result<(&File, &OsStr)> {
    Ok((
        pin._ancestors
            .last()
            .ok_or(Error::Invalid("parent handle required"))?,
        name(&pin.canonical)?,
    ))
}

/// The name must still refer to the held object right before a name-based syscall.
fn confirm(pin: &Pinned) -> Result<(&File, &OsStr, rfs::Stat)> {
    let (parent, name) = parent_of(pin)?;
    let held = rfs::fstat(&pin.file).map_err(errno)?;
    let named = rfs::statat(parent, name, AtFlags::SYMLINK_NOFOLLOW).map_err(errno)?;
    if !same(&held, &named) {
        return Err(Error::Stale("held object no longer at its path"));
    }
    Ok((parent, name, held))
}

pub(super) fn pin_mode(path: &Path, dir: bool, write: bool, host_root: bool) -> Result<Pinned> {
    absolute(path)?;
    // Only the trusted host root may resolve symlinks in its own spelling
    // (e.g. macOS /tmp -> /private/tmp); the result is then walked strictly.
    let path = if host_root {
        let canonical = std::fs::canonicalize(path)?;
        absolute(&canonical)?;
        canonical
    } else {
        path.to_path_buf()
    };
    let leaf = name(&path)?;
    let mut guards = walk(
        path.parent()
            .ok_or(Error::Invalid("root is not a workspace"))?,
    )?;
    let file = open_at(
        guards.last().expect("parent").as_fd(),
        leaf,
        dir,
        write,
        false,
    )?;
    let (file, identity, canonical) = inspect(file, path.clone(), dir)?;
    guards.shrink_to_fit();
    Ok(Pinned {
        file,
        identity,
        canonical,
        _ancestors: guards,
    })
}

pub(super) fn read_child(parent: &Pinned, name: &str, dir: bool) -> Result<Pinned> {
    let file = open_at(parent.file.as_fd(), OsStr::new(name), dir, false, false)?;
    let (file, identity, canonical) = inspect(file, parent.canonical.join(name), dir)?;
    Ok(Pinned {
        file,
        identity,
        canonical,
        _ancestors: vec![parent.file.try_clone()?],
    })
}

pub(super) fn create_directory(parent: &Pinned, name: &str) -> Result<Pinned> {
    rfs::mkdirat(&parent.file, name, Mode::from_raw_mode(0o755)).map_err(errno)?;
    let file = File::from(open_dir(parent.file.as_fd(), OsStr::new(name))?);
    let (file, identity, canonical) = inspect(file, parent.canonical.join(name), true)?;
    Ok(Pinned {
        file,
        identity,
        canonical,
        _ancestors: vec![parent.file.try_clone()?],
    })
}

/// O_CREAT|O_EXCL is the final absence check: an existing name is never replaced.
pub(super) fn create_new(path: &Path) -> Result<Pinned> {
    absolute(path)?;
    let leaf = name(path)?;
    let guards = walk(
        path.parent()
            .ok_or(Error::Invalid("root is not a workspace"))?,
    )?;
    let file = open_at(
        guards.last().expect("parent").as_fd(),
        leaf,
        false,
        true,
        true,
    )?;
    let (file, identity, canonical) = inspect(file, path.to_path_buf(), false)?;
    Ok(Pinned {
        file,
        identity,
        canonical,
        _ancestors: guards,
    })
}

pub(super) fn move_no_replace(source: &Pinned, destination: &Path) -> Result {
    absolute(destination)?;
    let (src_parent, src_name, held) = confirm(source)?;
    let dst_name = name(destination)?;
    let guards = walk(
        destination
            .parent()
            .ok_or(Error::Invalid("root is not a destination"))?,
    )?;
    let dst_parent = guards.last().expect("parent");
    rename_no_replace(src_parent, src_name, dst_parent, dst_name, &held)?;
    let moved = rfs::statat(dst_parent, dst_name, AtFlags::SYMLINK_NOFOLLOW).map_err(errno)?;
    if !same(&held, &moved) {
        // A concurrent writer swapped the source name; put the stranger back.
        let _ = rename_no_replace(dst_parent, dst_name, src_parent, src_name, &moved);
        return Err(Error::Stale("moved object identity changed"));
    }
    Ok(())
}

fn rename_no_replace(
    src_parent: &File,
    src_name: &OsStr,
    dst_parent: &File,
    dst_name: &OsStr,
    held: &rfs::Stat,
) -> Result {
    #[cfg(any(target_os = "linux", target_os = "android", target_vendor = "apple"))]
    {
        match rfs::renameat_with(
            src_parent,
            src_name,
            dst_parent,
            dst_name,
            rfs::RenameFlags::NOREPLACE,
        ) {
            Ok(()) => return Ok(()),
            Err(e) if e != Errno::INVAL && e != Errno::NOSYS && e != Errno::NOTSUP => {
                return Err(errno(e));
            }
            Err(_) => {}
        }
    }
    // Filesystems without an atomic no-replace rename: link() never replaces.
    if FileType::from_raw_mode(held.st_mode) != FileType::RegularFile {
        return Err(Error::Unsupported(
            "atomic no-replace directory rename unavailable on this filesystem",
        ));
    }
    rfs::linkat(src_parent, src_name, dst_parent, dst_name, AtFlags::empty()).map_err(errno)?;
    rfs::unlinkat(src_parent, src_name, AtFlags::empty()).map_err(errno)
}

pub(super) fn delete(source: Pinned) -> Result {
    let (parent, name, held) = confirm(&source)?;
    let flags = if FileType::from_raw_mode(held.st_mode) == FileType::Directory {
        AtFlags::REMOVEDIR
    } else {
        AtFlags::empty()
    };
    // Unix has no handle-based unlink. Move the name aside under an
    // unguessable name and verify it is still the pinned object before removal.
    let mut nonce = [0u8; 16];
    getrandom::fill(&mut nonce).map_err(|_| Error::Denied("host entropy unavailable"))?;
    let aside: String = nonce.iter().map(|b| format!("{b:02x}")).collect();
    let aside = OsString::from(format!(".melissa-delete-{aside}"));
    match rename_no_replace(parent, name, parent, &aside, &held) {
        // Directory without no-replace rename: rmdir by the confirmed name.
        Err(Error::Unsupported(_)) if flags == AtFlags::REMOVEDIR => {
            rfs::unlinkat(parent, name, flags).map_err(errno)?;
            drop(source);
            return Ok(());
        }
        r => r?,
    }
    let moved = rfs::statat(parent, &aside, AtFlags::SYMLINK_NOFOLLOW).map_err(errno)?;
    if !same(&held, &moved) {
        let _ = rename_no_replace(parent, &aside, parent, name, &moved);
        return Err(Error::Stale("deleted object identity changed"));
    }
    if let Err(e) = rfs::unlinkat(parent, &aside, flags) {
        // E.g. a directory that gained an entry: restore the original name.
        let _ = rename_no_replace(parent, &aside, parent, name, &moved);
        return Err(errno(e));
    }
    drop(source);
    Ok(())
}
