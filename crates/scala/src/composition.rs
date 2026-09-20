use std::fs::{File, OpenOptions};
use std::future::Future;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use color_eyre::eyre::{Context, Result, eyre};
use fs2::FileExt;
use scala_api::{ApiServer, PublicAuth, PublicAuthVerifier};
use scala_core::{
    ApiKeyStore, AppConfig, AppPaths, ApplicationCore, EffectivePublicAuthMode, PublicAuthStatus,
};
use scala_engine::{
    ControlClient, ControlClientError, EngineRegistry, RuntimeCatalogProvider, RuntimeManager,
    RuntimeManagerOptions, RuntimePackManager, TokioProcessSupervisor,
};
use scala_engine_llama_cpp::{
    ENGINE_ID as LLAMA_CPP_ENGINE_ID, LlamaCppAdapter, LlamaCppRuntimeCatalogProvider,
    LlamaCppSourceRuntimeCatalogProvider,
};
use scala_engine_ninfer::{
    ENGINE_ID as NINFER_ENGINE_ID, NinferAdapter, NinferRuntimeCatalogProvider,
};
use scala_engine_q27::{ENGINE_ID as Q27_ENGINE_ID, Q27Adapter, Q27RuntimeCatalogProvider};

const SERVER_START_LOCK_TIMEOUT: Duration = Duration::from_secs(10);
const SERVER_START_LOCK_RETRY: Duration = Duration::from_millis(50);
const SERVER_READY_TIMEOUT: Duration = Duration::from_secs(10);
const SERVER_READY_RETRY: Duration = Duration::from_millis(50);

pub struct ApplicationServices {
    pub registry: EngineRegistry,
    pub runtime_packs: Arc<RuntimePackManager>,
}

#[derive(Clone, Copy)]
pub enum ModelDiscoveryReadiness {
    RequireReady,
    AllowPending,
}

impl ApplicationServices {
    pub fn new(core: &ApplicationCore) -> Result<Self> {
        let registry = engine_registry(core)?;
        let runtime_packs = runtime_pack_manager(core, registry.clone())?;
        Ok(Self {
            registry,
            runtime_packs,
        })
    }

    pub async fn runtime_manager(&self, core: Arc<ApplicationCore>) -> Arc<RuntimeManager> {
        let jit = &core.config.server.jit;
        let options = RuntimeManagerOptions {
            jit_enabled: jit.enabled,
            primary_idle_ttl: Duration::from_secs(jit.primary_idle_ttl_seconds),
            auxiliary_idle_ttl: Duration::from_secs(jit.auxiliary_idle_ttl_seconds),
            max_idle_auxiliary_backends: jit.max_idle_auxiliary_backends,
            ..RuntimeManagerOptions::default()
        };
        RuntimeManager::initialize(
            core,
            self.registry.clone(),
            Arc::clone(&self.runtime_packs),
            Arc::new(TokioProcessSupervisor::default()),
            options,
        )
        .await
    }

    pub async fn start_server(
        &self,
        core: Arc<ApplicationCore>,
        model_discovery: ModelDiscoveryReadiness,
    ) -> Result<RunningServer> {
        let (public_auth, auth_status) = public_auth(&core).await?;
        if matches!(model_discovery, ModelDiscoveryReadiness::RequireReady) {
            core.ensure_model_discovery().await?;
        }
        let runtime = self.runtime_manager(Arc::clone(&core)).await;
        let server =
            match ApiServer::bind(Arc::clone(&core), Arc::clone(&runtime), public_auth).await {
                Ok(server) => server,
                Err(error) => {
                    runtime.shutdown().await;
                    return Err(error.into());
                }
            };
        RunningServer::start(&core.paths, server, auth_status).await
    }
}

pub async fn runtime_manager(core: Arc<ApplicationCore>) -> Result<Arc<RuntimeManager>> {
    Ok(ApplicationServices::new(&core)?.runtime_manager(core).await)
}

pub fn engine_registry(core: &ApplicationCore) -> Result<EngineRegistry> {
    engine_registry_from_config(&core.config, &core.config_path)
}

