use std::sync::Arc;
use std::sync::Mutex;
use std::time::Instant;

use async_trait::async_trait;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use uuid::Uuid;

use crate::error::OpenAiError;

pub(crate) const X_REQUEST_ID: HeaderName = HeaderName::from_static("x-request-id");
pub(crate) const X_CLIENT_REQUEST_ID: HeaderName = HeaderName::from_static("x-client-request-id");
const MAX_CLIENT_REQUEST_ID_BYTES: usize = 512;
const MAX_BEARER_CREDENTIAL_BYTES: usize = 1_024;

/// Injectable public-key verification boundary.
///
/// Implementations may re-read a mutable key store on each call so creation and
/// revocation take effect without restarting the gateway.
#[async_trait]
pub trait PublicAuthVerifier: Send + Sync + 'static {
    async fn verify(&self, credential: &str) -> bool;
}

#[derive(Clone)]
pub enum PublicAuth {
    Disabled,
    Required(Arc<dyn PublicAuthVerifier>),
}

impl PublicAuth {
    pub fn disabled() -> Self {
        Self::Disabled
    }

    pub fn required(verifier: Arc<dyn PublicAuthVerifier>) -> Self {
        Self::Required(verifier)
    }
}

#[derive(Clone, Debug)]
pub(crate) struct RequestCorrelation {
    pub(crate) request_id: String,
    pub(crate) client_request_id: Option<String>,
    details: Arc<Mutex<RequestDetails>>,
}

#[derive(Clone, Debug, Default)]
struct RequestDetails {
    model: Option<String>,
    streaming: Option<bool>,
}

impl RequestCorrelation {
    pub(crate) fn record_inference(&self, model: &str, streaming: bool) {
        if let Ok(mut details) = self.details.lock() {
            details.model = Some(model.to_owned());
            details.streaming = Some(streaming);
        }
    }
}

pub(crate) async fn request_id_middleware(mut request: Request, next: Next) -> Response {
    let started = Instant::now();
    let method = request.method().clone();
    let route = request.uri().path().to_owned();
    let request_id = format!("req_{}", Uuid::new_v4().simple());
    let client_request_id = match validate_client_request_id(request.headers()) {
        Ok(value) => value,
        Err(error) => {
            let mut response = error.into_response();
            insert_request_id(&mut response, &request_id);
            tracing::info!(
                request_id,
                %method,
                %route,
                status = response.status().as_u16(),
                latency_ms = started.elapsed().as_millis(),
                "public API request completed"
            );
            return response;
        }
    };
    let correlation = RequestCorrelation {
        request_id: request_id.clone(),
        client_request_id,
        details: Arc::new(Mutex::new(RequestDetails::default())),
    };
    request.extensions_mut().insert(correlation.clone());
    let mut response = next.run(request).await;
    insert_request_id(&mut response, &request_id);
    let details = correlation
        .details
        .lock()
        .map(|details| details.clone())
        .unwrap_or_default();
    tracing::info!(
        request_id = %correlation.request_id,
        client_request_id = ?correlation.client_request_id,
        %method,
        %route,
        model = ?details.model,
        streaming = ?details.streaming,
        status = response.status().as_u16(),
        latency_ms = started.elapsed().as_millis(),
        "public API request completed"
    );
    response
}

pub(crate) async fn public_auth_middleware(
    State(auth): State<PublicAuth>,
    request: Request,
    next: Next,
) -> Response {
    let verifier = match &auth {
        PublicAuth::Disabled => return next.run(request).await,
        PublicAuth::Required(verifier) => verifier,
    };
    let Some(credential) = bearer_credential(request.headers()) else {
        return OpenAiError::authentication().into_response();
    };
    if !verifier.verify(credential).await {
        return OpenAiError::authentication().into_response();
    }
    next.run(request).await
}

fn validate_client_request_id(headers: &HeaderMap) -> Result<Option<String>, OpenAiError> {
    let mut values = headers.get_all(&X_CLIENT_REQUEST_ID).iter();
    let Some(value) = values.next() else {
        return Ok(None);
    };
    if values.next().is_some()
        || value.as_bytes().is_empty()
        || value.as_bytes().len() > MAX_CLIENT_REQUEST_ID_BYTES
        || !value.as_bytes().is_ascii()
    {
        return Err(OpenAiError::invalid(
            "`X-Client-Request-Id` must be a single non-empty ASCII value of at most 512 characters.",
            Some("X-Client-Request-Id"),
            "invalid_header",
        ));
    }
    let value = value.to_str().map_err(|_| {
        OpenAiError::invalid(
            "`X-Client-Request-Id` must contain only ASCII characters.",
            Some("X-Client-Request-Id"),
            "invalid_header",
        )
    })?;
    Ok(Some(value.to_owned()))
}

fn bearer_credential(headers: &HeaderMap) -> Option<&str> {
    let mut values = headers.get_all(header::AUTHORIZATION).iter();
    let value = values.next()?;
    if values.next().is_some() || !value.as_bytes().is_ascii() {
        return None;
    }
    let value = value.to_str().ok()?;
    let (scheme, credential) = value.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("Bearer")
        || credential.is_empty()
        || credential.len() > MAX_BEARER_CREDENTIAL_BYTES
        || credential
            .bytes()
            .any(|byte| byte.is_ascii_whitespace() || byte.is_ascii_control())
    {
        return None;
    }
    Some(credential)
}

