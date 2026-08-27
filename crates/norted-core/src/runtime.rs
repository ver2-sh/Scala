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
const OBSERVATION_TIMEOUT: Duration = Duration::from_millis(1_500);
const STARTUP_GRACE: Duration = Duration::from_secs(30);
const UNREACHABLE_CLEANUP_AGE: Duration = Duration::from_secs(5 * 60);
const TIMEOUT_CLEANUP_AGE: Duration = Duration::from_secs(24 * 60 * 60);
const TIMEOUT_FAILURE_WINDOW: Duration = Duration::from_secs(60 * 60);
const TIMEOUT_FAILURES_REQUIRED: u32 = 3;

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
                let _ = fs::remove_file(failure_evidence_path_for(
                    &self.path,
                    &self.descriptor.instance_id,
                ));
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
    match tokio::time::timeout(OBSERVATION_TIMEOUT, observe_runtime_cycle(paths)).await {
        Ok(state) => state,
        Err(_) => ServerState::Failed {
            message: "runtime observation timed out".to_owned(),
        },
    }
}

async fn observe_runtime_cycle(paths: &AppPaths) -> ServerState {
    let directory = runtime_directory(paths);
    let candidates = match tokio::task::spawn_blocking(move || read_descriptors(&directory)).await {
        Ok(candidates) => candidates,
        Err(_) => return ServerState::Stopped,
    };

    let mut probes = tokio::task::JoinSet::new();
    for candidate in candidates {
        probes.spawn(async move {
            let outcome = probe(&candidate.descriptor).await;
            (candidate, outcome)
        });
    }

    let mut observations = Vec::new();
    while let Some(result) = probes.join_next().await {
        if let Ok(observation) = result {
            observations.push(observation);
        }
    }

    let healthy = observations
        .iter()
        .filter(|(_, outcome)| *outcome == ProbeOutcome::Healthy)
        .map(|(candidate, _)| candidate.descriptor.clone())
        .collect::<Vec<_>>();
    let _ = tokio::task::spawn_blocking(move || process_probe_evidence(observations)).await;

    let mut healthy = healthy;
    healthy.sort_by_key(|descriptor| std::cmp::Reverse(descriptor.started_at_unix));
    healthy
        .into_iter()
        .next()
        .map_or(ServerState::Stopped, |descriptor| ServerState::Running {
            endpoint: descriptor.endpoint,
        })
}

fn runtime_directory(paths: &AppPaths) -> PathBuf {
    paths.state_dir.join("runtime").join("servers")
}

#[derive(Debug)]
struct DescriptorCandidate {
    path: PathBuf,
    descriptor: RuntimeDescriptor,
}

impl DescriptorCandidate {
    fn age(&self) -> Duration {
        let age_seconds = unix_timestamp().saturating_sub(self.descriptor.started_at_unix);
        Duration::from_secs(u64::try_from(age_seconds).unwrap_or_default())
    }
}

fn read_descriptors(directory: &Path) -> Vec<DescriptorCandidate> {
    let Ok(entries) = fs::read_dir(directory) else {
        return Vec::new();
    };
    let mut candidates = entries
        .filter_map(std::result::Result::ok)
        .filter(|entry| {
            entry
                .path()
                .extension()
                .is_some_and(|extension| extension == "json")
        })
        .filter_map(|entry| {
            let path = entry.path();
            let bytes = fs::read(&path).ok()?;
            let descriptor = serde_json::from_slice::<RuntimeDescriptor>(&bytes).ok()?;
            (descriptor.schema_version == RUNTIME_SCHEMA_VERSION)
                .then_some(DescriptorCandidate { path, descriptor })
        })
        .collect::<Vec<_>>();
    candidates.sort_by_key(|candidate| std::cmp::Reverse(candidate.descriptor.started_at_unix));
    candidates
}