pub fn engine_registry_from_config(
    config: &AppConfig,
    config_path: &Path,
) -> Result<EngineRegistry> {
    let config_directory = config_path.parent().unwrap_or_else(|| Path::new("."));
    let mut registry = EngineRegistry::default();
    registry.register(Arc::new(LlamaCppAdapter::from_config(
        config.engine.get(LLAMA_CPP_ENGINE_ID),
        config_directory,
    )))?;
    registry.register(Arc::new(Q27Adapter::from_config(
        config.engine.get(Q27_ENGINE_ID),
        config_directory,
    )))?;
    registry.register(Arc::new(NinferAdapter::from_config(
        config.engine.get(NINFER_ENGINE_ID),
        config_directory,
    )))?;
    Ok(registry)
}

pub fn runtime_pack_manager(
    core: &ApplicationCore,
    registry: EngineRegistry,
) -> Result<Arc<RuntimePackManager>> {
    runtime_pack_manager_from_paths(&core.paths, registry)
}

pub fn runtime_pack_manager_from_paths(
    paths: &AppPaths,
    registry: EngineRegistry,
) -> Result<Arc<RuntimePackManager>> {
    let providers: Vec<Arc<dyn RuntimeCatalogProvider>> = vec![
        Arc::new(LlamaCppRuntimeCatalogProvider::new()),
        Arc::new(LlamaCppSourceRuntimeCatalogProvider::new()),
        Arc::new(Q27RuntimeCatalogProvider::new()),
        Arc::new(NinferRuntimeCatalogProvider::new()),
    ];
    Ok(RuntimePackManager::new(paths, registry, providers)?)
}

pub async fn discover_existing_control(paths: &AppPaths) -> Result<Option<ControlClient>> {
    match ControlClient::discover(paths).await {
        Ok(client) => {
            client.status().await.wrap_err(
                "a Scala passed its public identity probe, but its authenticated private control status could not be verified; refusing to attach or start a competing server",
            )?;
            Ok(Some(client))
        }
        Err(ControlClientError::Unavailable) => Ok(None),
        Err(error) => Err(error)
            .wrap_err("could not safely determine whether a Scala instance is already running"),
    }
}

pub struct ServerStartupGuard {
    lock: File,
}

impl ServerStartupGuard {
    pub async fn acquire(paths: &AppPaths) -> Result<Self> {
        let lock_path = paths.state_dir.join("runtime").join("server-start.lock");
        if let Some(parent) = lock_path.parent() {
            tokio::fs::create_dir_all(parent).await.with_context(|| {
                format!(
                    "could not create server startup lock directory {}",
                    parent.display()
                )
            })?;
        }
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)
            .with_context(|| {
                format!("could not open server startup lock {}", lock_path.display())
            })?;
        let started = Instant::now();
        loop {
            match lock.try_lock_exclusive() {
                Ok(()) => return Ok(Self { lock }),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if started.elapsed() >= SERVER_START_LOCK_TIMEOUT {
                        return Err(eyre!(
                            "another Scala process is still determining server ownership; retry after its startup completes"
                        ));
                    }
                    tokio::time::sleep(SERVER_START_LOCK_RETRY).await;
                }
                Err(error) => {
                    return Err(error).with_context(|| {
                        format!(
                            "could not acquire server startup lock {}",
                            lock_path.display()
                        )
                    });
                }
            }
        }
    }
}

impl Drop for ServerStartupGuard {
    fn drop(&mut self) {
        if let Err(error) = FileExt::unlock(&self.lock) {
            tracing::warn!(%error, "could not release server startup lock");
        }
    }
}

pub struct RunningServer {
    address: std::net::SocketAddr,
    auth_status: PublicAuthStatus,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    task: Option<tokio::task::JoinHandle<std::result::Result<(), scala_api::ApiError>>>,
}

impl RunningServer {
    async fn start(
        paths: &AppPaths,
        server: ApiServer,
        auth_status: PublicAuthStatus,
    ) -> Result<Self> {
        let address = server.local_addr();
        let (shutdown, shutdown_receiver) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(server.run(async move {
            let _ = shutdown_receiver.await;
        }));
        let mut running = Self {
            address,
            auth_status,
            shutdown: Some(shutdown),
            task: Some(task),
        };
        if let Err(error) = running.wait_until_ready(paths).await {
            let cleanup = running.shutdown().await;
            return match cleanup {
                Ok(()) => Err(error),
                Err(cleanup) => Err(error.wrap_err(format!(
                    "the failed server startup also could not shut down cleanly: {cleanup}"
                ))),
            };
        }
        Ok(running)
    }

