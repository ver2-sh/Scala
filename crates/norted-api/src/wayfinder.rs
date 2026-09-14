//! Consumer of Wayfinder's public local service contract, not its internals.
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixStream,
};

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

pub fn socket_path() -> Result<PathBuf> {
    use std::os::unix::fs::MetadataExt;
    let uid = std::fs::metadata("/proc/self")
        .map_err(|e| e.to_string())?
        .uid();
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let run = PathBuf::from(format!("/run/user/{uid}"));
            if run.is_dir() {
                run
            } else {
                PathBuf::from(format!("/tmp/wayfinder-{uid}"))
            }
        });
    if !runtime.is_absolute() {
        return Err("Runtime directory must be absolute".into());
    }
    Ok(runtime.join("wayfinder/app.sock"))
}
async fn connect(path: &Path) -> Result<UnixStream> {
    use std::os::unix::fs::MetadataExt;
    let stream = UnixStream::connect(path)
        .await
        .map_err(|_| "Wayfinder: not detected".to_owned())?;
    let uid = std::fs::metadata("/proc/self")
        .map_err(|e| e.to_string())?
        .uid();
    if stream.peer_cred().map_err(|e| e.to_string())?.uid() != uid {
        return Err("Wayfinder application socket belongs to another user".into());
    }
    Ok(stream)
}
#[derive(Default)]
pub struct Session {
    stream: tokio::sync::Mutex<Option<UnixStream>>,
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
pub async fn open(path: &Path, target: &str, service: &str) -> Result<UnixStream> {
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