#[derive(Deserialize)]
struct HealthIdentity {
    status: String,
    instance_id: String,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum ProbeOutcome {
    Healthy,
    IdentityMismatch,
    ConnectionFailed,
    InvalidHealth,
    TimedOut,
}

#[derive(Debug, Clone, Copy)]
enum ProbeError {
    ConnectionFailed,
    InvalidHealth,
}

async fn probe(descriptor: &RuntimeDescriptor) -> ProbeOutcome {
    let probe = async {
        let mut stream = TcpStream::connect(descriptor.address)
            .await
            .map_err(|_| ProbeError::ConnectionFailed)?;
        let host = match descriptor.address {
            SocketAddr::V4(address) => address.ip().to_string(),
            SocketAddr::V6(address) => format!("[{}]", address.ip()),
        };
        let request = format!(
            "GET /health HTTP/1.1\r\nHost: {host}:{}\r\nConnection: close\r\n\r\n",
            descriptor.address.port()
        );
        stream
            .write_all(request.as_bytes())
            .await
            .map_err(|_| ProbeError::InvalidHealth)?;
        let mut response = Vec::new();
        stream
            .read_to_end(&mut response)
            .await
            .map_err(|_| ProbeError::InvalidHealth)?;
        let response = String::from_utf8(response).map_err(|_| ProbeError::InvalidHealth)?;
        let (headers, body) = response
            .split_once("\r\n\r\n")
            .ok_or(ProbeError::InvalidHealth)?;
        if !headers
            .lines()
            .next()
            .ok_or(ProbeError::InvalidHealth)?
            .contains(" 200 ")
        {
            return Err(ProbeError::InvalidHealth);
        }
        serde_json::from_str::<HealthIdentity>(body).map_err(|_| ProbeError::InvalidHealth)
    };
    match tokio::time::timeout(HEALTH_TIMEOUT, probe).await {
        Err(_) => ProbeOutcome::TimedOut,
        Ok(Err(ProbeError::ConnectionFailed)) => ProbeOutcome::ConnectionFailed,
        Ok(Err(ProbeError::InvalidHealth)) => ProbeOutcome::InvalidHealth,
        Ok(Ok(health)) if health.status == "ok" && health.instance_id == descriptor.instance_id => {
            ProbeOutcome::Healthy
        }
        Ok(Ok(_)) => ProbeOutcome::IdentityMismatch,
    }
}

fn cleanup_stale_descriptors(candidates: Vec<DescriptorCandidate>) {
    for candidate in candidates {
        let unchanged = fs::read(&candidate.path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<RuntimeDescriptor>(&bytes).ok())
            .is_some_and(|current| {
                current.schema_version == candidate.descriptor.schema_version
                    && current.instance_id == candidate.descriptor.instance_id
                    && current.address == candidate.descriptor.address
                    && current.started_at_unix == candidate.descriptor.started_at_unix
            });
        if unchanged {
            let _ = fs::remove_file(candidate.path);
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct ProbeFailureEvidence {
    instance_id: String,
    descriptor_started_at_unix: i64,
    first_failure_unix: i64,
    last_failure_unix: i64,
    failure_count: u32,
}

fn process_probe_evidence(observations: Vec<(DescriptorCandidate, ProbeOutcome)>) {
    let mut stale = Vec::new();
    for (candidate, outcome) in observations {
        match outcome {
            ProbeOutcome::Healthy => clear_failure_evidence(&candidate),
            ProbeOutcome::IdentityMismatch if candidate.age() >= STARTUP_GRACE => {
                clear_failure_evidence(&candidate);
                stale.push(candidate);
            }
            ProbeOutcome::ConnectionFailed if candidate.age() >= UNREACHABLE_CLEANUP_AGE => {
                clear_failure_evidence(&candidate);
                stale.push(candidate);
            }
            ProbeOutcome::InvalidHealth | ProbeOutcome::TimedOut
                if candidate.age() >= STARTUP_GRACE =>
            {
                if record_probe_failure(&candidate) {
                    clear_failure_evidence(&candidate);
                    stale.push(candidate);
                }
            }
            ProbeOutcome::IdentityMismatch
            | ProbeOutcome::ConnectionFailed
            | ProbeOutcome::InvalidHealth
            | ProbeOutcome::TimedOut => {}
        }
    }
    cleanup_stale_descriptors(stale);
}

fn record_probe_failure(candidate: &DescriptorCandidate) -> bool {
    let now = unix_timestamp();
    let path = failure_evidence_path(candidate);
    let previous = fs::read(&path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<ProbeFailureEvidence>(&bytes).ok())
        .filter(|evidence| {
            evidence.instance_id == candidate.descriptor.instance_id
                && evidence.descriptor_started_at_unix == candidate.descriptor.started_at_unix
        });
    let evidence = previous.map_or_else(
        || ProbeFailureEvidence {
            instance_id: candidate.descriptor.instance_id.clone(),
            descriptor_started_at_unix: candidate.descriptor.started_at_unix,
            first_failure_unix: now,
            last_failure_unix: now,
            failure_count: 1,
        },
        |previous| ProbeFailureEvidence {
            last_failure_unix: now,
            failure_count: previous.failure_count.saturating_add(1),
            ..previous
        },
    );
    let confirmed = candidate.age() >= TIMEOUT_CLEANUP_AGE
        && evidence.failure_count >= TIMEOUT_FAILURES_REQUIRED
        && now.saturating_sub(evidence.first_failure_unix)
            >= i64::try_from(TIMEOUT_FAILURE_WINDOW.as_secs()).unwrap_or(i64::MAX);
    if !confirmed {
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Ok(bytes) = serde_json::to_vec(&evidence) {
            let _ = fs::write(path, bytes);
        }
    }
    confirmed
}

fn clear_failure_evidence(candidate: &DescriptorCandidate) {
    let _ = fs::remove_file(failure_evidence_path(candidate));
}

fn failure_evidence_path(candidate: &DescriptorCandidate) -> PathBuf {
    failure_evidence_path_for(&candidate.path, &candidate.descriptor.instance_id)
}

fn failure_evidence_path_for(descriptor_path: &Path, instance_id: &str) -> PathBuf {
    let state_runtime_directory = descriptor_path
        .parent()
        .and_then(Path::parent)
        .unwrap_or_else(|| descriptor_path.parent().unwrap_or_else(|| Path::new(".")));
    state_runtime_directory
        .join("failures")
        .join(format!("{instance_id}.json"))
}

fn unix_timestamp() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_secs()).ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::net::SocketAddr;
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use uuid::Uuid;

    use super::{
        ProbeFailureEvidence, RuntimeDescriptor, observe_runtime, runtime_directory, unix_timestamp,
    };
    use crate::{AppPaths, ServerState};

    #[tokio::test]
    async fn observes_healthy_instance_and_prunes_only_conservatively_stale_descriptors() {
        let paths = temporary_paths("cleanup");
        let directory = runtime_directory(&paths);
        fs::create_dir_all(&directory).expect("create runtime directory");

        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind health listener");
        let address = listener.local_addr().expect("listener address");
        let healthy_id = Uuid::new_v4().to_string();
        let server_id = healthy_id.clone();
        let server = tokio::spawn(async move {
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().await.expect("accept health probe");
                let mut request = [0_u8; 512];
                let _ = stream.read(&mut request).await;
                let body = format!(r#"{{"status":"ok","instance_id":"{server_id}"}}"#);
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream
                    .write_all(response.as_bytes())
                    .await
                    .expect("write health response");
            }
        });

        let healthy_path = write_descriptor(&directory, &healthy_id, address, 60);
        let mismatch_path = write_descriptor(&directory, &Uuid::new_v4().to_string(), address, 60);
        let unused_address = unused_loopback_address();
        let old_unreachable_id = Uuid::new_v4().to_string();
        let old_unreachable_path = write_descriptor(
            &directory,
            &old_unreachable_id,
            unused_address,
            25 * 60 * 60,
        );
        write_prior_failure_evidence(&directory, &old_unreachable_id);
        let recent_unreachable_path =
            write_descriptor(&directory, &Uuid::new_v4().to_string(), unused_address, 5);

        let state = observe_runtime(&paths).await;
        assert_eq!(
            state,
            ServerState::Running {
                endpoint: format!("http://{address}")
            }
        );
        server.await.expect("health server task");
        assert!(healthy_path.exists());
        assert!(!mismatch_path.exists());
        assert!(!old_unreachable_path.exists());
        assert!(recent_unreachable_path.exists());

        fs::remove_dir_all(&paths.state_dir).expect("remove temporary state directory");
    }

    #[tokio::test]
    async fn probes_multiple_slow_descriptors_with_one_bounded_latency() {
        let paths = temporary_paths("bounded");
        let directory = runtime_directory(&paths);
        fs::create_dir_all(&directory).expect("create runtime directory");

        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind slow listener");
        let address = listener.local_addr().expect("listener address");
        let server = tokio::spawn(async move {
            let mut connections = Vec::new();
            for _ in 0..4 {
                let (stream, _) = listener.accept().await.expect("accept slow probe");
                connections.push(stream);
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
            drop(connections);
        });
        let descriptor_paths = (0..4)
            .map(|_| {
                write_descriptor(
                    &directory,
                    &Uuid::new_v4().to_string(),
                    address,
                    25 * 60 * 60,
                )
            })
            .collect::<Vec<_>>();

        let started = Instant::now();
        let state = observe_runtime(&paths).await;
        let elapsed = started.elapsed();
        assert_eq!(state, ServerState::Stopped);
        assert!(
            elapsed < Duration::from_millis(1_300),
            "concurrent observation took {elapsed:?}"
        );
        assert!(descriptor_paths.iter().all(|path| path.exists()));

        server.abort();
        fs::remove_dir_all(&paths.state_dir).expect("remove temporary state directory");
    }

    fn temporary_paths(label: &str) -> AppPaths {
        let root = std::env::temp_dir().join(format!("norted-runtime-{label}-{}", Uuid::new_v4()));
        AppPaths {
            config_dir: root.join("config"),
            config_file: root.join("config/config.toml"),
            data_dir: root.join("data"),
            state_dir: root.join("state"),
            cache_dir: root.join("cache"),
            log_dir: root.join("logs"),
        }
    }

    fn write_descriptor(
        directory: &Path,
        instance_id: &str,
        address: SocketAddr,
        age_seconds: i64,
    ) -> PathBuf {
        let descriptor = RuntimeDescriptor {
            schema_version: 1,
            instance_id: instance_id.to_owned(),
            process_id: u32::MAX,
            endpoint: format!("http://{address}"),
            address,
            started_at_unix: unix_timestamp() - age_seconds,
        };
        let path = directory.join(format!("{instance_id}.json"));
        fs::write(
            &path,
            serde_json::to_vec(&descriptor).expect("serialize descriptor"),
        )
        .expect("write descriptor");
        path
    }

    fn unused_loopback_address() -> SocketAddr {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind unused port");
        let address = listener.local_addr().expect("unused address");
        drop(listener);
        address
    }

    fn write_prior_failure_evidence(servers_directory: &Path, instance_id: &str) {
        let failures_directory = servers_directory
            .parent()
            .expect("runtime directory")
            .join("failures");
        fs::create_dir_all(&failures_directory).expect("create failure evidence directory");
        let now = unix_timestamp();
        let descriptor = serde_json::from_slice::<RuntimeDescriptor>(
            &fs::read(servers_directory.join(format!("{instance_id}.json")))
                .expect("read descriptor"),
        )
        .expect("deserialize descriptor");
        let evidence = ProbeFailureEvidence {
            instance_id: instance_id.to_owned(),
            descriptor_started_at_unix: descriptor.started_at_unix,
            first_failure_unix: now - 2 * 60 * 60,
            last_failure_unix: now - 60 * 60,
            failure_count: 2,
        };
        fs::write(
            failures_directory.join(format!("{instance_id}.json")),
            serde_json::to_vec(&evidence).expect("serialize failure evidence"),
        )
        .expect("write failure evidence");
    }
}