    pub fn local_addr(&self) -> std::net::SocketAddr {
        self.address
    }

    pub fn auth_status(&self) -> &PublicAuthStatus {
        &self.auth_status
    }

    pub async fn run_while<F>(mut self, foreground: F) -> Result<()>
    where
        F: Future<Output = Result<()>>,
    {
        tokio::pin!(foreground);
        let mut task = self.task.take().expect("a running server has a task");
        tokio::select! {
            foreground_result = &mut foreground => {
                self.signal_shutdown();
                let server_result = Self::task_result(task.await);
                match foreground_result {
                    Ok(()) => server_result,
                    Err(error) => {
                        if let Err(shutdown_error) = server_result {
                            tracing::error!(%shutdown_error, "owned server cleanup failed after the foreground task returned an error");
                        }
                        Err(error)
                    }
                }
            }
            server_result = &mut task => {
                self.shutdown.take();
                Self::task_result(server_result).and_then(|()| {
                    Err(eyre!("the owned Scala stopped unexpectedly"))
                })
            }
        }
    }

    async fn wait_until_ready(&mut self, paths: &AppPaths) -> Result<()> {
        let ready = async {
            loop {
                match ControlClient::discover(paths).await {
                    Ok(client) => match client.status().await {
                        Ok(_) => return Ok(()),
                        Err(error) => {
                            tracing::debug!(%error, "owned control API is not ready yet");
                        }
                    },
                    Err(ControlClientError::Unavailable) => {}
                    Err(error) => return Err(error.into()),
                }
                tokio::time::sleep(SERVER_READY_RETRY).await;
            }
        };
        enum ReadyOutcome {
            Observation(Result<()>),
            Server(
                std::result::Result<
                    std::result::Result<(), scala_api::ApiError>,
                    tokio::task::JoinError,
                >,
            ),
        }
        let outcome = tokio::select! {
            result = tokio::time::timeout(SERVER_READY_TIMEOUT, ready) => {
                ReadyOutcome::Observation(result
                    .map_err(|_| eyre!("the owned Scala did not become discoverable within {} seconds", SERVER_READY_TIMEOUT.as_secs()))?
                )
            }
            result = self.task.as_mut().expect("a starting server has a task") => {
                ReadyOutcome::Server(result)
            }
        };
        match outcome {
            ReadyOutcome::Observation(result) => result,
            ReadyOutcome::Server(result) => {
                self.task.take();
                self.shutdown.take();
                Self::task_result(result)?;
                Err(eyre!(
                    "the owned Scala stopped before its control API became ready"
                ))
            }
        }
    }

    async fn shutdown(mut self) -> Result<()> {
        self.signal_shutdown();
        let Some(task) = self.task.take() else {
            return Ok(());
        };
        Self::task_result(task.await)
    }

    fn signal_shutdown(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
    }

    fn task_result(
        result: std::result::Result<
            std::result::Result<(), scala_api::ApiError>,
            tokio::task::JoinError,
        >,
    ) -> Result<()> {
        match result {
            Ok(result) => result.map_err(Into::into),
            Err(error) => Err(eyre!("owned server task failed: {error}")),
        }
    }
}

async fn public_auth(core: &ApplicationCore) -> Result<(PublicAuth, PublicAuthStatus)> {
    let key_store = ApiKeyStore::new(&core.paths);
    let auth_status = core
        .config
        .server
        .public_auth_status(key_store.active_count().await?)?;
    validate_public_auth_startup(&auth_status)?;
    let public_auth = match auth_status.effective_mode {
        EffectivePublicAuthMode::Disabled => PublicAuth::disabled(),
        EffectivePublicAuthMode::Required => {
            PublicAuth::required(Arc::new(KeyStoreVerifier(key_store)))
        }
    };
    Ok((public_auth, auth_status))
}

