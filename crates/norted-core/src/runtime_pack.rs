use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{ArtifactFormat, ArtifactNativeIdentity, ModelId};

pub const RUNTIME_MANIFEST_SCHEMA_VERSION: u32 = 2;
pub const MINIMUM_RUNTIME_MANIFEST_SCHEMA_VERSION: u32 = 1;
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
    SourceBuild,
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
    /// Exact product names required independently of compute capability when
    /// an upstream runtime deliberately supports only a named GPU family.
    #[serde(default)]
    pub required_nvidia_device_names: Vec<String>,
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
#[serde(rename_all = "snake_case", tag = "method", content = "details")]
pub enum RuntimeAcquisitionPlan {
    ReleaseAsset {
        download: RuntimeDownload,
        #[serde(default)]
        additional_downloads: Vec<RuntimeDownload>,
    },
    SourceBuild(Box<RuntimeSourceBuildPlan>),
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct RuntimeSourceSnapshot {
    pub repository: String,
    pub repository_url: String,
    pub source_branch: String,
    pub commit_sha: String,
    pub tree_sha: String,
    pub commit_timestamp_unix: i64,
    pub source_provider: String,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct RuntimeSourceBuildRecipe {
    pub recipe_version: String,
    #[serde(default)]
    pub build_system: RuntimeSourceBuildSystem,
    /// Digest of the provider-audited root build definition. Make recipes use
    /// this to bind the locally executed dependency/command graph to the exact
    /// definition admitted during catalog discovery.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build_definition_sha256: Option<String>,
    pub cmake_configuration_arguments: Vec<String>,
    pub build_target: String,
    pub entrypoint: PathBuf,
    pub accelerator_target: String,
    #[serde(default)]
    pub rejected_build_environment: Vec<String>,
}

#[derive(Debug, Clone, Copy, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeSourceBuildSystem {
    #[default]
    Cmake,
    Make,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct RuntimeSourceBuildPrerequisites {
    pub minimum_cmake_version: String,
    /// Minimum CUDA Toolkit version published by the source contract. `None`
    /// still requires a working nvcc, but does not invent an upstream floor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minimum_cuda_version: Option<String>,
    /// Exclusive upper CUDA Toolkit bound owned by the source recipe. `None`
    /// preserves historical unbounded prerequisite semantics.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub maximum_cuda_version_exclusive: Option<String>,
    pub requires_ninja: bool,
    pub requires_cpp20_compiler: bool,
    #[serde(default)]
    pub requires_make: bool,
    #[serde(default)]
    pub minimum_cpp_standard: Option<u16>,
    #[serde(default)]
    pub cpp_compiler: Option<String>,
    #[serde(default)]
    pub cuda_compiler: Option<PathBuf>,
    pub requires_pkg_config: bool,
    #[serde(default)]
    pub pkg_config_modules: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct RuntimeSourceBuildPlan {
    pub source: RuntimeSourceSnapshot,
    pub recipe: RuntimeSourceBuildRecipe,
    pub prerequisites: RuntimeSourceBuildPrerequisites,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct RuntimeSourceBuildToolchain {
    pub cmake_version: String,
    pub ninja_version: String,
    #[serde(default)]
    pub make_version: String,
    pub cpp_compiler: String,
    pub nvcc_version: String,
    pub pkg_config_version: String,
    #[serde(default)]
    pub system_dependencies: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct RuntimeSourceBuildProvenance {
    pub source: RuntimeSourceSnapshot,
    pub recipe_version: String,
    #[serde(default)]
    pub build_system: RuntimeSourceBuildSystem,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build_definition_sha256: Option<String>,
    pub cmake_configuration_arguments: Vec<String>,
    pub build_target: String,
    pub toolchain: RuntimeSourceBuildToolchain,
    pub build_platform: String,
    pub build_architecture: String,
    pub accelerator_target: String,
    pub built_at_unix: i64,
    pub entrypoint: PathBuf,
    pub entrypoint_sha256: String,
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
    pub acquisition: RuntimeAcquisitionPlan,
    #[serde(default)]
    pub supported_native_identities: Vec<ArtifactNativeIdentity>,
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
        validate_native_identities(&self.supported_formats, &self.supported_native_identities)?;
        match &self.acquisition {
            RuntimeAcquisitionPlan::ReleaseAsset {
                download,
                additional_downloads,
            } => {
                if !has_complete_release_package_identity(&self.identity.package) {
                    return Err(RuntimeManifestError::InvalidPackageIdentity);
                }
                if download.entrypoint_names.is_empty()
                    || download
                        .entrypoint_names
                        .iter()
                        .any(|name| !is_safe_entrypoint_basename(name))
                {
                    return Err(RuntimeManifestError::InvalidEntrypoint);
                }
                if self.identity.package.additional_assets.len() != additional_downloads.len() {
                    return Err(RuntimeManifestError::PackageComponentMismatch);
                }
                if download.url.trim().is_empty()
                    || additional_downloads
                        .iter()
                        .any(|download| download.url.trim().is_empty())
                {
                    return Err(RuntimeManifestError::PackageComponentMismatch);
                }
                for download in std::iter::once(download).chain(additional_downloads) {
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
            }
            RuntimeAcquisitionPlan::SourceBuild(plan) => {
                validate_source_build_plan(plan, &self.identity)?;
            }
        }
        Ok(())
    }

    pub fn download_size_bytes(&self) -> Option<u64> {
        match &self.acquisition {
            RuntimeAcquisitionPlan::ReleaseAsset {
                download,
                additional_downloads,
            } => Some(
                additional_downloads
                    .iter()
                    .fold(download.size_bytes, |total, download| {
                        total.saturating_add(download.size_bytes)
                    }),
            ),
            RuntimeAcquisitionPlan::SourceBuild(_) => None,
        }
    }

    pub fn release_assets(&self) -> Option<(&RuntimeDownload, &[RuntimeDownload])> {
        match &self.acquisition {
            RuntimeAcquisitionPlan::ReleaseAsset {
                download,
                additional_downloads,
            } => Some((download, additional_downloads)),
            RuntimeAcquisitionPlan::SourceBuild(_) => None,
        }
    }

    pub fn source_build(&self) -> Option<&RuntimeSourceBuildPlan> {
        match &self.acquisition {
            RuntimeAcquisitionPlan::SourceBuild(plan) => Some(plan),
            RuntimeAcquisitionPlan::ReleaseAsset { .. } => None,
        }
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub supported_native_identities: Vec<ArtifactNativeIdentity>,
    pub requirements: RuntimeRequirements,
    pub acquisition_method: RuntimeAcquisitionMethod,
    pub source_url: Option<String>,
    pub downloaded_archive_sha256: Option<String>,
    #[serde(default)]
    pub additional_downloaded_archive_sha256: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_build: Option<RuntimeSourceBuildProvenance>,
    /// Relative to the installation root for managed packs; absolute only for
    /// explicitly configured external binaries.
    pub entrypoint: PathBuf,
    pub entrypoint_sha256: String,
    pub installed_at_unix: Option<i64>,
    pub probe: RuntimeProbeObservation,
}

impl RuntimeManifest {
    pub fn validate(&self) -> Result<(), RuntimeManifestError> {
        if !(MINIMUM_RUNTIME_MANIFEST_SCHEMA_VERSION..=RUNTIME_MANIFEST_SCHEMA_VERSION)
            .contains(&self.schema_version)
        {
            return Err(RuntimeManifestError::UnsupportedSchema(self.schema_version));
        }
        self.identity.validate()?;
        if self.runtime_id != RuntimeId::from_identity(&self.identity) {
            return Err(RuntimeManifestError::IdentityMismatch);
        }
        if self.supported_formats.is_empty() {
            return Err(RuntimeManifestError::NoFormats);
        }
        validate_native_identities(&self.supported_formats, &self.supported_native_identities)?;
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
                    || self.source_build.is_some()
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
                    || !has_complete_release_package_identity(&self.identity.package)
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
                if self.source_build.is_some() {
                    return Err(RuntimeManifestError::UnexpectedSourceBuildProvenance);
                }
            }
            RuntimeAcquisitionMethod::SourceBuild => {
                if self.schema_version < 2 {
                    return Err(RuntimeManifestError::UnsupportedSchema(self.schema_version));
                }
                if !is_safe_relative_path(&self.entrypoint) {
                    return Err(RuntimeManifestError::InvalidEntrypoint);
                }
                if self.downloaded_archive_sha256.is_some()
                    || !self.additional_downloaded_archive_sha256.is_empty()
                {
                    return Err(RuntimeManifestError::UnexpectedArchiveDigest);
                }
                if self.source_url.as_deref().is_none_or(str::is_empty)
                    || self.installed_at_unix.is_none()
                {
                    return Err(RuntimeManifestError::MissingSourceBuildProvenance);
                }
                let provenance = self
                    .source_build
                    .as_ref()
                    .ok_or(RuntimeManifestError::MissingSourceBuildProvenance)?;
                validate_source_build_provenance(provenance, self)?;
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
    #[error("source-built runtime is missing exact source/build provenance")]
    MissingSourceBuildProvenance,
    #[error("non-source runtime contains source-build provenance")]
    UnexpectedSourceBuildProvenance,
    #[error("source-built runtime contains a release-archive digest")]
    UnexpectedArchiveDigest,
    #[error("source-build provenance is inconsistent with the runtime identity or manifest")]
    InvalidSourceBuildProvenance,
    #[error("source-build candidate metadata is invalid: {0}")]
    InvalidSourceBuild(String),
    #[error("runtime native artifact capabilities do not match its declared formats")]
    InvalidNativeCapabilities,
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
    /// A successful NVIDIA probe positively established that no device is
    /// available. False means absence is unproven, not that a GPU exists.
    #[serde(default)]
    pub nvidia_gpu_absence_confirmed: bool,
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
            nvidia_gpu_absence_confirmed: false,
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
    CheckingPrerequisites,
    FetchingSource,
    VerifyingSource,
    Configuring,
    Building,
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
        && !path.to_string_lossy().contains(['\\', ':'])
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

fn has_complete_release_package_identity(package: &RuntimePackageIdentity) -> bool {
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

fn has_complete_source_package_identity(package: &RuntimePackageIdentity) -> bool {
    package
        .repository
        .as_deref()
        .is_some_and(|value| !value.is_empty())
        && package
            .release_tag
            .as_deref()
            .is_none_or(|value| !value.trim().is_empty())
        && package.asset_id.is_none()
        && package.asset_name.is_none()
        && package.additional_assets.is_empty()
}

fn validate_native_identities(
    formats: &[ArtifactFormat],
    identities: &[ArtifactNativeIdentity],
) -> Result<(), RuntimeManifestError> {
    if identities.iter().any(|identity| match identity {
        ArtifactNativeIdentity::Ninfer(_) => !formats.contains(&ArtifactFormat::Ninfer),
    }) {
        return Err(RuntimeManifestError::InvalidNativeCapabilities);
    }
    Ok(())
}

fn validate_source_build_plan(
    plan: &RuntimeSourceBuildPlan,
    identity: &RuntimeIdentity,
) -> Result<(), RuntimeManifestError> {
    validate_source_snapshot(&plan.source)?;
    if !has_complete_source_package_identity(&identity.package)
        || identity.package.repository.as_deref() != Some(plan.source.repository.as_str())
        || identity.package.provider_id != plan.source.source_provider
        || identity.upstream_revision.as_deref() != Some(plan.source.commit_sha.as_str())
        || identity
            .package
            .release_tag
            .as_deref()
            .is_some_and(|tag| tag != plan.source.source_branch)
    {
        return Err(RuntimeManifestError::InvalidSourceBuild(
            "source snapshot does not match the runtime/provider identity".to_owned(),
        ));
    }
    let recipe = &plan.recipe;
    if recipe.recipe_version.trim().is_empty()
        || recipe.build_target.trim().is_empty()
        || !valid_build_target(recipe.build_system, &recipe.build_target)
        || recipe.accelerator_target.trim().is_empty()
        || !is_safe_relative_path(&recipe.entrypoint)
        || recipe
            .cmake_configuration_arguments
            .iter()
            .any(|argument| argument.is_empty() || argument.contains('\0'))
        || recipe
            .rejected_build_environment
            .iter()
            .any(|name| name.trim().is_empty() || name.contains('=') || name.contains('\0'))
        || recipe
            .build_definition_sha256
            .as_ref()
            .is_some_and(|digest| RuntimeDigest::sha256(digest.clone()).is_err())
        || (recipe.build_system == RuntimeSourceBuildSystem::Make
            && recipe.build_definition_sha256.is_none())
    {
        return Err(RuntimeManifestError::InvalidSourceBuild(
            "build recipe fields are incomplete or unsafe".to_owned(),
        ));
    }
    let prerequisites = &plan.prerequisites;
    if (recipe.build_system == RuntimeSourceBuildSystem::Cmake
        && prerequisites.minimum_cmake_version.trim().is_empty())
        || (recipe.build_system == RuntimeSourceBuildSystem::Make && !prerequisites.requires_make)
        || prerequisites
            .minimum_cuda_version
            .as_deref()
            .is_some_and(|version| version.trim().is_empty())
        || prerequisites
            .maximum_cuda_version_exclusive
            .as_deref()
            .is_some_and(|version| version.trim().is_empty())
        || prerequisites
            .minimum_cpp_standard
            .is_some_and(|standard| standard < 11)
        || prerequisites
            .cpp_compiler
            .as_deref()
            .is_some_and(|compiler| compiler.trim().is_empty() || compiler.contains('\0'))
        || prerequisites
            .cuda_compiler
            .as_deref()
            .is_some_and(|compiler| !compiler.is_absolute())
        || prerequisites
            .pkg_config_modules
            .iter()
            .any(|(name, version)| {
                name.trim().is_empty()
                    || version.trim().is_empty()
                    || name.contains('\0')
                    || version.contains('\0')
            })
    {
        return Err(RuntimeManifestError::InvalidSourceBuild(
            "build prerequisite metadata is incomplete".to_owned(),
        ));
    }
    Ok(())
}

fn valid_build_target(system: RuntimeSourceBuildSystem, target: &str) -> bool {
    if target.contains('\0') || target.chars().any(char::is_whitespace) {
        return false;
    }
    match system {
        RuntimeSourceBuildSystem::Cmake => target
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-')),
        RuntimeSourceBuildSystem::Make => {
            !target.contains(':') && is_safe_relative_path(Path::new(target))
        }
    }
}

fn validate_source_snapshot(source: &RuntimeSourceSnapshot) -> Result<(), RuntimeManifestError> {
    let expected_url = format!("https://github.com/{}.git", source.repository);
    if source.repository.trim().is_empty()
        || source.repository.matches('/').count() != 1
        || source.repository_url != expected_url
        || source.source_branch.trim().is_empty()
        || source.source_branch.contains('\0')
        || source.source_provider.trim().is_empty()
        || !is_full_git_sha(&source.commit_sha)
        || !is_full_git_sha(&source.tree_sha)
        || source.commit_timestamp_unix <= 0
    {
        return Err(RuntimeManifestError::InvalidSourceBuild(
            "source repository, branch, commit, tree, or timestamp is invalid".to_owned(),
        ));
    }
    Ok(())
}

fn validate_source_build_provenance(
    provenance: &RuntimeSourceBuildProvenance,
    manifest: &RuntimeManifest,
) -> Result<(), RuntimeManifestError> {
    validate_source_snapshot(&provenance.source)?;
    RuntimeDigest::sha256(provenance.entrypoint_sha256.clone())?;
    if let Some(digest) = &provenance.build_definition_sha256 {
        RuntimeDigest::sha256(digest.clone())?;
    }
    let fields_complete = !provenance.recipe_version.trim().is_empty()
        && !provenance.build_target.trim().is_empty()
        && valid_build_target(provenance.build_system, &provenance.build_target)
        && !provenance.accelerator_target.trim().is_empty()
        && provenance.built_at_unix > 0
        && match provenance.build_system {
            RuntimeSourceBuildSystem::Cmake => {
                !provenance.toolchain.cmake_version.trim().is_empty()
                    && !provenance.toolchain.ninja_version.trim().is_empty()
            }
            RuntimeSourceBuildSystem::Make => !provenance.toolchain.make_version.trim().is_empty(),
        }
        && !provenance.toolchain.cpp_compiler.trim().is_empty()
        && !provenance.toolchain.nvcc_version.trim().is_empty()
        && !provenance.toolchain.pkg_config_version.trim().is_empty()
        && provenance
            .cmake_configuration_arguments
            .iter()
            .all(|argument| !argument.is_empty() && !argument.contains('\0'));
    let identity_matches = manifest.identity.package.repository.as_deref()
        == Some(provenance.source.repository.as_str())
        && manifest.identity.package.provider_id == provenance.source.source_provider
        && manifest.identity.upstream_revision.as_deref()
            == Some(provenance.source.commit_sha.as_str())
        && manifest.identity.platform == provenance.build_platform
        && manifest.identity.architecture == provenance.build_architecture;
    if !fields_complete
        || !identity_matches
        || provenance.entrypoint != manifest.entrypoint
        || provenance.entrypoint_sha256 != manifest.entrypoint_sha256
        || !is_safe_relative_path(&provenance.entrypoint)
        || !has_complete_source_package_identity(&manifest.identity.package)
    {
        return Err(RuntimeManifestError::InvalidSourceBuildProvenance);
    }
    Ok(())
}

pub fn is_full_git_sha(value: &str) -> bool {
    value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
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
    use crate::NinferArtifactIdentity;

    use super::{
        ArtifactFormat, ArtifactNativeIdentity, RuntimeAcquisitionMethod, RuntimeDigest, RuntimeId,
        RuntimeIdentity, RuntimeManifest, RuntimePackageIdentity, RuntimeProbeObservation,
        RuntimeRequirements, RuntimeSourceBuildProvenance, RuntimeSourceBuildSystem,
        RuntimeSourceBuildToolchain, RuntimeSourceSnapshot, is_safe_relative_path,
    };
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

    #[test]
    fn schema_one_release_manifest_remains_readable_and_valid() {
        let identity = RuntimeIdentity {
            engine_id: "fixture".to_owned(),
            package_family: "fixture-release".to_owned(),
            version: "1.0.0".to_owned(),
            upstream_revision: Some("a".repeat(40)),
            platform: "linux".to_owned(),
            architecture: "x86_64".to_owned(),
            accelerator: "cpu".to_owned(),
            variant: "default".to_owned(),
            package: RuntimePackageIdentity {
                provider_id: "fixture-provider".to_owned(),
                repository: Some("owner/repository".to_owned()),
                release_tag: Some("v1.0.0".to_owned()),
                asset_id: Some("1".to_owned()),
                asset_name: Some("runtime.tar.gz".to_owned()),
                additional_assets: Vec::new(),
            },
        };
        let manifest = RuntimeManifest {
            schema_version: 1,
            runtime_id: RuntimeId::from_identity(&identity),
            identity,
            supported_formats: vec![ArtifactFormat::Gguf],
            supported_native_identities: Vec::new(),
            requirements: RuntimeRequirements::default(),
            acquisition_method: RuntimeAcquisitionMethod::OfficialReleaseAsset,
            source_url: Some("https://github.com/owner/repository/releases/tag/v1.0.0".to_owned()),
            downloaded_archive_sha256: Some("b".repeat(64)),
            additional_downloaded_archive_sha256: Vec::new(),
            source_build: None,
            entrypoint: "server".into(),
            entrypoint_sha256: "c".repeat(64),
            installed_at_unix: Some(1),
            probe: RuntimeProbeObservation {
                compatible: true,
                observed_engine_id: "fixture".to_owned(),
                observed_version: Some("1.0.0".to_owned()),
                observed_revision: None,
                detail: "fixture".to_owned(),
                observed_at_unix: 1,
            },
        };
        let mut legacy = serde_json::to_value(&manifest).expect("serialize manifest");
        let object = legacy.as_object_mut().expect("manifest object");
        object.remove("supported_native_identities");
        object.remove("source_build");
        object
            .get_mut("requirements")
            .and_then(serde_json::Value::as_object_mut)
            .expect("requirements")
            .remove("required_nvidia_device_names");
        let parsed: RuntimeManifest =
            serde_json::from_value(legacy).expect("read schema-one manifest");
        parsed
            .validate()
            .expect("schema-one manifest remains valid");
        assert_eq!(
            parsed.runtime_id,
            RuntimeId::from_identity(&parsed.identity),
            "schema evolution must not alter legacy runtime identity hashing"
        );
    }

    #[test]
    fn source_build_manifest_is_truthful_without_an_archive_digest() {
        let commit = "a".repeat(40);
        let tree = "b".repeat(40);
        let identity = RuntimeIdentity {
            engine_id: "ninfer".to_owned(),
            package_family: "ninfer-source".to_owned(),
            version: "git-20260828-aaaaaaaa".to_owned(),
            upstream_revision: Some(commit.clone()),
            platform: "linux".to_owned(),
            architecture: "x86_64".to_owned(),
            accelerator: "cuda".to_owned(),
            variant: "ninfer-serve-v1-sm120a".to_owned(),
            package: RuntimePackageIdentity {
                provider_id: "ninfer-official-source".to_owned(),
                repository: Some("Neroued/ninfer".to_owned()),
                release_tag: None,
                asset_id: None,
                asset_name: None,
                additional_assets: Vec::new(),
            },
        };
        let entrypoint = Path::new("source/build/apps/ninfer-serve").to_path_buf();
        let entrypoint_sha256 = "c".repeat(64);
        let source = RuntimeSourceSnapshot {
            repository: "Neroued/ninfer".to_owned(),
            repository_url: "https://github.com/Neroued/ninfer.git".to_owned(),
            source_branch: "master".to_owned(),
            commit_sha: commit,
            tree_sha: tree,
            commit_timestamp_unix: 1_787_957_614,
            source_provider: "ninfer-official-source".to_owned(),
        };
        let manifest = RuntimeManifest {
            schema_version: 2,
            runtime_id: RuntimeId::from_identity(&identity),
            identity,
            supported_formats: vec![ArtifactFormat::Ninfer],
            supported_native_identities: vec![ArtifactNativeIdentity::Ninfer(
                NinferArtifactIdentity {
                    container_version: 2,
                    model_id: "qwen3.6-27b".to_owned(),
                    weights_id: "groupwise-int".to_owned(),
                },
            )],
            requirements: RuntimeRequirements::default(),
            acquisition_method: RuntimeAcquisitionMethod::SourceBuild,
            source_url: Some("https://github.com/Neroued/ninfer/commit/aaaaaaaa".to_owned()),
            downloaded_archive_sha256: None,
            additional_downloaded_archive_sha256: Vec::new(),
            source_build: Some(RuntimeSourceBuildProvenance {
                source,
                recipe_version: "ninfer-serve-v1".to_owned(),
                build_system: RuntimeSourceBuildSystem::Cmake,
                build_definition_sha256: None,
                cmake_configuration_arguments: vec!["-G".to_owned(), "Ninja".to_owned()],
                build_target: "ninfer-serve".to_owned(),
                toolchain: RuntimeSourceBuildToolchain {
                    cmake_version: "3.31.0".to_owned(),
                    ninja_version: "1.12.1".to_owned(),
                    make_version: "not required".to_owned(),
                    cpp_compiler: "GNU C++ 14.2".to_owned(),
                    nvcc_version: "13.1".to_owned(),
                    pkg_config_version: "2.3.0".to_owned(),
                    system_dependencies: std::collections::BTreeMap::from([
                        ("libavformat".to_owned(), "61.7".to_owned()),
                        ("libcurl".to_owned(), "8.12".to_owned()),
                    ]),
                },
                build_platform: "linux".to_owned(),
                build_architecture: "x86_64".to_owned(),
                accelerator_target: "sm_120a".to_owned(),
                built_at_unix: 1_787_957_700,
                entrypoint: entrypoint.clone(),
                entrypoint_sha256: entrypoint_sha256.clone(),
            }),
            entrypoint,
            entrypoint_sha256,
            installed_at_unix: Some(1_787_957_700),
            probe: RuntimeProbeObservation {
                compatible: true,
                observed_engine_id: "ninfer".to_owned(),
                observed_version: None,
                observed_revision: None,
                detail: "fixture probe".to_owned(),
                observed_at_unix: 1_787_957_700,
            },
        };
        manifest.validate().expect("truthful source manifest");

        let mut with_fake_archive = manifest;
        with_fake_archive.downloaded_archive_sha256 = Some("d".repeat(64));
        assert!(with_fake_archive.validate().is_err());
    }

    #[test]
    fn external_manifest_remains_valid_without_managed_provenance() {
        let identity = RuntimeIdentity {
            engine_id: "ninfer".to_owned(),
            package_family: "external-binary".to_owned(),
            version: "external".to_owned(),
            upstream_revision: None,
            platform: std::env::consts::OS.to_owned(),
            architecture: std::env::consts::ARCH.to_owned(),
            accelerator: "cuda".to_owned(),
            variant: "external-binary".to_owned(),
            package: RuntimePackageIdentity {
                provider_id: "external".to_owned(),
                repository: None,
                release_tag: None,
                asset_id: None,
                asset_name: None,
                additional_assets: Vec::new(),
            },
        };
        let manifest = RuntimeManifest {
            schema_version: 2,
            runtime_id: RuntimeId::from_identity(&identity),
            identity,
            supported_formats: vec![ArtifactFormat::Ninfer],
            supported_native_identities: Vec::new(),
            requirements: RuntimeRequirements::default(),
            acquisition_method: RuntimeAcquisitionMethod::ExternalBinary,
            source_url: None,
            downloaded_archive_sha256: None,
            additional_downloaded_archive_sha256: Vec::new(),
            source_build: None,
            entrypoint: std::env::current_dir()
                .expect("current directory")
                .join("ninfer-serve"),
            entrypoint_sha256: "e".repeat(64),
            installed_at_unix: None,
            probe: RuntimeProbeObservation {
                compatible: true,
                observed_engine_id: "ninfer".to_owned(),
                observed_version: None,
                observed_revision: None,
                detail: "fixture probe".to_owned(),
                observed_at_unix: 1,
            },
        };
        manifest
            .validate()
            .expect("external manifest remains valid");
    }
}
