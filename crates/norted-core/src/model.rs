use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
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

    pub fn compatible_engines(self) -> Vec<String> {
        match self {
            Self::Gguf => vec!["llama.cpp".into()],
            Self::Q27 => vec!["q27".into()],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelArtifact {
    pub id: ModelId,
    pub display_name: String,
    pub path: PathBuf,
    pub format: ArtifactFormat,
    pub size_bytes: u64,
    pub hash: Option<String>,
    pub architecture: Option<String>,
    pub context_length: Option<u64>,
    pub compatible_engines: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct ModelRegistry {
    artifacts: Vec<ModelArtifact>,
    warnings: Vec<String>,
}

impl ModelRegistry {
    pub fn discover(search_paths: &[PathBuf]) -> Self {
        let mut registry = Self::default();
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
                match entry.metadata() {
                    Ok(metadata) => registry.artifacts.push(ModelArtifact {
                        id: model_id(path, format),
                        display_name: path
                            .file_stem()
                            .and_then(|name| name.to_str())
                            .unwrap_or("Unnamed model")
                            .to_owned(),
                        path: path.to_path_buf(),
                        format,
                        size_bytes: metadata.len(),
                        hash: None,
                        architecture: None,
                        context_length: None,
                        compatible_engines: format.compatible_engines(),
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

    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }
}

fn model_id(path: &Path, format: ArtifactFormat) -> ModelId {
    let mut hasher = DefaultHasher::new();
    path.hash(&mut hasher);
    ModelId(format!("{}-{:016x}", format.as_str(), hasher.finish()))
}