fn validate_public_auth_startup(status: &PublicAuthStatus) -> Result<()> {
    if !status.bind_allowed {
        return Err(eyre!(
            "public authentication is required for {}, but there are no active API keys; create one with `scala auth keys create --name <LABEL>` before serving",
            status.bind
        ));
    }
    Ok(())
}

#[derive(Clone)]
struct KeyStoreVerifier(ApiKeyStore);

#[async_trait::async_trait]
impl PublicAuthVerifier for KeyStoreVerifier {
    async fn verify(&self, credential: &str) -> bool {
        match self.0.verify(credential).await {
            Ok(verified) => verified,
            Err(error) => {
                tracing::error!(%error, "public API-key verification failed closed");
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use scala_core::{AppPaths, PublicAuthMode, RuntimePublisher, ServerConfig};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    use super::{ServerStartupGuard, discover_existing_control, validate_public_auth_startup};

    #[test]
    fn required_auth_without_an_active_key_fails_before_server_binding() {
        let status = ServerConfig {
            host: "0.0.0.0".to_owned(),
            port: 8742,
            auth: PublicAuthMode::Auto,
            ..ServerConfig::default()
        }
        .public_auth_status(0)
        .expect("auth status");
        let error = validate_public_auth_startup(&status).expect_err("must fail closed");
        assert!(error.to_string().contains("auth keys create"));
    }

    #[test]
    fn explicit_insecure_remote_override_is_allowed_but_marked() {
        let status = ServerConfig {
            host: "0.0.0.0".to_owned(),
            port: 8742,
            auth: PublicAuthMode::Disabled,
            ..ServerConfig::default()
        }
        .public_auth_status(0)
        .expect("auth status");
        validate_public_auth_startup(&status).expect("explicit override");
        assert!(status.insecure_remote);
    }

    #[tokio::test]
    async fn startup_guard_serializes_the_observe_or_bind_decision() {
        let paths = temporary_paths("startup-lock");
        let first = ServerStartupGuard::acquire(&paths)
            .await
            .expect("first startup guard");
        let second_paths = paths.clone();
        let mut second =
            tokio::spawn(async move { ServerStartupGuard::acquire(&second_paths).await });

        assert!(
            tokio::time::timeout(Duration::from_millis(100), &mut second)
                .await
                .is_err(),
            "a second startup decision must wait for the first"
        );
        drop(first);
        let second = tokio::time::timeout(Duration::from_secs(2), second)
            .await
            .expect("second startup guard timeout")
            .expect("second startup task")
            .expect("second startup guard");
        drop(second);
        std::fs::remove_dir_all(&paths.state_dir).expect("remove temporary state directory");
    }

    #[tokio::test]
    async fn attach_admission_requires_private_control_status() {
        let paths = temporary_paths("attach-control-status");
        let public_listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("public listener");
        let control_listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("control listener");
        let mut publisher = RuntimePublisher::publish(
            &paths,
            public_listener.local_addr().expect("public address"),
            control_listener.local_addr().expect("control address"),
            "test-control-token".to_owned(),
        )
        .expect("runtime publisher");
        let health = serde_json::json!({
            "status": "ok",
            "instance_id": publisher.instance_id(),
        })
        .to_string();
        let public = tokio::spawn(respond_once(public_listener, "200 OK", health));
        let control = tokio::spawn(respond_once(
            control_listener,
            "503 Service Unavailable",
            serde_json::json!({ "error": "control is not ready" }).to_string(),
        ));

        let error = discover_existing_control(&paths)
            .await
            .expect_err("public health alone must not admit attachment");
        assert!(
            error
                .to_string()
                .contains("authenticated private control status could not be verified")
        );
        public.await.expect("public response task");
        control.await.expect("control response task");
        publisher.cleanup();
        std::fs::remove_dir_all(&paths.state_dir).expect("remove temporary state directory");
    }

    async fn respond_once(listener: TcpListener, status: &str, body: String) {
        let (mut stream, _) = listener.accept().await.expect("accept test request");
        let mut request = [0_u8; 4096];
        let _ = stream.read(&mut request).await.expect("read test request");
        let response = format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        stream
            .write_all(response.as_bytes())
            .await
            .expect("write test response");
    }

    fn temporary_paths(label: &str) -> AppPaths {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("current time")
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("scala-{label}-{}-{unique}", std::process::id()));
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
}
