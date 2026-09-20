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

const RUNTIME_SCHEMA_VERSION: u32 = 2;
const HEALTH_TIMEOUT: Duration = Duration::from_millis(800);
const OBSERVATION_TIMEOUT: Duration = Duration::from_millis(1_500);
const STARTUP_GRACE: Duration = Duration::from_secs(30);
const UNREACHABLE_CLEANUP_AGE: Duration = Duration::from_secs(5 * 60);
const UNREACHABLE_FAILURE_WINDOW: Duration = Duration::from_secs(30);
const UNREACHABLE_FAILURES_REQUIRED: u32 = 3;
const TIMEOUT_CLEANUP_AGE: Duration = Duration::from_secs(24 * 60 * 60);
const TIMEOUT_FAILURE_WINDOW: Duration = Duration::from_secs(60 * 60);
const TIMEOUT_FAILURES_REQUIRED: u32 = 3;

#[derive(Clone, Serialize, Deserialize)]
pub struct RuntimeDescriptor {
    pub schema_version: u32,
    pub instance_id: String,
    pub process_id: u32,
    pub endpoint: String,
    pub address: SocketAddr,
    pub control_endpoint: String,
    pub control_address: SocketAddr,
    pub control_token: String,
    pub started_at_unix: i64,
}

impl std::fmt::Debug for RuntimeDescriptor {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RuntimeDescriptor")
            .field("schema_version", &self.schema_version)
            .field("instance_id", &self.instance_id)
            .field("process_id", &self.process_id)
            .field("endpoint", &self.endpoint)
            .field("address", &self.address)
            .field("control_endpoint", &self.control_endpoint)
            .field("control_address", &self.control_address)
            .field("control_token", &"<redacted>")
            .field("started_at_unix", &self.started_at_unix)
            .finish()
    }
}

pub struct RuntimePublisher {
    descriptor: RuntimeDescriptor,
    path: PathBuf,
    published: bool,
}

