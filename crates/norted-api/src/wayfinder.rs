//! Consumer of Wayfinder's public local service contract, not its internals.
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};

pub type Result<T> = std::result::Result<T, String>;
pub const HEADER_LIMIT: usize = 16384;

#[derive(Deserialize)]
pub struct Descriptor {
    version: u32,
    address: SocketAddr,
    service_address: SocketAddr,
    credential: String,
}
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

pub fn data_dir(config: &norted_core::LinkConfig) -> Result<PathBuf> {
    config
        .wayfinder_data_dir
        .clone()
        .or_else(|| {
            directories::ProjectDirs::from("org", "Wayfinder", "Wayfinder")
                .map(|d| d.data_local_dir().to_owned())
        })
        .ok_or_else(|| "Cannot locate Wayfinder data directory".into())
}
pub async fn descriptor(dir: &Path) -> Result<Descriptor> {
    let path = dir.join("control.json");
    let metadata = tokio::fs::symlink_metadata(&path)
        .await
        .map_err(|_| "Wayfinder is unavailable: private control descriptor missing".to_owned())?;
    if !metadata.is_file() || metadata.len() > HEADER_LIMIT as u64 {
        return Err("Invalid Wayfinder control descriptor".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err("Wayfinder control descriptor must be private (0600)".into());
        }
    }
    let bytes = tokio::fs::read(path).await.map_err(|e| e.to_string())?;
    let d: Descriptor = serde_json::from_slice(&bytes)
        .map_err(|_| "Wayfinder peer service v1 support is required".to_owned())?;
    if d.version != 1
        || !d.address.ip().is_loopback()
        || !d.service_address.ip().is_loopback()
        || d.credential.len() != 64
    {
        return Err("Invalid Wayfinder control descriptor".into());
    }
    Ok(d)
}
pub async fn control(http: &reqwest::Client, dir: &Path, operation: Value) -> Result<Value> {
    let d = descriptor(dir).await?;
    let mut response = http
        .post(format!("http://{}/control", d.address))
        .bearer_auth(d.credential)
        .json(&operation)
        .timeout(Duration::from_secs(4))
        .send()
        .await
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|e| e.to_string())? {
        if bytes.len() + chunk.len() > 128 * 1024 {
            return Err("Wayfinder control response too large".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    let reply: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    if let Some(error) = reply["error"].as_str() {
        return Err(error.to_owned());
    }
    reply
        .get("value")
        .cloned()
        .ok_or_else(|| "Invalid Wayfinder control reply".into())
}
pub async fn open(dir: &Path, target: &str, service: &str) -> Result<TcpStream> {
    let d = descriptor(dir).await?;
    let mut stream = TcpStream::connect(d.service_address)
        .await
        .map_err(|_| "Wayfinder service transport unavailable before dispatch".to_owned())?;
    stream.set_nodelay(true).map_err(|e| e.to_string())?;
    write_json(
        &mut stream,
        &json!({"version":1, "credential":d.credential, "target":target, "service":service}),
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
pub async fn read_json<T: DeserializeOwned>(stream: &mut TcpStream, limit: usize) -> Result<T> {
    let bytes = read_frame(stream, limit).await?;
    serde_json::from_slice(&bytes).map_err(|_| "Malformed protocol JSON".into())
}
pub async fn write_json<T: Serialize>(
    stream: &mut TcpStream,
    value: &T,
    limit: usize,
) -> Result<()> {
    let bytes = serde_json::to_vec(value).map_err(|e| e.to_string())?;
    if bytes.len() > limit {
        return Err("Protocol message exceeds limit".into());
    }
    write_frame(stream, &bytes).await
}
pub async fn read_frame(stream: &mut TcpStream, limit: usize) -> Result<Vec<u8>> {
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
pub async fn write_frame(stream: &mut TcpStream, bytes: &[u8]) -> Result<()> {
    stream
        .write_u32(bytes.len() as u32)
        .await
        .map_err(|e| e.to_string())?;
    stream.write_all(bytes).await.map_err(|e| e.to_string())
}
