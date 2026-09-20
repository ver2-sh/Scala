//! Public OpenAI-compatible gateway and private authenticated control listener.

mod auth;
mod chat;
mod completions;
mod embeddings;
mod error;
mod input;
mod link;
mod wayfinder;
use link::execution_profile_id;
mod responses;

use std::future::Future;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::{Json, State, rejection::JsonRejection};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Router, middleware};
use scala_core::{ApplicationCore, ModelProfilesStore, ModelRole, RuntimePublisher, ServerState};
use scala_engine::{
    CONTROL_LOAD_PATH, CONTROL_STATUS_PATH, CONTROL_UNLOAD_PATH, ControlErrorResponse,
    ControlLoadRequest, ControlStatus, ControlUnloadRequest, InferenceRoutingContext,
    RuntimeManager,
};
use serde::Serialize;
use tokio::net::TcpListener;
use uuid::Uuid;

pub use auth::{PublicAuth, PublicAuthVerifier};

const GATEWAY_SHUTDOWN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
const SCALA_SESSION_HEADER: &str = "x-scala-session";
const SCALA_ROLE_HEADER: &str = "x-scala-role";
const MAX_SESSION_ID_BYTES: usize = 128;
/// Public inference bodies are buffered for JSON parsing, but never beyond this
/// explicit large-context ceiling.
pub const MAX_INFERENCE_BODY_BYTES: usize = 32 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("could not bind API server to {address}: {source}")]
    Bind {
        address: SocketAddr,
        source: std::io::Error,
    },
    #[error("could not bind private control server to loopback: {0}")]
    ControlBind(std::io::Error),
    #[error("API server failed: {0}")]
    Serve(String),
    #[error("could not publish runtime state: {0}")]
    Runtime(#[from] scala_core::CoreError),
}

pub struct ApiServer {
    core: Arc<ApplicationCore>,
    runtime: Arc<RuntimeManager>,
    public_listener: TcpListener,
    control_listener: TcpListener,
    address: SocketAddr,
    control_address: SocketAddr,
    control_token: String,
    public_auth: PublicAuth,
    publisher: RuntimePublisher,
}

impl ApiServer {
    pub async fn bind(
        core: Arc<ApplicationCore>,
        runtime: Arc<RuntimeManager>,
        public_auth: PublicAuth,
    ) -> Result<Self, ApiError> {
        core.set_server_state(ServerState::Starting).await;
        let requested_address = SocketAddr::new(
            core.config
                .server
                .ip_addr()
                .map_err(|error| ApiError::Bind {
                    address: SocketAddr::from(([127, 0, 0, 1], core.config.server.port)),
                    source: std::io::Error::new(std::io::ErrorKind::InvalidInput, error),
                })?,
            core.config.server.port,
        );
        let public_listener = match TcpListener::bind(requested_address).await {
            Ok(listener) => listener,
            Err(source) => {
                core.set_server_state(ServerState::Failed {
                    message: source.to_string(),
                })
                .await;
                return Err(ApiError::Bind {
                    address: requested_address,
                    source,
                });
            }
        };
        let address = public_listener
            .local_addr()
            .map_err(|error| ApiError::Serve(error.to_string()))?;
        let control_listener = TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(ApiError::ControlBind)?;
        let control_address = control_listener
            .local_addr()
            .map_err(|error| ApiError::Serve(error.to_string()))?;
        runtime
            .initialize_benchmark_server()
            .await
            .map_err(ApiError::Serve)?;
        let control_token = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
        let publisher = RuntimePublisher::publish(
            &core.paths,
            address,
            control_address,
            control_token.clone(),
        )?;
        let public_endpoint = format!("http://{address}");
        runtime.set_public_endpoint(public_endpoint.clone()).await;
        core.set_server_state(ServerState::Running {
            endpoint: public_endpoint,
        })
        .await;
        Ok(Self {
            core,
            runtime,
            public_listener,
            control_listener,
            address,
            control_address,
            control_token,
            public_auth,
            publisher,
        })
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.address
    }

    pub fn control_addr(&self) -> SocketAddr {
        self.control_address
    }

