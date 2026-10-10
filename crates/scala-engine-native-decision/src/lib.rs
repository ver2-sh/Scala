//! Supervised native System One runtimes. No generative fallback.
use async_trait::async_trait;
use futures_util::StreamExt;
use scala_core::{
    AcquisitionMethod, ArtifactFormat, DecisionBundle, EngineConfig, EngineInstallation,
    EngineRevision, HostCapabilities, InstalledRuntime, ModelArtifact, ModelProfileId,
    ResolvedSettings, RuntimeCompatibility, RuntimeProbeObservation,
};
use scala_engine::*;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::RwLock,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

mod launcher;

pub const ENGINE_ID: &str = "native_decision";
const PROTOCOL: &str = "scala-native-decision-v1";
const LIMIT: usize = 16 * 1024 * 1024;

pub struct NativeDecisionAdapter {
    binaries: Vec<PathBuf>,
    error: Option<String>,
    client: reqwest::Client,
    proofs: RwLock<BTreeMap<String, Proof>>,
    observation_paths: tokio::sync::Mutex<BTreeMap<PathBuf, ObservationInventory>>,
    #[cfg(windows)]
    pending_probes: tokio::sync::Mutex<BTreeMap<PathBuf, PendingProbe>>,
    discovery_probes:
        tokio::sync::Mutex<BTreeMap<PathBuf, (String, std::time::Instant, EngineProbe)>>,
}
// Retain Windows metadata verification across the bounded discovery wait.
// Dropping the adapter/task aborts the owned capture; it never initializes CUDA.
#[cfg(windows)]
struct PendingProbe {
    key: Option<String>,
    task: std::sync::Arc<tokio::sync::Mutex<ProbeTask>>,
}
#[cfg(windows)]
struct ProbeTask {
    handle: tokio::task::JoinHandle<Result<EngineProbe, EngineError>>,
    result: Option<Result<EngineProbe, String>>,
}
#[cfg(windows)]
impl Drop for ProbeTask {
    fn drop(&mut self) {
        self.handle.abort();
    }
}
// Discovery change detectors only; never runtime identity or launch authority.
#[derive(Clone, Deserialize)]
struct ObservationInventory {
    paths: Vec<PathBuf>,
    inventory_paths: Vec<PathBuf>,
    #[serde(skip)]
    inventory_key: String,
}
#[derive(Clone)]
struct Proof {
    tuple: String,
    nonce: String,
    revision: String,
    backend: String,
    bundle_sha256: String,
    profile: Option<ModelProfileId>,
    ready: bool,
}
#[derive(Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct Probe {
    protocol: String,
    engine_id: String,
    version: String,
    revision: String,
    backend: String,
    model_code_sha256: Option<String>,
}
fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}
fn invalid(detail: impl Into<String>) -> EngineError {
    EngineError::InvalidConfiguration(detail.into())
}
fn supported_backend(backend: &str) -> bool {
    matches!(backend, "vllm-labels" | "torch-readout")
}
fn bundle(model: &ModelArtifact) -> Result<DecisionBundle, EngineError> {
    if model.format != ArtifactFormat::DecisionBundle || model.norted_package.is_some() {
        return Err(invalid("Expected a native Decision source bundle"));
    }
    let bundle = DecisionBundle::read(&model.path).map_err(invalid)?;
    if !supported_backend(&bundle.backend) {
        return Err(invalid("Unsupported native readout backend"));
    }
    Ok(bundle)
}
fn key(runtime: &InstalledRuntime, model: &ModelArtifact, settings: &ResolvedSettings) -> String {
    format!(
        "{:x}",
        Sha256::digest(
            serde_json::to_vec(&(
                runtime,
                &model.id,
                &model.path,
                &model.hash,
                model.size_bytes,
                &model.native_identity,
                &settings.engine_id,
                &settings.model_profile_id,
                &settings.configured
            ))
            .unwrap()
        )
    )
}
fn binding_matches(runtime: &InstalledRuntime, bundle: &DecisionBundle) -> bool {
    let Ok(probe) = serde_json::from_str::<Probe>(&runtime.manifest.probe.detail) else {
        return false;
    };
    if probe.protocol != PROTOCOL
        || probe.engine_id != ENGINE_ID
        || probe.version != runtime.manifest.identity.version
        || Some(&probe.revision) != runtime.manifest.identity.upstream_revision.as_ref()
        || probe.backend != bundle.backend
    {
        return false;
    }
    if bundle.backend == "torch-readout" {
        return probe.model_code_sha256.is_none();
    }
    let code = &bundle.sources["model"].files[Path::new(&bundle.bindings["shim"])].sha256;
    probe.model_code_sha256.as_ref() == Some(code)
}

