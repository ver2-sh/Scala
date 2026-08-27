use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::ModelId;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineRevision {
    pub engine_id: String,
    pub version: Option<String>,
    pub revision: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildProvenance {
    pub binary_path: PathBuf,
    pub source_repository: Option<String>,
    pub revision: Option<String>,
    pub build_options: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeProvenance {
    pub model_id: ModelId,
    pub model_hash: Option<String>,
    pub engine: EngineRevision,
    pub profile: String,
    pub process_id: Option<u32>,
}
