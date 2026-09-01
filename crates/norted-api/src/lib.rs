//! Public OpenAI-compatible gateway and private authenticated control listener.

mod auth;
mod chat;
mod error;
mod input;
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
use norted_core::{ApplicationCore, ModelProfilesStore, RuntimePublisher, ServerState};
use norted_engine::{
    CONTROL_LOAD_PATH, CONTROL_STATUS_PATH, CONTROL_UNLOAD_PATH, ControlErrorResponse,
    ControlLoadRequest, ControlStatus, RuntimeManager,
};
use serde::Serialize;
use tokio::net::TcpListener;
use uuid::Uuid;

pub use auth::{PublicAuth, PublicAuthVerifier};

const GATEWAY_SHUTDOWN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
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
    Runtime(#[from] norted_core::CoreError),
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
        let public = public_routes(
            PublicApiState {
                core: Arc::clone(&core),
                runtime: Arc::clone(&runtime),
                instance_id: publisher.instance_id().to_owned(),
            },
            public_auth,
        );
        let control = control_routes(ControlApiState {
            runtime: Arc::clone(&runtime),
            token: Arc::from(control_token),
        });
        let (shutdown_sender, shutdown_receiver) = tokio::sync::watch::channel(false);
        let mut servers = tokio::task::JoinSet::new();
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
}

fn public_routes(state: PublicApiState, auth: PublicAuth) -> Router {
    let openai_routes = Router::new()
        .route("/v1/models", get(models))
        .route("/v1/responses", post(responses::create))
        .route("/v1/chat/completions", post(chat::create))
        .layer(axum::extract::DefaultBodyLimit::max(
            MAX_INFERENCE_BODY_BYTES,
        ))
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
    owned_by: &'static str,
    created: i64,
}

async fn models(
    State(state): State<PublicApiState>,
) -> Result<Json<ModelList>, (StatusCode, String)> {
    let snapshot = state.core.snapshot().await;
    let profiles = ModelProfilesStore::new(&state.core.paths)
        .read()
        .await
        .map_err(|error| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("could not read Model Profiles: {error}"),
            )
        })?;
    Ok(Json(ModelList {
        object: "list",
        data: profiles
            .profiles
            .into_values()
            .map(|profile| ApiModel {
                id: profile.id.to_string(),
                object: "model",
                owned_by: "norted-user",
                created: snapshot
                    .models
                    .iter()
                    .find(|model| model.id == profile.model_id)
                    .map_or(0, |model| model.created),
            })
            .collect(),
    }))
}

#[derive(Clone)]
struct ControlApiState {
    runtime: Arc<RuntimeManager>,
    token: Arc<str>,
}

fn control_routes(state: ControlApiState) -> Router {
    Router::new()
        .route(CONTROL_STATUS_PATH, get(control_status))
        .route(CONTROL_LOAD_PATH, post(control_load))
        .route(CONTROL_UNLOAD_PATH, post(control_unload))
        .with_state(state)
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
                norted_engine::RuntimeError::ModelProfileNotFound(_)
                | norted_engine::RuntimeError::BoundModelMissing { .. }
                | norted_engine::RuntimeError::ModelNotFound(_) => StatusCode::NOT_FOUND,
                norted_engine::RuntimeError::AlreadyActive { .. }
                | norted_engine::RuntimeError::Busy(_) => StatusCode::CONFLICT,
                norted_engine::RuntimeError::Incompatible { .. } => {
                    StatusCode::UNPROCESSABLE_ENTITY
                }
                norted_engine::RuntimeError::EngineUnavailable(_) => {
                    StatusCode::SERVICE_UNAVAILABLE
                }
                norted_engine::RuntimeError::ShuttingDown => StatusCode::SERVICE_UNAVAILABLE,
                norted_engine::RuntimeError::StartupTimedOut(_) => StatusCode::GATEWAY_TIMEOUT,
                norted_engine::RuntimeError::StartupFailed(_) => StatusCode::BAD_GATEWAY,
                _ => StatusCode::INTERNAL_SERVER_ERROR,
            },
            message: error.to_string(),
        })
}

async fn control_unload(
    State(state): State<ControlApiState>,
    headers: HeaderMap,
) -> Result<Json<ControlStatus>, ControlApiError> {
    authorize(&headers, &state.token)?;
    state
        .runtime
        .unload()
        .await
        .map(Json)
        .map_err(|error| ControlApiError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: error.to_string(),
        })
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
    use norted_core::{AppPaths, ModelId};
    use norted_engine::{
        BackendLifecycle, EngineRegistry, RuntimeCatalogProvider, RuntimeManagerOptions,
        RuntimePackManager, TokioProcessSupervisor,
    };
    use serde_json::json;
    use tower::ServiceExt;

    async fn control_fixture() -> (tempfile::TempDir, Arc<RuntimeManager>, ModelId) {
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
        norted_core::ModelProfilesStore::new(&paths)
            .update({
                let model_id = model_id.clone();
                move |state| {
                    state.create(
                        norted_core::ModelProfileId::new("fixture").expect("profile ID"),
                        "Fixture",
                        model_id,
                        norted_core::EngineId::new("llama.cpp").expect("engine ID"),
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
            core,
            registry,
            packs,
            Arc::new(TokioProcessSupervisor::default()),
            RuntimeManagerOptions::default(),
        )
        .await;
        (temporary, manager, model_id)
    }

    #[test]
    fn model_object_contains_only_the_supported_openai_fields() {
        let value = serde_json::to_value(ApiModel {
            id: "example".to_owned(),
            object: "model",
            owned_by: "norted-local",
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
        let (_temporary, runtime, model_id) = control_fixture().await;
        let router = control_routes(ControlApiState {
            runtime: Arc::clone(&runtime),
            token: Arc::from("fixture-token"),
        });
        let request = ControlLoadRequest {
            model_profile_id: norted_core::ModelProfileId::new("fixture").expect("profile ID"),
            runtime_id: None,
            invocation_settings: norted_core::SettingsPatch::default(),
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
        assert_eq!(admitted.backend.lifecycle, BackendLifecycle::Loading);
        assert_eq!(admitted.backend.model_id.as_ref(), Some(&model_id));

        // The handler and response are gone, but the manager-owned task still
        // records its eventual failure in authoritative state.
        let failed = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                let status = runtime.status().await;
                if status.backend.lifecycle == BackendLifecycle::Failed {
                    return status;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("background load completion");
        assert_eq!(failed.backend.generation, admitted.backend.generation);
        assert!(failed.backend.failure.is_some());
    }
}