async fn digest(path: &Path) -> Result<String, EngineError> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let mut file = std::fs::File::open(path).map_err(|e| invalid(e.to_string()))?;
        let mut hasher = Sha256::new();
        std::io::copy(&mut file, &mut hasher).map_err(|e| invalid(e.to_string()))?;
        Ok(format!("{:x}", hasher.finalize()))
    })
    .await
    .map_err(|e| invalid(e.to_string()))?
}
fn settings_valid(settings: &ResolvedSettings) -> bool {
    settings.engine_id == ENGINE_ID
        && settings.configured.is_empty()
        && settings.model_profile_id.is_some()
}

impl NativeDecisionAdapter {
    pub fn from_config(config: Option<&EngineConfig>, directory: &Path) -> Self {
        let mut error = None;
        let mut binaries = Vec::new();
        if let Some(config) = config.filter(|c| c.enabled) {
            if !config.settings.is_empty()
                || !config.env.is_empty()
                || config
                    .native
                    .keys()
                    .any(|k| !matches!(k.as_str(), "binary" | "binaries"))
                || (config.native.contains_key("binary") && config.native.contains_key("binaries"))
            {
                error = Some("Use only explicit native.binary or native.binaries; inference overrides are unsupported".into());
            }
            let entries = if let Some(value) = config.native.get("binaries") {
                match value.as_array() {
                    Some(values) => values.iter().collect::<Vec<_>>(),
                    None => {
                        error = Some("native.binaries must be an array of paths".into());
                        Vec::new()
                    }
                }
            } else {
                config.native.get("binary").into_iter().collect()
            };
            for entry in entries {
                if let Some(value) = entry.as_str().filter(|s| !s.is_empty()) {
                    let path = PathBuf::from(value);
                    binaries.push(if path.is_absolute() {
                        path
                    } else {
                        directory.join(path)
                    });
                } else {
                    error = Some("Native runtime paths must be nonempty strings".into());
                }
            }
        }
        Self {
            binaries,
            error,
            client: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(600))
                .build()
                .expect("native HTTP client"),
            proofs: RwLock::new(BTreeMap::new()),
            observation_paths: tokio::sync::Mutex::new(BTreeMap::new()),
            discovery_probes: tokio::sync::Mutex::new(BTreeMap::new()),
            #[cfg(windows)]
            pending_probes: tokio::sync::Mutex::new(BTreeMap::new()),
        }
    }
    async fn probe_path(path: &Path) -> Result<EngineProbe, EngineError> {
        let path = tokio::fs::canonicalize(path)
            .await
            .map_err(|e| invalid(e.to_string()))?;
        let (sha, p) = Self::inspect(&path).await?;
        let detail = serde_json::to_string(&p).map_err(|e| invalid(e.to_string()))?;
        Ok(EngineProbe {
            installation: InstallationState::Installed {
                installation: Box::new(EngineInstallation {
                    engine: EngineRevision {
                        engine_id: ENGINE_ID.into(),
                        version: Some(p.version),
                        revision: Some(p.revision),
                    },
                    source_repository: None,
                    acquisition_method: AcquisitionMethod::ExternalBinary,
                    binary_path: path,
                    binary_sha256: Some(sha),
                    build: None,
                    platform: std::env::consts::OS.into(),
                    architecture: std::env::consts::ARCH.into(),
                    runtime_variant: Some(p.backend),
                    acquired_at_unix: None,
                    observed_at_unix: now(),
                }),
            },
            update: UpdateState::Unknown,
            healthy: true,
            detail,
        })
    }
    async fn inspect(path: &Path) -> Result<(String, Probe), EngineError> {
        let sha = digest(path).await?;
        let mut arguments = launcher::launch_prefix(path).await?;
        arguments.push("--scala-probe".into());
        let arguments = arguments
            .iter()
            .map(|a| {
                a.to_str()
                    .ok_or_else(|| invalid("Invalid launcher argument encoding"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let output = capture_owned_command(
            &launcher::executable(path).await?,
            &arguments,
            &BTreeMap::new(),
            &[],
            // Native Windows DLL closure reads can take longer when the OS
            // file cache is cold. This complete verification is independent of
            // the unchanged bounded public discovery deadline below.
            Duration::from_secs(if cfg!(windows) { 120 } else { 30 }),
        )
        .await?;
        if !output.success || output.stdout.len() > 65536 {
            return Err(invalid("Native runtime dependency/identity probe failed"));
        }
        let probe: Probe =
            serde_json::from_str(&output.stdout).map_err(|e| invalid(e.to_string()))?;
        if probe.protocol != PROTOCOL
            || probe.engine_id != ENGINE_ID
            || probe.version != "1"
            || probe.revision.len() != 64
            || !probe.revision.bytes().all(|b| b.is_ascii_hexdigit())
            || !supported_backend(&probe.backend)
        {
            return Err(invalid(
                "Native runtime does not implement the qualified runtime protocol",
            ));
        }
        Ok((sha, probe))
    }
    async fn binary_observation_key(&self, binary: &Path) -> Option<String> {
        let cached = self.observation_paths.lock().await.get(binary).cloned();
        if let Some(inventory) = cached
            && local_runtime_observation_key(inventory.inventory_paths.clone())
                .await
                .as_ref()
                == Some(&inventory.inventory_key)
        {
            return local_runtime_observation_key(inventory.paths.clone()).await;
        }
        self.observation_paths.lock().await.remove(binary);
        // Only the explicit prepare.py launcher shape is understood. Unknown
        // wrappers remain uncached, and are still authoritatively probed.
        let (interpreter, runner, mut controls) = launcher::observation_launcher(binary).await?;
        let config = runner.with_file_name("runtime.json");
        controls.extend([
            binary.to_owned(),
            runner,
            config.clone(),
            interpreter.clone(),
        ]);
        let before = local_runtime_observation_key(controls.clone()).await?;
        // Enumerate once per launcher/configuration and dependency topology.
        // Large vLLM inventories may take longer than two seconds; this
        // bounded enumeration imports no runtime/tensor code and performs
        // no hashes or persistent writes.
        let output = capture_owned_command(
            &interpreter,
            &[
                "-I",
                "-B",
                "-c",
                include_str!("observation.py"),
                config.to_str()?,
            ],
            &BTreeMap::new(),
            &[],
            Duration::from_secs(5),
        )
        .await
        .ok()?;
        if !output.success
            || output.stdout.len() > 16 * 1024 * 1024
            || local_runtime_observation_key(controls.clone()).await? != before
        {
            return None;
        }
        let mut inventory: ObservationInventory = serde_json::from_str(&output.stdout).ok()?;
        inventory.paths.extend(controls.iter().cloned());
        inventory.inventory_paths.extend(controls);
        inventory.paths.sort();
        inventory.paths.dedup();
        inventory.inventory_paths.sort();
        inventory.inventory_paths.dedup();
        inventory.inventory_key =
            local_runtime_observation_key(inventory.inventory_paths.clone()).await?;
        let key = local_runtime_observation_key(inventory.paths.clone()).await;
        self.observation_paths
            .lock()
            .await
            .insert(binary.to_owned(), inventory);
        key
    }
    async fn discovery_probe(&self, path: &Path) -> Result<EngineProbe, EngineError> {
        let before = self.binary_observation_key(path).await;
        if let Some(key) = &before
            && let Some((observed, at, probe)) = self.discovery_probes.lock().await.get(path)
            && key == observed
            && at.elapsed() < Duration::from_secs(300)
        {
            return Ok(probe.clone());
        }
        self.discovery_probes.lock().await.remove(path);
        #[cfg(not(windows))]
        let probe = Self::probe_path(path).await?;
        #[cfg(windows)]
        let probe = {
            let task_state = {
                let mut pending = self.pending_probes.lock().await;
                if pending.get(path).is_some_and(|entry| entry.key != before) {
                    pending.remove(path);
                }
                pending
                    .entry(path.to_owned())
                    .or_insert_with(|| {
                        let path = path.to_owned();
                        PendingProbe {
                            key: before.clone(),
                            task: std::sync::Arc::new(tokio::sync::Mutex::new(ProbeTask {
                                handle: tokio::spawn(async move { Self::probe_path(&path).await }),
                                result: None,
                            })),
                        }
                    })
                    .task
                    .clone()
            };
            let mut task = task_state.lock().await;
            if task.result.is_none() {
                task.result = Some(
                    (&mut task.handle)
                        .await
                        .map_err(|error| error.to_string())
                        .and_then(|result| result.map_err(|error| error.to_string())),
                );
            }
            let result = task
                .result
                .as_ref()
                .expect("completed metadata probe")
                .clone();
            drop(task);
            let mut pending = self.pending_probes.lock().await;
            if pending
                .get(path)
                .is_some_and(|entry| std::sync::Arc::ptr_eq(&entry.task, &task_state))
            {
                pending.remove(path);
            }
            drop(pending);
            result.map_err(invalid)?
        };
        let after = self.binary_observation_key(path).await;
        if before.is_some() && before != after {
            return Err(invalid("Native runtime changed during discovery"));
        }
        if let Some(key) = after.filter(|_| before.is_some()) {
            self.discovery_probes.lock().await.insert(
                path.to_owned(),
                (key, std::time::Instant::now(), probe.clone()),
            );
        }
        Ok(probe)
    }
    async fn read(
        &self,
        request: reqwest::RequestBuilder,
    ) -> Result<(reqwest::StatusCode, Value), EngineError> {
        let response = request
            .send()
            .await
            .map_err(|e| EngineError::BackendUnavailable(e.to_string()))?;
        let status = response.status();
        let mut chunks = response.bytes_stream();
        let mut bytes = Vec::new();
        while let Some(chunk) = chunks.next().await {
            let chunk = chunk.map_err(|e| EngineError::BackendUnavailable(e.to_string()))?;
            if bytes.len().saturating_add(chunk.len()) > LIMIT {
                return Err(EngineError::Operation(
                    "Native response exceeded size limit".into(),
                ));
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok((
            status,
            serde_json::from_slice(&bytes).map_err(|e| EngineError::Operation(e.to_string()))?,
        ))
    }
    fn proof(&self, endpoint: &str) -> Result<Proof, EngineError> {
        self.proofs
            .read()
            .unwrap()
            .get(endpoint)
            .cloned()
            .ok_or_else(|| EngineError::Unsupported("No native launch proof".into()))
    }
    async fn qualify(&self, endpoint: &str) -> Result<bool, EngineError> {
        let proof = self.proof(endpoint)?;
        if let Some(current) = self.proofs.write().unwrap().get_mut(endpoint) {
            current.ready = false;
        }
        let (status, observed) = self
            .read(
                self.client
                    .get(format!("{endpoint}/health"))
                    .timeout(Duration::from_secs(2)),
            )
            .await?;
        if !status.is_success() {
            return Ok(false);
        }
        if observed
            != json!({"protocol":PROTOCOL,"runtime_revision":proof.revision,"backend":proof.backend,
            "bundle_sha256":proof.bundle_sha256,"launch_nonce":proof.nonce,"native_readout":true})
        {
            return Err(invalid(
                "Native startup identity/readout differs from the verified launch",
            ));
        }
        let (status, validator) = self
            .read(
                self.client
                    .post(format!("{endpoint}/v1/systemone"))
                    .timeout(Duration::from_secs(2))
                    .json(&json!({})),
            )
            .await?;
        if status != reqwest::StatusCode::BAD_REQUEST
            || validator
                != json!({"error":{"type":"invalid_request_error","message":"\"state\" must be provided"}})
        {
            return Err(invalid("Native System One validator did not qualify"));
        }
        let mut proofs = self.proofs.write().unwrap();
        let current = proofs
            .get_mut(endpoint)
            .ok_or_else(|| invalid("Native launch was invalidated"))?;
        if current.nonce != proof.nonce {
            return Err(invalid("Native process changed during qualification"));
        }
        current.ready = true;
        Ok(true)
    }
}

#[async_trait]
impl EngineAdapter for NativeDecisionAdapter {
    fn identity(&self) -> EngineIdentity {
        EngineIdentity {
            id: ENGINE_ID.into(),
            display_name: "Native Decision".into(),
            upstream_repository: String::new(),
        }
    }
    fn capabilities(&self) -> EngineCapabilities {
        EngineCapabilities {
            artifact_formats: vec![ArtifactFormat::DecisionBundle],
            api: vec![ApiCapability::Decision],
            features: vec![EngineFeature::Decision],
        }
    }
    fn supports_model_capability(&self, model: &ModelArtifact, capability: ApiCapability) -> bool {
        capability == ApiCapability::Decision && self.compatibility(model).is_supported()
    }
    fn native_options(&self) -> Vec<NativeOption> {
        Vec::new()
    }
    fn compatibility(&self, model: &ModelArtifact) -> CompatibilityDecision {
        match bundle(model) {
            Ok(_) => CompatibilityDecision::Supported,
            Err(e) => CompatibilityDecision::Unsupported {
                reason: e.to_string(),
            },
        }
    }
    fn runtime_compatibility(&self, runtime: &InstalledRuntime) -> CompatibilityDecision {
        if cfg!(any(unix, windows))
            && runtime.validate().is_ok()
            && runtime.manifest.identity.platform == std::env::consts::OS
            && runtime.manifest.identity.architecture == std::env::consts::ARCH
            && runtime.manifest.identity.engine_id == ENGINE_ID
            && runtime.manifest.identity.version == "1"
            && supported_backend(&runtime.manifest.identity.variant)
            && runtime
                .manifest
                .identity
                .upstream_revision
                .as_ref()
                .is_some_and(|r| r.len() == 64)
        {
            CompatibilityDecision::Supported
        } else {
            CompatibilityDecision::Unsupported {
                reason: "Missing or incompatible native Decision runtime identity".into(),
            }
        }
    }
    fn runtime_model_compatibility(
        &self,
        runtime: &InstalledRuntime,
        model: &ModelArtifact,
        _: &HostCapabilities,
        _settings: Option<&ResolvedSettings>,
    ) -> RuntimeCompatibility {
        match bundle(model) {
            Ok(b)
                if self.runtime_compatibility(runtime).is_supported()
                    && b.backend == runtime.manifest.identity.variant
                    && binding_matches(runtime, &b) =>
            {
                RuntimeCompatibility::Compatible
            }
            _ => {
                RuntimeCompatibility::Incompatible("Native source/readout/runtime mismatch".into())
            }
        }
    }
    fn validate_configuration(
        &self,
        runtime: &InstalledRuntime,
        model: Option<&ModelArtifact>,
        host: &HostCapabilities,
        settings: &ResolvedSettings,
    ) -> Result<(), EngineError> {
        if let Some(error) = &self.error {
            return Err(invalid(error.clone()));
        }
        if !settings_valid(settings) {
            return Err(invalid(
                "Native Decision has no supported inference overrides; bind a Model Profile",
            ));
        }
        if model.is_some_and(|m| {
            !self
                .runtime_model_compatibility(runtime, m, host, Some(settings))
                .is_usable()
        }) {
            return Err(invalid("Native source/runtime mismatch"));
        }
        Ok(())
    }
    fn supports_native_decision_candidate(
        &self,
        runtime: &InstalledRuntime,
        model: &ModelArtifact,
        settings: &ResolvedSettings,
    ) -> bool {
        settings_valid(settings)
            && self.error.is_none()
            && bundle(model).is_ok_and(|b| {
                b.backend == runtime.manifest.identity.variant && binding_matches(runtime, &b)
            })
    }
    fn supports_native_decision(
        &self,
        runtime: &InstalledRuntime,
        model: &ModelArtifact,
        settings: &ResolvedSettings,
    ) -> bool {
        self.supports_native_decision_candidate(runtime, model, settings)
            && self
                .proofs
                .read()
                .unwrap()
                .values()
                .any(|p| p.ready && p.tuple == key(runtime, model, settings))
    }
    async fn probe(&self) -> Result<EngineProbe, EngineError> {
        if self.binaries.is_empty() {
            return Ok(EngineProbe {
                installation: InstallationState::NotInstalled,
                update: UpdateState::Unknown,
                healthy: false,
                detail: "No explicit native Decision runtime configured".into(),
            });
        }
        let mut failure = None;
        for path in &self.binaries {
            match self.discovery_probe(path).await {
                Ok(probe) => return Ok(probe),
                Err(error) => {
                    if failure.is_none() {
                        failure = Some(error);
                    }
                }
            }
        }
        Err(failure.expect("nonempty runtime inventory"))
    }
    async fn external_runtime_probes(&self) -> Vec<Result<EngineProbe, EngineError>> {
        if self.binaries.is_empty() {
            return vec![self.probe().await];
        }
        // Finish each variant inside the shared eight-second cold-probe bound.
        // A hung/failed variant must not discard another completed observation.
        futures_util::future::join_all(self.binaries.iter().map(|path| async move {
            tokio::time::timeout(Duration::from_millis(7750), self.discovery_probe(path))
                .await
                .map_err(|_| EngineError::TimedOut("Native runtime discovery variant".into()))?
        }))
        .await
    }
    async fn external_runtime_observation_key(&self) -> Option<String> {
        let mut keys = Vec::new();
        for binary in &self.binaries {
            keys.push(self.binary_observation_key(binary).await?);
        }
        Some(format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&keys).ok()?)
        ))
    }

    async fn probe_runtime(
        &self,
        runtime: &InstalledRuntime,
    ) -> Result<RuntimeProbeObservation, EngineError> {
        let (sha, p) = Self::inspect(&runtime.entrypoint_path()).await?;
        if !self.runtime_compatibility(runtime).is_supported()
            || sha != runtime.manifest.entrypoint_sha256
            || Some(&p.revision) != runtime.manifest.identity.upstream_revision.as_ref()
            || p.backend != runtime.manifest.identity.variant
            || serde_json::from_str::<Probe>(&runtime.manifest.probe.detail)
                .ok()
                .as_ref()
                != Some(&p)
        {
            return Err(invalid("Native runtime identity changed"));
        }
        Ok(RuntimeProbeObservation {
            compatible: true,
            observed_engine_id: ENGINE_ID.into(),
            observed_version: Some(p.version.clone()),
            observed_revision: Some(p.revision.clone()),
            detail: serde_json::to_string(&p).map_err(|e| invalid(e.to_string()))?,
            observed_at_unix: now(),
        })
    }
    async fn prepare_model_input(
        &self,
        model: &ModelArtifact,
    ) -> Result<PreparedModelInput, EngineError> {
        bundle(model)?;
        let mut model = model.clone();
        model.hash = Some(digest(&model.path).await?);
        Ok(PreparedModelInput {
            primary: model,
            auxiliary: Vec::new(),
            primary_file_identity: None,
        })
    }
    async fn build_launch_spec(&self, request: LaunchRequest) -> Result<LaunchSpec, EngineError> {
        if let Some(error) = &self.error {
            return Err(invalid(error.clone()));
        }
        if !settings_valid(&request.settings)
            || !bundle(&request.model.primary).is_ok_and(|b| {
                b.backend == request.runtime.manifest.identity.variant
                    && binding_matches(&request.runtime, &b)
            })
        {
            return Err(invalid("Native source/runtime/settings mismatch"));
        }
        self.probe_runtime(&request.runtime).await?;
        if !request.backend_address.ip().is_loopback() {
            return Err(invalid("Native backend must be loopback"));
        }
        let runtime = &request.runtime;
        let installation = EngineInstallation {
            engine: EngineRevision {
                engine_id: ENGINE_ID.into(),
                version: Some(runtime.manifest.identity.version.clone()),
                revision: runtime.manifest.identity.upstream_revision.clone(),
            },
            source_repository: None,
            acquisition_method: AcquisitionMethod::ExternalBinary,
            binary_path: runtime.entrypoint_path(),
            binary_sha256: Some(runtime.manifest.entrypoint_sha256.clone()),
            build: None,
            platform: runtime.manifest.identity.platform.clone(),
            architecture: runtime.manifest.identity.architecture.clone(),
            runtime_variant: Some(runtime.manifest.identity.variant.clone()),
            acquired_at_unix: None,
            observed_at_unix: now(),
        };
        let mut arguments = launcher::launch_prefix(&runtime.entrypoint_path()).await?;
        arguments.extend(vec![
            "--bundle".into(),
            request.model.primary.path.clone().into_os_string(),
            "--host".into(),
            request.backend_address.ip().to_string().into(),
            "--port".into(),
            request.backend_address.port().to_string().into(),
            "--launch-nonce".into(),
            uuid::Uuid::new_v4().to_string().into(),
            "--runtime-revision".into(),
            runtime
                .manifest
                .identity
                .upstream_revision
                .clone()
                .unwrap()
                .into(),
        ]);
        Ok(LaunchSpec {
            executable: launcher::executable(&runtime.entrypoint_path()).await?,
            arguments,
            environment: launcher::runtime_environment()?,
            environment_remove: vec![],
            inherits_parent_environment: false,
            supervise_process_tree: true,
            working_directory: None,
            temporary_files: vec![],
            endpoint: Some(format!("http://{}", request.backend_address)),
            normalized_settings: BTreeMap::new(),
            settings: request.settings,
            native_arguments: vec![],
            installation,
            runtime: request.runtime,
            model: request.model,
            accelerator_binding: request.accelerator_binding,
        })
    }
    async fn prepare_launch_attempt(&self, spec: &LaunchSpec) -> Result<(), EngineError> {
        let endpoint = spec
            .endpoint
            .as_ref()
            .ok_or_else(|| invalid("Missing native endpoint"))?;
        let tuple = key(&spec.runtime, &spec.model.primary, &spec.settings);
        self.proofs
            .write()
            .unwrap()
            .retain(|e, p| e != endpoint && p.tuple != tuple);
        self.probe_runtime(&spec.runtime).await?;
        let sha = digest(&spec.model.primary.path).await?;
        if spec.model.primary.hash.as_deref() != Some(&sha) {
            return Err(invalid("Native bundle changed since preparation"));
        }
        let b = bundle(&spec.model.primary)?;
        let nonce = spec
            .arguments
            .windows(2)
            .find(|a| a[0] == "--launch-nonce")
            .map(|a| a[1].to_string_lossy().into_owned())
            .ok_or_else(|| invalid("Missing native launch nonce"))?;
        self.proofs.write().unwrap().insert(
            endpoint.clone(),
            Proof {
                tuple,
                nonce,
                revision: spec
                    .runtime
                    .manifest
                    .identity
                    .upstream_revision
                    .clone()
                    .unwrap(),
                backend: b.backend,
                bundle_sha256: sha,
                profile: spec.settings.model_profile_id.clone(),
                ready: false,
            },
        );
        Ok(())
    }
    async fn clear_launch_state(&self, endpoint: Option<&str>) {
        let mut proofs = self.proofs.write().unwrap();
        if let Some(endpoint) = endpoint {
            proofs.remove(endpoint);
        } else {
            proofs.clear();
        }
    }
    async fn health(&self, process: &ProcessDescriptor) -> Result<bool, EngineError> {
        self.qualify(
            process
                .endpoint
                .as_deref()
                .ok_or_else(|| invalid("Missing native endpoint"))?,
        )
        .await
    }
    async fn startup_generation_settings(
        &self,
        _: &ProcessDescriptor,
    ) -> Result<Option<EffectiveGenerationSettings>, EngineError> {
        Ok(None)
    }
    async fn effective_generation_settings(
        &self,
        _: &ProcessDescriptor,
    ) -> Result<EffectiveGenerationSettings, EngineError> {
        Err(EngineError::Unsupported(
            "Native Decision has no text generation defaults".into(),
        ))
    }
    async fn decide(
        &self,
        endpoint: &str,
        request: DecisionRequest,
    ) -> Result<DecisionOutput, EngineError> {
        request
            .validate()
            .map_err(EngineError::InvalidDecisionRequest)?;
        let proof = self.proof(endpoint)?;
        if !proof.ready || proof.profile.as_ref() != Some(&request.model_profile_id) {
            return Err(EngineError::Unsupported(
                "Native pair has not qualified".into(),
            ));
        }
        let (status, body) = self
            .read(
                self.client
                    .post(format!("{endpoint}/v1/systemone"))
                    .json(&json!({"state":request.state,"questions":request.questions})),
            )
            .await?;
        if !status.is_success() {
            return Err(if matches!(status.as_u16(), 400 | 422) {
                EngineError::InvalidDecisionRequest(body.to_string())
            } else {
                EngineError::BackendUnavailable(format!("Native System One HTTP {status}"))
            });
        }
        let usage = native_usage(body.get("usage"))?;
        let answers: BTreeMap<String, DecisionAnswer> = serde_json::from_value(
            body.get("answers")
                .cloned()
                .ok_or_else(|| EngineError::Operation("Missing native answers".into()))?,
        )
        .map_err(|e| EngineError::Operation(e.to_string()))?;
        validate_answers(&request, &answers)?;
        Ok(DecisionOutput {
            model: body.get("model").and_then(Value::as_str).map(str::to_owned),
            answers,
            usage,
        })
    }
    async fn infer(&self, _: &str, _: InferenceRequest) -> Result<InferenceOutput, EngineError> {
        Err(EngineError::Unsupported("text generation".into()))
    }
    async fn infer_stream(
        &self,
        _: &str,
        _: InferenceRequest,
        _: InferenceActivityReporter,
    ) -> Result<InferenceStream, EngineError> {
        Err(EngineError::Unsupported("text generation".into()))
    }
}
fn native_usage(value: Option<&Value>) -> Result<Option<InferenceUsage>, EngineError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let u = value
        .as_object()
        .ok_or_else(|| EngineError::Operation("Invalid native usage".into()))?;
    let count = |name: &str| -> Result<Option<u64>, EngineError> {
        u.get(name)
            .map(|v| {
                v.as_u64()
                    .ok_or_else(|| EngineError::Operation("Invalid native token count".into()))
            })
            .transpose()
    };
    let (Some(input), Some(output)) = (count("input_tokens")?, count("output_tokens")?) else {
        return Ok(None);
    };
    Ok(Some(InferenceUsage {
        input_tokens: input,
        output_tokens: output,
        total_tokens: input
            .checked_add(output)
            .ok_or_else(|| EngineError::Operation("Native usage overflow".into()))?,
        ..Default::default()
    }))
}

fn validate_answers(
    request: &DecisionRequest,
    answers: &BTreeMap<String, DecisionAnswer>,
) -> Result<(), EngineError> {
    if answers.keys().ne(request.questions.keys()) {
        return Err(EngineError::Operation(
            "Native answer names differ from request".into(),
        ));
    }
    for (name, answer) in answers {
        let valid = match (&request.questions[name], answer) {
            (DecisionQuestion::Choice { criteria, .. }, DecisionAnswer::Choice { choice, .. }) => {
                criteria.contains_key(choice)
            }
            (DecisionQuestion::Score { criteria, .. }, DecisionAnswer::Score { score, .. }) => {
                score.is_finite() && *score >= 0.0 && *score <= (criteria.len() - 1) as f64
            }
            (DecisionQuestion::Noul { .. }, DecisionAnswer::Noul { noul, .. }) => {
                noul.is_finite() && (0.0..=1.0).contains(noul)
            }
            _ => false,
        };
        if !valid {
            return Err(EngineError::Operation(format!(
                "Native answer `{name}` violates the typed contract"
            )));
        }
        let (probabilities, confidence, observations) = match answer {
            DecisionAnswer::Choice {
                probabilities,
                confidence,
                observations,
                ..
            }
            | DecisionAnswer::Score {
                probabilities,
                confidence,
                observations,
                ..
            }
            | DecisionAnswer::Noul {
                probabilities,
                confidence,
                observations,
                ..
            } => (probabilities, confidence, observations),
        };
        let probability = |v: f64| v.is_finite() && (0.0..=1.0).contains(&v);
        if confidence.is_some_and(|v| !probability(v))
            || observations
                .unknown_probability
                .is_some_and(|v| !probability(v))
        {
            return Err(EngineError::Operation(
                "Invalid native confidence/unknown probability".into(),
            ));
        }
        if let Some(probabilities) = probabilities {
            let names: Vec<String> = match &request.questions[name] {
                DecisionQuestion::Choice { criteria, .. } => criteria.keys().cloned().collect(),
                DecisionQuestion::Score { criteria, .. } => {
                    (0..criteria.len()).map(|i| i.to_string()).collect()
                }
                DecisionQuestion::Noul { .. } => vec!["false".into(), "true".into()],
            };
            let mut names = names;
            names.sort();
            if probabilities.keys().ne(names.iter())
                || probabilities.values().any(|v| !probability(*v))
            {
                return Err(EngineError::Operation(
                    "Native probability inventory differs from typed criteria".into(),
                ));
            }
        }
    }
    Ok(())
}

#[cfg(all(test, any(unix, windows)))]
mod tests;
