//! Public HTTP gateway. Responses API routing will be added behind this boundary.

use std::future::Future;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use norted_core::{ApplicationCore, RuntimePublisher, ServerState};
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
    #[error("could not publish runtime state: {0}")]
    Runtime(#[from] norted_core::CoreError),
}

pub struct ApiServer {
    core: Arc<ApplicationCore>,
    listener: TcpListener,
    address: SocketAddr,
    publisher: RuntimePublisher,
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
        let publisher = RuntimePublisher::publish(&core.paths, address)?;
        core.set_server_state(ServerState::Running {
            endpoint: format!("http://{address}"),
        })
        .await;
        Ok(Self {
            core,
            listener,
            address,
            publisher,
        })
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.address
    }

    pub async fn run(
        self,
        shutdown: impl Future<Output = ()> + Send + 'static,
    ) -> Result<(), ApiError> {
        let ApiServer {
            core,
            listener,
            mut publisher,
            ..
        } = self;
        let app = routes(ApiState {
            core: core.clone(),
            instance_id: publisher.instance_id().to_owned(),
        });
        let result = axum::serve(listener, app)
            .with_graceful_shutdown(shutdown)
            .await;
        publisher.cleanup();
        match result {
            Ok(()) => {
                core.set_server_state(ServerState::Stopped).await;
                Ok(())
            }
            Err(error) => {
                core.set_server_state(ServerState::Failed {
                    message: error.to_string(),
                })
                .await;
                Err(ApiError::Serve(error))
            }
        }
    }
}

#[derive(Clone)]
struct ApiState {
    core: Arc<ApplicationCore>,
    instance_id: String,
}

fn routes(state: ApiState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/v1/models", get(models))
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

async fn health(State(state): State<ApiState>) -> (StatusCode, Json<HealthResponse>) {
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

async fn models(State(state): State<ApiState>) -> Json<ModelList> {
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
