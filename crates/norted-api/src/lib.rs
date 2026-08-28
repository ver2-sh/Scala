//! Public OpenAI-compatible gateway and private authenticated control listener.

mod responses;

use std::future::Future;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use axum::extract::{Json, State, rejection::JsonRejection};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use norted_core::{ApplicationCore, RuntimePublisher, ServerState};
use norted_engine::{
    CONTROL_LOAD_PATH, CONTROL_STATUS_PATH, CONTROL_UNLOAD_PATH, ControlErrorResponse,
    ControlLoadRequest, ControlStatus, RuntimeManager,
};
use serde::Serialize;
use tokio::net::TcpListener;
use uuid::Uuid;

const GATEWAY_SHUTDOWN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

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
    publisher: RuntimePublisher,
}

impl ApiServer {
    pub async fn bind(
        core: Arc<ApplicationCore>,
        runtime: Arc<RuntimeManager>,
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
            mut publisher,
            ..
        } = self;
        let public = public_routes(PublicApiState {
            core: Arc::clone(&core),
            runtime: Arc::clone(&runtime),
            instance_id: publisher.instance_id().to_owned(),
        });
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

fn public_routes(state: PublicApiState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/v1/models", get(models))
        .route("/v1/responses", post(responses::create))
        .with_state(state)
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

async fn models(State(state): State<PublicApiState>) -> Json<ModelList> {
    let snapshot = state.core.snapshot().await;
    Json(ModelList {
        object: "list",
        data: snapshot
            .models
            .into_iter()
            .map(|model| ApiModel {
                id: model.id.0,
                object: "model",
                owned_by: "norted-local",
                created: model.created,
            })
            .collect(),
    })
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
) -> Result<Json<ControlStatus>, ControlApiError> {
    authorize(&headers, &state.token)?;
    let Json(request) = payload.map_err(|error| ControlApiError {
        status: StatusCode::BAD_REQUEST,
        message: format!("invalid load request: {}", error.body_text()),
    })?;
    state
        .runtime
        .load_with_settings(
            request.model_id,
            request.runtime_id,
            request.profile,
            request.settings,
        )
        .await
        .map(Json)
        .map_err(|error| ControlApiError {
            status: match error {
                norted_engine::RuntimeError::ModelNotFound(_) => StatusCode::NOT_FOUND,
                norted_engine::RuntimeError::AlreadyActive { .. }
                | norted_engine::RuntimeError::Busy(_) => StatusCode::CONFLICT,
                norted_engine::RuntimeError::Incompatible { .. } => {
                    StatusCode::UNPROCESSABLE_ENTITY
                }
                norted_engine::RuntimeError::EngineUnavailable(_) => {
                    StatusCode::SERVICE_UNAVAILABLE
                }
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
    use super::ApiModel;

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
}
