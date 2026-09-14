//! Observational Norted Link contracts. These never enter local authority stores.
use crate::{
    BackendLifecycle, BackendLoadProgress, BackendParallelism, BackendResidency, InferenceActivity,
};
use norted_core::{
    ArtifactFormat, ArtifactNativeIdentity, ModelArtifactProvenance, ModelId, ModelProfileId,
    ModelRole, RuntimeId,
};
use serde::{Deserialize, Serialize};

pub const LINK_VERSION: u32 = 1;
pub const LINK_SERVICE: &str = "norted.link.v1";
pub const CONTROL_LINK_PATH: &str = "/control/v1/link";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LinkSnapshot {
    pub enabled: bool,
    pub node_id: Option<String>,
    pub node_name: Option<String>,
    pub error: Option<String>,
    pub peers: Vec<LinkPeer>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinkPeer {
    pub node_id: String,
    pub name: String,
    pub reachable: bool,
    pub last_seen: i64,
    pub error: Option<String>,
    pub state: Option<NodeInventory>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeInventory {
    pub version: u32,
    pub node_id: String,
    pub name: String,
    pub server_version: String,
    pub hardware: String,
    pub engines: Vec<String>,
    pub models: Vec<LinkModel>,
    pub profiles: Vec<LinkProfile>,
    pub benchmarks: Vec<LinkBenchmark>,
    pub benchmark_error: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LinkModel {
    pub id: ModelId,
    pub display_name: String,
    pub format: ArtifactFormat,
    pub size_bytes: u64,
    pub created: i64,
    pub hash: Option<String>,
    pub architecture: Option<String>,
    pub context_length: Option<u64>,
    pub native_identity: Option<ArtifactNativeIdentity>,
    pub provenance: Option<ModelArtifactProvenance>,
    pub package_provenance: Option<serde_json::Value>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LinkProfile {
    pub id: ModelProfileId,
    pub display_name: String,
    pub model_id: ModelId,
    pub engine_id: String,
    pub role: ModelRole,
    pub installed: bool,
    pub backend: Option<LinkBackend>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LinkBackend {
    pub lifecycle: BackendLifecycle,
    pub generation: u64,
    pub residency: BackendResidency,
    pub context_length: Option<String>,
    pub parallel_requests: Option<BackendParallelism>,
    pub activities: Vec<InferenceActivity>,
    pub primary_lease_count: usize,
    pub last_used_unix: i64,
    pub engine_id: Option<String>,
    pub runtime_id: Option<RuntimeId>,
    pub runtime_version: Option<String>,
    pub runtime_variant: Option<String>,
    pub load_progress: Option<BackendLoadProgress>,
    pub failure: Option<String>,
    pub active_requests: usize,
    pub retiring: bool,
}
impl LinkProfile {
    pub fn usable(&self) -> bool {
        self.installed
            && self
                .backend
                .as_ref()
                .is_some_and(|b| b.lifecycle == BackendLifecycle::Running && !b.retiring)
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LinkControlRequest {
    pub node_id: String,
    pub profile_id: ModelProfileId,
    pub action: LinkAction,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkAction {
    Load,
    Unload,
}

/// The delimiter cannot occur in a local ModelProfileId. Full stable IDs avoid
/// prefix collisions and display-name changes.
pub fn qualified_alias(profile: &str, node: &str) -> String {
    format!("{profile}@{node}")
}

// Forwarded inference must acquire an already-running local backend. The scope
// ends with request admission, and never changes runtime selection or stores.
tokio::task_local! { pub static REQUIRE_LOADED: ModelProfileId; }

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LinkBenchmark {
    pub profile_id: ModelProfileId,
    pub run_id: String,
    pub status: String,
    pub started_unix_ms: u128,
    pub intelligence: Option<f64>,
    pub agentic: Option<f64>,
    pub coding: Option<f64>,
    pub speed: serde_json::Value,
}
