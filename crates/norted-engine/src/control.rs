use std::time::Duration;

use norted_core::{
    AppPaths, LoadProfileName, LoadSettingsPatch, ModelId, RuntimeId, observe_runtime_descriptor,
};
use reqwest::StatusCode;
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::ControlStatus;

pub const CONTROL_STATUS_PATH: &str = "/control/v1/status";
pub const CONTROL_LOAD_PATH: &str = "/control/v1/load";
pub const CONTROL_UNLOAD_PATH: &str = "/control/v1/unload";
const STATUS_TIMEOUT: Duration = Duration::from_secs(5);
const LOAD_TIMEOUT: Duration = Duration::from_secs(6 * 60);
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
    #[error("private control request failed: {0}")]
    Transport(String),
    #[error("private control request was rejected ({status}): {message}")]
    Rejected { status: StatusCode, message: String },
    #[error("private control response was invalid: {0}")]
    InvalidResponse(String),
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
        )
        .await
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
        self.send(
            self.client
                .post(format!("{}{}", self.endpoint, CONTROL_LOAD_PATH))
                .timeout(LOAD_TIMEOUT)
                .json(&ControlLoadRequest {
                    model_id,
                    runtime_id,
                    profile,
                    settings,
                }),
        )
        .await
    }

    pub async fn unload(&self) -> Result<ControlStatus, ControlClientError> {
        self.send(
            self.client
                .post(format!("{}{}", self.endpoint, CONTROL_UNLOAD_PATH))
                .timeout(UNLOAD_TIMEOUT),
        )
        .await
    }

    async fn send<T: DeserializeOwned>(
        &self,
        request: reqwest::RequestBuilder,
    ) -> Result<T, ControlClientError> {
        let response = request
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|error| ControlClientError::Transport(error.to_string()))?;
        let status = response.status();
        let body = response
            .bytes()
            .await
            .map_err(|error| ControlClientError::Transport(error.to_string()))?;
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
            .map_err(|error| ControlClientError::InvalidResponse(error.to_string()))
    }
}

fn loopback_endpoint(address: std::net::SocketAddr) -> String {
    if address.is_ipv6() {
        format!("http://[{}]:{}", address.ip(), address.port())
    } else {
        format!("http://{address}")
    }
}
