use std::time::Duration;

use norted_core::{
    AppPaths, LoadProfileName, LoadSettingsPatch, ModelId, RuntimeId, observe_runtime_descriptor,
};
use reqwest::StatusCode;
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::{BackendLifecycle, ControlStatus};

pub const CONTROL_STATUS_PATH: &str = "/control/v1/status";
pub const CONTROL_LOAD_PATH: &str = "/control/v1/load";
pub const CONTROL_UNLOAD_PATH: &str = "/control/v1/unload";
const STATUS_TIMEOUT: Duration = Duration::from_secs(5);
const LOAD_ADMISSION_TIMEOUT: Duration = Duration::from_secs(15);
const LOAD_POLL_INTERVAL: Duration = Duration::from_millis(200);
const UNLOAD_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControlLoadRequest {
    pub model_id: ModelId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_id: Option<RuntimeId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<LoadProfileName>,
    #[serde(default, skip_serializing_if = "LoadSettingsPatch::is_empty")]
    pub settings: LoadSettingsPatch,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControlErrorResponse {
    pub error: String,
}

#[derive(Clone)]
pub struct ControlClient {
    client: reqwest::Client,
    endpoint: String,
    token: String,
}

impl std::fmt::Debug for ControlClient {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ControlClient")
            .field("endpoint", &self.endpoint)
            .field("token", &"<redacted>")
            .finish_non_exhaustive()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ControlClientError {
    #[error("no running Norted Server control instance is available")]
    Unavailable,
    #[error("could not discover a running Norted Server instance: {0}")]
    Discovery(String),
    #[error("could not create the private control client: {0}")]
    Client(String),
    #[error("private control {operation} timed out after {timeout_seconds}s")]
    Timeout {
        operation: &'static str,
        timeout_seconds: u64,
    },
    #[error("private control connection failed during {operation}: {message}")]
    Connection {
        operation: &'static str,
        message: String,
    },
    #[error("private control response body failed during {operation}: {message}")]
    Body {
        operation: &'static str,
        message: String,
    },
    #[error("private control transport failed during {operation}: {message}")]
    Transport {
        operation: &'static str,
        message: String,
    },
    #[error("private control request was rejected ({status}): {message}")]
    Rejected { status: StatusCode, message: String },
    #[error("private control response was invalid: {0}")]
    InvalidResponse(String),
    #[error("model load failed: {0}")]
    LoadFailed(String),
    #[error("model load did not complete: {0}")]
    LoadCancelled(String),
}

impl ControlClient {
    pub async fn discover(paths: &AppPaths) -> Result<Self, ControlClientError> {
        let descriptor = observe_runtime_descriptor(paths)
            .await
            .map_err(|error| ControlClientError::Discovery(error.to_string()))?
            .ok_or(ControlClientError::Unavailable)?;
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(2))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| ControlClientError::Client(error.to_string()))?;
        Ok(Self {
            client,
            endpoint: loopback_endpoint(descriptor.control_address),
            token: descriptor.control_token,
        })
    }

    pub async fn status(&self) -> Result<ControlStatus, ControlClientError> {
        self.send(
            self.client
                .get(format!("{}{}", self.endpoint, CONTROL_STATUS_PATH))
                .timeout(STATUS_TIMEOUT),
            "status request",
            STATUS_TIMEOUT,
        )
        .await
        .map(|(_, status)| status)
    }

    pub async fn load(&self, model_id: ModelId) -> Result<ControlStatus, ControlClientError> {
        self.load_with_runtime(model_id, None).await
    }

    pub async fn load_with_runtime(
        &self,
        model_id: ModelId,
        runtime_id: Option<RuntimeId>,
    ) -> Result<ControlStatus, ControlClientError> {
        self.load_with_settings(model_id, runtime_id, None, LoadSettingsPatch::default())
            .await
    }

    pub async fn load_with_settings(
        &self,
        model_id: ModelId,
        runtime_id: Option<RuntimeId>,
        profile: Option<LoadProfileName>,
        settings: LoadSettingsPatch,
    ) -> Result<ControlStatus, ControlClientError> {
        let admitted = self
            .start_load_with_settings(model_id.clone(), runtime_id, profile, settings)
            .await?;
        self.wait_for_admitted_load(model_id, admitted.backend.generation)
            .await
    }

    pub async fn start_load(&self, model_id: ModelId) -> Result<ControlStatus, ControlClientError> {
        self.start_load_with_runtime(model_id, None).await
    }

    pub async fn start_load_with_runtime(
        &self,
        model_id: ModelId,
        runtime_id: Option<RuntimeId>,
    ) -> Result<ControlStatus, ControlClientError> {
        self.start_load_with_settings(model_id, runtime_id, None, LoadSettingsPatch::default())
            .await
    }

    pub async fn start_load_with_settings(
        &self,
        model_id: ModelId,
        runtime_id: Option<RuntimeId>,
        profile: Option<LoadProfileName>,
        settings: LoadSettingsPatch,
    ) -> Result<ControlStatus, ControlClientError> {
        let expected_model = model_id.clone();
        let (response_status, status) = self
            .send::<ControlStatus>(
                self.client
                    .post(format!("{}{}", self.endpoint, CONTROL_LOAD_PATH))
                    .timeout(LOAD_ADMISSION_TIMEOUT)
                    .json(&ControlLoadRequest {
                        model_id,
                        runtime_id,
                        profile,
                        settings,
                    }),
                "load admission",
                LOAD_ADMISSION_TIMEOUT,
            )
            .await?;
        if response_status != StatusCode::ACCEPTED {
            return Err(ControlClientError::InvalidResponse(format!(
                "load admission returned {response_status}, expected 202 Accepted"
            )));
        }
        if status.backend.lifecycle != BackendLifecycle::Loading
            || status.backend.model_id.as_ref() != Some(&expected_model)
        {
            return Err(ControlClientError::InvalidResponse(
                "accepted load response did not identify the requested model as Loading".to_owned(),
            ));
        }
        Ok(status)
    }

    pub async fn unload(&self) -> Result<ControlStatus, ControlClientError> {
        self.send(
            self.client
                .post(format!("{}{}", self.endpoint, CONTROL_UNLOAD_PATH))
                .timeout(UNLOAD_TIMEOUT),
            "unload request",
            UNLOAD_TIMEOUT,
        )
        .await
        .map(|(_, status)| status)
    }

    async fn send<T: DeserializeOwned>(
        &self,
        request: reqwest::RequestBuilder,
        operation: &'static str,
        timeout: Duration,
    ) -> Result<(StatusCode, T), ControlClientError> {
        let response = request
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|error| transport_error(error, operation, timeout))?;
        let status = response.status();
        let body = response.bytes().await.map_err(|error| {
            if error.is_timeout() {
                ControlClientError::Timeout {
                    operation,
                    timeout_seconds: timeout.as_secs(),
                }
            } else {
                ControlClientError::Body {
                    operation,
                    message: error.to_string(),
                }
            }
        })?;
        if !status.is_success() {
            let message = serde_json::from_slice::<ControlErrorResponse>(&body)
                .map(|response| response.error)
                .unwrap_or_else(|_| {
                    status
                        .canonical_reason()
                        .unwrap_or("request failed")
                        .to_owned()
                });
            return Err(ControlClientError::Rejected { status, message });
        }
        serde_json::from_slice(&body)
            .map(|parsed| (status, parsed))
            .map_err(|error| ControlClientError::InvalidResponse(error.to_string()))
    }

    async fn wait_for_admitted_load(
        &self,
        model_id: ModelId,
        generation: u64,
    ) -> Result<ControlStatus, ControlClientError> {
        loop {
            let status = self.status().await?;
            if status.backend.generation != generation {
                return Err(ControlClientError::LoadCancelled(format!(
                    "load generation {generation} for model `{model_id}` was superseded by generation {}",
                    status.backend.generation
                )));
            }
            if status
                .backend
                .model_id
                .as_ref()
                .is_some_and(|observed| observed != &model_id)
            {
                return Err(ControlClientError::LoadCancelled(format!(
                    "load generation {generation} changed from model `{model_id}` to `{}`",
                    status
                        .backend
                        .model_id
                        .as_ref()
                        .expect("checked as present")
                )));
            }
            match status.backend.lifecycle {
                BackendLifecycle::Loading => {
                    tokio::time::sleep(LOAD_POLL_INTERVAL).await;
                }
                BackendLifecycle::Running => return Ok(status),
                BackendLifecycle::Failed => {
                    return Err(ControlClientError::LoadFailed(
                        status.backend.failure.unwrap_or_else(|| {
                            format!(
                                "load generation {generation} for model `{model_id}` failed without a reason"
                            )
                        }),
                    ));
                }
                BackendLifecycle::Stopping => {
                    return Err(ControlClientError::LoadCancelled(format!(
                        "load generation {generation} for model `{model_id}` was cancelled and is stopping"
                    )));
                }
                BackendLifecycle::Stopped => {
                    return Err(ControlClientError::LoadCancelled(format!(
                        "load generation {generation} for model `{model_id}` stopped before reaching Running"
                    )));
                }
            }
        }
    }
}

