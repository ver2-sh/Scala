use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{ArtifactFormat, ModelId};

pub const RUNTIME_MANIFEST_SCHEMA_VERSION: u32 = 1;
pub const RUNTIME_SELECTIONS_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RuntimeId(pub String);

impl RuntimeId {
    pub fn new(value: impl Into<String>) -> Result<Self, RuntimeIdentityError> {
        let value = value.into();
        if value.is_empty()
            || matches!(value.as_str(), "." | "..")
            || value.len() > 240
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        {
            return Err(RuntimeIdentityError::InvalidId(value));
        }
        Ok(Self(value))
    }

    pub fn from_identity(identity: &RuntimeIdentity) -> Self {
        let encoded = serde_json::to_vec(identity)
            .expect("serializing a runtime identity with string fields cannot fail");
        let digest = Sha256::digest(encoded);
        let mut readable = [
            identity.engine_id.as_str(),
            identity.version.as_str(),
            identity.platform.as_str(),
            identity.architecture.as_str(),
            identity.accelerator.as_str(),
            identity.variant.as_str(),
        ]
        .into_iter()
        .map(sanitize_id_part)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-");
        readable.truncate(180);
        Self(format!(
            "{}-{}",
            if readable.is_empty() {
                "runtime"
            } else {
                &readable
            },
            hex_prefix(&digest, 16)
        ))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for RuntimeId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

impl std::str::FromStr for RuntimeId {
    type Err = RuntimeIdentityError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value.to_owned())
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct RuntimePackageIdentity {
    pub provider_id: String,
    pub repository: Option<String>,
    pub release_tag: Option<String>,
    pub asset_id: Option<String>,
    pub asset_name: Option<String>,
    #[serde(default)]
    pub additional_assets: Vec<RuntimePackageAssetIdentity>,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct RuntimePackageAssetIdentity {
    pub asset_id: String,
    pub asset_name: String,
    pub role: String,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct RuntimeIdentity {
    pub engine_id: String,
    pub package_family: String,
    pub version: String,
    pub upstream_revision: Option<String>,
    pub platform: String,
    pub architecture: String,
    pub accelerator: String,
    pub variant: String,
    pub package: RuntimePackageIdentity,
}

impl RuntimeIdentity {
    pub fn validate(&self) -> Result<(), RuntimeIdentityError> {
        for (name, value) in [
            ("engine_id", self.engine_id.as_str()),
            ("package_family", self.package_family.as_str()),
            ("version", self.version.as_str()),
            ("platform", self.platform.as_str()),
            ("architecture", self.architecture.as_str()),
            ("accelerator", self.accelerator.as_str()),
            ("variant", self.variant.as_str()),
            ("provider_id", self.package.provider_id.as_str()),
        ] {
            if value.trim().is_empty() {
                return Err(RuntimeIdentityError::EmptyField(name));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Eq, PartialEq, thiserror::Error)]
pub enum RuntimeIdentityError {
    #[error("runtime identity field `{0}` cannot be empty")]
    EmptyField(&'static str),
    #[error("invalid runtime ID `{0}`")]
    InvalidId(String),
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeAcquisitionMethod {
    OfficialReleaseAsset,
    PreseededOfficialPack,
    ExternalBinary,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeArchiveFormat {
    Zip,
    TarGz,
}

impl RuntimeArchiveFormat {
    pub fn extension(self) -> &'static str {
        match self {
            Self::Zip => "zip",
            Self::TarGz => "tar.gz",
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct RuntimeDigest {
    pub algorithm: String,
    pub value: String,
}

impl RuntimeDigest {
    pub fn sha256(value: impl Into<String>) -> Result<Self, RuntimeDigestError> {
        let value = value.into().to_ascii_lowercase();
        if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(RuntimeDigestError);
        }
        Ok(Self {
            algorithm: "sha256".to_owned(),
            value,
        })
    }

    pub fn parse_github(value: &str) -> Result<Self, RuntimeDigestError> {
        let (algorithm, digest) = value.split_once(':').ok_or(RuntimeDigestError)?;
        if !algorithm.eq_ignore_ascii_case("sha256") {
            return Err(RuntimeDigestError);
        }
        Self::sha256(digest)
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, thiserror::Error)]
#[error("expected a SHA-256 digest containing exactly 64 hexadecimal characters")]
pub struct RuntimeDigestError;

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeReleaseChannel {
    Stable,
    Latest,
    Prerelease,
}

#[derive(Debug, Clone, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct RuntimeRequirements {
    pub requires_nvidia_gpu: bool,
    pub minimum_nvidia_driver: Option<String>,
    pub minimum_vram_bytes: Option<u64>,
    /// A nominal upstream hardware class (for example, "24 GiB-class"), not
    /// an exact byte floor. Host detection applies a documented reporting and
    /// reservation allowance rather than comparing this directly to bytes.
    #[serde(default)]
    pub minimum_vram_class_gib: Option<u16>,
    /// A nominal VRAM class known to be insufficient, without claiming the
    /// next exact usable class. Devices at or below this class are rejected;
    /// larger devices still depend on any unverified requirement below.
    #[serde(default)]
    pub minimum_vram_exclusive_class_gib: Option<u16>,
    /// Exact CUDA compute capabilities compiled into this runtime. An empty
    /// set means the runtime's CUDA target coverage is not known.
    #[serde(default)]
    pub supported_cuda_compute_capabilities: Vec<ComputeCapability>,
    /// Legacy schema-v1 unverified compatibility conditions. New manifests
    /// should use `unverified_requirements`; preserving this meaning keeps old
    /// manifests fail-closed.
    #[serde(default)]
    pub notes: Vec<String>,
    /// Informational upstream guidance that does not affect compatibility.
    #[serde(default)]
    pub advisories: Vec<String>,
    /// Compatibility conditions that the current host probe cannot verify.
    #[serde(default)]
    pub unverified_requirements: Vec<String>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
pub struct ComputeCapability {
    pub major: u16,
    pub minor: u16,
}

impl ComputeCapability {
    pub const fn new(major: u16, minor: u16) -> Self {
        Self { major, minor }
    }
}

impl std::fmt::Display for ComputeCapability {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}.{}", self.major, self.minor)
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct RuntimeDownload {
    pub url: String,
    pub size_bytes: u64,
    pub digest: Option<RuntimeDigest>,
    pub archive_format: RuntimeArchiveFormat,
    /// Basenames accepted as the package entrypoint. The installer still
    /// requires exactly one matching regular file after safe extraction.
    pub entrypoint_names: Vec<String>,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct AvailableRuntime {
    pub runtime_id: RuntimeId,
    pub identity: RuntimeIdentity,
    pub display_name: String,
    pub supported_formats: Vec<ArtifactFormat>,
    pub source_url: String,
    pub published_at_unix: Option<i64>,
    pub channels: Vec<RuntimeReleaseChannel>,
    pub prerelease: bool,
    pub download: RuntimeDownload,
    #[serde(default)]
    pub additional_downloads: Vec<RuntimeDownload>,
    pub requirements: RuntimeRequirements,
}

impl AvailableRuntime {
    pub fn validate(&self) -> Result<(), RuntimeManifestError> {
        self.identity.validate()?;
        if self.runtime_id != RuntimeId::from_identity(&self.identity) {
            return Err(RuntimeManifestError::IdentityMismatch);
        }
        if self.supported_formats.is_empty() {
            return Err(RuntimeManifestError::NoFormats);
        }
        if !has_complete_managed_package_identity(&self.identity.package) {
            return Err(RuntimeManifestError::InvalidPackageIdentity);
        }
        if self.download.entrypoint_names.is_empty()
            || self
                .download
                .entrypoint_names
                .iter()
                .any(|name| !is_safe_entrypoint_basename(name))
        {
            return Err(RuntimeManifestError::InvalidEntrypoint);
        }
        if self.identity.package.additional_assets.len() != self.additional_downloads.len() {
            return Err(RuntimeManifestError::PackageComponentMismatch);
        }
        if self.download.url.trim().is_empty()
            || self
                .additional_downloads
                .iter()
                .any(|download| download.url.trim().is_empty())
        {
            return Err(RuntimeManifestError::PackageComponentMismatch);
        }
        for download in std::iter::once(&self.download).chain(&self.additional_downloads) {
            if download.size_bytes == 0
                || download.digest.as_ref().is_some_and(|digest| {
                    !digest.algorithm.eq_ignore_ascii_case("sha256")
                        || RuntimeDigest::sha256(digest.value.clone()).is_err()
                })
                || download
                    .entrypoint_names
                    .iter()
                    .any(|name| !is_safe_entrypoint_basename(name))
            {
                return Err(RuntimeManifestError::InvalidDownload);
            }
        }
        Ok(())
    }

    pub fn download_size_bytes(&self) -> u64 {
        self.additional_downloads
            .iter()
            .fold(self.download.size_bytes, |total, download| {
                total.saturating_add(download.size_bytes)
            })
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct RuntimeProbeObservation {
    pub compatible: bool,
    pub observed_engine_id: String,
    pub observed_version: Option<String>,
    pub observed_revision: Option<String>,
    pub detail: String,
    pub observed_at_unix: i64,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct RuntimeManifest {
    pub schema_version: u32,
    pub runtime_id: RuntimeId,
    pub identity: RuntimeIdentity,
    pub supported_formats: Vec<ArtifactFormat>,
    pub requirements: RuntimeRequirements,
    pub acquisition_method: RuntimeAcquisitionMethod,
    pub source_url: Option<String>,
    pub downloaded_archive_sha256: Option<String>,
    #[serde(default)]
    pub additional_downloaded_archive_sha256: Vec<String>,
    /// Relative to the installation root for managed packs; absolute only for
    /// explicitly configured external binaries.
    pub entrypoint: PathBuf,
    pub entrypoint_sha256: String,
    pub installed_at_unix: Option<i64>,
    pub probe: RuntimeProbeObservation,
}

impl RuntimeManifest {
    pub fn validate(&self) -> Result<(), RuntimeManifestError> {
        if self.schema_version != RUNTIME_MANIFEST_SCHEMA_VERSION {
            return Err(RuntimeManifestError::UnsupportedSchema(self.schema_version));
        }
        self.identity.validate()?;
        if self.runtime_id != RuntimeId::from_identity(&self.identity) {
            return Err(RuntimeManifestError::IdentityMismatch);
        }
        if self.supported_formats.is_empty() {
            return Err(RuntimeManifestError::NoFormats);
        }
        RuntimeDigest::sha256(self.entrypoint_sha256.clone())?;
        if let Some(digest) = &self.downloaded_archive_sha256 {
            RuntimeDigest::sha256(digest.clone())?;
        }
        for digest in &self.additional_downloaded_archive_sha256 {
            RuntimeDigest::sha256(digest.clone())?;
        }
        match self.acquisition_method {
            RuntimeAcquisitionMethod::ExternalBinary => {
                if !self.entrypoint.is_absolute() {
                    return Err(RuntimeManifestError::InvalidEntrypoint);
                }
                if self.downloaded_archive_sha256.is_some()
                    || !self.additional_downloaded_archive_sha256.is_empty()
                    || self.installed_at_unix.is_some()
                {
                    return Err(RuntimeManifestError::InvalidExternalProvenance);
                }
            }
            RuntimeAcquisitionMethod::OfficialReleaseAsset
            | RuntimeAcquisitionMethod::PreseededOfficialPack => {
                if !is_safe_relative_path(&self.entrypoint) {
                    return Err(RuntimeManifestError::InvalidEntrypoint);
                }
                if self.downloaded_archive_sha256.is_none() {
                    return Err(RuntimeManifestError::MissingArchiveDigest);
                }
                if self.source_url.as_deref().is_none_or(str::is_empty)
                    || !has_complete_managed_package_identity(&self.identity.package)
                {
                    return Err(RuntimeManifestError::InvalidPackageIdentity);
                }
                if self.additional_downloaded_archive_sha256.len()
                    != self.identity.package.additional_assets.len()
                {
                    return Err(RuntimeManifestError::PackageComponentMismatch);
                }
                if self.installed_at_unix.is_none() {
                    return Err(RuntimeManifestError::MissingInstallTime);
                }
            }
        }
        if !self.probe.compatible || self.probe.observed_engine_id != self.identity.engine_id {
            return Err(RuntimeManifestError::FailedProbe);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Eq, PartialEq, thiserror::Error)]
pub enum RuntimeManifestError {
    #[error(transparent)]
    Identity(#[from] RuntimeIdentityError),
    #[error(transparent)]
    Digest(#[from] RuntimeDigestError),
    #[error("runtime manifest schema {0} is unsupported")]
    UnsupportedSchema(u32),
    #[error("runtime ID does not match its structured identity")]
    IdentityMismatch,
    #[error("runtime declares no supported artifact formats")]
    NoFormats,
    #[error("runtime entrypoint is invalid")]
    InvalidEntrypoint,
    #[error("managed runtime is missing its verified archive digest")]
    MissingArchiveDigest,
    #[error("managed runtime is missing its installation timestamp")]
    MissingInstallTime,
    #[error("external runtime contains managed acquisition provenance")]
    InvalidExternalProvenance,
    #[error("runtime package asset identity and download components do not match")]
    PackageComponentMismatch,
    #[error("runtime package identity is incomplete")]
    InvalidPackageIdentity,
    #[error("runtime download metadata is invalid")]
    InvalidDownload,
    #[error("runtime did not pass its engine adapter probe")]
    FailedProbe,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct InstalledRuntime {
    pub manifest: RuntimeManifest,
    pub installation_root: PathBuf,
}

impl InstalledRuntime {
    pub fn entrypoint_path(&self) -> PathBuf {
        if self.manifest.entrypoint.is_absolute() {
            self.manifest.entrypoint.clone()
        } else {
            self.installation_root.join(&self.manifest.entrypoint)
        }
    }

    pub fn validate(&self) -> Result<(), RuntimeManifestError> {
        self.manifest.validate()
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state", content = "reason")]
pub enum RuntimeCompatibility {
    Recommended,
    Compatible,
    NeedsAttention(String),
    Incompatible(String),
}

impl RuntimeCompatibility {
    pub fn is_usable(&self) -> bool {
        !matches!(self, Self::Incompatible(_))
    }

    pub fn preference_rank(&self) -> u8 {
        match self {
            Self::Recommended => 0,
            Self::Compatible => 1,
            Self::NeedsAttention(_) => 2,
            Self::Incompatible(_) => 3,
        }
    }
}

#[derive(Debug, Clone, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct AcceleratorDevice {
    /// Runtime-facing accelerator family, currently `cuda` for NVIDIA GPUs.
    pub accelerator: String,
    /// Stable physical device identity reported by the vendor, when available.
    pub stable_id: Option<String>,
    pub name: Option<String>,
    pub vram_bytes: Option<u64>,
    pub driver_version: Option<String>,
    /// Numeric compute capability reported by the accelerator vendor. This is
    /// never inferred from a marketing name.
    #[serde(default)]
    pub compute_capability: Option<ComputeCapability>,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct HostCapabilities {
    pub platform: String,
    pub architecture: String,
    pub accelerators: Vec<AcceleratorDevice>,
    /// Parent CUDA visibility captured with the hardware observation. Engines
    /// that bind CUDA devices must reconcile this deliberately.
    pub cuda_visible_devices: Option<String>,
    pub observations: Vec<String>,
}

impl HostCapabilities {
    pub fn current_without_accelerator_probe() -> Self {
        Self {
            platform: std::env::consts::OS.to_owned(),
            architecture: std::env::consts::ARCH.to_owned(),
            accelerators: Vec::new(),
            cuda_visible_devices: std::env::var("CUDA_VISIBLE_DEVICES").ok(),
            observations: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeUpdatePreference {
    Stable,
    Latest,
    Pinned,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct RuntimeSelections {
    pub schema_version: u32,
    pub format_defaults: BTreeMap<ArtifactFormat, RuntimeId>,
    pub model_overrides: BTreeMap<ModelId, RuntimeId>,
    pub update_preferences: BTreeMap<RuntimeId, RuntimeUpdatePreference>,
}

impl Default for RuntimeSelections {
    fn default() -> Self {
        Self {
            schema_version: RUNTIME_SELECTIONS_SCHEMA_VERSION,
            format_defaults: BTreeMap::new(),
            model_overrides: BTreeMap::new(),
            update_preferences: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeSelectionSource {
    Invocation,
    ModelOverride,
    FormatDefault,
    Fallback,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct RuntimeSelection {
    pub runtime: InstalledRuntime,
    pub source: RuntimeSelectionSource,
    pub notices: Vec<String>,
    /// Exact physical accelerator selected while evaluating this runtime.
    pub accelerator: Option<AcceleratorDevice>,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state", content = "detail")]
pub enum RuntimeUpdateState {
    Current,
    Unmanaged,
    NewerCompatibleVersion {
        runtime_id: RuntimeId,
        version: String,
    },
    Pinned {
        newer_runtime_id: Option<RuntimeId>,
        newer_version: Option<String>,
    },
    CatalogUnavailable(String),
    ProviderError(String),
    NoLongerPublished,
    ChannelUnavailable {
        preference: RuntimeUpdatePreference,
    },
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeOperationPhase {
    Downloading,
    Verifying,
    Extracting,
    Probing,
    Installing,
    Installed,
    Failed,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct RuntimeOperationProgress {
    pub runtime_id: RuntimeId,
    pub phase: RuntimeOperationPhase,
    pub bytes_completed: Option<u64>,
    pub bytes_total: Option<u64>,
    pub detail: String,
}

pub fn is_safe_relative_path(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && !path.is_absolute()
        && path
            .components()
            .any(|component| matches!(component, Component::Normal(_)))
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_) | Component::CurDir))
}

fn is_safe_entrypoint_basename(name: &str) -> bool {
    let name = name.trim();
    !name.is_empty()
        && !matches!(name, "." | "..")
        && !name.contains(['/', '\\', ':'])
        && Path::new(name)
            .file_name()
            .is_some_and(|value| value == name)
}

fn has_complete_managed_package_identity(package: &RuntimePackageIdentity) -> bool {
    package
        .repository
        .as_deref()
        .is_some_and(|value| !value.is_empty())
        && package
            .release_tag
            .as_deref()
            .is_some_and(|value| !value.is_empty())
        && package
            .asset_id
            .as_deref()
            .is_some_and(|value| !value.is_empty())
        && package
            .asset_name
            .as_deref()
            .is_some_and(|value| !value.is_empty())
        && package.additional_assets.iter().all(|asset| {
            !asset.asset_id.is_empty() && !asset.asset_name.is_empty() && !asset.role.is_empty()
        })
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

#[cfg(test)]
mod tests {
    use super::{RuntimeDigest, RuntimeId, is_safe_relative_path};
    use std::path::Path;

    #[test]
    fn runtime_references_and_digests_are_strict() {
        assert!(RuntimeId::new("llama-b1-windows-x86_64-cuda-deadbeef").is_ok());
        for invalid in [
            "",
            ".",
            "..",
            "../escape",
            "has space",
            "slash/value",
            "a\\b",
        ] {
            assert!(RuntimeId::new(invalid).is_err(), "accepted {invalid}");
        }
        assert!(RuntimeDigest::sha256("a".repeat(64)).is_ok());
        assert!(RuntimeDigest::sha256("x".repeat(64)).is_err());
    }

    #[test]
    fn managed_entrypoints_are_contained_relative_paths() {
        assert!(is_safe_relative_path(Path::new("bin/llama-server.exe")));
        for invalid in [
            "",
            ".",
            "../llama-server",
            "/bin/llama-server",
            "C:\\escape.exe",
        ] {
            assert!(
                !is_safe_relative_path(Path::new(invalid)),
                "accepted {invalid}"
            );
        }
    }
}
