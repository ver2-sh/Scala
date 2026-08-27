use std::fs::{self, OpenOptions};
use std::io::Write;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use uuid::Uuid;

use crate::{AppPaths, CoreError, Result, ServerState};

const RUNTIME_SCHEMA_VERSION: u32 = 1;
const HEALTH_TIMEOUT: Duration = Duration::from_millis(800);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeDescriptor {
    pub schema_version: u32,
    pub instance_id: String,
    pub process_id: u32,
    pub endpoint: String,
    pub address: SocketAddr,
    pub started_at_unix: i64,
}

pub struct RuntimePublisher {
    descriptor: RuntimeDescriptor,
    path: PathBuf,
    published: bool,
}

impl RuntimePublisher {
    pub fn publish(paths: &AppPaths, address: SocketAddr) -> Result<Self> {
        let instance_id = Uuid::new_v4().to_string();
        let probe_address = match address {
            SocketAddr::V4(address) if address.ip().is_unspecified() => {
                SocketAddr::from(([127, 0, 0, 1], address.port()))
            }
            SocketAddr::V6(address) if address.ip().is_unspecified() => {
                SocketAddr::from(([0, 0, 0, 0, 0, 0, 0, 1], address.port()))
            }
            address => address,
        };
        let descriptor = RuntimeDescriptor {
            schema_version: RUNTIME_SCHEMA_VERSION,
            instance_id: instance_id.clone(),
            process_id: std::process::id(),
            endpoint: format!("http://{address}"),
            address: probe_address,
            started_at_unix: unix_timestamp(),
        };
        let directory = runtime_directory(paths);
        fs::create_dir_all(&directory).map_err(|source| CoreError::RuntimeState {
            path: directory.clone(),
            source,
        })?;
        let path = directory.join(format!("{instance_id}.json"));
        let temporary = directory.join(format!(".{instance_id}.tmp"));
        let bytes = serde_json::to_vec_pretty(&descriptor).map_err(|source| {
            CoreError::InvalidRuntimeDescriptor {
                path: temporary.clone(),
                source,
            }
        })?;
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)
            .map_err(|source| CoreError::RuntimeState {
                path: temporary.clone(),
                source,
            })?;
        if let Err(source) = file.write_all(&bytes).and_then(|()| file.sync_all()) {
            let _ = fs::remove_file(&temporary);
            return Err(CoreError::RuntimeState {
                path: temporary,
                source,
            });
        }
        fs::rename(&temporary, &path).map_err(|source| {
            let _ = fs::remove_file(&temporary);
            CoreError::RuntimeState {
                path: path.clone(),
                source,
            }
        })?;
        Ok(Self {
            descriptor,
            path,
            published: true,
        })
    }

    pub fn instance_id(&self) -> &str {
        &self.descriptor.instance_id
    }

    pub fn descriptor(&self) -> &RuntimeDescriptor {
        &self.descriptor
    }

    pub fn cleanup(&mut self) {
        if self.published {
            let owned = fs::read(&self.path)
                .ok()
                .and_then(|bytes| serde_json::from_slice::<RuntimeDescriptor>(&bytes).ok())
                .is_some_and(|current| current.instance_id == self.descriptor.instance_id);
            if owned {
                let _ = fs::remove_file(&self.path);
            }
            self.published = false;
        }
    }
}

impl Drop for RuntimePublisher {
    fn drop(&mut self) {
        self.cleanup();
    }
}

pub async fn observe_runtime(paths: &AppPaths) -> ServerState {
    let directory = runtime_directory(paths);
    let descriptors = match tokio::task::spawn_blocking(move || read_descriptors(&directory)).await
    {
        Ok(descriptors) => descriptors,
        Err(_) => return ServerState::Stopped,
    };
    for descriptor in descriptors {
        if probe(&descriptor).await {
            return ServerState::Running {
                endpoint: descriptor.endpoint,
            };
        }
    }
    ServerState::Stopped
}

fn runtime_directory(paths: &AppPaths) -> PathBuf {
    paths.state_dir.join("runtime").join("servers")
}

fn read_descriptors(directory: &Path) -> Vec<RuntimeDescriptor> {
    let Ok(entries) = fs::read_dir(directory) else {
        return Vec::new();
    };
    let mut descriptors = entries
        .filter_map(std::result::Result::ok)
        .filter(|entry| {
            entry
                .path()
                .extension()
                .is_some_and(|extension| extension == "json")
        })
        .filter_map(|entry| fs::read(entry.path()).ok())
        .filter_map(|bytes| serde_json::from_slice::<RuntimeDescriptor>(&bytes).ok())
        .filter(|descriptor| descriptor.schema_version == RUNTIME_SCHEMA_VERSION)
        .collect::<Vec<_>>();
    descriptors.sort_by_key(|descriptor| std::cmp::Reverse(descriptor.started_at_unix));
    descriptors
}

#[derive(Deserialize)]
struct HealthIdentity {
    status: String,
    instance_id: String,
}

async fn probe(descriptor: &RuntimeDescriptor) -> bool {
    let probe = async {
        let mut stream = TcpStream::connect(descriptor.address).await.ok()?;
        let host = match descriptor.address {
            SocketAddr::V4(address) => address.ip().to_string(),
            SocketAddr::V6(address) => format!("[{}]", address.ip()),
        };
        let request = format!(
            "GET /health HTTP/1.1\r\nHost: {host}:{}\r\nConnection: close\r\n\r\n",
            descriptor.address.port()
        );
        stream.write_all(request.as_bytes()).await.ok()?;
        let mut response = Vec::new();
        stream.read_to_end(&mut response).await.ok()?;
        let response = String::from_utf8(response).ok()?;
        let (headers, body) = response.split_once("\r\n\r\n")?;
        if !headers.lines().next()?.contains(" 200 ") {
            return None;
        }
        let health: HealthIdentity = serde_json::from_str(body).ok()?;
        Some(health.status == "ok" && health.instance_id == descriptor.instance_id)
    };
    tokio::time::timeout(HEALTH_TIMEOUT, probe)
        .await
        .ok()
        .flatten()
        .unwrap_or(false)
}

fn unix_timestamp() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_secs()).ok())
        .unwrap_or(0)
}
