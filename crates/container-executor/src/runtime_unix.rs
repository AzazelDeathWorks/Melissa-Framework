//! Unix Docker client discovery. Executable and socket come from fixed
//! installation locations or the host user's environment, never model fields.
use crate::{ExecutorError, ExecutorResult};
use std::{
    fs::{File, OpenOptions, Permissions},
    io::{Read, Seek},
    os::{
        fd::AsFd,
        unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    },
    path::{Component, Path, PathBuf},
};

fn fail(message: &str) -> ExecutorError {
    ExecutorError::Unsupported(message.into())
}

const MAX_CLIENT_BYTES: u64 = 256 * 1024 * 1024;

pub(super) fn private_config() -> ExecutorResult<tempfile::TempDir> {
    Ok(tempfile::Builder::new()
        .permissions(Permissions::from_mode(0o700))
        .tempdir()?)
}

const CLIENTS: &[&str] = &[
    "/usr/bin/docker",
    "/usr/local/bin/docker",
    "/opt/homebrew/bin/docker",
    "/Applications/Docker.app/Contents/Resources/bin/docker",
    "/snap/bin/docker",
];

pub(super) const COMPOSE_PLUGINS: &[&str] = &[
    "/usr/libexec/docker/cli-plugins/docker-compose",
    "/usr/lib/docker/cli-plugins/docker-compose",
    "/usr/local/lib/docker/cli-plugins/docker-compose",
    "/usr/local/libexec/docker/cli-plugins/docker-compose",
    "/opt/homebrew/lib/docker/cli-plugins/docker-compose",
    "/Applications/Docker.app/Contents/Resources/cli-plugins/docker-compose",
];

/// `MELISSA_DOCKER` (absolute) overrides the fixed installation list.
pub(super) fn client() -> ExecutorResult<PathBuf> {
    if let Some(path) = std::env::var_os("MELISSA_DOCKER") {
        let path = PathBuf::from(path);
        if !path.is_absolute() {
            return Err(fail("MELISSA_DOCKER must be an absolute path"));
        }
        return Ok(path);
    }
    CLIENTS
        .iter()
        .map(PathBuf::from)
        .find(|p| p.is_file())
        .ok_or_else(|| fail("Docker CLI not found; install Docker or set MELISSA_DOCKER"))
}

pub(super) fn compose_plugin() -> ExecutorResult<PathBuf> {
    let home = std::env::var_os("HOME")
        .map(|h| PathBuf::from(h).join(".docker/cli-plugins/docker-compose"));
    COMPOSE_PLUGINS
        .iter()
        .map(PathBuf::from)
        .chain(home)
        .find(|p| p.is_file())
        .ok_or_else(|| fail("docker-compose CLI plugin not found"))
}

/// DOCKER_HOST (unix:// only), then Docker, Docker Desktop and Podman sockets.
pub(super) fn endpoint() -> ExecutorResult<String> {
    if let Some(host) = std::env::var_os("DOCKER_HOST") {
        let host = host
            .into_string()
            .map_err(|_| fail("DOCKER_HOST encoding"))?;
        if host.starts_with("unix:///") {
            return Ok(host);
        }
        return Err(fail("only local unix:// Docker endpoints are supported"));
    }
    let mut sockets = vec![PathBuf::from("/var/run/docker.sock")];
    if let Some(home) = std::env::var_os("HOME") {
        sockets.push(PathBuf::from(&home).join(".docker/run/docker.sock"));
        sockets.push(PathBuf::from(&home).join(".colima/default/docker.sock"));
        sockets.push(PathBuf::from(&home).join(".orbstack/run/docker.sock"));
    }
    if let Some(runtime) = std::env::var_os("XDG_RUNTIME_DIR") {
        sockets.push(PathBuf::from(&runtime).join("docker.sock"));
        sockets.push(PathBuf::from(&runtime).join("podman/podman.sock"));
    }
    sockets.push(PathBuf::from("/run/podman/podman.sock"));
    use std::os::unix::fs::FileTypeExt;
    sockets
        .into_iter()
        .find(|p| std::fs::metadata(p).is_ok_and(|m| m.file_type().is_socket()))
        .and_then(|p| p.to_str().map(|p| format!("unix://{p}")))
        .ok_or_else(|| fail("no local Docker/Podman socket; start the engine or set DOCKER_HOST"))
}

