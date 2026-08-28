use std::collections::HashSet;
use std::fs::Metadata;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use walkdir::WalkDir;

#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ModelId(pub String);

impl std::fmt::Display for ModelId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
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
    #[serde(default)]
    pub auxiliary_artifacts: Vec<AuxiliaryArtifact>,
}

impl std::fmt::Display for ArtifactFormat {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.as_str().fmt(formatter)
    }
}

impl std::str::FromStr for ArtifactFormat {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.to_ascii_lowercase().as_str() {
            "gguf" => Ok(Self::Gguf),
            "q27" => Ok(Self::Q27),
            _ => Err(format!(
                "unsupported artifact format `{value}`; expected gguf or q27"
            )),
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "role", content = "name")]
pub enum AuxiliaryArtifactRole {
    Tokenizer,
    Other(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuxiliaryArtifact {
    pub role: AuxiliaryArtifactRole,
    pub path: PathBuf,
    pub size_bytes: u64,
    pub hash: Option<String>,
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
                    Ok(metadata) => {
                        let auxiliary_artifacts = discover_auxiliary_artifacts(
                            &canonical_path,
                            format,
                            &mut registry.warnings,
                        );
                        registry.artifacts.push(ModelArtifact {
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
                            auxiliary_artifacts,
                        });
                    }
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

fn discover_auxiliary_artifacts(
    primary: &Path,
    format: ArtifactFormat,
    warnings: &mut Vec<String>,
) -> Vec<AuxiliaryArtifact> {
    if format != ArtifactFormat::Q27 {
        return Vec::new();
    }
    let Some(tokenizer) = q27_tokenizer_candidate(primary, warnings) else {
        return Vec::new();
    };
    let metadata = match std::fs::metadata(&tokenizer) {
        Ok(metadata) if metadata.is_file() => metadata,
        Ok(_) => {
            warnings.push(format!(
                "q27 tokenizer companion is not a file: {}",
                tokenizer.display()
            ));
            return Vec::new();
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(error) => {
            warnings.push(format!(
                "could not inspect q27 tokenizer companion {}: {error}",
                tokenizer.display()
            ));
            return Vec::new();
        }
    };
    let path = match tokenizer.canonicalize() {
        Ok(path) => path,
        Err(error) => {
            warnings.push(format!(
                "could not resolve q27 tokenizer companion {}: {error}",
                tokenizer.display()
            ));
            return Vec::new();
        }
    };
    if let Err(reason) = validate_q27_tokenizer_header(&path) {
        warnings.push(format!(
            "q27 tokenizer companion {} is invalid: {reason}",
            path.display()
        ));
        return Vec::new();
    }
    vec![AuxiliaryArtifact {
        role: AuxiliaryArtifactRole::Tokenizer,
        path,
        size_bytes: metadata.len(),
        hash: None,
    }]
}

fn q27_tokenizer_candidate(primary: &Path, warnings: &mut Vec<String>) -> Option<PathBuf> {
    let exact = primary.with_extension("tok");
    if exact.is_file() {
        return Some(exact);
    }
    let model_stem = primary.file_stem()?.to_str()?;
    let parent = primary.parent()?;
    let entries = match std::fs::read_dir(parent) {
        Ok(entries) => entries,
        Err(error) => {
            warnings.push(format!(
                "could not inspect tokenizer companions beside {}: {error}",
                primary.display()
            ));
            return None;
        }
    };
    let mut candidates = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extension.eq_ignore_ascii_case("tok"))
        })
        .filter_map(|path| {
            let stem = path.file_stem()?.to_str()?;
            model_stem
                .strip_prefix(stem)
                .is_some_and(|suffix| suffix.starts_with('-'))
                .then_some((stem.len(), path))
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| right.0.cmp(&left.0));
    let longest = candidates.first()?.0;
    let mut longest_candidates = candidates
        .into_iter()
        .take_while(|(length, _)| *length == longest)
        .map(|(_, path)| path);
    let candidate = longest_candidates.next()?;
    if longest_candidates.next().is_some() {
        warnings.push(format!(
            "q27 tokenizer companion is ambiguous for {}",
            primary.display()
        ));
        None
    } else {
        Some(candidate)
    }
}

fn validate_q27_tokenizer_header(path: &Path) -> Result<(), String> {
    let mut file = std::fs::File::open(path).map_err(|error| error.to_string())?;
    let mut header = [0_u8; 8];
    file.read_exact(&mut header)
        .map_err(|error| format!("could not read Q27T header: {error}"))?;
    if &header[..4] != b"Q27T" {
        return Err("expected Q27T magic".to_owned());
    }
    let version = u32::from_le_bytes(header[4..8].try_into().expect("four-byte version"));
    if version != 1 {
        return Err(format!(
            "unsupported tokenizer version {version}; expected 1"
        ));
    }
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::{ArtifactFormat, AuxiliaryArtifactRole, ModelRegistry};

    #[test]
    fn q27_primary_keeps_its_id_and_owns_the_tokenizer_companion() {
        let root =
            std::env::temp_dir().join(format!("norted-q27-companion-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).expect("model fixture directory");
        let model = root.join("example-q4s.q27");
        let tokenizer = root.join("example.tok");
        std::fs::write(&model, b"fixture").expect("q27 fixture");
        let mut tokenizer_bytes = b"Q27T".to_vec();
        tokenizer_bytes.extend_from_slice(&1_u32.to_le_bytes());
        std::fs::write(&tokenizer, tokenizer_bytes).expect("tokenizer fixture");

        let registry = ModelRegistry::discover(std::slice::from_ref(&root));
        assert_eq!(registry.artifacts().len(), 1);
        let artifact = &registry.artifacts()[0];
        assert_eq!(artifact.format, ArtifactFormat::Q27);
        assert_eq!(artifact.auxiliary_artifacts.len(), 1);
        assert_eq!(
            artifact.auxiliary_artifacts[0].role,
            AuxiliaryArtifactRole::Tokenizer
        );
        assert_eq!(
            artifact.auxiliary_artifacts[0].path,
            tokenizer.canonicalize().expect("canonical tokenizer")
        );

        std::fs::remove_dir_all(&root).expect("remove model fixture directory");
    }
}