fn insert_request_id(response: &mut Response, request_id: &str) {
    let value = HeaderValue::from_str(request_id).expect("generated request IDs are valid headers");
    response.headers_mut().insert(X_REQUEST_ID, value);
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use axum::Json;
    use axum::Router;
    use axum::body::{Body, to_bytes};
    use axum::http::{Request, StatusCode, header};
    use axum::middleware;
    use axum::response::{IntoResponse, Response};
    use axum::routing::post;
    use scala_core::ApiKeyStore;
    use serde_json::{Value, json};
    use tower::ServiceExt;

    use super::{
        PublicAuth, PublicAuthVerifier, X_CLIENT_REQUEST_ID, X_REQUEST_ID, public_auth_middleware,
        request_id_middleware,
    };

    struct ExactVerifier {
        calls: AtomicUsize,
    }

    #[derive(Clone)]
    struct StoreVerifier(ApiKeyStore);

    #[async_trait::async_trait]
    impl PublicAuthVerifier for StoreVerifier {
        async fn verify(&self, credential: &str) -> bool {
            self.0.verify(credential).await.unwrap_or(false)
        }
    }

    #[async_trait::async_trait]
    impl PublicAuthVerifier for ExactVerifier {
        async fn verify(&self, credential: &str) -> bool {
            self.calls.fetch_add(1, Ordering::Relaxed);
            credential == "scala_sk_valid"
        }
    }

    fn protected_router(verifier: Arc<ExactVerifier>) -> Router {
        Router::new()
            .route(
                "/v1/test",
                post(|_: Json<Value>| async { StatusCode::NO_CONTENT }),
            )
            .route_layer(middleware::from_fn_with_state(
                PublicAuth::required(verifier),
                public_auth_middleware,
            ))
            .layer(middleware::from_fn(request_id_middleware))
    }

    #[tokio::test]
    async fn missing_and_wrong_bearer_credentials_are_rejected_before_the_route() {
        let verifier = Arc::new(ExactVerifier {
            calls: AtomicUsize::new(0),
        });
        let router = protected_router(Arc::clone(&verifier));

        let missing = router
            .clone()
            .oneshot(
                Request::post("/v1/test")
                    .body(Body::from(vec![b'x'; 1024 * 1024]))
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(missing.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(missing.headers()[header::WWW_AUTHENTICATE], "Bearer");
        assert!(missing.headers().contains_key(&X_REQUEST_ID));
        assert_eq!(verifier.calls.load(Ordering::Relaxed), 0);

        let wrong = router
            .oneshot(
                Request::post("/v1/test")
                    .header(header::AUTHORIZATION, "Bearer wrong")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(wrong.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(verifier.calls.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn valid_bearer_credential_reaches_the_route() {
        let verifier = Arc::new(ExactVerifier {
            calls: AtomicUsize::new(0),
        });
        let response = protected_router(Arc::clone(&verifier))
            .oneshot(
                Request::post("/v1/test")
                    .header(header::AUTHORIZATION, "Bearer scala_sk_valid")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(json!({}).to_string()))
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert_eq!(verifier.calls.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn key_store_revocation_takes_effect_without_rebuilding_the_router() {
        let temporary = tempfile::tempdir().expect("temporary API-key store");
        let store = ApiKeyStore::from_data_dir(temporary.path());
        let created = store
            .create(Some("integration".to_owned()))
            .await
            .expect("create API key");
        let secret = created.secret().to_owned();
        let key_id = created.summary().key_id.clone();
        let router = Router::new()
            .route(
                "/v1/test",
                post(|_: Json<Value>| async { StatusCode::NO_CONTENT }),
            )
            .route_layer(middleware::from_fn_with_state(
                PublicAuth::required(Arc::new(StoreVerifier(store.clone()))),
                public_auth_middleware,
            ))
            .layer(middleware::from_fn(request_id_middleware));

        let request = || {
            Request::post("/v1/test")
                .header(header::AUTHORIZATION, format!("Bearer {secret}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from("{}"))
                .expect("request")
        };
        let accepted = router
            .clone()
            .oneshot(request())
            .await
            .expect("accepted response");
        assert_eq!(accepted.status(), StatusCode::NO_CONTENT);

        store.revoke(key_id).await.expect("revoke API key");
        let revoked = router.oneshot(request()).await.expect("revoked response");
        assert_eq!(revoked.status(), StatusCode::UNAUTHORIZED);
        assert!(revoked.headers().contains_key(&X_REQUEST_ID));
    }

    #[tokio::test]
    async fn request_id_is_server_generated_and_client_id_is_validated() {
        let router = Router::new()
            .route("/", post(|| async { ().into_response() }))
            .layer(middleware::from_fn(request_id_middleware));
        let response = router
            .clone()
            .oneshot(
                Request::post("/")
                    .header(&X_CLIENT_REQUEST_ID, "client-correlation")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        let request_id = response.headers()[&X_REQUEST_ID]
            .to_str()
            .expect("ASCII request ID");
        assert!(request_id.starts_with("req_"));
        assert_ne!(request_id, "client-correlation");

        let invalid = router
            .oneshot(
                Request::post("/")
                    .header(&X_CLIENT_REQUEST_ID, "x".repeat(513))
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);
        assert!(invalid.headers().contains_key(&X_REQUEST_ID));
        let body = to_bytes(invalid.into_body(), 16 * 1024)
            .await
            .expect("error body");
        assert!(String::from_utf8_lossy(&body).contains("invalid_header"));
    }

    #[tokio::test]
    async fn streaming_response_headers_include_the_server_request_id() {
        let router = Router::new()
            .route(
                "/stream",
                post(|| async {
                    Response::builder()
                        .header(header::CONTENT_TYPE, "text/event-stream")
                        .body(Body::from("data: [DONE]\n\n"))
                        .expect("streaming response")
                }),
            )
            .layer(middleware::from_fn(request_id_middleware));
        let response = router
            .oneshot(
                Request::post("/stream")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            "text/event-stream"
        );
        assert!(response.headers().contains_key(&X_REQUEST_ID));
    }
}
