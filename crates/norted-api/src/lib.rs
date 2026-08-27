//! Public HTTP gateway. Responses API routing will be added behind this boundary.

use std::future::Future;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use norted_core::{ApplicationCore, ServerState};
use serde::Serialize;
use tokio::net::TcpListener;

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("could not bind API server to {address}: {source}")]
    Bind {
        address: SocketAddr,
        source: std::io::Error,
    },
    #[error("API server failed: {0}")]
    Serve(std::io::Error),
}

pub struct ApiServer {
    core: Arc<ApplicationCore>,
    listener: TcpListener,
    address: SocketAddr,
}

impl ApiServer {
    pub async fn bind(core: Arc<ApplicationCore>) -> Result<Self, ApiError> {
        core.set_server_state(ServerState::Starting).await;
        let address = SocketAddr::new(
            core.config
                .server
                .ip_addr()
                .map_err(|error| ApiError::Bind {
                    address: SocketAddr::from(([127, 0, 0, 1], core.config.server.port)),
                    source: std::io::Error::new(std::io::ErrorKind::InvalidInput, error),
                })?,
            core.config.server.port,
        );
        let listener = match TcpListener::bind(address).await {
            Ok(listener) => listener,
            Err(source) => {
                core.set_server_state(ServerState::Failed {
                    message: source.to_string(),
                })
                .await;
                return Err(ApiError::Bind { address, source });
            }
        };
        let address = listener.local_addr().map_err(ApiError::Serve)?;
        core.set_server_state(ServerState::Running {
            endpoint: format!("http://{address}"),
        })
        .await;
        Ok(Self {
            core,
            listener,
            address,
        })
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.address
    }

    pub async fn run(
        self,
        shutdown: impl Future<Output = ()> + Send + 'static,
    ) -> Result<(), ApiError> {
        let app = routes(self.core.clone());
        let result = axum::serve(self.listener, app)
            .with_graceful_shutdown(shutdown)
            .await;
        match result {
            Ok(()) => {
                self.core.set_server_state(ServerState::Stopped).await;
                Ok(())
            }
            Err(error) => {
                self.core
                    .set_server_state(ServerState::Failed {
                        message: error.to_string(),
                    })
                    .await;
                Err(ApiError::Serve(error))
            }
        }
    }
}

fn routes(core: Arc<ApplicationCore>) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/v1/models", get(models))
        .with_state(core)
}

#[derive(Serialize)]
struct HealthResponse {
    status: &'static str,
    server: &'static str,
    version: &'static str,
    model_count: usize,
}

async fn health(State(core): State<Arc<ApplicationCore>>) -> (StatusCode, Json<HealthResponse>) {
    let snapshot = core.snapshot().await;
    (
        StatusCode::OK,
        Json(HealthResponse {
            status: "ok",
            server: snapshot.server.label(),
            version: env!("CARGO_PKG_VERSION"),
            model_count: snapshot.models.len(),
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
}

async fn models(State(core): State<Arc<ApplicationCore>>) -> Json<ModelList> {
    let snapshot = core.snapshot().await;
    Json(ModelList {
        object: "list",
        data: snapshot
            .models
            .into_iter()
            .map(|model| ApiModel {
                id: model.id.0,
                object: "model",
                owned_by: "local",
            })
            .collect(),
    })
}
