//! Owner-only Unix domain socket. No HTTP listener or network port. Both
//! ends verify the peer's kernel-reported uid before exchanging a frame.
use super::*;
use std::{
    os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{UnixListener, UnixStream},
};

const MAX_FRAME: usize = 64 * 1024 * 1024;
#[derive(Clone)]
pub struct Client {
    path: PathBuf,
}

fn uid() -> u32 {
    rustix::process::geteuid().as_raw()
}

/// Short, per-user directory: socket paths are limited to ~104 bytes on macOS.
fn socket_dir() -> Result<PathBuf> {
    let dir = match std::env::var_os("XDG_RUNTIME_DIR").filter(|d| Path::new(d).is_absolute()) {
        Some(runtime) => PathBuf::from(runtime).join("melissa"),
        None => PathBuf::from(format!("/tmp/melissa-{}", uid())),
    };
    match std::fs::DirBuilder::new().mode(0o700).create(&dir) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(_) => return Err(Error::Io),
    }
    let meta = std::fs::symlink_metadata(&dir).map_err(|_| Error::Io)?;
    if !meta.is_dir() || meta.uid() != uid() || meta.permissions().mode() & 0o077 != 0 {
        return Err(Error::Denied(
            "Yerel arayüz klasörü yalnızca bu kullanıcıya ait olmalı.",
        ));
    }
    Ok(dir)
}

fn authenticate(stream: &UnixStream) -> std::io::Result<()> {
    if stream.peer_cred()?.uid() != uid() {
        return Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied));
    }
    Ok(())
}

impl Client {
    pub fn for_project(home: &Path, workspace: &Path) -> Result<Self> {
        let home = crate::app::absolute(home)?;
        let workspace = std::fs::canonicalize(workspace)?;
        let identity = format!(
            "{}|{}|{}",
            uid(),
            home.to_string_lossy(),
            workspace.to_string_lossy()
        );
        let hash = blake3::hash(identity.as_bytes()).to_hex();
        Ok(Self {
            path: socket_dir()?.join(format!("ui-{}.sock", &hash[..32])),
        })
    }
    pub async fn connect_or_start(home: &Path, workspace: &Path) -> Result<Self> {
        let client = Self::for_project(home, workspace)?;
        // One creator at a time; a stale socket is removed only under the lock.
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(client.path.with_extension("lock"))?;
        rustix::fs::flock(&lock, rustix::fs::FlockOperation::LockExclusive)
            .map_err(|_| Error::Io)?;
        match client.probe().await {
            Ok(()) => return Ok(client),
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                return Err(Error::Unavailable(
                    "Yerel arayüz bağlantısı meşgul veya erişim reddedildi.",
                ));
            }
            Err(_) => {}
        }
        if let Ok(meta) = std::fs::symlink_metadata(&client.path) {
            use std::os::unix::fs::FileTypeExt;
            if !meta.file_type().is_socket() || meta.uid() != uid() {
                return Err(Error::Denied(
                    "Yerel arayüz soket yolu beklenmeyen bir dosya.",
                ));
            }
            std::fs::remove_file(&client.path)?;
        }
        let listener = UnixListener::bind(&client.path).map_err(|_| Error::Io)?;
        std::fs::set_permissions(&client.path, std::fs::Permissions::from_mode(0o600))?;
        drop(lock);
        let host = Host::start(home, workspace)?;
        tokio::spawn(serve(listener, host));
        Ok(client)
    }
    async fn probe(&self) -> std::io::Result<()> {
        let stream = UnixStream::connect(&self.path).await?;
        authenticate(&stream)
    }
    pub async fn request(&self, request: Request) -> Result<Response> {
        let mut stream = UnixStream::connect(&self.path).await.map_err(|_| {
            Error::Unavailable("Görev bağlantısı kesildi; aynı istek kimliğiyle yeniden bağlanın.")
        })?;
        authenticate(&stream)
            .map_err(|_| Error::Denied("Yerel görev sahibinin kullanıcı kimliği doğrulanamadı."))?;
        let bytes = serde_json::to_vec(&request).map_err(|_| Error::Store)?;
        if bytes.len() > MAX_FRAME {
            return Err(Error::Usage("Arayüz aktarımı çok büyük."));
        }
        stream.write_u32_le(bytes.len() as u32).await?;
        stream.write_all(&bytes).await?;
        let len = stream.read_u32_le().await? as usize;
        if len > MAX_FRAME {
            return Err(Error::Denied("Arayüz yanıtı aktarım sınırını aşıyor."));
        }
        let mut bytes = vec![0; len];
        stream.read_exact(&mut bytes).await?;
        serde_json::from_slice(&bytes).map_err(|_| Error::Store)
    }
}

async fn serve(listener: UnixListener, host: Host) {
    loop {
        let Ok((mut connected, _)) = listener.accept().await else {
            break;
        };
        if authenticate(&connected).is_err() {
            continue;
        }
        let host = host.clone();
        tokio::spawn(async move {
            let operation = async {
                let len = match connected.read_u32_le().await {
                    Ok(len) => len as usize,
                    // A liveness probe connects and closes without a frame.
                    Err(_) => return Ok(()),
                };
                if len > MAX_FRAME {
                    return Err(Error::Denied("Arayüz isteği aktarım sınırını aşıyor."));
                }
                let mut bytes = vec![0; len];
                connected.read_exact(&mut bytes).await?;
                let request: Request = serde_json::from_slice(&bytes)
                    .map_err(|_| Error::Usage("Arayüz isteği JSON değil."))?;
                let response = host.request(request).await?;
                let bytes = serde_json::to_vec(&response).map_err(|_| Error::Store)?;
                if bytes.len() > MAX_FRAME {
                    return Err(Error::Unavailable(
                        "Görünüm tek aktarım için çok büyük; görev kaydı korunuyor.",
                    ));
                }
                connected.write_u32_le(bytes.len() as u32).await?;
                connected.write_all(&bytes).await?;
                connected.flush().await?;
                Ok::<_, Error>(())
            };
            let _ = tokio::time::timeout(std::time::Duration::from_secs(120), operation).await;
        });
    }
}