    pub async fn run(
        self,
        shutdown: impl Future<Output = ()> + Send + 'static,
    ) -> Result<(), ApiError> {
        let ApiServer {
            core,
            runtime,
            public_listener,
            control_listener,
            control_token,
            public_auth,
            mut publisher,
            ..
        } = self;
        let link = if core.config.link.enabled {
            match link::Link::new(core.clone(), runtime.clone()) {
                Ok(link) => Some(link),
                Err(error) => {
                    tracing::warn!(%error, "Scala Link unavailable");
                    None
                }
            }
        } else {
            None
        };
        let public = public_routes(
            PublicApiState {
                core: Arc::clone(&core),
                runtime: Arc::clone(&runtime),
                instance_id: publisher.instance_id().to_owned(),
                link: link.clone(),
            },
            public_auth,
        );
        let control = control_routes(ControlApiState {
            runtime: Arc::clone(&runtime),
            token: Arc::from(control_token),
            link: link.clone(),
        });
        let (shutdown_sender, shutdown_receiver) = tokio::sync::watch::channel(false);
        let mut servers = tokio::task::JoinSet::new();
        if let Some(link) = link {
            match TcpListener::bind("127.0.0.1:0").await {
                Ok(listener) => {
                    servers.spawn(link.run(listener, shutdown_receiver.clone()));
                }
                Err(error) => tracing::warn!(%error, "Scala Link listener unavailable"),
            }
        }
        let public_shutdown = shutdown_receiver.clone();
        servers.spawn(async move {
            axum::serve(public_listener, public)
                .with_graceful_shutdown(wait_for_shutdown(public_shutdown))
                .await
        });
        servers.spawn(async move {
            axum::serve(control_listener, control)
                .with_graceful_shutdown(wait_for_shutdown(shutdown_receiver))
                .await
        });

        let mut shutdown = Box::pin(shutdown);
        let first_server_result = tokio::select! {
            () = &mut shutdown => None,
            result = servers.join_next() => result,
        };
        let mut failure = match first_server_result {
            Some(Ok(Ok(()))) => Some("a server listener stopped unexpectedly".to_owned()),
            Some(Ok(Err(error))) => Some(error.to_string()),
            Some(Err(error)) => Some(error.to_string()),
            None => None,
        };
        let _ = shutdown_sender.send(true);
        let graceful_shutdown = tokio::time::timeout(GATEWAY_SHUTDOWN_TIMEOUT, async {
            while let Some(result) = servers.join_next().await {
                match result {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) if failure.is_none() => failure = Some(error.to_string()),
                    Err(error) if failure.is_none() => failure = Some(error.to_string()),
                    Ok(Err(_)) | Err(_) => {}
                }
            }
        })
        .await;
        if graceful_shutdown.is_err() {
            failure.get_or_insert_with(|| "gateway graceful shutdown timed out".to_owned());
            servers.abort_all();
            while servers.join_next().await.is_some() {}
        }
        runtime.shutdown().await;
        publisher.cleanup();
        if let Some(message) = failure {
            core.set_server_state(ServerState::Failed {
                message: message.clone(),
            })
            .await;
            Err(ApiError::Serve(message))
        } else {
            core.set_server_state(ServerState::Stopped).await;
            Ok(())
        }
    }
}

async fn wait_for_shutdown(mut receiver: tokio::sync::watch::Receiver<bool>) {
    if *receiver.borrow() {
        return;
    }
    while receiver.changed().await.is_ok() {
        if *receiver.borrow() {
            return;
        }
    }
}

#[derive(Clone)]
struct PublicApiState {
    core: Arc<ApplicationCore>,
    runtime: Arc<RuntimeManager>,
    instance_id: String,
    link: Option<Arc<link::Link>>,
}

fn public_routes(state: PublicApiState, auth: PublicAuth) -> Router {
    let openai_routes = Router::new()
        .route("/v1/models", get(models))
        .route("/v1/models/{model}", get(retrieve_model))
        .route("/v1/completions", post(completions::create))
        .route("/v1/embeddings", post(embeddings::create))
        .route("/v1/responses", post(responses::create))
        .route("/v1/chat/completions", post(chat::create))
        .layer(axum::extract::DefaultBodyLimit::max(
            MAX_INFERENCE_BODY_BYTES,
        ))
        .route_layer(middleware::from_fn_with_state(state.clone(), link::route))
        .route_layer(middleware::from_fn_with_state(
            auth,
            auth::public_auth_middleware,
        ));
    Router::new()
        .route("/health", get(health))
        .merge(openai_routes)
        .with_state(state)
        .layer(middleware::from_fn(auth::request_id_middleware))
}

#[derive(Serialize)]
struct HealthResponse {
    status: &'static str,
    server: &'static str,
    version: &'static str,
    model_count: usize,
    instance_id: String,
}

