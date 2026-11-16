//! Unix Docker client discovery. Executable and socket come from fixed
//! installation locations or the host user's environment, never model fields.
use crate::{ExecutorError, ExecutorResult};
use std::{
    fs::File,
    io::Read,
    os::{fd::AsFd, unix::fs::MetadataExt},
    path::{Component, Path, PathBuf},
};

fn fail(message: &str) -> ExecutorError {
    ExecutorError::Unsupported(message.into())
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
        let next = openat(ancestors.last().expect("root").as_fd(), part, dir, Mode::empty())
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
    if !meta.is_file() || (single_link && meta.nlink() != 1) || meta.len() > 256 * 1024 * 1024 {
        return Err(fail(
            "Docker client must be a bounded, unlinked regular installation file",
        ));
    }
    let mut hash = blake3::Hasher::new();
    let mut bytes = [0u8; 65536];
    loop {
        let count = file.read(&mut bytes)?;
        if count == 0 {
            break;
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
