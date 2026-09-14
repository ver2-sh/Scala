//! Consumer of Wayfinder's public local service contract, not its internals.
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[cfg(target_os = "linux")]
use tokio::net::UnixStream as ApplicationStream;
// Unsupported platforms retain local serving; no local transport is opened.
#[cfg(not(any(target_os = "linux", windows)))]
use tokio::io::DuplexStream as ApplicationStream;
#[cfg(windows)]
use tokio::net::windows::named_pipe::NamedPipeClient as ApplicationStream;

#[cfg(windows)]
mod windows;

pub type Result<T> = std::result::Result<T, String>;
pub const HEADER_LIMIT: usize = 16384;

#[derive(Clone, Deserialize)]
pub struct Node {
    pub id: String,
    pub name: String,
    pub local: bool,
    pub reachable: bool,
}
#[derive(Deserialize)]
pub struct Status {
    pub nodes: Vec<Node>,
    pub conflict: bool,
}

#[cfg(not(windows))]
pub fn socket_path() -> Result<PathBuf> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        // A normal login has XDG_RUNTIME_DIR even when Wayfinder is a system
        // service. Only select a personal/development endpoint if provisioned.
        .filter(|runtime| runtime.join("wayfinder").is_dir())
        .unwrap_or_else(|| PathBuf::from("/run"));
    if !runtime.is_absolute() {
        return Err("Runtime directory must be absolute".into());
    }
    Ok(runtime.join("wayfinder/app.sock"))
}
#[cfg(windows)]
pub fn socket_path() -> Result<PathBuf> {
    Ok(PathBuf::from(r"\\.\pipe\wayfinder-app-v1"))
}
#[cfg(windows)]
async fn connect(path: &Path) -> Result<ApplicationStream> {
    use tokio::net::windows::named_pipe::ClientOptions;
    // Retry only pipe admission, never a dispatched application operation.
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            match ClientOptions::new().open(path) {
                Ok(stream) => {
                    windows::verify_owner(&stream)?;
                    return Ok(stream);
                }
                Err(error) if error.raw_os_error() == Some(231) => {
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
                Err(error) => {
                    return Err(format!(
                        "Wayfinder: local application pipe unavailable: {error}"
                    ));
                }
            }
        }
    })
    .await
    .map_err(|_| "Wayfinder application pipe busy".to_owned())?
}
#[cfg(target_os = "linux")]
async fn connect(path: &Path) -> Result<ApplicationStream> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    // The protected directory is the trust anchor, not the application's UID.
    // Authorized applications may connect but cannot replace the daemon socket.
    let dir = path.parent().ok_or("Missing application directory")?;
    let runtime = dir.parent().ok_or("Missing runtime parent")?;
    let parent = std::fs::symlink_metadata(runtime).map_err(|e| e.to_string())?;
    let directory = std::fs::symlink_metadata(dir).map_err(|e| e.to_string())?;
    let socket = std::fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if !parent.is_dir()
        || (parent.uid() != 0 && parent.uid() != directory.uid())
        || parent.mode() & 0o022 != 0
        || !directory.is_dir()
        || directory.mode() & 0o027 != 0
        || !socket.file_type().is_socket()
        || socket.uid() != directory.uid()
        || socket.mode() & 0o007 != 0
    {
        return Err("Untrusted Wayfinder application endpoint permissions".into());
    }
    let stream = ApplicationStream::connect(path)
        .await
        .map_err(|_| "Wayfinder: not detected or local application access denied".to_owned())?;
    if stream.peer_cred().map_err(|e| e.to_string())?.uid() != directory.uid() {
        return Err("Wayfinder peer does not own the protected application directory".into());
    }
    Ok(stream)
}
#[cfg(not(any(target_os = "linux", windows)))]
async fn connect(_path: &Path) -> Result<ApplicationStream> {
    Err("Wayfinder local applications require Linux or Windows".into())
}
#[derive(Default)]
pub struct Session {
    stream: tokio::sync::Mutex<Option<ApplicationStream>>,
}
impl Session {
    pub async fn call(&self, path: &Path, operation: Value) -> Result<Value> {
        let mut session = self.stream.lock().await;
        let result = tokio::time::timeout(Duration::from_secs(4), async {
            if session.is_none() {
                *session = Some(connect(path).await?);
            }
            let stream = session.as_mut().unwrap();
            write_json(stream, &operation, HEADER_LIMIT).await?;
            let reply: Value = read_json(stream, 128 * 1024).await?;
            if let Some(error) = reply["error"].as_str() {
                return Err(error.to_owned());
            }
            reply
                .get("value")
                .cloned()
                .ok_or_else(|| "Invalid Wayfinder reply".into())
        })
        .await
        .unwrap_or_else(|_| Err("Wayfinder application request timed out".into()));
        if result.is_err() {
            *session = None;
        }
        result
    }
    pub async fn disconnect(&self) {
        *self.stream.lock().await = None;
    }
}
pub async fn open(path: &Path, target: &str, service: &str) -> Result<ApplicationStream> {
    let mut stream = connect(path).await?;
    write_json(
        &mut stream,
        &json!({"op":"open_service", "target":target, "service":service}),
        HEADER_LIMIT,
    )
    .await?;
    let reply: Value = read_json(&mut stream, HEADER_LIMIT).await?;
    if let Some(error) = reply["error"].as_str() {
        return Err(error.into());
    }
    if reply != json!({"version":1,"ready":true}) {
        return Err("Invalid Wayfinder service admission".into());
    }
    Ok(stream)
}
pub async fn read_json<T: DeserializeOwned>(
    stream: &mut (impl tokio::io::AsyncRead + Unpin),
    limit: usize,
) -> Result<T> {
    let bytes = read_frame(stream, limit).await?;
    serde_json::from_slice(&bytes).map_err(|_| "Malformed protocol JSON".into())
}
pub async fn write_json<T: Serialize>(
    stream: &mut (impl tokio::io::AsyncWrite + Unpin),
    value: &T,
    limit: usize,
) -> Result<()> {
    let bytes = serde_json::to_vec(value).map_err(|e| e.to_string())?;
    if bytes.len() > limit {
        return Err("Protocol message exceeds limit".into());
    }
    write_frame(stream, &bytes).await
}
pub async fn read_frame(
    stream: &mut (impl tokio::io::AsyncRead + Unpin),
    limit: usize,
) -> Result<Vec<u8>> {
    let len = stream.read_u32().await.map_err(|_| {
        "Peer disconnected before response completed; outcome may be unknown; no retry".to_owned()
    })? as usize;
    if len > limit {
        return Err("Protocol frame exceeds limit".into());
    }
    let mut bytes = vec![0; len];
    stream
        .read_exact(&mut bytes)
        .await
        .map_err(|_| "Peer disconnected during response; no retry".to_owned())?;
    Ok(bytes)
}
pub async fn write_frame(
    stream: &mut (impl tokio::io::AsyncWrite + Unpin),
    bytes: &[u8],
) -> Result<()> {
    stream
        .write_u32(bytes.len() as u32)
        .await
        .map_err(|e| e.to_string())?;
    stream.write_all(bytes).await.map_err(|e| e.to_string())
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};

    #[tokio::test]
    async fn endpoint_trust_rejects_replacement_and_exposure() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("wayfinder");
        std::fs::create_dir(&directory).unwrap();
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o2750)).unwrap();
        let path = directory.join("app.sock");
        let _listener = tokio::net::UnixListener::bind(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o660)).unwrap();
        assert!(connect(&path).await.is_ok());
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o2770)).unwrap();
        assert!(
            connect(&path).await.is_err(),
            "applications could replace endpoint"
        );
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o2750)).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o666)).unwrap();
        assert!(connect(&path).await.is_err(), "world-authorized socket");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o660)).unwrap();
        let alias = root.path().join("alias");
        symlink(&directory, &alias).unwrap();
        assert!(
            connect(&alias.join("app.sock")).await.is_err(),
            "symlink endpoint directory"
        );
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o777)).unwrap();
        assert!(connect(&path).await.is_err(), "unprotected runtime parent");
    }
}