async fn health(State(state): State<PublicApiState>) -> (StatusCode, Json<HealthResponse>) {
    let snapshot = state.core.snapshot().await;
    (
        StatusCode::OK,
        Json(HealthResponse {
            status: "ok",
            server: snapshot.server.label(),
            version: env!("CARGO_PKG_VERSION"),
            model_count: snapshot.models.len(),
            instance_id: state.instance_id,
        }),
    )
}

#[derive(Serialize)]
struct ModelList {
    object: &'static str,
    data: Vec<ApiModel>,
}

#[derive(Serialize)]
struct ApiModel {
    id: String,
    object: &'static str,
    owned_by: String,
    created: i64,
}

async fn public_models(state: &PublicApiState) -> Result<Vec<ApiModel>, error::OpenAiError> {
    let snapshot = state.core.snapshot().await;
    let profiles = ModelProfilesStore::new(&state.core.paths)
        .read()
        .await
        .map_err(|e| error::runtime_error(scala_engine::RuntimeError::Operation(e.to_string())))?;
    let mut models: Vec<_> = profiles
        .profiles
        .into_values()
        .map(|profile| ApiModel {
            id: profile.id.to_string(),
            object: "model",
            owned_by: "scala-user".into(),
            created: snapshot
                .models
                .iter()
                .find(|model| model.id == profile.model_id)
                .map_or(0, |model| model.created),
        })
        .collect();
    if let Some(link) = &state.link {
        let linked = link.snapshot().await;
        let mut counts = std::collections::BTreeMap::<String, usize>::new();
        for model in &models {
            *counts.entry(model.id.clone()).or_default() += 1;
        }
        for peer in &linked.peers {
            if let Some(inventory) = &peer.state {
                for profile in &inventory.profiles {
                    *counts.entry(profile.id.to_string()).or_default() += 1;
                }
            }
        }
        if let Some(node) = &linked.node_id {
            for model in &mut models {
                model.owned_by = node.clone();
                if counts.get(&model.id).copied().unwrap_or(0) > 1 {
                    model.id = scala_engine::link::qualified_alias(&model.id, node);
                }
            }
        }
        for peer in linked.peers.iter().filter(|p| p.reachable) {
            if let Some(inventory) = &peer.state {
                for profile in inventory.profiles.iter().filter(|p| p.installed) {
                    let name = profile.id.as_str();
                    models.push(ApiModel {
                        id: if counts.get(name).copied().unwrap_or(0) > 1 {
                            scala_engine::link::qualified_alias(name, &peer.node_id)
                        } else {
                            name.into()
                        },
                        object: "model",
                        owned_by: peer.node_id.clone(),
                        created: inventory
                            .models
                            .iter()
                            .find(|m| m.id == profile.model_id)
                            .map_or(0, |m| m.created),
                    });
                }
            }
        }
    }
    models.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(models)
}

async fn models(
    State(state): State<PublicApiState>,
) -> Result<Json<ModelList>, error::OpenAiError> {
    Ok(Json(ModelList {
        object: "list",
        data: public_models(&state).await?,
    }))
}

async fn retrieve_model(
    State(state): State<PublicApiState>,
    axum::extract::Path(model): axum::extract::Path<String>,
) -> Result<Json<ApiModel>, error::OpenAiError> {
    if state.link.is_some() {
        link::resolve(&state, &model).await?;
    }
    let listed = public_models(&state).await?;
    listed
        .into_iter()
        .find(|profile| {
            profile.id == model
                || model
                    .split_once('@')
                    .is_some_and(|(id, node)| profile.id == id && profile.owned_by == node)
        })
        .map(|mut profile| {
            profile.id = model;
            Json(profile)
        })
        .ok_or_else(error::OpenAiError::model_not_found)
}

#[derive(Clone)]
struct ControlApiState {
    runtime: Arc<RuntimeManager>,
    token: Arc<str>,
    link: Option<Arc<link::Link>>,
}

fn control_routes(state: ControlApiState) -> Router {
    Router::new()
        .route(
            scala_engine::link::CONTROL_LINK_PATH,
            get(control_link_status).post(control_link_action),
        )
        .route(CONTROL_STATUS_PATH, get(control_status))
        .route(CONTROL_LOAD_PATH, post(control_load))
        .route(CONTROL_UNLOAD_PATH, post(control_unload))
        .route(
            scala_engine::benchmark::CONTROL_BENCHMARK_PATH,
            post(control_benchmark),
        )
        .with_state(state)
}

