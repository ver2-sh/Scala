//! Norted federation over Wayfinder named services. Owner facts stay on the owner.
use crate::{
    MAX_INFERENCE_BODY_BYTES, PublicApiState,
    error::OpenAiError,
    wayfinder::{self, Result},
};
use axum::{
    body::Body,
    extract::{Request, State},
    http::{HeaderMap, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use futures_util::{StreamExt, stream};
use norted_core::{ApplicationCore, ModelProfileId, ModelProfilesStore};
use norted_engine::{RuntimeManager, link::*};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::AsyncReadExt,
    net::{TcpListener, TcpStream},
    sync::{RwLock, Semaphore},
    task::JoinSet,
};
use tower::ServiceExt;

const STATE_LIMIT: usize = 1024 * 1024;
const REQUEST_LIMIT: usize = MAX_INFERENCE_BODY_BYTES + 32768;
const CHUNK: usize = 32768;
const STALE_SECONDS: i64 = 15;
const OPERATION_TIMEOUT: Duration = Duration::from_secs(3600);
const SURFACES: &[&str] = &[
    "/v1/chat/completions",
    "/v1/responses",
    "/v1/completions",
    "/v1/embeddings",
];

tokio::task_local! { static EXECUTION_PROFILE: ModelProfileId; }
pub(crate) fn execution_profile_id(
    alias: String,
) -> std::result::Result<ModelProfileId, norted_core::SettingsError> {
    EXECUTION_PROFILE
        .try_with(Clone::clone)
        .map(Ok)
        .unwrap_or_else(|_| ModelProfileId::new(alias))
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    version: u32,
    source: String,
    target: String,
    hops: u8,
    operation: Operation,
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
enum Operation {
    State,
    Control {
        profile_id: ModelProfileId,
        action: LinkAction,
    },
    Inference {
        profile_id: ModelProfileId,
        path: String,
        body: Value,
        headers: BTreeMap<String, String>,
    },
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResponseHead {
    version: u32,
    status: u16,
    headers: BTreeMap<String, String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Preface {
    version: u32,
    credential: String,
    source: String,
    target: String,
    service: String,
}

pub(crate) struct Link {
    core: Arc<ApplicationCore>,
    runtime: Arc<RuntimeManager>,
    dir: PathBuf,
    credential: String,
    http: reqwest::Client,
    hardware: RwLock<String>,
    snapshot: RwLock<LinkSnapshot>,
    slots: Arc<Semaphore>,
}

impl Link {
    pub(crate) fn new(
        core: Arc<ApplicationCore>,
        runtime: Arc<RuntimeManager>,
    ) -> Result<Arc<Self>> {
        Ok(Arc::new(Self {
            dir: wayfinder::data_dir(&core.config.link)?,
            core,
            runtime,
            credential: format!(
                "{}{}",
                uuid::Uuid::new_v4().simple(),
                uuid::Uuid::new_v4().simple()
            ),
            http: reqwest::Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .map_err(|e| e.to_string())?,
            hardware: RwLock::new(format!(
                "{} {}",
                std::env::consts::OS,
                std::env::consts::ARCH
            )),
            snapshot: RwLock::new(LinkSnapshot {
                enabled: true,
                ..Default::default()
            }),
            slots: Arc::new(Semaphore::new(16)),
        }))
    }
    pub(crate) async fn snapshot(&self) -> LinkSnapshot {
        let mut snapshot = self.snapshot.read().await.clone();
        for peer in &mut snapshot.peers {
            if crate::unix_timestamp() - peer.last_seen > STALE_SECONDS {
                peer.reachable = false;
                peer.error
                    .get_or_insert_with(|| "State is stale; waiting for owner".into());
            }
        }
        snapshot
    }
    async fn inventory(&self) -> Result<NodeInventory> {
        let identity = self.snapshot.read().await;
        let node_id = identity
            .node_id
            .clone()
            .ok_or("Wayfinder identity unavailable")?;
        let name = identity
            .node_name
            .clone()
            .ok_or("Wayfinder identity unavailable")?;
        drop(identity);
        let snapshot = self.core.snapshot().await;
        let status = self.runtime.status().await;
        let profiles = ModelProfilesStore::new(&self.core.paths)
            .read()
            .await
            .map_err(|e| e.to_string())?;
        let (benchmarks, benchmark_error) = match self.runtime.link_benchmarks().await {
            Ok(rows) => (rows, None),
            Err(error) => (Vec::new(), Some(error)),
        };
        Ok(NodeInventory {
            version: LINK_VERSION,
            node_id,
            name,
            server_version: env!("CARGO_PKG_VERSION").into(),
            benchmarks, benchmark_error,
            hardware: self.hardware.read().await.clone(),
            engines: status
                .engines
                .iter()
                .map(|e| e.identity.id.clone())
                .collect(),
            models: snapshot
                .models
                .iter()
                .map(|m| LinkModel {
                    id: m.id.clone(),
                    display_name: m.display_name.clone(),
                    format: m.format,
                    size_bytes: m.size_bytes,
                    created: m.created,
                    hash: m.hash.clone(),
                    architecture: m.architecture.clone(),
                    context_length: m.context_length,
                    native_identity: m.native_identity.clone(),
                    package_provenance: m.norted_package.as_ref().map(|p| json!({
                        "kind":p.kind, "schema":p.manifest_schema, "version":p.manifest_version,
                        "manifest_sha256":p.manifest_sha256, "primary_sha256":p.expected_primary_sha256,
                        "build_key":p.build_key, "master_id":p.master_id, "quant_recipe_key":p.quant_recipe_key,
                        "canonical_source_lineage_key":p.canonical_source_lineage_key
                    })),
                    provenance: m.provenance.clone().map(|mut p| {
                        if p.provider == "local_import" {
                            p.source = None;
                        }
                        p
                    }),
                })
                .collect(),
            profiles: profiles
                .profiles
                .values()
                .map(|p| LinkProfile {
                    id: p.id.clone(),
                    display_name: p.display_name.clone(),
                    model_id: p.model_id.clone(),
                    engine_id: p.engine_id.to_string(),
                    role: p.role,
                    installed: snapshot.models.iter().any(|m| m.id == p.model_id),
                    backend: status.backend(&p.id).map(|b| LinkBackend {
                        lifecycle: b.lifecycle,
                        generation: b.generation, residency: b.residency,
                        context_length: b.provenance.as_ref().and_then(|p| p.settings.effective.iter().find(|(id,_)| id.as_str().ends_with(".context_length")).map(|(_,v)| v.value.clone())),
                        parallel_requests: b.parallel_requests.clone(), activities: b.activities.clone(),
                        primary_lease_count: b.primary_lease_count, last_used_unix: b.last_used_unix,
                        engine_id: b.engine_id.clone(),
                        runtime_id: b.runtime_id.clone(),
                        runtime_version: b.runtime_version.clone(),
                        runtime_variant: b.runtime_variant.clone(),
                        load_progress: b.load_progress.clone(),
                        failure: b.failure.clone(),
                        active_requests: b.active_request_count,
                        retiring: b.retiring,
                    }),
                })
                .collect(),
        })
    }
    pub(crate) async fn run(
        self: Arc<Self>,
        listener: TcpListener,
        mut shutdown: tokio::sync::watch::Receiver<bool>,
    ) -> std::io::Result<()> {
        let address = listener.local_addr()?;
        let mut tasks = JoinSet::new();
        let refresh = self.clone();
        tasks.spawn(async move {
            let host = norted_engine::detect_host_capabilities().await;
            *refresh.hardware.write().await = format!(
                "{} {} · {}",
                host.platform,
                host.architecture,
                host.accelerators
                    .iter()
                    .filter_map(|gpu| gpu.name.clone())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            loop {
                if let Err(error) = refresh.refresh(address).await {
                    let mut snapshot = refresh.snapshot.write().await;
                    snapshot.error = Some(error);
                    for peer in &mut snapshot.peers {
                        peer.reachable = false;
                    }
                }
                tokio::time::sleep(Duration::from_secs(3)).await;
            }
        });
        loop {
            tokio::select! {
                _ = shutdown.changed() => break,
                Some(_) = tasks.join_next(), if tasks.len() > 1 => {},
                accepted = listener.accept() => {
                    let (stream, _) = accepted?;
                    let Ok(permit) = self.slots.clone().try_acquire_owned() else { continue; };
                    let link = self.clone();
                    tasks.spawn(async move {
                        let _permit = permit;
                        let _ = tokio::time::timeout(OPERATION_TIMEOUT, link.serve_connection(stream)).await;
                    });
                }
            }
        }
        tasks.abort_all();
        while tasks.join_next().await.is_some() {}
        let _ = wayfinder::control(&self.http, &self.dir, json!({"op":"unregister_service", "service":LINK_SERVICE, "credential":self.credential})).await;
        Ok(())
    }
    async fn refresh(&self, address: std::net::SocketAddr) -> Result<()> {
        let status: wayfinder::Status = serde_json::from_value(
            wayfinder::control(&self.http, &self.dir, json!({"op":"status"})).await?,
        )
        .map_err(|e| e.to_string())?;
        let local = status
            .nodes
            .iter()
            .find(|n| n.local)
            .ok_or("Missing Wayfinder identity")?;
        if !valid_node_id(&local.id) {
            return Err("Invalid Wayfinder node ID".into());
        }
        {
            let mut snapshot = self.snapshot.write().await;
            snapshot.node_id = Some(local.id.clone());
            snapshot.node_name = Some(local.name.clone());
            snapshot
                .peers
                .retain(|p| status.nodes.iter().any(|n| !n.local && n.id == p.node_id));
        }
        if status.conflict {
            return Err("Wayfinder membership conflict; federation unavailable".into());
        }
        wayfinder::control(&self.http, &self.dir, json!({"op":"register_service", "service":LINK_SERVICE, "address":address, "credential":self.credential})).await?;
        self.snapshot.write().await.error = None;
        let refresh = stream::iter(status.nodes.into_iter().filter(|n| !n.local).map(
            |node| async move {
                let result = if node.reachable {
                    tokio::time::timeout(
                        Duration::from_secs(4),
                        self.request_state(&node.id, Operation::State),
                    )
                    .await
                    .unwrap_or_else(|_| Err("Peer state refresh timed out".into()))
                } else {
                    Err("Wayfinder peer is unreachable".into())
                };
                let mut snapshot = self.snapshot.write().await;
                let known = snapshot.peers.iter().position(|p| p.node_id == node.id);
                // A member without a responding Norted service is not a Norted peer.
                match result {
                    Ok(state) => {
                        let peer = LinkPeer {
                            node_id: node.id,
                            name: node.name,
                            reachable: true,
                            last_seen: crate::unix_timestamp(),
                            error: None,
                            state: Some(state),
                        };
                        if let Some(index) = known {
                            snapshot.peers[index] = peer;
                        } else {
                            snapshot.peers.push(peer);
                        }
                    }
                    Err(error) => {
                        if let Some(index) = known {
                            snapshot.peers[index].reachable = false;
                            snapshot.peers[index].error = Some(error);
                        } else if error.contains("protocol version")
                            || error.contains("owner inventory")
                        {
                            snapshot.peers.push(LinkPeer {
                                node_id: node.id,
                                name: node.name,
                                reachable: false,
                                last_seen: 0,
                                error: Some(error),
                                state: None,
                            });
                        }
                    }
                }
                snapshot.peers.sort_by(|a, b| a.node_id.cmp(&b.node_id));
            },
        ))
        .buffer_unordered(8)
        .collect::<Vec<_>>();
        let _ = tokio::time::timeout(Duration::from_secs(10), refresh).await;
        Ok(())
    }
    async fn request(&self, target: &str, operation: Operation) -> Result<Response> {
        let permit = self
            .slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| "Norted Link request capacity reached")?;
        let source = self
            .snapshot
            .read()
            .await
            .node_id
            .clone()
            .ok_or("Wayfinder identity unavailable")?;
        let mut stream = tokio::time::timeout(
            Duration::from_secs(6),
            wayfinder::open(&self.dir, target, LINK_SERVICE),
        )
        .await
        .map_err(|_| "Peer unavailable before application dispatch")??;
        let envelope = Envelope {
            version: LINK_VERSION,
            source,
            target: target.into(),
            hops: 1,
            operation,
        };
        // Never retry from here: the peer may already have admitted the operation.
        tokio::time::timeout(
            Duration::from_secs(15),
            wayfinder::write_json(&mut stream, &envelope, REQUEST_LIMIT),
        )
        .await
        .map_err(|_| "Dispatch timed out; outcome unknown; inspect owner before retry")??;
        let head: ResponseHead = tokio::time::timeout(
            OPERATION_TIMEOUT,
            wayfinder::read_json(&mut stream, wayfinder::HEADER_LIMIT),
        )
        .await
        .map_err(|_| "Owner response timed out after dispatch; no retry")??;
        if head.version != LINK_VERSION {
            return Err("Norted Link protocol version mismatch".into());
        }
        let status = StatusCode::from_u16(head.status).map_err(|_| "Invalid owner status")?;
        let sse = head
            .headers
            .get("content-type")
            .is_some_and(|v| v.starts_with("text/event-stream"));
        let body = stream::unfold(Some((stream, permit)), move |state| async move {
            let (mut stream, permit) = state?;
            match tokio::time::timeout(OPERATION_TIMEOUT, wayfinder::read_frame(&mut stream, CHUNK))
                .await
            {
                Ok(Ok(bytes)) if bytes.is_empty() => None,
                Ok(Ok(bytes)) => Some((Ok::<_, std::io::Error>(bytes), Some((stream, permit)))),
                other => {
                    let error = match other {
                        Ok(Err(error)) => error,
                        _ => "Peer response timed out during stream; no retry".into(),
                    };
                    if sse {
                        let envelope = json!({"error":{"type":"server_error", "code":"link_stream_interrupted", "message":error}});
                        Some((
                            Ok(format!("\n\nevent: error\ndata: {envelope}\n\n").into_bytes()),
                            None,
                        ))
                    } else {
                        Some((Err(std::io::Error::other(error)), None))
                    }
                }
            }
        });
        let mut response = Response::new(Body::from_stream(body));
        *response.status_mut() = status;
        for (name, value) in head.headers {
            if matches!(
                name.as_str(),
                "content-type" | "cache-control" | "x-request-id"
            ) {
                response.headers_mut().insert(
                    axum::http::HeaderName::from_bytes(name.as_bytes())
                        .map_err(|e| e.to_string())?,
                    value.parse().map_err(|_| "Invalid owner response header")?,
                );
            } else {
                return Err("Invalid owner response header".into());
            }
        }
        Ok(response)
    }
    async fn request_state(&self, target: &str, operation: Operation) -> Result<NodeInventory> {
        let response = self.request(target, operation).await?;
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), STATE_LIMIT)
            .await
            .map_err(|e| e.to_string())?;
        if !status.is_success() {
            let value: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
            return Err(value["error"]["message"]
                .as_str()
                .unwrap_or("Owner rejected Link operation")
                .into());
        }
        let state: NodeInventory =
            serde_json::from_slice(&bytes).map_err(|_| "Malformed owner inventory")?;
        if state.version != LINK_VERSION
            || state.node_id != target
            || state.profiles.len() > 4096
            || state.models.len() > 4096
        {
            return Err("Incompatible or invalid owner inventory".into());
        }
        let mut profiles = BTreeSet::new();
        if state.profiles.iter().any(|p| {
            !profiles.insert(p.id.clone())
                || norted_core::EngineId::new(p.engine_id.clone()).is_err()
        }) {
            return Err("Invalid owner profile identity".into());
        }
        Ok(state)
    }
    pub(crate) async fn control(&self, request: LinkControlRequest) -> Result<NodeInventory> {
        let peer = self
            .snapshot()
            .await
            .peers
            .into_iter()
            .find(|p| p.node_id == request.node_id)
            .ok_or("Unknown Norted peer")?;
        if !peer.reachable {
            return Err("Owner unavailable; no operation dispatched".into());
        }
        let result = self
            .request_state(
                &request.node_id,
                Operation::Control {
                    profile_id: request.profile_id,
                    action: request.action,
                },
            )
            .await;
        if let Ok(state) = &result {
            let mut snapshot = self.snapshot.write().await;
            if let Some(peer) = snapshot
                .peers
                .iter_mut()
                .find(|p| p.node_id == request.node_id)
            {
                peer.state = Some(state.clone());
                peer.last_seen = crate::unix_timestamp();
            }
        }
        result
    }
    async fn serve_connection(&self, mut stream: TcpStream) -> Result<()> {
        let preface: Preface = tokio::time::timeout(
            Duration::from_secs(5),
            wayfinder::read_json(&mut stream, wayfinder::HEADER_LIMIT),
        )
        .await
        .map_err(|_| "Service preface timed out")??;
        let own_id = self
            .snapshot
            .read()
            .await
            .node_id
            .clone()
            .ok_or("Wayfinder identity unavailable")?;
        if preface.version != 1
            || preface.service != LINK_SERVICE
            || !crate::constant_time_equal(&preface.credential, &self.credential)
            || preface.target != own_id
            || preface.source == own_id
            || !valid_node_id(&preface.source)
        {
            return Err("Invalid authenticated service preface".into());
        }
        wayfinder::write_json(
            &mut stream,
            &json!({"version":1,"ready":true}),
            wayfinder::HEADER_LIMIT,
        )
        .await?;
        let envelope = tokio::time::timeout(
            Duration::from_secs(20),
            wayfinder::read_json::<Envelope>(&mut stream, REQUEST_LIMIT),
        )
        .await;
        let envelope = match envelope {
            Ok(Ok(envelope)) => envelope,
            _ => {
                return send_response(
                    &mut stream,
                    link_error(
                        StatusCode::BAD_REQUEST,
                        "Malformed or oversized Norted Link request",
                        "invalid_link_request",
                    )
                    .into_response(),
                )
                .await;
            }
        };
        if envelope.version != LINK_VERSION
            || envelope.hops != 1
            || envelope.source != preface.source
            || envelope.target != own_id
        {
            return send_response(&mut stream, link_error(StatusCode::BAD_REQUEST, "Invalid Norted Link version, identity, or hop count; forwarding loops are rejected", "invalid_link_route").into_response()).await;
        }
        let (mut read, mut write) = stream.split();
        let operation = async {
            let response = self.dispatch(envelope.operation, &preface.source).await;
            send_response_to(&mut write, response).await
        };
        let mut byte = [0];
        // There is exactly one request per service stream. Any further caller
        // data or disconnect cancels admission/inference and drops the body.
        tokio::select! { result = operation => result, _ = read.read(&mut byte) => Ok(()) }
    }
    async fn dispatch(&self, operation: Operation, source: &str) -> Response {
        match operation {
            Operation::State => inventory_response(self.inventory().await),
            Operation::Control { profile_id, action } => {
                let result = match action {
                    LinkAction::Load => self.runtime.start_load(profile_id).await,
                    LinkAction::Unload => self.runtime.unload(profile_id).await,
                };
                match result {
                    Ok(_) => inventory_response(self.inventory().await),
                    Err(error) => link_error(
                        StatusCode::BAD_GATEWAY,
                        error.to_string(),
                        "remote_control_failed",
                    )
                    .into_response(),
                }
            }
            Operation::Inference {
                profile_id,
                path,
                body,
                headers,
            } => {
                if !SURFACES.contains(&path.as_str())
                    || headers.keys().any(|name| {
                        !matches!(
                            name.as_str(),
                            "x-norted-session"
                                | "x-norted-role"
                                | "x-request-id"
                                | "x-client-request-id"
                        )
                    })
                {
                    return link_error(
                        StatusCode::BAD_REQUEST,
                        "Invalid Link inference surface or headers",
                        "invalid_link_request",
                    )
                    .into_response();
                }
                let alias = body.get("model").and_then(Value::as_str).unwrap_or("");
                let own_id = self
                    .snapshot
                    .read()
                    .await
                    .node_id
                    .clone()
                    .unwrap_or_default();
                if alias != profile_id.as_str()
                    && alias != qualified_alias(profile_id.as_str(), &own_id)
                {
                    return link_error(
                        StatusCode::BAD_REQUEST,
                        "Model does not match the exact hosted profile",
                        "invalid_link_route",
                    )
                    .into_response();
                }
                let bytes = match serde_json::to_vec(&body) {
                    Ok(b) if b.len() <= MAX_INFERENCE_BODY_BYTES => b,
                    _ => {
                        return link_error(
                            StatusCode::PAYLOAD_TOO_LARGE,
                            "Inference body exceeds 32 MiB",
                            "request_too_large",
                        )
                        .into_response();
                    }
                };
                let mut request = Request::builder()
                    .method("POST")
                    .uri(path)
                    .header("content-type", "application/json");
                for (name, value) in &headers {
                    if name != "x-norted-session" {
                        request = request.header(name, value);
                    }
                }
                // Independent callers' sessions cannot collide with local clients.
                request = request.header(
                    "x-norted-session",
                    format!(
                        "link-{}",
                        session_digest(
                            source,
                            headers
                                .get("x-norted-session")
                                .map(String::as_str)
                                .unwrap_or("default")
                        )
                    ),
                );
                let request = match request.body(Body::from(bytes)) {
                    Ok(r) => r,
                    Err(_) => {
                        return link_error(
                            StatusCode::BAD_REQUEST,
                            "Invalid forwarded request",
                            "invalid_link_request",
                        )
                        .into_response();
                    }
                };
                let state = PublicApiState {
                    core: self.core.clone(),
                    runtime: self.runtime.clone(),
                    instance_id: own_id,
                    link: None,
                };
                let router = crate::public_routes(state, crate::PublicAuth::disabled());
                EXECUTION_PROFILE
                    .scope(
                        profile_id.clone(),
                        REQUIRE_LOADED.scope(profile_id, async {
                            router
                                .oneshot(request)
                                .await
                                .unwrap_or_else(|never| match never {})
                        }),
                    )
                    .await
            }
        }
    }
}

fn valid_node_id(id: &str) -> bool {
    id.len() == 64
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn session_digest(source: &str, session: &str) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(format!("{source}:{session}")))
}
fn inventory_response(result: Result<NodeInventory>) -> Response {
    match result {
        Ok(state) => match serde_json::to_vec(&state) {
            Ok(bytes) if bytes.len() <= STATE_LIMIT => {
                ([("content-type", "application/json")], bytes).into_response()
            }
            _ => link_error(
                StatusCode::PAYLOAD_TOO_LARGE,
                "Owner inventory exceeds 1 MiB limit",
                "inventory_too_large",
            )
            .into_response(),
        },
        Err(error) => {
            link_error(StatusCode::SERVICE_UNAVAILABLE, error, "link_unavailable").into_response()
        }
    }
}
async fn send_response(stream: &mut TcpStream, response: Response) -> Result<()> {
    send_response_to(stream, response).await
}
async fn send_response_to<W: tokio::io::AsyncWrite + Unpin>(
    stream: &mut W,
    response: Response,
) -> Result<()> {
    use tokio::io::AsyncWriteExt;
    let headers = response
        .headers()
        .iter()
        .filter(|(n, _)| {
            matches!(
                n.as_str(),
                "content-type" | "cache-control" | "x-request-id"
            )
        })
        .filter_map(|(n, v)| Some((n.to_string(), v.to_str().ok()?.into())))
        .collect();
    let head = serde_json::to_vec(&ResponseHead {
        version: LINK_VERSION,
        status: response.status().as_u16(),
        headers,
    })
    .map_err(|e| e.to_string())?;
    if head.len() > wayfinder::HEADER_LIMIT {
        return Err("Response headers too large".into());
    }
    stream
        .write_u32(head.len() as u32)
        .await
        .map_err(|e| e.to_string())?;
    stream.write_all(&head).await.map_err(|e| e.to_string())?;
    let mut body = response.into_body().into_data_stream();
    while let Some(bytes) = body.next().await {
        let bytes = bytes.map_err(|e| e.to_string())?;
        for chunk in bytes.chunks(CHUNK) {
            stream
                .write_u32(chunk.len() as u32)
                .await
                .map_err(|e| e.to_string())?;
            stream.write_all(chunk).await.map_err(|e| e.to_string())?;
        }
    }
    stream.write_u32(0).await.map_err(|e| e.to_string())?;
    Ok(())
}
pub(crate) fn link_error(
    status: StatusCode,
    message: impl Into<String>,
    code: &'static str,
) -> OpenAiError {
    OpenAiError::link(status, message, code)
}

pub(crate) async fn route(
    State(state): State<PublicApiState>,
    request: Request,
    next: Next,
) -> Response {
    if request
        .headers()
        .keys()
        .any(|name| name.as_str().starts_with("x-norted-link-"))
    {
        return link_error(
            StatusCode::BAD_REQUEST,
            "Internal Link routing headers are not accepted from API clients",
            "invalid_link_route",
        )
        .into_response();
    }
    let Some(link) = state.link.as_ref() else {
        return next.run(request).await;
    };
    if request.method() != "POST" || !SURFACES.contains(&request.uri().path()) {
        return next.run(request).await;
    }
    if !request
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| {
            v.split(';').next().is_some_and(|t| {
                t.trim() == "application/json"
                    || (t.trim().starts_with("application/") && t.trim().ends_with("+json"))
            })
        })
    {
        return next.run(request).await;
    }
    let (parts, body) = request.into_parts();
    let bytes = match axum::body::to_bytes(body, MAX_INFERENCE_BODY_BYTES).await {
        Ok(bytes) => bytes,
        Err(_) => {
            return link_error(
                StatusCode::PAYLOAD_TOO_LARGE,
                "Inference body exceeds 32 MiB",
                "request_too_large",
            )
            .into_response();
        }
    };
    let value: Value = match serde_json::from_slice(&bytes) {
        Ok(value) => value,
        Err(_) => {
            return next
                .run(Request::from_parts(parts, Body::from(bytes)))
                .await;
        }
    };
    let Some(alias) = value.get("model").and_then(Value::as_str) else {
        return next
            .run(Request::from_parts(parts, Body::from(bytes)))
            .await;
    };
    match resolve(&state, alias).await {
        Ok(Target::Local(profile)) => {
            EXECUTION_PROFILE
                .scope(
                    profile,
                    next.run(Request::from_parts(parts, Body::from(bytes))),
                )
                .await
        }
        Ok(Target::Remote { node, profile }) => {
            if let Some(correlation) = parts.extensions.get::<crate::auth::RequestCorrelation>() {
                correlation.record_inference(
                    alias,
                    value
                        .get("stream")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                );
            }
            if let Err(error) = crate::inference_routing_context(&parts.headers) {
                return error.into_response();
            }
            let headers = forwarded_headers(&parts.headers);
            let operation = Operation::Inference {
                profile_id: profile,
                path: parts.uri.path().into(),
                body: value,
                headers,
            };
            match link.request(&node, operation).await {
                Ok(response) => response,
                Err(error) => link_error(StatusCode::BAD_GATEWAY, error, "link_transport_error")
                    .into_response(),
            }
        }
        Err(error) => error.into_response(),
    }
}
fn forwarded_headers(headers: &HeaderMap) -> BTreeMap<String, String> {
    [
        "x-norted-session",
        "x-norted-role",
        "x-request-id",
        "x-client-request-id",
    ]
    .into_iter()
    .filter_map(|name| Some((name.into(), headers.get(name)?.to_str().ok()?.into())))
    .collect()
}
pub(crate) enum Target {
    Local(ModelProfileId),
    Remote {
        node: String,
        profile: ModelProfileId,
    },
}
pub(crate) async fn resolve(
    state: &PublicApiState,
    alias: &str,
) -> std::result::Result<Target, OpenAiError> {
    let profiles = ModelProfilesStore::new(&state.core.paths)
        .read()
        .await
        .map_err(|e| {
            link_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                e.to_string(),
                "profile_store_error",
            )
        })?;
    let snapshot = match &state.link {
        Some(link) => link.snapshot().await,
        None => LinkSnapshot::default(),
    };
    let (name, selected_node) = match alias.split_once('@') {
        Some((name, node)) if valid_node_id(node) => (name, Some(node)),
        Some(_) => {
            return Err(link_error(
                StatusCode::BAD_REQUEST,
                "Qualified model must be <profile-id>@<full-wayfinder-node-id>",
                "invalid_model_alias",
            ));
        }
        None => (alias, None),
    };
    let profile = ModelProfileId::new(name).map_err(|_| {
        link_error(
            StatusCode::BAD_REQUEST,
            "Invalid Model Profile ID",
            "invalid_model_alias",
        )
    })?;
    let mut candidates = Vec::new();
    if profiles.profiles.contains_key(&profile)
        && selected_node.is_none_or(|node| snapshot.node_id.as_deref() == Some(node))
    {
        candidates.push((snapshot.node_id.clone().unwrap_or_default(), true, true));
    }
    for peer in &snapshot.peers {
        if selected_node.is_none_or(|node| node == peer.node_id)
            && let Some(p) = peer
                .state
                .as_ref()
                .and_then(|s| s.profiles.iter().find(|p| p.id == profile))
        {
            candidates.push((peer.node_id.clone(), false, peer.reachable && p.usable()));
        }
    }
    if candidates.len() > 1 {
        return Err(link_error(
            StatusCode::CONFLICT,
            format!(
                "Ambiguous Model Profile `{name}`. Select {}",
                candidates
                    .iter()
                    .map(|(id, _, _)| qualified_alias(name, id))
                    .collect::<Vec<_>>()
                    .join(" or ")
            ),
            "ambiguous_model",
        ));
    }
    match candidates.pop() {
        Some((_, true, _)) => Ok(Target::Local(profile)),
        Some((node, false, true)) => Ok(Target::Remote { node, profile }),
        Some((node, false, false)) => Err(link_error(
            StatusCode::SERVICE_UNAVAILABLE,
            format!(
                "Profile `{name}` on {node} is unreachable, stale, or not loaded; inspect/load it on its owner"
            ),
            "remote_profile_unavailable",
        )),
        None => Err(OpenAiError::model_not_found()),
    }
}