fn transport_error(
    error: reqwest::Error,
    operation: &'static str,
    timeout: Duration,
) -> ControlClientError {
    if error.is_timeout() {
        ControlClientError::Timeout {
            operation,
            timeout_seconds: timeout.as_secs(),
        }
    } else if error.is_connect() {
        ControlClientError::Connection {
            operation,
            message: error.to_string(),
        }
    } else if error.is_body() || error.is_decode() {
        ControlClientError::Body {
            operation,
            message: error.to_string(),
        }
    } else {
        ControlClientError::Transport {
            operation,
            message: error.to_string(),
        }
    }
}

fn loopback_endpoint(address: std::net::SocketAddr) -> String {
    if address.is_ipv6() {
        format!("http://[{}]:{}", address.ip(), address.port())
    } else {
        format!("http://{address}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BackendStatus, ControlStatus};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    fn status(
        generation: u64,
        lifecycle: BackendLifecycle,
        failure: Option<&str>,
    ) -> ControlStatus {
        ControlStatus {
            public_endpoint: None,
            available_engine_count: 0,
            installed_engine_count: 0,
            running_engine_count: usize::from(lifecycle == BackendLifecycle::Running),
            engines: Vec::new(),
            backend: BackendStatus {
                generation,
                lifecycle,
                model_id: Some(ModelId("fixture".to_owned())),
                engine_id: None,
                runtime_id: None,
                runtime_version: None,
                runtime_variant: None,
                runtime_executable_sha256: None,
                process_id: None,
                private_endpoint: None,
                load_progress: None,
                failure: failure.map(str::to_owned),
                provenance: None,
            },
            recent_events: Vec::new(),
        }
    }

    async fn client_with_responses(
        responses: Vec<(StatusCode, ControlStatus)>,
    ) -> (ControlClient, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
        let address = listener.local_addr().expect("listener address");
        let server = tokio::spawn(async move {
            for (status_code, body) in responses {
                let (mut stream, _) = listener.accept().await.expect("request");
                let mut request = vec![0_u8; 16 * 1024];
                let _ = stream.read(&mut request).await.expect("read request");
                let body = serde_json::to_vec(&body).expect("response JSON");
                let reason = status_code.canonical_reason().unwrap_or("response");
                let response = format!(
                    "HTTP/1.1 {} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    status_code.as_u16(),
                    body.len()
                );
                stream
                    .write_all(response.as_bytes())
                    .await
                    .expect("write headers");
                stream.write_all(&body).await.expect("write body");
            }
        });
        let client = ControlClient {
            client: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .expect("client"),
            endpoint: loopback_endpoint(address),
            token: "private-test-token".to_owned(),
        };
        (client, server)
    }

    #[tokio::test]
    async fn start_load_returns_on_accepted_admission() {
        let (client, server) = client_with_responses(vec![(
            StatusCode::ACCEPTED,
            status(12, BackendLifecycle::Loading, None),
        )])
        .await;
        let admitted = tokio::time::timeout(
            Duration::from_secs(1),
            client.start_load(ModelId("fixture".to_owned())),
        )
        .await
        .expect("short-lived admission")
        .expect("accepted load");

        assert_eq!(admitted.backend.lifecycle, BackendLifecycle::Loading);
        assert_eq!(admitted.backend.generation, 12);
        server.await.expect("server task");
    }

    #[tokio::test]
    async fn blocking_load_polls_until_its_generation_is_running() {
        let (client, server) = client_with_responses(vec![
            (
                StatusCode::ACCEPTED,
                status(22, BackendLifecycle::Loading, None),
            ),
            (StatusCode::OK, status(22, BackendLifecycle::Loading, None)),
            (StatusCode::OK, status(22, BackendLifecycle::Running, None)),
        ])
        .await;
        let final_status = client
            .load(ModelId("fixture".to_owned()))
            .await
            .expect("blocking load");

        assert_eq!(final_status.backend.lifecycle, BackendLifecycle::Running);
        assert_eq!(final_status.backend.generation, 22);
        server.await.expect("server task");
    }

    #[tokio::test]
    async fn blocking_load_reports_the_authoritative_failure() {
        let (client, server) = client_with_responses(vec![
            (
                StatusCode::ACCEPTED,
                status(31, BackendLifecycle::Loading, None),
            ),
            (
                StatusCode::OK,
                status(31, BackendLifecycle::Failed, Some("exact engine failure")),
            ),
        ])
        .await;
        let error = client
            .load(ModelId("fixture".to_owned()))
            .await
            .expect_err("failed load");

        assert!(matches!(
            error,
            ControlClientError::LoadFailed(message) if message == "exact engine failure"
        ));
        server.await.expect("server task");
    }

    #[tokio::test]
    async fn blocking_load_rejects_a_later_generation_as_success() {
        let (client, server) = client_with_responses(vec![
            (
                StatusCode::ACCEPTED,
                status(40, BackendLifecycle::Loading, None),
            ),
            (StatusCode::OK, status(41, BackendLifecycle::Running, None)),
        ])
        .await;
        let error = client
            .load(ModelId("fixture".to_owned()))
            .await
            .expect_err("superseded load");

        assert!(matches!(error, ControlClientError::LoadCancelled(_)));
        server.await.expect("server task");
    }
}