async fn control_link_status(
    State(state): State<ControlApiState>,
    headers: HeaderMap,
) -> Result<Json<scala_engine::link::LinkSnapshot>, ControlApiError> {
    authorize(&headers, &state.token)?;
    Ok(Json(match state.link {
        Some(link) => link.snapshot().await,
        None => Default::default(),
    }))
}

async fn control_link_action(
    State(state): State<ControlApiState>,
    headers: HeaderMap,
    payload: Result<Json<scala_engine::link::LinkControlRequest>, JsonRejection>,
) -> Result<Json<scala_engine::link::NodeInventory>, ControlApiError> {
    authorize(&headers, &state.token)?;
    let Json(request) = payload.map_err(|e| ControlApiError {
        status: StatusCode::BAD_REQUEST,
        message: e.body_text(),
    })?;
    let link = state.link.ok_or_else(|| ControlApiError {
        status: StatusCode::SERVICE_UNAVAILABLE,
        message: "Scala Link is disabled or unavailable".into(),
    })?;
    tokio::time::timeout(std::time::Duration::from_secs(55), link.control(request))
        .await
        .map_err(|_| ControlApiError {
            status: StatusCode::GATEWAY_TIMEOUT,
            message: "Link control timed out after possible dispatch; inspect owner before retry"
                .into(),
        })?
        .map(Json)
        .map_err(|message| ControlApiError {
            status: StatusCode::BAD_GATEWAY,
            message,
        })
}

async fn control_benchmark(
    State(state): State<ControlApiState>,
    headers: HeaderMap,
    payload: Result<Json<scala_engine::benchmark::BenchmarkRequest>, JsonRejection>,
) -> Result<Json<serde_json::Value>, ControlApiError> {
    authorize(&headers, &state.token)?;
    let Json(request) = payload.map_err(|error| ControlApiError {
        status: StatusCode::BAD_REQUEST,
        message: error.body_text(),
    })?;
    state
        .runtime
        .benchmark_control(request)
        .await
        .map(Json)
        .map_err(|message| ControlApiError {
            status: if message.starts_with("Busy:") {
                StatusCode::CONFLICT
            } else {
                StatusCode::BAD_REQUEST
            },
            message,
        })
}

async fn control_status(
    State(state): State<ControlApiState>,
    headers: HeaderMap,
) -> Result<Json<ControlStatus>, ControlApiError> {
    authorize(&headers, &state.token)?;
    Ok(Json(state.runtime.status().await))
}

async fn control_load(
    State(state): State<ControlApiState>,
    headers: HeaderMap,
    payload: Result<Json<ControlLoadRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<ControlStatus>), ControlApiError> {
    authorize(&headers, &state.token)?;
    let Json(request) = payload.map_err(|error| ControlApiError {
        status: StatusCode::BAD_REQUEST,
        message: format!("invalid load request: {}", error.body_text()),
    })?;
    state
        .runtime
        .start_load_with_settings(
            request.model_profile_id,
            request.runtime_id,
            request.invocation_settings,
        )
        .await
        .map(|status| (StatusCode::ACCEPTED, Json(status)))
        .map_err(|error| ControlApiError {
            status: match error {
                scala_engine::RuntimeError::ModelProfileNotFound(_)
                | scala_engine::RuntimeError::BoundModelMissing { .. }
                | scala_engine::RuntimeError::ModelNotFound(_) => StatusCode::NOT_FOUND,
                scala_engine::RuntimeError::Busy(_)
                | scala_engine::RuntimeError::BenchmarkReserved => StatusCode::CONFLICT,
                scala_engine::RuntimeError::Incompatible { .. } => StatusCode::UNPROCESSABLE_ENTITY,
                scala_engine::RuntimeError::EngineUnavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
                scala_engine::RuntimeError::ShuttingDown => StatusCode::SERVICE_UNAVAILABLE,
                scala_engine::RuntimeError::StartupTimedOut(_) => StatusCode::GATEWAY_TIMEOUT,
                scala_engine::RuntimeError::StartupFailed(_) => StatusCode::BAD_GATEWAY,
                _ => StatusCode::INTERNAL_SERVER_ERROR,
            },
            message: error.to_string(),
        })
}