impl RuntimePublisher {
    pub fn publish(
        paths: &AppPaths,
        address: SocketAddr,
        control_address: SocketAddr,
        control_token: String,
    ) -> Result<Self> {
        if !control_address.ip().is_loopback() {
            return Err(CoreError::InvalidControlAddress {
                address: control_address,
            });
        }
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
            control_endpoint: format!("http://{control_address}"),
            control_address,
            control_token,
            started_at_unix: unix_timestamp(),
        };
        let directory = runtime_directory(paths);
        fs::create_dir_all(&directory).map_err(|source| CoreError::RuntimeState {
            path: directory.clone(),
            source,
        })?;
        restrict_directory_permissions(&directory)?;
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
        restrict_file_permissions(&temporary)?;
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

#[derive(Debug, thiserror::Error)]
pub enum RuntimeObservationError {
    #[error("runtime observation timed out")]
    TimedOut,
    #[error("runtime observation task failed: {0}")]
    TaskFailed(String),
    #[error("could not read runtime descriptors from {path}: {source}")]
    ReadDescriptors {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

pub async fn observe_runtime(
    paths: &AppPaths,
) -> std::result::Result<ServerState, RuntimeObservationError> {
    observe_runtime_with_timeout(paths, OBSERVATION_TIMEOUT).await
}

pub async fn observe_runtime_descriptor(
    paths: &AppPaths,
) -> std::result::Result<Option<RuntimeDescriptor>, RuntimeObservationError> {
    observe_runtime_descriptor_with_policy(paths, true).await
}

/// Observes an existing Server without recording probe failures or removing
/// stale runtime descriptors. This is intended for strictly read-only tools.
pub async fn observe_runtime_descriptor_read_only(
    paths: &AppPaths,
) -> std::result::Result<Option<RuntimeDescriptor>, RuntimeObservationError> {
    observe_runtime_descriptor_with_policy(paths, false).await
}

async fn observe_runtime_descriptor_with_policy(
    paths: &AppPaths,
    process_evidence: bool,
) -> std::result::Result<Option<RuntimeDescriptor>, RuntimeObservationError> {
    tokio::time::timeout(
        OBSERVATION_TIMEOUT,
        observe_runtime_descriptor_cycle(paths, process_evidence),
    )
    .await
    .map_err(|_| RuntimeObservationError::TimedOut)?
}

async fn observe_runtime_with_timeout(
    paths: &AppPaths,
    timeout: Duration,
) -> std::result::Result<ServerState, RuntimeObservationError> {
    if timeout.is_zero() {
        return Err(RuntimeObservationError::TimedOut);
    }
    tokio::time::timeout(timeout, observe_runtime_descriptor_cycle(paths, true))
        .await
        .map_err(|_| RuntimeObservationError::TimedOut)?
        .map(|descriptor| {
            descriptor.map_or(ServerState::Stopped, |descriptor| ServerState::Running {
                endpoint: descriptor.endpoint,
            })
        })
}

async fn observe_runtime_descriptor_cycle(
    paths: &AppPaths,
    process_evidence: bool,
) -> std::result::Result<Option<RuntimeDescriptor>, RuntimeObservationError> {
    let directory = runtime_directory(paths);
    let read_directory = directory.clone();
    let candidates =
        match tokio::task::spawn_blocking(move || read_descriptors(&read_directory)).await {
            Ok(Ok(candidates)) => candidates,
            Ok(Err(source)) => {
                return Err(RuntimeObservationError::ReadDescriptors {
                    path: directory,
                    source,
                });
            }
            Err(error) => return Err(RuntimeObservationError::TaskFailed(error.to_string())),
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
        match result {
            Ok(observation) => observations.push(observation),
            Err(error) => return Err(RuntimeObservationError::TaskFailed(error.to_string())),
        }
    }

    let healthy = observations
        .iter()
        .filter(|(_, outcome)| *outcome == ProbeOutcome::Healthy)
        .map(|(candidate, _)| candidate.descriptor.clone())
        .collect::<Vec<_>>();
    if process_evidence {
        let _ = tokio::task::spawn_blocking(move || process_probe_evidence(observations)).await;
    }

    let mut healthy = healthy;
    healthy.sort_by_key(|descriptor| std::cmp::Reverse(descriptor.started_at_unix));
    Ok(healthy.into_iter().next())
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

fn read_descriptors(directory: &Path) -> std::io::Result<Vec<DescriptorCandidate>> {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
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
            (descriptor.schema_version == RUNTIME_SCHEMA_VERSION
                && descriptor.control_address.ip().is_loopback()
                && !descriptor.control_token.is_empty())
            .then_some(DescriptorCandidate { path, descriptor })
        })
        .collect::<Vec<_>>();
    candidates.sort_by_key(|candidate| std::cmp::Reverse(candidate.descriptor.started_at_unix));
    Ok(candidates)
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
    failure_kind: ProbeFailureKind,
    first_failure_unix: i64,
    last_failure_unix: i64,
    failure_count: u32,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ProbeFailureKind {
    ConnectionFailed,
    Unresponsive,
}

#[derive(Debug, Clone, Copy)]
struct FailurePolicy {
    minimum_descriptor_age: Duration,
    minimum_failure_window: Duration,
    failures_required: u32,
}

const CONNECTION_FAILURE_POLICY: FailurePolicy = FailurePolicy {
    minimum_descriptor_age: UNREACHABLE_CLEANUP_AGE,
    minimum_failure_window: UNREACHABLE_FAILURE_WINDOW,
    failures_required: UNREACHABLE_FAILURES_REQUIRED,
};

const TIMEOUT_FAILURE_POLICY: FailurePolicy = FailurePolicy {
    minimum_descriptor_age: TIMEOUT_CLEANUP_AGE,
    minimum_failure_window: TIMEOUT_FAILURE_WINDOW,
    failures_required: TIMEOUT_FAILURES_REQUIRED,
};

fn process_probe_evidence(observations: Vec<(DescriptorCandidate, ProbeOutcome)>) {
    let mut stale = Vec::new();
    for (candidate, outcome) in observations {
        match outcome {
            ProbeOutcome::Healthy => clear_failure_evidence(&candidate),
            ProbeOutcome::IdentityMismatch if candidate.age() >= STARTUP_GRACE => {
                clear_failure_evidence(&candidate);
                stale.push(candidate);
            }
            ProbeOutcome::ConnectionFailed if candidate.age() >= STARTUP_GRACE => {
                if record_probe_failure(
                    &candidate,
                    ProbeFailureKind::ConnectionFailed,
                    CONNECTION_FAILURE_POLICY,
                ) {
                    clear_failure_evidence(&candidate);
                    stale.push(candidate);
                }
            }
            ProbeOutcome::InvalidHealth | ProbeOutcome::TimedOut
                if candidate.age() >= STARTUP_GRACE =>
            {
                if record_probe_failure(
                    &candidate,
                    ProbeFailureKind::Unresponsive,
                    TIMEOUT_FAILURE_POLICY,
                ) {
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

fn record_probe_failure(
    candidate: &DescriptorCandidate,
    failure_kind: ProbeFailureKind,
    policy: FailurePolicy,
) -> bool {
    let now = unix_timestamp();
    let path = failure_evidence_path(candidate);
    let previous = fs::read(&path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<ProbeFailureEvidence>(&bytes).ok())
        .filter(|evidence| {
            evidence.instance_id == candidate.descriptor.instance_id
                && evidence.descriptor_started_at_unix == candidate.descriptor.started_at_unix
                && evidence.failure_kind == failure_kind
        });
    let evidence = previous.map_or_else(
        || ProbeFailureEvidence {
            instance_id: candidate.descriptor.instance_id.clone(),
            descriptor_started_at_unix: candidate.descriptor.started_at_unix,
            failure_kind,
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
    let confirmed = candidate.age() >= policy.minimum_descriptor_age
        && evidence.failure_count >= policy.failures_required
        && now.saturating_sub(evidence.first_failure_unix)
            >= i64::try_from(policy.minimum_failure_window.as_secs()).unwrap_or(i64::MAX);
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

#[cfg(unix)]
fn restrict_directory_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|source| {
        CoreError::RuntimeState {
            path: path.to_owned(),
            source,
        }
    })
}

#[cfg(not(unix))]
fn restrict_directory_permissions(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn restrict_file_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(|source| {
        CoreError::RuntimeState {
            path: path.to_owned(),
            source,
        }
    })
}

#[cfg(not(unix))]
fn restrict_file_permissions(_path: &Path) -> Result<()> {
    Ok(())
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
        DescriptorCandidate, ProbeFailureEvidence, ProbeFailureKind, ProbeOutcome,
        RUNTIME_SCHEMA_VERSION, RuntimeDescriptor, RuntimeObservationError, failure_evidence_path,
        observe_runtime, observe_runtime_with_timeout, process_probe_evidence, runtime_directory,
        unix_timestamp,
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
        let recent_unreachable_path =
            write_descriptor(&directory, &Uuid::new_v4().to_string(), unused_address, 5);

        let one_off_unreachable_path = write_descriptor(
            &directory,
            &Uuid::new_v4().to_string(),
            unused_address,
            25 * 60 * 60,
        );

        let state = observe_runtime(&paths)
            .await
            .expect("observe runtime state");
        assert_eq!(
            state,
            ServerState::Running {
                endpoint: format!("http://{address}")
            }
        );
        server.await.expect("health server task");
        assert!(healthy_path.exists());
        assert!(!mismatch_path.exists());
        assert!(one_off_unreachable_path.exists());
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
        let state = observe_runtime(&paths)
            .await
            .expect("observe bounded runtime state");
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

    #[tokio::test]
    async fn observation_timeout_is_an_observer_error_not_a_server_failure() {
        let paths = temporary_paths("observer-timeout");
        let result = observe_runtime_with_timeout(&paths, Duration::ZERO).await;

        assert!(matches!(result, Err(RuntimeObservationError::TimedOut)));
    }

    #[test]
    fn connection_failure_cleanup_requires_repeated_matching_evidence() {
        let paths = temporary_paths("connection-evidence");
        let directory = runtime_directory(&paths);
        fs::create_dir_all(&directory).expect("create runtime directory");
        let instance_id = Uuid::new_v4().to_string();
        let descriptor_path = write_descriptor(
            &directory,
            &instance_id,
            unused_loopback_address(),
            25 * 60 * 60,
        );

        process_probe_evidence(vec![(
            read_candidate(&descriptor_path),
            ProbeOutcome::ConnectionFailed,
        )]);
        assert!(descriptor_path.exists());

        write_prior_failure_evidence(&directory, &instance_id, ProbeFailureKind::ConnectionFailed);
        process_probe_evidence(vec![(
            read_candidate(&descriptor_path),
            ProbeOutcome::ConnectionFailed,
        )]);
        assert!(!descriptor_path.exists());

        fs::remove_dir_all(&paths.state_dir).expect("remove temporary state directory");
    }

    #[test]
    fn healthy_observation_clears_matching_failure_evidence() {
        let paths = temporary_paths("healthy-clears-evidence");
        let directory = runtime_directory(&paths);
        fs::create_dir_all(&directory).expect("create runtime directory");
        let instance_id = Uuid::new_v4().to_string();
        let descriptor_path = write_descriptor(
            &directory,
            &instance_id,
            unused_loopback_address(),
            25 * 60 * 60,
        );
        write_prior_failure_evidence(&directory, &instance_id, ProbeFailureKind::ConnectionFailed);
        let candidate = read_candidate(&descriptor_path);
        let evidence_path = failure_evidence_path(&candidate);

        process_probe_evidence(vec![(candidate, ProbeOutcome::Healthy)]);
        assert!(descriptor_path.exists());
        assert!(!evidence_path.exists());

        fs::remove_dir_all(&paths.state_dir).expect("remove temporary state directory");
    }

    fn temporary_paths(label: &str) -> AppPaths {
        let root = std::env::temp_dir().join(format!("scala-runtime-{label}-{}", Uuid::new_v4()));
        AppPaths {
            config_dir: root.join("config"),
            config_file: root.join("config/config.toml"),
            data_dir: root.join("data"),
            state_dir: root.join("state"),
            cache_dir: root.join("cache"),
            log_dir: root.join("logs"),
            runtimes_dir: root.join("data/runtimes"),
            runtime_cache_dir: root.join("cache/runtime-packs"),
            runtime_selections_file: root.join("data/runtime-selections.json"),
            settings_file: root.join("data/settings.json"),
            settings_lock_file: root.join("data/.settings.lock"),
            model_profiles_file: root.join("data/model-profiles.json"),
            model_profiles_lock_file: root.join("data/.model-profiles.lock"),
        }
    }

    fn write_descriptor(
        directory: &Path,
        instance_id: &str,
        address: SocketAddr,
        age_seconds: i64,
    ) -> PathBuf {
        let descriptor = RuntimeDescriptor {
            schema_version: RUNTIME_SCHEMA_VERSION,
            instance_id: instance_id.to_owned(),
            process_id: u32::MAX,
            endpoint: format!("http://{address}"),
            address,
            control_endpoint: format!("http://{address}"),
            control_address: address,
            control_token: "test-control-token".to_owned(),
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

    fn read_candidate(path: &Path) -> DescriptorCandidate {
        DescriptorCandidate {
            path: path.to_owned(),
            descriptor: serde_json::from_slice(
                &fs::read(path).expect("read runtime descriptor candidate"),
            )
            .expect("deserialize runtime descriptor candidate"),
        }
    }

    fn write_prior_failure_evidence(
        servers_directory: &Path,
        instance_id: &str,
        failure_kind: ProbeFailureKind,
    ) {
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
            failure_kind,
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
