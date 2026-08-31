use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use norted_engine::RuntimeError;
use serde_json::{Value, json};

/// The single public error contract used by every OpenAI-compatible route.
///
/// Messages in this type are safe to return to remote clients. Internal errors
/// must be logged before they are mapped here.
#[derive(Debug)]
pub(crate) struct OpenAiError {
    pub(crate) status: StatusCode,
    message: String,
    kind: &'static str,
    parameter: Option<String>,
    code: &'static str,
}

impl OpenAiError {
    pub(crate) fn invalid(
        message: impl Into<String>,
        parameter: Option<impl Into<String>>,
        code: &'static str,
    ) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: message.into(),
            kind: "invalid_request_error",
            parameter: parameter.map(Into::into),
            code,
        }
    }

    pub(crate) fn unsupported(message: impl Into<String>, parameter: impl Into<String>) -> Self {
        Self::invalid(message, Some(parameter), "unsupported_value")
    }

    pub(crate) fn authentication() -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            message: "Invalid or missing API key.".to_owned(),
            kind: "authentication_error",
            parameter: None,
            code: "invalid_api_key",
        }
    }

    pub(crate) fn malformed_json(rejection: &JsonRejection) -> Self {
        if rejection.status() == StatusCode::PAYLOAD_TOO_LARGE {
            Self {
                status: StatusCode::PAYLOAD_TOO_LARGE,
                message: "The request body exceeds the 32 MiB limit.".to_owned(),
                kind: "invalid_request_error",
                parameter: None,
                code: "request_too_large",
            }
        } else {
            Self::invalid(
                "Malformed JSON request body.",
                None::<String>,
                "invalid_json",
            )
        }
    }

    pub(crate) fn streaming_backend_failure() -> Self {
        Self {
            status: StatusCode::BAD_GATEWAY,
            message: "The local inference backend failed while generating the response.".to_owned(),
            kind: "server_error",
            parameter: None,
            code: "backend_inference_error",
        }
    }

    pub(crate) fn envelope(&self) -> Value {
        json!({
            "error": self.error_object(),
        })
    }

    pub(crate) fn error_object(&self) -> Value {
        json!({
            "message": self.message,
            "type": self.kind,
            "param": self.parameter,
            "code": self.code,
        })
    }
}

impl IntoResponse for OpenAiError {
    fn into_response(self) -> Response {
        let authentication_error = self.status == StatusCode::UNAUTHORIZED;
        let mut response = (self.status, Json(self.envelope())).into_response();
        if authentication_error {
            response.headers_mut().insert(
                header::WWW_AUTHENTICATE,
                axum::http::HeaderValue::from_static("Bearer"),
            );
        }
        response
    }
}

pub(crate) fn runtime_error(error: RuntimeError) -> OpenAiError {
    tracing::warn!(%error, "public inference request could not be routed");
    match error {
        RuntimeError::ModelNotFound(_) => OpenAiError {
            status: StatusCode::NOT_FOUND,
            message: "The requested model does not exist in the local model registry.".to_owned(),
            kind: "invalid_request_error",
            parameter: Some("model".to_owned()),
            code: "model_not_found",
        },
        RuntimeError::ModelProfileNotLoaded(_) => OpenAiError {
            status: StatusCode::CONFLICT,
            message: "The requested Model Profile is not currently loaded.".to_owned(),
            kind: "invalid_request_error",
            parameter: Some("model".to_owned()),
            code: "model_not_loaded",
        },
        RuntimeError::BackendCrashed(_) | RuntimeError::InferenceUnavailable(_) => OpenAiError {
            status: StatusCode::SERVICE_UNAVAILABLE,
            message: "The local inference backend is unavailable.".to_owned(),
            kind: "server_error",
            parameter: None,
            code: "backend_unavailable",
        },
        RuntimeError::InferenceTimedOut(_) => OpenAiError {
            status: StatusCode::GATEWAY_TIMEOUT,
            message: "The local inference backend timed out while completing the request."
                .to_owned(),
            kind: "server_error",
            parameter: None,
            code: "backend_timeout",
        },
        RuntimeError::Inference(_) => OpenAiError {
            status: StatusCode::BAD_GATEWAY,
            message: "The local inference backend failed to complete the request.".to_owned(),
            kind: "server_error",
            parameter: None,
            code: "backend_inference_error",
        },
        RuntimeError::InvalidGenerationSettings(_) => OpenAiError {
            status: StatusCode::BAD_REQUEST,
            message: "The requested generation settings cannot be honored by the active engine."
                .to_owned(),
            kind: "invalid_request_error",
            parameter: None,
            code: "unsupported_generation_settings",
        },
        _ => OpenAiError {
            status: StatusCode::SERVICE_UNAVAILABLE,
            message: "The local inference runtime is not available for this request.".to_owned(),
            kind: "server_error",
            parameter: None,
            code: "backend_unavailable",
        },
    }
}

#[cfg(test)]
mod tests {
    use axum::Router;
    use axum::body::{Body, to_bytes};
    use axum::extract::{Json, rejection::JsonRejection};
    use axum::http::{StatusCode, header};
    use axum::response::IntoResponse;
    use axum::routing::post;
    use serde_json::Value;
    use tower::ServiceExt;

    use super::OpenAiError;
    use crate::MAX_INFERENCE_BODY_BYTES;

    #[test]
    fn authentication_error_is_sanitized_and_challenges_bearer() {
        let response = OpenAiError::authentication().into_response();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(response.headers()[header::WWW_AUTHENTICATE], "Bearer");
    }

    #[tokio::test]
    async fn oversized_json_has_a_clean_413_error_envelope() {
        async fn parse(
            payload: Result<Json<Value>, JsonRejection>,
        ) -> Result<Json<Value>, OpenAiError> {
            payload.map_err(|error| OpenAiError::malformed_json(&error))
        }

        let router =
            Router::new()
                .route("/", post(parse))
                .layer(axum::extract::DefaultBodyLimit::max(
                    MAX_INFERENCE_BODY_BYTES,
                ));
        let response = router
            .oneshot(
                axum::http::Request::post("/")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(vec![b'x'; MAX_INFERENCE_BODY_BYTES + 1]))
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
        let body = to_bytes(response.into_body(), 16 * 1024)
            .await
            .expect("error body");
        assert!(String::from_utf8_lossy(&body).contains("request_too_large"));
    }
}
