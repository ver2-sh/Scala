use std::collections::{HashMap, HashSet};
use std::fs::Metadata;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use walkdir::WalkDir;

use crate::norted_package::{
    PackageDirectory, discover_package_directory, norted_package_manifest_name,
};

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
    Ninfer,
}

impl ArtifactFormat {
    pub fn from_path(path: &Path) -> Option<Self> {
        match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
            "gguf" => Some(Self::Gguf),
            "q27" => Some(Self::Q27),
            "ninfer" => Some(Self::Ninfer),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Gguf => "gguf",
            Self::Q27 => "q27",
            Self::Ninfer => "ninfer",
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "format", content = "identity")]
pub enum ArtifactNativeIdentity {
    Gguf(GgufArtifactIdentity),
    Ninfer(NinferArtifactIdentity),
}

#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
pub struct GgufArtifactIdentity {
    pub version: u32,
    pub architecture: String,
    pub context_length: Option<u64>,
    pub expert_count: Option<u64>,
    pub expert_used_count: Option<u64>,
    #[serde(default)]
    pub rope_frequency_base: Option<String>,
    #[serde(default)]
    pub rope_scaling_factor: Option<String>,
    #[serde(default)]
    pub rope_scaling_factor_key: Option<String>,
    #[serde(default)]
    pub chat_template_sha256: Option<String>,
    pub tokenizer_metadata_sha256: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum GgufMetadataError {
    #[error("could not read GGUF metadata: {0}")]
    Io(#[from] std::io::Error),
    #[error("GGUF magic is not recognized")]
    InvalidMagic,
    #[error("GGUF version {0} is unsupported")]
    UnsupportedVersion(u32),
    #[error("GGUF metadata is malformed: {0}")]
    Malformed(String),
}

#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
pub struct NinferArtifactIdentity {
    pub container_version: u32,
    pub model_id: String,
    pub weights_id: String,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct NinferContainerMetadata {
    pub identity: NinferArtifactIdentity,
    pub metadata_bytes_read: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum NinferContainerError {
    #[error("could not read NInfer container metadata: {0}")]
    Io(#[from] std::io::Error),
    #[error("NInfer container magic is not recognized")]
    InvalidMagic,
    #[error("NInfer container version {0} is unsupported; version 2 is required")]
    UnsupportedVersion(u8),
    #[error("NInfer directory length must be between 1 and {maximum} bytes; observed {observed}")]
    InvalidDirectoryLength { observed: u64, maximum: u64 },
    #[error("NInfer directory range is truncated or overflows the file")]
    TruncatedDirectory,
    #[error("NInfer directory JSON is invalid: {0}")]
    InvalidDirectory(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelArtifactProvenance {
    /// Stable identity shared by every primary member of one atomic managed
    /// acquisition.
    pub acquisition_id: String,
    /// Acquisition mechanism (`huggingface` or `local_import`).
    pub provider: String,
    pub repository: Option<String>,
    pub logical_id: Option<String>,
    pub source: Option<String>,
    pub revision: Option<String>,
    pub remote_filename: Option<String>,
    pub acquired_at_unix: i64,
    pub size_bytes: u64,
    pub digest: Option<String>,
}

pub const MODEL_LIBRARY_RECEIPT_FILENAME: &str = ".norted-library.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelLibraryReceiptMember {
    pub path: PathBuf,
    pub format: ArtifactFormat,
    pub provenance: ModelArtifactProvenance,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelLibraryReceipt {
    pub schema_version: u32,
    pub acquisition_id: String,
    pub members: Vec<ModelLibraryReceiptMember>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_identity: Option<ArtifactNativeIdentity>,
    #[serde(default)]
    pub auxiliary_artifacts: Vec<AuxiliaryArtifact>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub norted_package: Option<crate::NortedPackageBinding>,
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
            "ninfer" => Ok(Self::Ninfer),
            _ => Err(format!(
                "unsupported artifact format `{value}`; expected gguf, q27, or ninfer"
            )),
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "role", content = "name")]
pub enum AuxiliaryArtifactRole {
    Manifest,
    Tokenizer,
    Projector,
    Sharp,
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
        let mut package_directories = HashMap::new();
        for root in search_paths {
            if !root.exists() {
                registry
                    .warnings
                    .push(format!("model path does not exist: {}", root.display()));
                continue;
            }
            for entry in WalkDir::new(root)
                .follow_links(false)
                .into_iter()
                .filter_entry(|entry| entry.file_name() != ".norted-staging")
            {
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
                let package = discover_package_for_artifact(
                    root,
                    &canonical_path,
                    format,
                    &mut package_directories,
                );
                let package_member = match package {
                    Some(PackageDirectory::Valid {
                        members: _,
                        ref suppressed,
                    }) if suppressed.contains(&canonical_path) => continue,
                    Some(PackageDirectory::Valid { ref members, .. }) => {
                        members.get(&canonical_path).cloned()
                    }
                    Some(PackageDirectory::Invalid { reason, claimed }) => {
                        if claimed
                            .as_ref()
                            .is_none_or(|claimed| claimed.contains(&canonical_path))
                        {
                            registry.warnings.push(format!(
                                "artifact {} was rejected because its claimed Norted package is invalid: {reason}",
                                canonical_path.display()
                            ));
                            continue;
                        }
                        None
                    }
                    None => None,
                };
                match entry.metadata() {
                    Ok(metadata) => {
                        let native_identity = if let Some(identity) = package_member
                            .as_ref()
                            .and_then(|member| member.native_identity.clone())
                        {
                            Some(identity)
                        } else if format == ArtifactFormat::Gguf {
                            inspect_gguf_metadata(&canonical_path)
                                .ok()
                                .map(ArtifactNativeIdentity::Gguf)
                        } else if format == ArtifactFormat::Ninfer {
                            match inspect_ninfer_container(&canonical_path) {
                                Ok(metadata) => {
                                    Some(ArtifactNativeIdentity::Ninfer(metadata.identity))
                                }
                                Err(error) => {
                                    registry.warnings.push(format!(
                                        "NInfer artifact {} was rejected: {error}",
                                        canonical_path.display()
                                    ));
                                    continue;
                                }
                            }
                        } else {
                            None
                        };
                        let auxiliary_artifacts = package_member
                            .as_ref()
                            .map(|member| member.auxiliary.clone())
                            .unwrap_or_else(|| {
                                discover_auxiliary_artifacts(
                                    &canonical_path,
                                    format,
                                    &mut registry.warnings,
                                )
                            });
                        let (architecture, context_length) = match &native_identity {
                            Some(ArtifactNativeIdentity::Gguf(identity)) => {
                                (Some(identity.architecture.clone()), identity.context_length)
                            }
                            _ => (None, None),
                        };
                        let provenance = read_library_receipt(
                            root,
                            &canonical_path,
                            format,
                            &mut registry.warnings,
                        );
                        let logical_identity = provenance
                            .as_ref()
                            .and_then(|value| value.logical_id.as_deref());
                        registry.artifacts.push(ModelArtifact {
                            id: model_id(path, format, &identity, logical_identity),
                            display_name: path
                                .file_stem()
                                .and_then(|name| name.to_str())
                                .unwrap_or("Unnamed model")
                                .to_owned(),
                            path: canonical_path,
                            format,
                            size_bytes: metadata.len(),
                            created: artifact_timestamp(&metadata),
                            architecture,
                            context_length,
                            // Receipt digests describe acquisition-time source
                            // verification. A current content digest is set only
                            // by the load-time package verification path.
                            hash: None,
                            provenance,
                            native_identity,
                            auxiliary_artifacts,
                            norted_package: package_member.map(|member| member.binding),
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

fn discover_package_for_artifact(
    search_root: &Path,
    artifact: &Path,
    format: ArtifactFormat,
    cache: &mut HashMap<(PathBuf, ArtifactFormat), Option<PackageDirectory>>,
) -> Option<PackageDirectory> {
    let search_root = search_root.canonicalize().ok()?;
    let mut directory = artifact.parent()?;
    while directory.starts_with(&search_root) {
        if directory
            .join(norted_package_manifest_name(format))
            .exists()
        {
            let package = cache
                .entry((directory.to_path_buf(), format))
                .or_insert_with(|| discover_package_directory(directory, format))
                .clone();
            match &package {
                Some(PackageDirectory::Valid {
                    members,
                    suppressed,
                }) if members.contains_key(artifact) || suppressed.contains(artifact) => {
                    return package;
                }
                Some(PackageDirectory::Invalid { claimed, .. })
                    if claimed
                        .as_ref()
                        .is_none_or(|claimed| claimed.contains(artifact)) =>
                {
                    return package;
                }
                _ => {}
            }
        }
        if directory == search_root {
            break;
        }
        directory = directory.parent()?;
    }
    None
}

const MAX_GGUF_METADATA_BYTES: u64 = 128 * 1024 * 1024;
const MAX_GGUF_METADATA_ENTRIES: u64 = 1_000_000;
const MAX_GGUF_STRING_BYTES: u64 = 64 * 1024 * 1024;
const MAX_GGUF_ARRAY_ITEMS: u64 = 16_000_000;

/// Reads only the bounded GGUF header/metadata region. Tensor payloads are
/// never mapped or hashed. The tokenizer digest covers the exact encoded
/// `tokenizer.ggml.*` metadata values and is used only as a conservative
/// speculative-model vocabulary compatibility proof.
pub fn inspect_gguf_metadata(path: &Path) -> Result<GgufArtifactIdentity, GgufMetadataError> {
    let file = std::fs::File::open(path)?;
    let mut reader = GgufReader {
        inner: std::io::BufReader::new(file),
        consumed: 0,
    };
    let magic = reader.bytes::<4>(None)?;
    if &magic != b"GGUF" {
        return Err(GgufMetadataError::InvalidMagic);
    }
    let version = reader.u32(None)?;
    if !(2..=3).contains(&version) {
        return Err(GgufMetadataError::UnsupportedVersion(version));
    }
    let _tensor_count = reader.u64(None)?;
    let metadata_count = reader.u64(None)?;
    if metadata_count > MAX_GGUF_METADATA_ENTRIES {
        return Err(GgufMetadataError::Malformed(format!(
            "metadata entry count {metadata_count} exceeds the safety limit"
        )));
    }

    let mut architecture = None;
    let mut architecture_numbers = HashMap::<String, u64>::new();
    let mut architecture_decimals = HashMap::<String, String>::new();
    let mut chat_template_sha256 = None;
    let mut tokenizer = Sha256::new();
    let mut tokenizer_fields = 0_u64;

    for _ in 0..metadata_count {
        let key = reader.string(None)?;
        let value_type = reader.u32(None)?;
        if key.starts_with("tokenizer.ggml.") {
            tokenizer.update((key.len() as u64).to_le_bytes());
            tokenizer.update(key.as_bytes());
            tokenizer.update(value_type.to_le_bytes());
            reader.value(value_type, None, Some(&mut tokenizer))?;
            tokenizer_fields += 1;
            continue;
        }
        let capture = key == "general.architecture"
            || key.ends_with(".context_length")
            || key.ends_with(".expert_count")
            || key.ends_with(".expert_used_count")
            || key.ends_with(".rope.freq_base")
            || key.ends_with(".rope.scaling.factor")
            || key.ends_with(".rope.scale_linear")
            || key == "tokenizer.chat_template";
        let value = reader.value(value_type, capture.then_some(&key), None)?;
        if key == "general.architecture" {
            architecture = value.and_then(GgufScalar::into_string);
        } else if key == "tokenizer.chat_template" {
            chat_template_sha256 = value.and_then(GgufScalar::into_string).map(|template| {
                let digest = Sha256::digest(template.as_bytes());
                format!("{digest:x}")
            });
        } else if key.ends_with(".rope.freq_base")
            || key.ends_with(".rope.scaling.factor")
            || key.ends_with(".rope.scale_linear")
        {
            if let Some(value) = value.and_then(GgufScalar::into_decimal_string) {
                architecture_decimals.insert(key, value);
            }
        } else if let Some(value) = value.and_then(GgufScalar::into_u64) {
            if key.ends_with(".context_length")
                || key.ends_with(".expert_count")
                || key.ends_with(".expert_used_count")
            {
                architecture_numbers.insert(key, value);
            }
        }
    }
    let architecture = architecture.ok_or_else(|| {
        GgufMetadataError::Malformed("missing string `general.architecture`".to_owned())
    })?;
    let context_length = architecture_numbers
        .get(&format!("{architecture}.context_length"))
        .copied();
    let expert_count = architecture_numbers
        .get(&format!("{architecture}.expert_count"))
        .copied();
    let expert_used_count = architecture_numbers
        .get(&format!("{architecture}.expert_used_count"))
        .copied();
    let rope_frequency_base = architecture_decimals
        .get(&format!("{architecture}.rope.freq_base"))
        .cloned();
    let rope_scaling_factor_key = [
        format!("{architecture}.rope.scaling.factor"),
        format!("{architecture}.rope.scale_linear"),
    ]
    .into_iter()
    .find(|key| architecture_decimals.contains_key(key));
    let rope_scaling_factor = rope_scaling_factor_key
        .as_ref()
        .and_then(|key| architecture_decimals.get(key))
        .cloned();
    Ok(GgufArtifactIdentity {
        version,
        architecture,
        context_length,
        expert_count,
        expert_used_count,
        rope_frequency_base,
        rope_scaling_factor,
        rope_scaling_factor_key,
        chat_template_sha256,
        tokenizer_metadata_sha256: (tokenizer_fields > 0)
            .then(|| format!("{:x}", tokenizer.finalize())),
    })
}

enum GgufScalar {
    Unsigned(u64),
    Signed(i64),
    Decimal(String),
    String(String),
}

impl GgufScalar {
    fn into_u64(self) -> Option<u64> {
        match self {
            Self::Unsigned(value) => Some(value),
            Self::Signed(value) => u64::try_from(value).ok(),
            Self::Decimal(_) | Self::String(_) => None,
        }
    }

    fn into_decimal_string(self) -> Option<String> {
        match self {
            Self::Unsigned(value) => Some(value.to_string()),
            Self::Signed(value) => Some(value.to_string()),
            Self::Decimal(value) => Some(value),
            Self::String(_) => None,
        }
    }

    fn into_string(self) -> Option<String> {
        match self {
            Self::String(value) => Some(value),
            Self::Unsigned(_) | Self::Signed(_) | Self::Decimal(_) => None,
        }
    }
}

struct GgufReader<R> {
    inner: R,
    consumed: u64,
}

impl<R: Read> GgufReader<R> {
    fn raw(
        &mut self,
        bytes: &mut [u8],
        mut digest: Option<&mut Sha256>,
    ) -> Result<(), GgufMetadataError> {
        self.consumed = self
            .consumed
            .checked_add(bytes.len() as u64)
            .ok_or_else(|| {
                GgufMetadataError::Malformed("metadata byte count overflowed".to_owned())
            })?;
        if self.consumed > MAX_GGUF_METADATA_BYTES {
            return Err(GgufMetadataError::Malformed(format!(
                "metadata exceeds the {MAX_GGUF_METADATA_BYTES}-byte safety limit"
            )));
        }
        self.inner.read_exact(bytes)?;
        if let Some(digest) = digest.as_mut() {
            digest.update(bytes);
        }
        Ok(())
    }

    fn bytes<const N: usize>(
        &mut self,
        digest: Option<&mut Sha256>,
    ) -> Result<[u8; N], GgufMetadataError> {
        let mut bytes = [0_u8; N];
        self.raw(&mut bytes, digest)?;
        Ok(bytes)
    }

    fn u32(&mut self, digest: Option<&mut Sha256>) -> Result<u32, GgufMetadataError> {
        Ok(u32::from_le_bytes(self.bytes(digest)?))
    }

    fn u64(&mut self, digest: Option<&mut Sha256>) -> Result<u64, GgufMetadataError> {
        Ok(u64::from_le_bytes(self.bytes(digest)?))
    }

    fn string(&mut self, digest: Option<&mut Sha256>) -> Result<String, GgufMetadataError> {
        let mut digest = digest;
        let length = self.u64(digest.as_deref_mut())?;
        if length > MAX_GGUF_STRING_BYTES {
            return Err(GgufMetadataError::Malformed(format!(
                "string length {length} exceeds the safety limit"
            )));
        }
        let length = usize::try_from(length)
            .map_err(|_| GgufMetadataError::Malformed("string length overflowed".to_owned()))?;
        let mut bytes = vec![0_u8; length];
        self.raw(&mut bytes, digest)?;
        String::from_utf8(bytes)
            .map_err(|_| GgufMetadataError::Malformed("metadata string is not UTF-8".to_owned()))
    }

    fn value(
        &mut self,
        value_type: u32,
        capture_key: Option<&str>,
        mut digest: Option<&mut Sha256>,
    ) -> Result<Option<GgufScalar>, GgufMetadataError> {
        let scalar = match value_type {
            0 => GgufScalar::Unsigned(u64::from(self.bytes::<1>(digest.as_deref_mut())?[0])),
            1 => GgufScalar::Signed(i64::from(i8::from_le_bytes(
                self.bytes::<1>(digest.as_deref_mut())?,
            ))),
            2 => GgufScalar::Unsigned(u64::from(u16::from_le_bytes(
                self.bytes(digest.as_deref_mut())?,
            ))),
            3 => GgufScalar::Signed(i64::from(i16::from_le_bytes(
                self.bytes(digest.as_deref_mut())?,
            ))),
            4 => GgufScalar::Unsigned(u64::from(self.u32(digest.as_deref_mut())?)),
            5 => GgufScalar::Signed(i64::from(i32::from_le_bytes(
                self.bytes(digest.as_deref_mut())?,
            ))),
            6 => {
                let value = f32::from_le_bytes(self.bytes::<4>(digest.as_deref_mut())?);
                GgufScalar::Decimal(value.to_string())
            }
            7 => GgufScalar::Unsigned(u64::from(self.bytes::<1>(digest.as_deref_mut())?[0])),
            8 => GgufScalar::String(self.string(digest.as_deref_mut())?),
            9 => {
                let element_type = self.u32(digest.as_deref_mut())?;
                let count = self.u64(digest.as_deref_mut())?;
                if count > MAX_GGUF_ARRAY_ITEMS {
                    return Err(GgufMetadataError::Malformed(format!(
                        "array item count {count} exceeds the safety limit"
                    )));
                }
                for _ in 0..count {
                    self.value(element_type, None, digest.as_deref_mut())?;
                }
                return Ok(None);
            }
            10 => GgufScalar::Unsigned(self.u64(digest.as_deref_mut())?),
            11 => GgufScalar::Signed(i64::from_le_bytes(self.bytes(digest.as_deref_mut())?)),
            12 => {
                let value = f64::from_le_bytes(self.bytes::<8>(digest)?);
                GgufScalar::Decimal(value.to_string())
            }
            other => {
                return Err(GgufMetadataError::Malformed(format!(
                    "unknown value type {other}"
                )));
            }
        };
        Ok(capture_key.map(|_| scalar))
    }
}

const NINFER_V2_MAGIC: [u8; 8] = [b'N', b'I', b'N', b'F', b'E', b'R', 0, 2];
const NINFER_PREFIX_BYTES: u64 = 16;
const NINFER_PAYLOAD_ALIGNMENT: u64 = 4096;
const MAX_NINFER_DIRECTORY_BYTES: u64 = 16 * 1024 * 1024;

pub fn inspect_ninfer_container(
    path: &Path,
) -> Result<NinferContainerMetadata, NinferContainerError> {
    let mut file = std::fs::File::open(path)?;
    let file_bytes = file.metadata()?.len();
    inspect_ninfer_reader(&mut file, file_bytes)
}

fn inspect_ninfer_reader(
    reader: &mut impl Read,
    file_bytes: u64,
) -> Result<NinferContainerMetadata, NinferContainerError> {
    let mut prefix = [0_u8; NINFER_PREFIX_BYTES as usize];
    reader.read_exact(&mut prefix)?;
    if prefix[..8] != NINFER_V2_MAGIC {
        if &prefix[..7] == b"NINFER\0" {
            return Err(NinferContainerError::UnsupportedVersion(prefix[7]));
        }
        return Err(NinferContainerError::InvalidMagic);
    }
    let json_bytes = u64::from_le_bytes(
        prefix[8..16]
            .try_into()
            .expect("the NInfer prefix contains an eight-byte directory length"),
    );
    if json_bytes == 0 || json_bytes > MAX_NINFER_DIRECTORY_BYTES {
        return Err(NinferContainerError::InvalidDirectoryLength {
            observed: json_bytes,
            maximum: MAX_NINFER_DIRECTORY_BYTES,
        });
    }
    let metadata_end = NINFER_PREFIX_BYTES
        .checked_add(json_bytes)
        .ok_or(NinferContainerError::TruncatedDirectory)?;
    let payload_offset = metadata_end
        .checked_add(NINFER_PAYLOAD_ALIGNMENT - 1)
        .map(|value| value / NINFER_PAYLOAD_ALIGNMENT * NINFER_PAYLOAD_ALIGNMENT)
        .ok_or(NinferContainerError::TruncatedDirectory)?;
    if metadata_end > file_bytes || payload_offset > file_bytes {
        return Err(NinferContainerError::TruncatedDirectory);
    }
    let json_length =
        usize::try_from(json_bytes).map_err(|_| NinferContainerError::TruncatedDirectory)?;
    let mut directory = vec![0_u8; json_length];
    reader.read_exact(&mut directory)?;
    let value: serde_json::Value = serde_json::from_slice(&directory)
        .map_err(|error| NinferContainerError::InvalidDirectory(error.to_string()))?;
    let identity = validate_ninfer_directory(&value, file_bytes - payload_offset)?;
    Ok(NinferContainerMetadata {
        identity,
        metadata_bytes_read: NINFER_PREFIX_BYTES + json_bytes,
    })
}

fn validate_ninfer_directory(
    value: &serde_json::Value,
    payload_bytes: u64,
) -> Result<NinferArtifactIdentity, NinferContainerError> {
    let root = value
        .as_object()
        .ok_or_else(|| invalid_ninfer_directory("root must be an object"))?;
    require_exact_keys(root, &["identity", "objects"], "root")?;
    let identity = root["identity"]
        .as_object()
        .ok_or_else(|| invalid_ninfer_directory("identity must be an object"))?;
    require_exact_keys(identity, &["model_id", "weights_id"], "identity")?;
    let model_id = nonempty_json_string(&identity["model_id"], "identity.model_id")?;
    let weights_id = nonempty_json_string(&identity["weights_id"], "identity.weights_id")?;
    let objects = root["objects"]
        .as_array()
        .filter(|objects| !objects.is_empty())
        .ok_or_else(|| invalid_ninfer_directory("objects must be a non-empty array"))?;
    let mut names = HashSet::with_capacity(objects.len());
    let mut cursor = 0_u64;
    for (index, object) in objects.iter().enumerate() {
        let object = object.as_object().ok_or_else(|| {
            invalid_ninfer_directory(format!("objects[{index}] must be an object"))
        })?;
        let kind = nonempty_json_string(
            object.get("kind").ok_or_else(|| {
                invalid_ninfer_directory(format!("objects[{index}].kind is missing"))
            })?,
            &format!("objects[{index}].kind"),
        )?;
        match kind.as_str() {
            "tensor" => {
                require_exact_keys(
                    object,
                    &[
                        "name", "kind", "shape", "format", "layout", "offset", "bytes",
                    ],
                    &format!("objects[{index}]"),
                )?;
                let shape = object["shape"].as_array().ok_or_else(|| {
                    invalid_ninfer_directory(format!("objects[{index}].shape must be an array"))
                })?;
                if shape
                    .iter()
                    .any(|dimension| dimension.as_u64().is_none_or(|value| value == 0))
                {
                    return Err(invalid_ninfer_directory(format!(
                        "objects[{index}].shape dimensions must be positive integers"
                    )));
                }
                nonempty_json_string(&object["format"], &format!("objects[{index}].format"))?;
                nonempty_json_string(&object["layout"], &format!("objects[{index}].layout"))?;
            }
            "resource" => {
                require_exact_keys(
                    object,
                    &["name", "kind", "encoding", "offset", "bytes"],
                    &format!("objects[{index}]"),
                )?;
                nonempty_json_string(&object["encoding"], &format!("objects[{index}].encoding"))?;
            }
            _ => {
                return Err(invalid_ninfer_directory(format!(
                    "objects[{index}].kind must be `tensor` or `resource`"
                )));
            }
        }
        let name = nonempty_json_string(&object["name"], &format!("objects[{index}].name"))?;
        if !names.insert(name) {
            return Err(invalid_ninfer_directory(format!(
                "objects[{index}].name is duplicated"
            )));
        }
        let offset = object["offset"].as_u64().ok_or_else(|| {
            invalid_ninfer_directory(format!("objects[{index}].offset must be an integer"))
        })?;
        let bytes = object["bytes"]
            .as_u64()
            .filter(|value| *value > 0)
            .ok_or_else(|| {
                invalid_ninfer_directory(format!(
                    "objects[{index}].bytes must be a positive integer"
                ))
            })?;
        let end = offset.checked_add(bytes).ok_or_else(|| {
            invalid_ninfer_directory(format!("objects[{index}] payload range overflows"))
        })?;
        if offset < cursor || end > payload_bytes {
            return Err(invalid_ninfer_directory(format!(
                "objects[{index}] payload range is unordered, overlapping, or outside the file"
            )));
        }
        cursor = end;
    }
    Ok(NinferArtifactIdentity {
        container_version: 2,
        model_id,
        weights_id,
    })
}

fn require_exact_keys(
    object: &serde_json::Map<String, serde_json::Value>,
    expected: &[&str],
    label: &str,
) -> Result<(), NinferContainerError> {
    if object.len() != expected.len() || expected.iter().any(|key| !object.contains_key(*key)) {
        return Err(invalid_ninfer_directory(format!(
            "{label} has missing or unexpected members"
        )));
    }
    Ok(())
}

fn nonempty_json_string(
    value: &serde_json::Value,
    label: &str,
) -> Result<String, NinferContainerError> {
    value
        .as_str()
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| invalid_ninfer_directory(format!("{label} must be a non-empty string")))
}

fn invalid_ninfer_directory(message: impl Into<String>) -> NinferContainerError {
    NinferContainerError::InvalidDirectory(message.into())
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

pub fn q27_tokenizer_candidate(primary: &Path, warnings: &mut Vec<String>) -> Option<PathBuf> {
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
    let candidates = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extension.eq_ignore_ascii_case("tok"))
        })
        .collect::<Vec<_>>();
    let filenames = candidates
        .iter()
        .filter_map(|path| path.file_name()?.to_str().map(str::to_owned))
        .collect::<Vec<_>>();
    let primary_name = primary.file_name()?.to_str()?;
    match select_q27_tokenizer_filename(primary_name, &filenames) {
        Ok(Some(filename)) => Some(parent.join(filename)),
        Ok(None) => None,
        Err(reason) => {
            warnings.push(format!("{reason} for {}", primary.display()));
            None
        }
    }
}

/// Selects the same tokenizer companion for local discovery and remote
/// acquisition. Exact stem matches win, followed by the longest unambiguous
/// prefix. A sole `.tok` file covers the common `MODEL.q27` + `TOKENIZER.tok`
/// upstream layout.
pub fn select_q27_tokenizer_filename(
    primary_filename: &str,
    tokenizer_filenames: &[String],
) -> Result<Option<String>, String> {
    let model_stem = Path::new(primary_filename)
        .file_stem()
        .and_then(|value| value.to_str())
        .ok_or_else(|| "q27 primary filename is not valid UTF-8".to_owned())?;
    let exact = tokenizer_filenames.iter().find(|candidate| {
        Path::new(candidate)
            .file_stem()
            .and_then(|value| value.to_str())
            .is_some_and(|stem| stem.eq_ignore_ascii_case(model_stem))
    });
    if let Some(exact) = exact {
        return Ok(Some(exact.clone()));
    }
    let mut candidates = tokenizer_filenames
        .iter()
        .filter_map(|candidate| {
            let stem = Path::new(candidate).file_stem()?.to_str()?;
            model_stem
                .strip_prefix(stem)
                .is_some_and(|suffix| suffix.starts_with('-'))
                .then_some((stem.len(), candidate))
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| right.0.cmp(&left.0));
    if let Some((longest, candidate)) = candidates.first() {
        if candidates
            .iter()
            .skip(1)
            .any(|(length, _)| length == longest)
        {
            return Err("q27 tokenizer companion is ambiguous".to_owned());
        }
        return Ok(Some((*candidate).clone()));
    }
    match tokenizer_filenames {
        [] => Ok(None),
        [only] => Ok(Some(only.clone())),
        _ => Err("q27 tokenizer companion is ambiguous".to_owned()),
    }
}

pub fn validate_q27_tokenizer_header(path: &Path) -> Result<(), String> {
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

pub fn model_library_receipt_path(acquisition_root: &Path) -> PathBuf {
    acquisition_root.join(MODEL_LIBRARY_RECEIPT_FILENAME)
}

fn read_library_receipt(
    search_root: &Path,
    primary: &Path,
    format: ArtifactFormat,
    warnings: &mut Vec<String>,
) -> Option<ModelArtifactProvenance> {
    let search_root = search_root.canonicalize().ok()?;
    let mut acquisition_root = primary.parent()?;
    let path = loop {
        let candidate = model_library_receipt_path(acquisition_root);
        if candidate.exists() {
            break candidate;
        }
        if acquisition_root == search_root || !acquisition_root.starts_with(&search_root) {
            return None;
        }
        acquisition_root = acquisition_root.parent()?;
    };
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(error) => {
            warnings.push(format!(
                "could not read model-library receipt {}: {error}",
                path.display()
            ));
            return None;
        }
    };
    let receipt: ModelLibraryReceipt = match serde_json::from_slice(&bytes) {
        Ok(receipt) => receipt,
        Err(error) => {
            warnings.push(format!(
                "could not parse model-library receipt {}: {error}",
                path.display()
            ));
            return None;
        }
    };
    if receipt.schema_version != 2
        || receipt.acquisition_id.is_empty()
        || receipt.members.is_empty()
    {
        warnings.push(format!(
            "model-library receipt {} has an unsupported or empty acquisition identity",
            path.display()
        ));
        return None;
    }
    let acquisition_root = path.parent()?.canonicalize().ok()?;
    let mut seen = HashSet::new();
    let mut matched = None;
    for member in &receipt.members {
        if !safe_receipt_path(&member.path)
            || !seen.insert(member.path.clone())
            || member.provenance.acquisition_id != receipt.acquisition_id
        {
            warnings.push(format!(
                "model-library receipt {} contains unsafe or conflicting acquisition members",
                path.display()
            ));
            return None;
        }
        let member_path = acquisition_root.join(&member.path);
        let Ok(canonical) = member_path.canonicalize() else {
            warnings.push(format!(
                "model-library receipt {} references a missing acquisition member {}",
                path.display(),
                member.path.display()
            ));
            return None;
        };
        let Ok(metadata) = canonical.metadata() else {
            return None;
        };
        if !canonical.starts_with(&acquisition_root)
            || !metadata.is_file()
            || metadata.len() != member.provenance.size_bytes
            || ArtifactFormat::from_path(&canonical) != Some(member.format)
        {
            warnings.push(format!(
                "model-library receipt {} no longer matches acquisition member {}",
                path.display(),
                member.path.display()
            ));
            return None;
        }
        if canonical == primary && member.format == format {
            matched = Some(member);
        }
    }
    let member = matched;
    let Some(member) = member else {
        warnings.push(format!(
            "model-library receipt {} does not identify artifact {}",
            path.display(),
            primary.display()
        ));
        return None;
    };
    Some(member.provenance.clone())
}

fn safe_receipt_path(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && !path.is_absolute()
        && !path.to_string_lossy().contains('\\')
        && path
            .components()
            .all(|component| matches!(component, std::path::Component::Normal(_)))
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
    use std::io::Write;

    use sha2::{Digest, Sha256};

    use super::{
        ArtifactFormat, ArtifactNativeIdentity, AuxiliaryArtifactRole, ModelRegistry,
        NinferContainerError, inspect_ninfer_container,
    };

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

    #[test]
    fn q27_upstream_model_and_tokenizer_names_pair_when_tokenizer_is_unique() {
        let root =
            std::env::temp_dir().join(format!("norted-q27-upstream-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).expect("model fixture directory");
        let model = root.join("MODEL.q27");
        let tokenizer = root.join("TOKENIZER.tok");
        std::fs::write(&model, b"fixture").expect("q27 fixture");
        let mut tokenizer_bytes = b"Q27T".to_vec();
        tokenizer_bytes.extend_from_slice(&1_u32.to_le_bytes());
        std::fs::write(&tokenizer, tokenizer_bytes).expect("tokenizer fixture");

        let registry = ModelRegistry::discover(std::slice::from_ref(&root));
        let artifact = registry.artifacts().first().expect("q27 artifact");
        assert_eq!(artifact.auxiliary_artifacts.len(), 1);
        assert_eq!(
            artifact.auxiliary_artifacts[0].path,
            tokenizer.canonicalize().expect("canonical tokenizer")
        );

        std::fs::remove_dir_all(&root).expect("remove model fixture directory");
    }

    #[test]
    fn obsolete_norted_package_schemas_fail_with_rebuild_guidance() {
        let temporary = tempfile::tempdir().expect("package fixtures");
        for (directory, artifact, manifest, document) in [
            (
                "q27",
                "old.q27",
                "Q27-MANIFEST.json",
                serde_json::json!({"schema": 5}),
            ),
            (
                "ninfer",
                "old.ninfer",
                "NINFER-MANIFEST.json",
                serde_json::json!({"schema": "norted.ninfer-manifest", "schema_version": 5}),
            ),
        ] {
            let root = temporary.path().join(directory);
            std::fs::create_dir(&root).expect("fixture directory");
            std::fs::write(root.join(artifact), b"obsolete claimed package").expect("artifact");
            std::fs::write(root.join(manifest), serde_json::to_vec(&document).unwrap())
                .expect("manifest");
            let registry = ModelRegistry::discover(&[root]);
            assert!(registry.artifacts().is_empty());
            assert!(registry.warnings().iter().any(|warning| {
                warning.contains("rebuild this artifact with the current Norted Builder")
            }));
        }
    }

    #[test]
    fn current_q27_package_binds_sharp_only_as_artifact_provenance() {
        let temporary = tempfile::tempdir().expect("q27 package fixture");
        let root = temporary.path();
        let model_bytes = b"current q27";
        let sharp = b"artifact companion template";
        let mut tokenizer_bytes = b"Q27T".to_vec();
        tokenizer_bytes.extend_from_slice(&1_u32.to_le_bytes());
        std::fs::write(root.join("model.q27"), model_bytes).unwrap();
        std::fs::write(root.join("tokenizer.tok"), &tokenizer_bytes).unwrap();
        std::fs::write(root.join("Sharp.jinja"), sharp).unwrap();

        let mut lineage = serde_json::json!({"builder": "fixture"});
        let lineage_key = sha(&serde_json::to_vec(&lineage).unwrap());
        lineage["key"] = serde_json::Value::String(lineage_key);
        let manifest = serde_json::json!({
            "schema": 6,
            "source_lineage": lineage.clone(),
            "sharp": {
                "filename": "Sharp.jinja",
                "template_sha256": sha(sharp),
                "resolved_commit": "fixture-commit",
                "version": "fixture-version"
            },
            "tokenizer": {
                "filename": "tokenizer.tok",
                "size": tokenizer_bytes.len(),
                "sha256": sha(&tokenizer_bytes)
            },
            "outputs": {
                "q6": {
                    "filename": "model.q27",
                    "size": model_bytes.len(),
                    "sha256": sha(model_bytes),
                    "source_lineage": lineage,
                    "tokenizer_sha256": sha(&tokenizer_bytes)
                }
            }
        });
        std::fs::write(
            root.join("Q27-MANIFEST.json"),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();

        let registry = ModelRegistry::discover(&[root.to_path_buf()]);
        let artifact = registry
            .artifacts()
            .iter()
            .find(|artifact| artifact.norted_package.is_some())
            .unwrap_or_else(|| panic!("current q27 package: {:?}", registry.warnings()));
        let package = artifact.norted_package.as_ref().unwrap();
        assert_eq!(package.manifest_version, 6);
        assert_eq!(
            package.sharp.as_ref().unwrap().path,
            root.join("Sharp.jinja").canonicalize().unwrap()
        );
        assert!(
            artifact
                .auxiliary_artifacts
                .iter()
                .any(|auxiliary| auxiliary.role == AuxiliaryArtifactRole::Sharp)
        );
    }

    #[test]
    fn build_manifest_suppresses_projector_and_raw_artifacts_remain_supported() {
        for schema in [2, 3] {
            let temporary = tempfile::tempdir().expect("GGUF package fixture");
            let root = temporary.path();
            std::fs::write(root.join("model.gguf"), b"model").expect("model");
            std::fs::write(root.join("unrelated.gguf"), b"raw").expect("raw model");
            std::fs::write(root.join("mmproj-F16.gguf"), b"projector").expect("projector");
            let effective_source =
                effective_quant_source_fixture("convert-safetensors-and-quantize");
            let source_key = effective_source["key"].as_str().unwrap().to_owned();
            let mut manifest = serde_json::json!({
                "schema":schema,"build_key":"3".repeat(64),"master":{"master_id":"5".repeat(64)},"outputs":{
                    "model.gguf":{"filename":"model.gguf","size":5,"sha256":sha(b"model"),"quant":"UD-Q6_K_XL"},
                    "mmproj-F16.gguf":{"filename":"mmproj-F16.gguf","size":9,"sha256":sha(b"projector"),"quant":"high-precision vision projector","format":"high-precision-projector","projector_key":"4".repeat(64)}
                }
            });
            if schema == 2 {
                manifest["lineage"] = serde_json::json!({"quants":{"UD-Q6_K_XL":{
                    "raw_quant_key":"6".repeat(64),"provider_recipe_key":"7".repeat(64)
                }}});
            } else {
                manifest["lineage"] = serde_json::json!({"quants":{"UD-Q6_K_XL":{
                    "raw_quant_key":"6".repeat(64),"unsloth_quant_recipe_key":"7".repeat(64),
                    "effective_quant_source":effective_source
                }}});
            }
            std::fs::write(
                root.join("BUILD-MANIFEST.json"),
                serde_json::to_vec(&manifest).unwrap(),
            )
            .unwrap();
            let registry = ModelRegistry::discover(&[root.to_path_buf()]);
            assert_eq!(registry.artifacts().len(), 2, "{:?}", registry.warnings());
            let package = registry
                .artifacts()
                .iter()
                .find(|artifact| artifact.norted_package.is_some())
                .expect("package model");
            assert_eq!(
                package.auxiliary_artifacts[0].role,
                AuxiliaryArtifactRole::Projector
            );
            let binding = package.norted_package.as_ref().unwrap();
            assert_eq!(binding.manifest_version, schema);
            assert_eq!(
                binding.canonical_source_lineage_key.as_deref(),
                (schema == 3).then_some(source_key.as_str())
            );
            assert!(
                registry
                    .artifacts()
                    .iter()
                    .any(|artifact| artifact.path.ends_with("unrelated.gguf")
                        && artifact.norted_package.is_none())
            );
            assert!(
                registry
                    .artifacts()
                    .iter()
                    .all(|artifact| !artifact.path.ends_with("mmproj-F16.gguf"))
            );
        }
    }

    #[test]
    fn schema_three_build_routes_use_durable_lineage_for_fresh_and_reuse_publications() {
        for action in [
            "reuse-cache",
            "convert-safetensors-and-quantize",
            "transform-high-precision-gguf",
        ] {
            let temporary = tempfile::tempdir().expect("schema-3 route fixture");
            let root = temporary.path();
            std::fs::write(root.join("model.gguf"), b"model").unwrap();
            let durable_route = if action == "transform-high-precision-gguf" {
                "transform-high-precision-gguf"
            } else {
                "convert-safetensors-and-quantize"
            };
            let effective_source = effective_quant_source_fixture(durable_route);
            let source_key = effective_source["key"].as_str().unwrap().to_owned();
            let route = if action == "reuse-cache" {
                serde_json::json!({"action":action,"cache_source_key":source_key})
            } else {
                serde_json::json!({"action":action,"artifact":"source","steps":["transform","publish"],"effective_source":{"key":"route-local-detail"},"restoration":null,"current_source_anchor":{}})
            };
            let manifest = serde_json::json!({
                "schema":3,"build_key":"3".repeat(64),
                "route":{"quants":{"UD-Q6_K_XL":route}},
                "lineage":{"quants":{"UD-Q6_K_XL":{
                    "raw_quant_key":"6".repeat(64),"unsloth_quant_recipe_key":"7".repeat(64),
                    "effective_quant_source":effective_source,
                    "current_source_anchor":{},"quantization":{"performed_by_norted":true}
                }}},
                "outputs":{"model.gguf":{"filename":"model.gguf","size":5,"sha256":sha(b"model"),"quant":"UD-Q6_K_XL"}}
            });
            std::fs::write(
                root.join("BUILD-MANIFEST.json"),
                serde_json::to_vec(&manifest).unwrap(),
            )
            .unwrap();
            let registry = ModelRegistry::discover(&[root.to_path_buf()]);
            assert_eq!(registry.artifacts().len(), 1, "{:?}", registry.warnings());
            assert_eq!(
                registry.artifacts()[0]
                    .norted_package
                    .as_ref()
                    .and_then(|package| package.canonical_source_lineage_key.as_deref()),
                Some(source_key.as_str())
            );
        }
    }

    #[test]
    fn ninfer_v2_admission_recovers_native_identity_without_reading_payload() {
        let directory = serde_json::json!({
            "identity": {
                "model_id": "native/model-from-container",
                "weights_id": "native-weights"
            },
            "objects": [{
                "name": "frontend/tokenizer.json",
                "kind": "resource",
                "encoding": "raw-bytes-v1",
                "offset": 0,
                "bytes": 1
            }]
        });
        let temporary = tempfile::tempdir().expect("temporary NInfer fixture directory");
        let path = temporary.path().join("misleading-filename.ninfer");
        write_ninfer_fixture(&path, &directory, 4 * 1024 * 1024 * 1024);

        let metadata = inspect_ninfer_container(&path).expect("valid NInfer metadata");
        assert_eq!(metadata.identity.container_version, 2);
        assert_eq!(metadata.identity.model_id, "native/model-from-container");
        assert_eq!(metadata.identity.weights_id, "native-weights");
        assert!(metadata.metadata_bytes_read < 4096);

        let registry = ModelRegistry::discover(&[temporary.path().to_path_buf()]);
        assert_eq!(registry.artifacts().len(), 1);
        assert!(matches!(
            &registry.artifacts()[0].native_identity,
            Some(ArtifactNativeIdentity::Ninfer(identity))
                if identity.model_id == "native/model-from-container"
                    && identity.weights_id == "native-weights"
        ));
    }

    #[test]
    fn ninfer_admission_fails_closed_for_bad_framing_and_directory_bounds() {
        let temporary = tempfile::tempdir().expect("temporary NInfer fixture directory");
        let valid_directory = serde_json::json!({
            "identity": {"model_id": "model", "weights_id": "weights"},
            "objects": [{
                "name": "resource",
                "kind": "resource",
                "encoding": "raw-bytes-v1",
                "offset": 0,
                "bytes": 1
            }]
        });

        let bad_magic = temporary.path().join("bad-magic.ninfer");
        std::fs::write(&bad_magic, [0_u8; 16]).expect("bad magic fixture");
        assert!(matches!(
            inspect_ninfer_container(&bad_magic),
            Err(NinferContainerError::InvalidMagic)
        ));

        let old_version = temporary.path().join("old-version.ninfer");
        let mut old_prefix = *b"NINFER\0\x01";
        old_prefix[7] = 1;
        std::fs::write(&old_version, [old_prefix.as_slice(), &[0_u8; 8]].concat())
            .expect("old version fixture");
        assert!(matches!(
            inspect_ninfer_container(&old_version),
            Err(NinferContainerError::UnsupportedVersion(1))
        ));

        let truncated = temporary.path().join("truncated.ninfer");
        let mut truncated_prefix = b"NINFER\0\x02".to_vec();
        truncated_prefix.extend_from_slice(&1024_u64.to_le_bytes());
        std::fs::write(&truncated, truncated_prefix).expect("truncated fixture");
        assert!(matches!(
            inspect_ninfer_container(&truncated),
            Err(NinferContainerError::TruncatedDirectory | NinferContainerError::Io(_))
        ));

        let absurd = temporary.path().join("absurd.ninfer");
        let mut absurd_prefix = b"NINFER\0\x02".to_vec();
        absurd_prefix.extend_from_slice(&(17_u64 * 1024 * 1024).to_le_bytes());
        std::fs::write(&absurd, absurd_prefix).expect("absurd fixture");
        assert!(matches!(
            inspect_ninfer_container(&absurd),
            Err(NinferContainerError::InvalidDirectoryLength { .. })
        ));

        let malformed = temporary.path().join("malformed.ninfer");
        write_ninfer_fixture(
            &malformed,
            &serde_json::json!({
                "identity": valid_directory["identity"],
                "objects": [{
                    "name": "outside",
                    "kind": "resource",
                    "encoding": "raw-bytes-v1",
                    "offset": 99,
                    "bytes": 1
                }]
            }),
            1,
        );
        assert!(matches!(
            inspect_ninfer_container(&malformed),
            Err(NinferContainerError::InvalidDirectory(_))
        ));
    }

    fn write_ninfer_fixture(path: &std::path::Path, directory: &serde_json::Value, payload: u64) {
        let json = serde_json::to_vec(directory).expect("serialize NInfer directory fixture");
        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(path)
            .expect("create NInfer fixture");
        file.write_all(b"NINFER\0\x02").expect("write NInfer magic");
        file.write_all(&(json.len() as u64).to_le_bytes())
            .expect("write NInfer directory length");
        file.write_all(&json).expect("write NInfer directory");
        let metadata_end = 16_u64 + json.len() as u64;
        let payload_offset = metadata_end.div_ceil(4096) * 4096;
        file.set_len(payload_offset + payload)
            .expect("size sparse NInfer payload");
    }

    fn sha(bytes: &[u8]) -> String {
        format!("{:x}", Sha256::digest(bytes))
    }

    fn effective_quant_source_fixture(route: &str) -> serde_json::Value {
        let artifact = "8".repeat(64);
        let key_material = serde_json::json!({
            "repository":"example/source",
            "artifact":artifact,
            "route":route
        });
        let key = sha(&serde_json::to_vec(&key_material).unwrap());
        serde_json::json!({
            "schema":3,
            "created_under_source_identity":"9".repeat(64),
            "repository":"example/source",
            "parent_artifact_id":"source",
            "view_id":"source#language-mtp",
            "artifact_identity":{"key":artifact},
            "route":route,
            "restoration":null,
            "key_material":key_material,
            "key":key
        })
    }
}
