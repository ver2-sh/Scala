use std::collections::HashSet;
use std::fs::Metadata;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use walkdir::WalkDir;

#[derive(Debug, Clone, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ModelId(pub String);

impl std::fmt::Display for ModelId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ArtifactFormat {
    Gguf,
    Q27,
}

impl ArtifactFormat {
    pub fn from_path(path: &Path) -> Option<Self> {
        match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
            "gguf" => Some(Self::Gguf),
            "q27" => Some(Self::Q27),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Gguf => "gguf",
            Self::Q27 => "q27",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelArtifactProvenance {
    pub logical_id: Option<String>,
    pub source: Option<String>,
    pub revision: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelArtifact {
    pub id: ModelId,
    pub display_name: String,
    pub path: PathBuf,
    pub format: ArtifactFormat,
    pub size_bytes: u64,
    /// Last-modified time of the local artifact, expressed as Unix seconds.
    pub created: i64,
    pub hash: Option<String>,
    pub architecture: Option<String>,
    pub context_length: Option<u64>,
    pub provenance: Option<ModelArtifactProvenance>,
}

#[derive(Debug, Clone, Default)]
pub struct ModelRegistry {
    artifacts: Vec<ModelArtifact>,
    warnings: Vec<String>,
}

impl ModelRegistry {
    pub fn discover(search_paths: &[PathBuf]) -> Self {
        let mut registry = Self::default();
        let mut seen = HashSet::new();
        for root in search_paths {
            if !root.exists() {
                registry
                    .warnings
                    .push(format!("model path does not exist: {}", root.display()));
                continue;
            }
            for entry in WalkDir::new(root).follow_links(false) {
                let entry = match entry {
                    Ok(entry) => entry,
                    Err(error) => {
                        registry.warnings.push(error.to_string());
                        continue;
                    }
                };
                if !entry.file_type().is_file() {
                    continue;
                }
                let path = entry.path();
                let Some(format) = ArtifactFormat::from_path(path) else {
                    continue;
                };
                let canonical_path = match path.canonicalize() {
                    Ok(path) => path,
                    Err(error) => {
                        registry.warnings.push(format!(
                            "could not resolve model path {}: {error}",
                            path.display()
                        ));
                        continue;
                    }
                };
                let identity = canonical_identity(&canonical_path);
                if !seen.insert(identity.clone()) {
                    continue;
                }
                match entry.metadata() {
                    Ok(metadata) => registry.artifacts.push(ModelArtifact {
                        id: model_id(path, format, &identity, None),
                        display_name: path
                            .file_stem()
                            .and_then(|name| name.to_str())
                            .unwrap_or("Unnamed model")
                            .to_owned(),
                        path: canonical_path,
                        format,
                        size_bytes: metadata.len(),
                        created: artifact_timestamp(&metadata),
                        hash: None,
                        architecture: None,
                        context_length: None,
                        provenance: None,
                    }),
                    Err(error) => registry
                        .warnings
                        .push(format!("could not inspect {}: {error}", path.display())),
                }
            }
        }
        registry
            .artifacts
            .sort_by(|left, right| left.display_name.cmp(&right.display_name));
        registry
    }

    pub fn artifacts(&self) -> &[ModelArtifact] {
        &self.artifacts
    }

    pub fn get(&self, id: &ModelId) -> Option<&ModelArtifact> {
        self.artifacts.iter().find(|artifact| &artifact.id == id)
    }

    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }
}

fn model_id(
    path: &Path,
    format: ArtifactFormat,
    canonical_identity: &str,
    logical_identity: Option<&str>,
) -> ModelId {
    let readable = path
        .file_stem()
        .and_then(|name| name.to_str())
        .map(sanitize_id_part)
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "model".to_owned());
    let identity = logical_identity.unwrap_or(canonical_identity);
    let digest = Sha256::digest(format!(
        "norted-model-id-v1\0{}\0{identity}",
        format.as_str()
    ));
    ModelId(format!("{readable}-{}", hex_prefix(&digest, 12)))
}

fn canonical_identity(path: &Path) -> String {
    let identity = path.to_string_lossy().replace('\\', "/");
    if cfg!(windows) {
        identity.to_lowercase()
    } else {
        identity
    }
}

fn sanitize_id_part(value: &str) -> String {
    let mut output = String::new();
    let mut separator = false;
    for character in value.chars() {
        if character.is_ascii_alphanumeric() {
            output.push(character.to_ascii_lowercase());
            separator = false;
        } else if !separator && !output.is_empty() {
            output.push('-');
            separator = true;
        }
    }
    output.trim_end_matches('-').to_owned()
}

fn hex_prefix(bytes: &[u8], length: usize) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(length);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        if output.len() == length {
            break;
        }
        output.push(HEX[(byte & 0x0f) as usize] as char);
        if output.len() == length {
            break;
        }
    }
    output
}

fn artifact_timestamp(metadata: &Metadata) -> i64 {
    metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .and_then(|duration| i64::try_from(duration.as_secs()).ok())
        .unwrap_or(0)
}