async fn control_unload(
    State(state): State<ControlApiState>,
    headers: HeaderMap,
    payload: Result<Json<ControlUnloadRequest>, JsonRejection>,
) -> Result<Json<ControlStatus>, ControlApiError> {
    authorize(&headers, &state.token)?;
    let Json(request) = payload.map_err(|error| ControlApiError {
        status: StatusCode::BAD_REQUEST,
        message: format!("invalid unload request: {}", error.body_text()),
    })?;
    state
        .runtime
        .unload(request.model_profile_id)
        .await
        .map(Json)
        .map_err(|error| ControlApiError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: error.to_string(),
        })
}

fn inference_routing_context(
    headers: &HeaderMap,
) -> Result<InferenceRoutingContext, error::OpenAiError> {
    let session_id = match headers.get(SCALA_SESSION_HEADER) {
        None => "default".to_owned(),
        Some(value) => {
            let value = value.to_str().map_err(|_| {
                error::OpenAiError::invalid(
                    "`X-Scala-Session` must contain visible ASCII text.",
                    None::<String>,
                    "invalid_header",
                )
            })?;
            if value.is_empty()
                || value.len() > MAX_SESSION_ID_BYTES
                || value.chars().any(char::is_control)
            {
                return Err(error::OpenAiError::invalid(
                    "`X-Scala-Session` must be 1 to 128 bytes of visible text.",
                    None::<String>,
                    "invalid_header",
                ));
            }
            value.to_owned()
        }
    };
    let role = match headers.get(SCALA_ROLE_HEADER) {
        None => None,
        Some(value) => match value.to_str().ok() {
            Some("primary") => Some(ModelRole::Primary),
            Some("auxiliary") => Some(ModelRole::Auxiliary),
            _ => {
                return Err(error::OpenAiError::invalid(
                    "`X-Scala-Role` must be `primary` or `auxiliary`.",
                    None::<String>,
                    "invalid_header",
                ));
            }
        },
    };
    Ok(InferenceRoutingContext { session_id, role })
}

fn authorize(headers: &HeaderMap, expected: &str) -> Result<(), ControlApiError> {
    let supplied = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "));
    if supplied.is_some_and(|supplied| constant_time_equal(supplied, expected)) {
        Ok(())
    } else {
        Err(ControlApiError {
            status: StatusCode::UNAUTHORIZED,
            message: "private control authentication failed".to_owned(),
        })
    }
}

fn constant_time_equal(left: &str, right: &str) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.as_bytes()
        .iter()
        .zip(right.as_bytes())
        .fold(0_u8, |difference, (left, right)| {
            difference | (left ^ right)
        })
        == 0
}

fn unix_timestamp() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_secs()).ok())
        .unwrap_or(0)
}

struct ControlApiError {
    status: StatusCode,
    message: String,
}