/// Resolve the trusted spelling once, then hold every canonical component
/// opened with O_NOFOLLOW relative to its parent while hashing the client.
pub(crate) fn pin_client_mode(
    executable: &Path,
    single_link: bool,
) -> ExecutorResult<(File, Vec<File>, String)> {
    use rustix::fs::{Mode, OFlags, open, openat};
    let canonical = std::fs::canonicalize(executable)?;
    let dir = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let mut ancestors = vec![File::from(
        open("/", dir, Mode::empty()).map_err(std::io::Error::from)?,
    )];
    let parts: Vec<_> = canonical
        .components()
        .filter_map(|c| match c {
            Component::Normal(n) => Some(n.to_owned()),
            _ => None,
        })
        .collect();
    let (leaf, parents) = parts
        .split_last()
        .ok_or_else(|| fail("Docker client path"))?;
    for part in parents {
        let next = openat(
            ancestors.last().expect("root").as_fd(),
            part,
            dir,
            Mode::empty(),
        )
        .map_err(|_| fail("Docker client path has a linked or non-directory ancestor"))?;
        ancestors.push(File::from(next));
    }
    let mut file = File::from(
        openat(
            ancestors.last().expect("parent").as_fd(),
            leaf,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
            Mode::empty(),
        )
        .map_err(|_| fail("Docker client must be a regular installation file"))?,
    );
    let meta = file.metadata()?;
    if !meta.is_file() || (single_link && meta.nlink() != 1) || meta.len() > MAX_CLIENT_BYTES {
        return Err(fail(
            "Docker client must be a bounded, unlinked regular installation file",
        ));
    }
    let mut hash = blake3::Hasher::new();
    let mut bytes = [0u8; 65536];
    let mut bounded = (&mut file).take(MAX_CLIENT_BYTES + 1);
    let mut total = 0;
    loop {
        let count = bounded.read(&mut bytes)?;
        if count == 0 {
            break;
        }
        total += count as u64;
        if total > MAX_CLIENT_BYTES {
            return Err(fail("Docker client grew beyond the size limit"));
        }
        hash.update(&bytes[..count]);
    }
    let identity = format!(
        "unix-file:{:016x}:{:016x}:blake3:{}",
        meta.dev(),
        meta.ino(),
        hash.finalize().to_hex()
    );
    Ok((file, ancestors, identity))
}

/// Destination must be inside the host's private config, never a task mount.
pub(super) fn stage_client(
    mut source: File,
    destination: &Path,
    source_identity: &str,
) -> ExecutorResult<(File, Vec<File>, String)> {
    if source.metadata()?.mode() & 0o111 == 0 {
        return Err(fail("Docker client is not executable"));
    }
    source.rewind()?;
    let mut output = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(destination)?;
    let copied = std::io::copy(&mut source.take(MAX_CLIENT_BYTES + 1), &mut output)?;
    if copied > MAX_CLIENT_BYTES {
        return Err(fail("Docker client copy exceeds the size limit"));
    }
    output.set_permissions(Permissions::from_mode(0o500))?;
    drop(output);
    let (file, ancestors, identity) = pin_client_mode(destination, true)?;
    if identity.rsplit(':').next() != source_identity.rsplit(':').next() {
        return Err(fail(
            "private client differs from pinned installation bytes",
        ));
    }
    Ok((file, ancestors, identity))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, os::unix::fs::symlink, process::Command};

    fn client(directory: &Path) -> PathBuf {
        let source = directory.join("client");
        fs::write(&source, b"#!/bin/sh\nprintf original").unwrap();
        fs::set_permissions(&source, Permissions::from_mode(0o700)).unwrap();
        source
    }

    #[test]
    fn staged_client_uses_pinned_bytes_after_source_path_replacement() {
        let installed = tempfile::tempdir().unwrap();
        let config = private_config().unwrap();
        let source = client(installed.path());
        let (file, _guards, identity) = pin_client_mode(&source, true).unwrap();
        fs::rename(&source, installed.path().join("previous")).unwrap();
        fs::write(&source, b"#!/bin/sh\nprintf replacement").unwrap();
        let copy = config.path().join("docker");
        let (_pin, _ancestors, private_identity) = stage_client(file, &copy, &identity).unwrap();
        assert_eq!(
            identity.rsplit(':').next(),
            private_identity.rsplit(':').next()
        );
        assert_eq!(Command::new(&copy).output().unwrap().stdout, b"original");
        assert_eq!(fs::metadata(&copy).unwrap().mode() & 0o777, 0o500);
        assert_eq!(fs::metadata(config.path()).unwrap().mode() & 0o777, 0o700);
    }

    #[test]
    fn staged_client_rejects_in_place_changes_between_hash_and_copy() {
        let installed = tempfile::tempdir().unwrap();
        let config = private_config().unwrap();
        let source = client(installed.path());
        let (file, _guards, identity) = pin_client_mode(&source, true).unwrap();
        fs::write(&source, b"#!/bin/sh\nprintf replacement").unwrap();
        assert!(stage_client(file, &config.path().join("docker"), &identity).is_err());
    }

    #[test]
    fn staging_never_follows_or_overwrites_an_existing_destination() {
        let installed = tempfile::tempdir().unwrap();
        let config = private_config().unwrap();
        let source = client(installed.path());
        let target = installed.path().join("untouched");
        fs::write(&target, b"untouched").unwrap();
        let copy = config.path().join("docker");
        symlink(&target, &copy).unwrap();
        let (file, _guards, identity) = pin_client_mode(&source, true).unwrap();
        assert!(stage_client(file, &copy, &identity).is_err());
        assert_eq!(fs::read(&target).unwrap(), b"untouched");
        assert!(copy.is_symlink());
    }

    #[test]
    fn oversized_and_non_executable_clients_are_rejected() {
        let directory = tempfile::tempdir().unwrap();
        let source = client(directory.path());
        OpenOptions::new()
            .write(true)
            .open(&source)
            .unwrap()
            .set_len(MAX_CLIENT_BYTES + 1)
            .unwrap();
        assert!(pin_client_mode(&source, true).is_err());
        fs::write(&source, b"not executable").unwrap();
        fs::set_permissions(&source, Permissions::from_mode(0o600)).unwrap();
        let (file, _guards, identity) = pin_client_mode(&source, true).unwrap();
        assert!(stage_client(file, &directory.path().join("docker"), &identity).is_err());
    }
}
