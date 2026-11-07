//! User-restricted Windows named pipe. No HTTP listener or network port.
use super::*;
use std::{ffi::c_void, path::Path, ptr};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::windows::named_pipe::{ClientOptions, NamedPipeServer, ServerOptions},
};
use windows_sys::Win32::{
    Foundation::{CloseHandle, HANDLE, LocalFree},
    Security::{
        Authorization::{
            ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
        },
        GetTokenInformation, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER, TokenUser,
    },
    System::{
        Pipes::GetNamedPipeServerProcessId,
        Threading::{
            GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
        },
    },
};

const MAX_FRAME: usize = 64 * 1024 * 1024;
#[derive(Clone)]
pub struct Client {
    name: String,
}
impl Client {
    pub fn for_project(home: &Path, workspace: &Path) -> Result<Self> {
        let home = crate::app::absolute(home)?;
        let workspace = std::fs::canonicalize(workspace)?;
        let identity = format!(
            "{}|{}|{}",
            sid(unsafe { GetCurrentProcess() })?,
            home.to_string_lossy().to_lowercase(),
            workspace.to_string_lossy().to_lowercase()
        );
        Ok(Self {
            name: format!(
                r"\\.\pipe\melissa-ui-{}",
                blake3::hash(identity.as_bytes()).to_hex()
            ),
        })
    }
    pub async fn connect_or_start(home: &Path, workspace: &Path) -> Result<Self> {
        let client = Self::for_project(home, workspace)?;
        match client.probe().await {
            Ok(()) => return Ok(client),
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                return Err(Error::Unavailable(
                    "Yerel arayüz bağlantısı meşgul veya erişim reddedildi.",
                ));
            }
            Err(_) => {}
        }
        let server = match create(&client.name, true) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                // Another same-user client may have won the creation race.
                for _ in 0..20 {
                    if client.probe().await.is_ok() {
                        return Ok(client);
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(25)).await;
                }
                return Err(Error::Unavailable(
                    "Yerel görev sahibi bağlantısı kurulamadı.",
                ));
            }
            Err(_) => return Err(Error::Io),
        };
        let host = Host::start(home, workspace)?;
        let name = client.name.clone();
        tokio::spawn(async move {
            serve(name, server, host).await;
        });
        Ok(client)
    }
    async fn probe(&self) -> std::io::Result<()> {
        let pipe = ClientOptions::new().open(&self.name)?;
        authenticate(&pipe)?;
        Ok(())
    }
    pub async fn request(&self, request: Request) -> Result<Response> {
        let mut pipe = loop {
            match ClientOptions::new().open(&self.name) {
                Ok(p) => break p,
                Err(e) if e.raw_os_error() == Some(231) => {
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await
                }
                Err(_) => {
                    return Err(Error::Unavailable(
                        "Görev bağlantısı kesildi; aynı istek kimliğiyle yeniden bağlanın.",
                    ));
                }
            }
        };
        authenticate(&pipe)
            .map_err(|_| Error::Denied("Yerel görev sahibinin kullanıcı kimliği doğrulanamadı."))?;
        let bytes = serde_json::to_vec(&request).map_err(|_| Error::Store)?;
        if bytes.len() > MAX_FRAME {
            return Err(Error::Usage("Arayüz aktarımı çok büyük."));
        }
        pipe.write_u32_le(bytes.len() as u32).await?;
        pipe.write_all(&bytes).await?;
        let len = pipe.read_u32_le().await? as usize;
        if len > MAX_FRAME {
            return Err(Error::Denied("Arayüz yanıtı aktarım sınırını aşıyor."));
        }
        let mut bytes = vec![0; len];
        pipe.read_exact(&mut bytes).await?;
        serde_json::from_slice(&bytes).map_err(|_| Error::Store)
    }
}
fn authenticate(pipe: &tokio::net::windows::named_pipe::NamedPipeClient) -> std::io::Result<()> {
    use std::os::windows::io::AsRawHandle;
    unsafe {
        let mut pid = 0;
        if GetNamedPipeServerProcessId(pipe.as_raw_handle() as HANDLE, &mut pid) == 0 {
            return Err(std::io::Error::last_os_error());
        }
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if process.is_null() {
            return Err(std::io::Error::last_os_error());
        }
        let identity = sid(process);
        CloseHandle(process);
        match (identity, sid(GetCurrentProcess())) {
            (Ok(a), Ok(b)) if a == b => {}
            _ => return Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied)),
        }
    }
    Ok(())
}
fn sid(process: HANDLE) -> Result<String> {
    unsafe {
        let mut token = ptr::null_mut();
        if OpenProcessToken(process, TOKEN_QUERY, &mut token) == 0 {
            return Err(Error::Io);
        }
        let mut len = 0;
        GetTokenInformation(token, TokenUser, ptr::null_mut(), 0, &mut len);
        let mut buffer = vec![0u64; (len as usize).div_ceil(8)];
        let ok = GetTokenInformation(token, TokenUser, buffer.as_mut_ptr().cast(), len, &mut len);
        CloseHandle(token);
        if ok == 0 {
            return Err(Error::Io);
        }
        let user = &*buffer.as_ptr().cast::<TOKEN_USER>();
        let mut text = ptr::null_mut();
        if ConvertSidToStringSidW(user.User.Sid, &mut text) == 0 {
            return Err(Error::Io);
        }
        let mut n = 0;
        while *text.add(n) != 0 {
            n += 1;
        }
        let value = String::from_utf16_lossy(std::slice::from_raw_parts(text, n));
        LocalFree(text.cast());
        Ok(value)
    }
}
fn create(name: &str, first: bool) -> std::io::Result<NamedPipeServer> {
    let sid = sid(unsafe { GetCurrentProcess() })
        .map_err(|_| std::io::Error::from(std::io::ErrorKind::PermissionDenied))?;
    let sddl: Vec<u16> = format!("D:P(A;;GA;;;{sid})")
        .encode_utf16()
        .chain(Some(0))
        .collect();
    unsafe {
        let mut descriptor = ptr::null_mut();
        if ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            1,
            &mut descriptor,
            ptr::null_mut(),
        ) == 0
        {
            return Err(std::io::Error::last_os_error());
        }
        let mut attributes = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor,
            bInheritHandle: 0,
        };
        let result = ServerOptions::new()
            .first_pipe_instance(first)
            .reject_remote_clients(true)
            .create_with_security_attributes_raw(
                name,
                (&mut attributes as *mut SECURITY_ATTRIBUTES).cast::<c_void>(),
            );
        LocalFree(descriptor);
        result
    }
}
async fn serve(name: String, mut server: NamedPipeServer, host: Host) {
    loop {
        if server.connect().await.is_err() {
            break;
        }
        let next = match create(&name, false) {
            Ok(n) => n,
            Err(_) => break,
        };
        let mut connected = std::mem::replace(&mut server, next);
        let host = host.clone();
        tokio::spawn(async move {
            let operation = async {
                let len = connected.read_u32_le().await? as usize;
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