impl IntoResponse for ControlApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(ControlErrorResponse {
                error: self.message,
            }),
        )
            .into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::{Body, to_bytes};
    use axum::http::Request;
    use scala_core::{AppPaths, ModelId};
    use scala_engine::{
        BackendLifecycle, EngineRegistry, RuntimeCatalogProvider, RuntimeManagerOptions,
        RuntimePackManager, TokioProcessSupervisor,
    };
    use serde_json::json;
    use tower::ServiceExt;

    pub(super) async fn control_fixture() -> (
        tempfile::TempDir,
        Arc<RuntimeManager>,
        ModelId,
        Arc<ApplicationCore>,
    ) {
        let temporary = tempfile::tempdir().expect("temporary control fixture");
        let root = temporary.path();
        let model_dir = root.join("models");
        std::fs::create_dir_all(&model_dir).expect("model directory");
        std::fs::write(model_dir.join("fixture.gguf"), b"fixture").expect("model fixture");
        let paths = AppPaths {
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
        };
        paths.ensure_required().expect("fixture paths");
        std::fs::write(
            &paths.config_file,
            format!(
                "version = 1\n\n[models]\npaths = [{}]\n",
                serde_json::to_string(&model_dir).expect("model path string")
            ),
        )
        .expect("fixture config");
        let core = ApplicationCore::load_from_paths(paths.clone())
            .await
            .expect("fixture core");
        core.refresh_models().await.expect("model discovery");
        let model_id = core
            .snapshot()
            .await
            .models
            .into_iter()
            .next()
            .expect("discovered model")
            .id;
        scala_core::ModelProfilesStore::new(&paths)
            .update({
                let model_id = model_id.clone();
                move |state| {
                    state.create(
                        scala_core::ModelProfileId::new("fixture").expect("profile ID"),
                        "Fixture",
                        model_id,
                        scala_core::EngineId::new("llama.cpp").expect("engine ID"),
                    )?;
                    Ok(())
                }
            })
            .await
            .expect("model profile fixture");
        let registry = EngineRegistry::default();
        let packs = RuntimePackManager::new(
            &paths,
            registry.clone(),
            Vec::<Arc<dyn RuntimeCatalogProvider>>::new(),
        )
        .expect("runtime packs");
        let manager = RuntimeManager::initialize(
            core.clone(),
            registry,
            packs,
            Arc::new(TokioProcessSupervisor::default()),
            RuntimeManagerOptions::default(),
        )
        .await;
        (temporary, manager, model_id, core)
    }

    #[test]
    fn model_object_contains_only_the_supported_openai_fields() {
        let value = serde_json::to_value(ApiModel {
            id: "example".to_owned(),
            object: "model",
            owned_by: "scala-local".into(),
            created: 1_234_567_890,
        })
        .expect("serialize API model");
        let object = value.as_object().expect("API model object");

        assert_eq!(object.len(), 4);
        assert!(object.contains_key("id"));
        assert!(object.contains_key("created"));
        assert!(object.contains_key("object"));
        assert!(object.contains_key("owned_by"));
    }

    #[test]
    fn responses_and_chat_share_the_same_canonical_inference_shape() {
        let responses = super::responses::parse_request(json!({
            "model": "model",
            "input": [{"role": "user", "content": "hello"}],
            "max_output_tokens": 32,
            "top_p": 0.75,
            "stream": true
        }))
        .expect("Responses request")
        .normalized
        .inference_request()
        .expect("Model Profile ID");
        let chat = super::chat::parse_request(json!({
            "model": "model",
            "messages": [{"role": "user", "content": "hello"}],
            "max_completion_tokens": 32,
            "top_p": 0.75,
            "stream": true
        }))
        .expect("Chat request")
        .normalized
        .inference_request()
        .expect("Model Profile ID");

        assert_eq!(responses.model_profile_id, chat.model_profile_id);
        assert_eq!(responses.messages.len(), chat.messages.len());
        assert_eq!(responses.messages[0].role, chat.messages[0].role);
        assert_eq!(responses.messages[0].content, chat.messages[0].content);
        assert_eq!(responses.max_output_tokens, chat.max_output_tokens);
        assert_eq!(responses.generation_settings, chat.generation_settings);
        assert_eq!(responses.generation_settings.temperature, None);
        assert_eq!(responses.generation_settings.top_p, Some(0.75));
        assert_eq!(responses.stream, chat.stream);
    }

    #[tokio::test]
    async fn control_load_returns_accepted_loading_and_outlives_the_response() {
        let (_temporary, runtime, model_id, _core) = control_fixture().await;
        let router = control_routes(ControlApiState {
            runtime: Arc::clone(&runtime),
            token: Arc::from("fixture-token"),
            link: None,
        });
        let request = ControlLoadRequest {
            model_profile_id: scala_core::ModelProfileId::new("fixture").expect("profile ID"),
            runtime_id: None,
            invocation_settings: scala_core::SettingsPatch::default(),
        };
        let response = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            router.oneshot(
                Request::post(CONTROL_LOAD_PATH)
                    .header("authorization", "Bearer fixture-token")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&request).expect("load request JSON"),
                    ))
                    .expect("load request"),
            ),
        )
        .await
        .expect("load admission response")
        .expect("control response");
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let body = to_bytes(response.into_body(), 1024 * 1024)
            .await
            .expect("response body");
        let admitted: ControlStatus = serde_json::from_slice(&body).expect("control status");
        let admitted_backend = admitted.newest_backend().expect("admitted backend");
        assert_eq!(admitted_backend.lifecycle, BackendLifecycle::Loading);
        assert_eq!(&admitted_backend.model_id, &model_id);

        // The handler and response are gone, but the manager-owned task still
        // records its eventual failure in authoritative state.
        let failed = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                let status = runtime.status().await;
                if status
                    .newest_backend()
                    .is_some_and(|backend| backend.lifecycle == BackendLifecycle::Failed)
                {
                    return status;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("background load completion");
        assert_eq!(
            failed.newest_backend().expect("failed backend").generation,
            admitted_backend.generation
        );
        assert!(
            failed
                .newest_backend()
                .expect("failed backend")
                .failure
                .is_some()
        );
    }
}
