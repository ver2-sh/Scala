//! q27 runtime catalog, process adapter, and OpenAI-compatible translation.

use std::cmp::Ordering;
use std::collections::{BTreeMap, VecDeque};
use std::ffi::OsString;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use bytes::Bytes;
use futures_util::stream::BoxStream;
use futures_util::{StreamExt, stream};
use minijinja::{Environment, ErrorKind, context};
use norted_core::{
    AcceleratorDevice, AcquisitionMethod, ArtifactFormat, AuxiliaryArtifactRole, AvailableRuntime,
    ComputeCapability, EngineConfig, EngineInstallation, EngineRevision, HostCapabilities,
    InstalledRuntime, LoadSettingDefinition, LoadSettingId, LoadSettingKind, LoadSettingScope,
    LoadSettingValue, LoadSettingsSchema, ModelArtifact, NortedPackageBinding, NortedPackagePolicy,
    Q27PackagePolicy, RuntimeAcquisitionMethod, RuntimeAcquisitionPlan, RuntimeArchiveFormat,
    RuntimeCompatibility, RuntimeDigest, RuntimeDownload, RuntimeId, RuntimeIdentity,
    RuntimePackageIdentity, RuntimeProbeObservation, RuntimeReleaseChannel, RuntimeRequirements,
};
use norted_engine::{
    ApiCapability, CatalogError, CompatibilityDecision, EffectiveGenerationSettings, EngineAdapter,
    EngineCapabilities, EngineError, EngineFeature, EngineIdentity, EngineProbe,
    GenerationSettingsPatch, GitHubRelease, GitHubReleaseAsset, GitHubReleaseClient,
    InferenceEvent, InferenceFinishReason, InferenceMessage, InferenceOutput, InferenceRequest,
    InferenceRole, InferenceStream, InferenceUsage, InstallationState, LaunchRequest, LaunchSpec,
    NativeOption, OptionValueKind, PreparedAuxiliaryArtifact, PreparedModelInput,
    ProcessDescriptor, RuntimeCatalogProvider, StartupObservation, UpdateState, capture_command,
    common_load_setting_definitions, compatibility_for, compatibility_for_nvidia_device,
    isolated_cuda_environment, prepare_norted_package_input,
    revalidate_norted_package_before_launch, visible_nvidia_devices,
};
use reqwest::redirect::Policy;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::io::AsyncReadExt;

pub const ENGINE_ID: &str = "q27";
pub const UPSTREAM_REPOSITORY: &str = "https://github.com/signalnine/q27";
pub const GITHUB_REPOSITORY: &str = "signalnine/q27";
pub const PROVIDER_ID: &str = "q27-official-github";
pub const PACKAGE_FAMILY: &str = "q27-official-release";

mod model;

use model::{Q27Tier, inspect_q27_model};

const PROBE_TIMEOUT: Duration = Duration::from_secs(15);
const HEALTH_TIMEOUT: Duration = Duration::from_secs(2);
const INFERENCE_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const SSE_FRAME_LIMIT: usize = 1024 * 1024;
const SHARP_TEMPLATE_LIMIT: u64 = 4 * 1024 * 1024;

// These values can change authentication, sampling, or prompt semantics behind
// Norted's back. They are removed from the inherited environment and rejected
// in the adapter-specific environment map.
const MANAGED_ENVIRONMENT_VARIABLES: &[&str] = &[
    "Q27_API_KEY",
    "Q27_FORCE_TEMP",
    "Q27_FORCE_TOP_P",
    "Q27_SAMPLED",
    "Q27_BARE",
    "Q27_KV",
    "Q27_MAXD",
    "Q27_PMIN",
    "Q27_SUFFIX",
    "Q27_SUFFIX_W",
    "CUDA_VISIBLE_DEVICES",
];

// q27 parses MODEL and TOKENIZER positionally. Norted owns those arguments,
// its private bind, authentication state, thinking mode, and sampler defaults.
const MANAGED_NATIVE_ARGUMENTS: &[&str] = &[
    "--host",
    "--port",
    "--api-key",
    "--api-key-file",
    "--think",
    "--no-think",
    "--request-think",
    "--think-budget",
    "--temp",
    "--top-p",
    "--top-k",
    "--min-p",
    "--constrain-tools",
];

// q27 v0.6.2 parses options with a hand-written exact-match argv loop. Keep
// this allowlist at that managed binary contract: unknown arguments are
// silently ignored upstream, and boolean/value arity must not be guessed.
const ALLOWED_VALUE_NATIVE_ARGUMENTS: &[&str] = &[
    "--ctx",
    "--slots",
    "--slot1-ctx",
    "--prefix-cache",
    "--prefix-cache-max-gb",
    "--prefix-cache-min",
    "--prefix-cache-max-tokens",
    "--prefix-cache-step",
    "--prefix-cache-ram-gb",
];
const ALLOWED_BOOLEAN_NATIVE_ARGUMENTS: &[&str] = &["--fast-head", "--no-fast-head", "--kv-fp16"];

#[derive(Debug, Clone, Copy, Default)]
pub struct Q27RuntimeCatalogProvider;

impl Q27RuntimeCatalogProvider {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl RuntimeCatalogProvider for Q27RuntimeCatalogProvider {
    fn id(&self) -> &'static str {
        PROVIDER_ID
    }

    fn engine_id(&self) -> &'static str {
        ENGINE_ID
    }

    fn repository(&self) -> &'static str {
        GITHUB_REPOSITORY
    }

    async fn fetch(
        &self,
        github: &GitHubReleaseClient,
    ) -> Result<Vec<AvailableRuntime>, CatalogError> {
        let releases = github.releases(GITHUB_REPOSITORY).await?;
        catalog_runtimes(&releases)
    }

    async fn fetch_reference(
        &self,
        github: &GitHubReleaseClient,
        reference: &str,
    ) -> Result<Vec<AvailableRuntime>, CatalogError> {
        let Some(tag) = q27_tag_from_reference(reference) else {
            return Ok(Vec::new());
        };
        let Some(release) = github.release_by_tag(GITHUB_REPOSITORY, &tag).await? else {
            return Ok(Vec::new());
        };
        let mut runtimes = catalog_runtimes(&[release])?;
        for runtime in &mut runtimes {
            runtime
                .channels
                .retain(|channel| matches!(channel, RuntimeReleaseChannel::Prerelease));
        }
        Ok(runtimes)
    }
}

fn q27_tag_from_reference(reference: &str) -> Option<String> {
    let candidate = reference
        .trim()
        .strip_prefix('v')
        .unwrap_or(reference.trim());
    let numeric = candidate.split('.').collect::<Vec<_>>();
    if numeric.len() == 3
        && numeric
            .iter()
            .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
    {
        return Some(format!("v{candidate}"));
    }

    let parts = reference.split('-').collect::<Vec<_>>();
    let start = parts.iter().position(|part| *part == "q27")? + 1;
    let version = parts.get(start..start + 3)?;
    version
        .iter()
        .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| format!("v{}.{}.{}", version[0], version[1], version[2]))
}

struct QualifiedAsset<'a> {
    release: &'a GitHubRelease,
    asset: &'a GitHubReleaseAsset,
    version: String,
    digest: RuntimeDigest,
    published_at_unix: Option<i64>,
    ordinal: usize,
}

fn catalog_runtimes(releases: &[GitHubRelease]) -> Result<Vec<AvailableRuntime>, CatalogError> {
    let mut qualified = Vec::new();
    for (ordinal, release) in releases.iter().enumerate() {
        if release.draft {
            continue;
        }
        for asset in &release.assets {
            let Some((version, digest)) = qualify_release_asset(release, asset) else {
                continue;
            };
            qualified.push(QualifiedAsset {
                release,
                asset,
                version,
                digest,
                published_at_unix: release
                    .published_at
                    .as_deref()
                    .and_then(parse_github_timestamp),
                ordinal,
            });
        }
    }

    let latest_release_id = qualified
        .iter()
        .max_by(|left, right| compare_qualified(left, right))
        .map(|qualified| qualified.release.id);
    let stable_release_id = qualified
        .iter()
        .filter(|qualified| !qualified.release.prerelease)
        .max_by(|left, right| compare_qualified(left, right))
        .map(|qualified| qualified.release.id);

    let mut runtimes = Vec::new();
    for qualified in qualified {
        for variant in variants_for(&qualified.version) {
            let identity = RuntimeIdentity {
                engine_id: ENGINE_ID.to_owned(),
                package_family: PACKAGE_FAMILY.to_owned(),
                version: qualified.version.clone(),
                upstream_revision: exact_commit(&qualified.release.target_commitish),
                platform: "linux".to_owned(),
                architecture: "x86_64".to_owned(),
                accelerator: "cuda".to_owned(),
                variant: variant.id.to_owned(),
                package: RuntimePackageIdentity {
                    provider_id: PROVIDER_ID.to_owned(),
                    repository: Some(GITHUB_REPOSITORY.to_owned()),
                    release_tag: Some(qualified.release.tag_name.clone()),
                    asset_id: Some(qualified.asset.id.to_string()),
                    asset_name: Some(qualified.asset.name.clone()),
                    additional_assets: Vec::new(),
                },
            };
            let mut channels = Vec::new();
            if stable_release_id == Some(qualified.release.id) {
                channels.push(RuntimeReleaseChannel::Stable);
            }
            if latest_release_id == Some(qualified.release.id) {
                channels.push(RuntimeReleaseChannel::Latest);
            }
            if qualified.release.prerelease {
                channels.push(RuntimeReleaseChannel::Prerelease);
            }
            let runtime = AvailableRuntime {
                runtime_id: RuntimeId::from_identity(&identity),
                identity,
                display_name: format!(
                    "q27 {} Linux x86_64 CUDA {}",
                    qualified.version, variant.label
                ),
                supported_formats: vec![ArtifactFormat::Q27],
                source_url: qualified.release.html_url.clone(),
                published_at_unix: qualified.published_at_unix,
                channels,
                prerelease: qualified.release.prerelease,
                acquisition: RuntimeAcquisitionPlan::ReleaseAsset {
                    download: RuntimeDownload {
                        url: qualified.asset.browser_download_url.clone(),
                        size_bytes: qualified.asset.size,
                        digest: Some(qualified.digest.clone()),
                        archive_format: RuntimeArchiveFormat::TarGz,
                        entrypoint_names: vec![variant.entrypoint.to_owned()],
                    },
                    additional_downloads: Vec::new(),
                },
                supported_native_identities: Vec::new(),
                requirements: requirements_for(&qualified.version, variant),
            };
            runtime.validate().map_err(|error| CatalogError::Provider {
                provider: PROVIDER_ID.to_owned(),
                message: format!(
                    "official q27 release `{}` produced invalid runtime metadata: {error}",
                    qualified.release.tag_name
                ),
            })?;
            runtimes.push(runtime);
        }
    }
    Ok(runtimes)
}

fn qualify_release_asset(
    release: &GitHubRelease,
    asset: &GitHubReleaseAsset,
) -> Option<(String, RuntimeDigest)> {
    if asset.state != "uploaded" || asset.size == 0 {
        return None;
    }
    let version = asset
        .name
        .strip_prefix("q27-v")?
        .strip_suffix("-linux-x86_64.tar.gz")?;
    if version.is_empty()
        || release.tag_name != format!("v{version}")
        || !valid_version_component(version)
        || !version_at_least(version, 0, 2, 0)
    {
        return None;
    }
    let expected_url_prefix = format!(
        "https://github.com/{GITHUB_REPOSITORY}/releases/download/{}/",
        release.tag_name
    );
    if !asset.browser_download_url.starts_with(&expected_url_prefix) {
        return None;
    }
    let digest = RuntimeDigest::parse_github(asset.digest.as_deref()?).ok()?;
    Some((version.to_owned(), digest))
}

fn valid_version_component(version: &str) -> bool {
    version
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
}

fn compare_qualified(left: &QualifiedAsset<'_>, right: &QualifiedAsset<'_>) -> Ordering {
    left.published_at_unix
        .cmp(&right.published_at_unix)
        .then_with(|| compare_versions(&left.version, &right.version))
        // GitHub returns releases newest first; a lower ordinal wins a final tie.
        .then_with(|| right.ordinal.cmp(&left.ordinal))
}

fn compare_versions(left: &str, right: &str) -> Ordering {
    let numeric = |value: &str| {
        value
            .split(['.', '-', '_'])
            .map(|part| part.parse::<u64>().ok())
            .collect::<Vec<_>>()
    };
    let left_numeric = numeric(left);
    let right_numeric = numeric(right);
    for index in 0..left_numeric.len().max(right_numeric.len()) {
        match (
            left_numeric.get(index).copied().flatten(),
            right_numeric.get(index).copied().flatten(),
        ) {
            (Some(left), Some(right)) if left != right => return left.cmp(&right),
            (Some(left), None) if left != 0 => return Ordering::Greater,
            (None, Some(right)) if right != 0 => return Ordering::Less,
            _ => {}
        }
    }
    left.cmp(right)
}

#[derive(Clone, Copy)]
struct RuntimeVariant {
    id: &'static str,
    label: &'static str,
    entrypoint: &'static str,
    minimum_vram_class_gib: Option<u16>,
    minimum_vram_exclusive_class_gib: Option<u16>,
}

const Q27_VARIANTS: &[RuntimeVariant] = &[
    RuntimeVariant {
        id: "w8",
        label: "W8",
        entrypoint: "q27-server-w8",
        minimum_vram_class_gib: Some(24),
        minimum_vram_exclusive_class_gib: None,
    },
    RuntimeVariant {
        id: "w12",
        label: "W12",
        entrypoint: "q27-server",
        minimum_vram_class_gib: Some(32),
        minimum_vram_exclusive_class_gib: None,
    },
    RuntimeVariant {
        id: "w16",
        label: "W16",
        entrypoint: "q27-server-w16",
        minimum_vram_class_gib: None,
        // W16 is wider than the W12 build that already OOMs on 24 GiB. This is
        // a proven lower bound, not an invented exact W16 requirement.
        minimum_vram_exclusive_class_gib: Some(24),
    },
];

fn variants_for(_version: &str) -> &'static [RuntimeVariant] {
    Q27_VARIANTS
}

fn requirements_for(version: &str, variant: &RuntimeVariant) -> RuntimeRequirements {
    let tri_arch = version_at_least(version, 0, 3, 1);
    let supported_cuda_compute_capabilities = if tri_arch {
        vec![
            ComputeCapability::new(8, 6),
            ComputeCapability::new(8, 9),
            ComputeCapability::new(12, 0),
        ]
    } else {
        vec![ComputeCapability::new(8, 6), ComputeCapability::new(12, 0)]
    };
    let mut advisories = vec![
        match variant.id {
            "w8" => "W8 is the q27 build intended for 24 GiB cards".to_owned(),
            "w12" => {
                "W12 is q27's default server build and needs a 32 GiB-class card"
                    .to_owned()
            }
            _ => "W16 is a specialist repetition-heavy/file-re-emission build, not q27's recommended live-traffic default; upstream publishes no separate W16 VRAM floor"
                .to_owned(),
        },
        "Model-tier VRAM is checked separately from Q27 metadata before selection".to_owned(),
    ];
    let mut unverified_requirements = Vec::new();
    if variant.id == "w16" {
        unverified_requirements.push(
            "W16 needs more VRAM than a 24 GiB-class device, but upstream has not published an exact specialist-build floor"
                .to_owned(),
        );
    }
    if tri_arch {
        advisories.push(
            "Prebuilt binaries statically link CUDA 13.2; q27 documents NVIDIA driver branch r580 or newer"
                .to_owned(),
        );
    }
    if version == "0.6.2" {
        unverified_requirements.push(
            "The v0.6.2 ELF requires glibc 2.38 and libstdc++ with GLIBCXX_3.4.32".to_owned(),
        );
    }
    RuntimeRequirements {
        requires_nvidia_gpu: true,
        minimum_nvidia_driver: tri_arch.then(|| "580".to_owned()),
        minimum_vram_bytes: None,
        minimum_vram_class_gib: variant.minimum_vram_class_gib,
        minimum_vram_exclusive_class_gib: variant.minimum_vram_exclusive_class_gib,
        supported_cuda_compute_capabilities,
        required_nvidia_device_names: Vec::new(),
        notes: Vec::new(),
        advisories,
        unverified_requirements,
    }
}

fn q27_tier_compatibility(tier: Q27Tier, device: &AcceleratorDevice) -> RuntimeCompatibility {
    compatibility_for_nvidia_device(
        &RuntimeRequirements {
            requires_nvidia_gpu: true,
            minimum_vram_class_gib: Some(tier.minimum_vram_class_gib()),
            ..RuntimeRequirements::default()
        },
        device,
    )
}

fn q27_runtime_preference(variant: &str, device: Option<&AcceleratorDevice>) -> u16 {
    if variant == "w16" {
        return 1000;
    }
    let class_32 = RuntimeRequirements {
        requires_nvidia_gpu: true,
        minimum_vram_class_gib: Some(32),
        ..RuntimeRequirements::default()
    };
    let confirmed_32 = device.is_some_and(|device| {
        matches!(
            compatibility_for_nvidia_device(&class_32, device),
            RuntimeCompatibility::Recommended | RuntimeCompatibility::Compatible
        )
    });
    match variant {
        "w12" if confirmed_32 => 0,
        "w8" if !confirmed_32 => 0,
        "w8" | "w12" => 10,
        _ => 100,
    }
}

/// The official q27 Makefile builds W8/W16 with explicit Q27_W_MAX defines
/// and the ordinary server with engine.cuh's Q27_W_MAX=12 default. This is an
/// exact package-identity capability, not a filename or model-name guess.
fn q27_compiled_w_max(identity: &RuntimeIdentity, external: bool) -> Option<u64> {
    if external
        || identity.engine_id != ENGINE_ID
        || identity.package.repository.as_deref() != Some(GITHUB_REPOSITORY)
        || identity.version != "0.6.2"
    {
        return None;
    }
    match identity.variant.as_str() {
        "w8" => Some(8),
        "w12" => Some(12),
        "w16" => Some(16),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum Q27PackageKvMode {
    Fp8,
    Turbo5k,
    Turbo3,
}

impl Q27PackageKvMode {
    const QUALITY_ORDER: [Self; 3] = [Self::Fp8, Self::Turbo5k, Self::Turbo3];

    const fn as_str(self) -> &'static str {
        match self {
            Self::Fp8 => "fp8",
            Self::Turbo5k => "turbo5k",
            Self::Turbo3 => "turbo3",
        }
    }
}

const Q27_V062_PACKAGE_KV_MODES: &[Q27PackageKvMode] =
    &[Q27PackageKvMode::Fp8, Q27PackageKvMode::Turbo3];
#[cfg(test)]
const ALL_PACKAGE_KV_MODES: &[Q27PackageKvMode] = &Q27PackageKvMode::QUALITY_ORDER;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
struct Q27PackageRuntimeCapabilities {
    trustworthy_identity: bool,
    raw_completions: bool,
    exact_sharp_renderer: bool,
    thinking: bool,
    unlimited_think_budget: bool,
    temperature_top_p: bool,
    top_k_min_p: bool,
    mtp_environment: bool,
    fast_head_control: bool,
    bounded_startup_observation: bool,
    compiled_w_max: Option<u64>,
    supported_kv_modes: &'static [Q27PackageKvMode],
}

fn q27_package_capabilities(
    identity: &RuntimeIdentity,
    acquisition: &RuntimeAcquisitionMethod,
) -> Q27PackageRuntimeCapabilities {
    let external = acquisition == &RuntimeAcquisitionMethod::ExternalBinary;
    let exact_managed_v062 = matches!(
        acquisition,
        RuntimeAcquisitionMethod::OfficialReleaseAsset
            | RuntimeAcquisitionMethod::PreseededOfficialPack
    ) && identity.engine_id == ENGINE_ID
        && identity.package_family == PACKAGE_FAMILY
        && identity.package.provider_id == PROVIDER_ID
        && identity.package.repository.as_deref() == Some(GITHUB_REPOSITORY)
        && identity.package.release_tag.as_deref() == Some("v0.6.2")
        && identity.version == "0.6.2";
    Q27PackageRuntimeCapabilities {
        trustworthy_identity: exact_managed_v062,
        // These are proven directly from q27 v0.6.2 source. New runtime
        // identities remain unknown until their own provenance/probe is wired.
        raw_completions: exact_managed_v062,
        exact_sharp_renderer: exact_managed_v062,
        thinking: exact_managed_v062,
        // v0.6.2 has no separate thinking-budget limiter: the ordinary
        // max-token limit is the only generation bound on its raw route.
        unlimited_think_budget: exact_managed_v062,
        temperature_top_p: exact_managed_v062,
        top_k_min_p: false,
        mtp_environment: exact_managed_v062,
        fast_head_control: exact_managed_v062,
        bounded_startup_observation: exact_managed_v062,
        compiled_w_max: q27_compiled_w_max(identity, external),
        // q27 v0.6.2 source contains fp8 and turbo3. It does not contain the
        // package policy's intermediate turbo5k mode.
        supported_kv_modes: if exact_managed_v062 {
            Q27_V062_PACKAGE_KV_MODES
        } else {
            &[]
        },
    }
}

fn q27_package_prelaunch_failures(
    capabilities: Q27PackageRuntimeCapabilities,
) -> Vec<&'static str> {
    let mut reasons = Vec::new();
    if !capabilities.raw_completions {
        reasons.push("raw /v1/completions prompt handling is unproven");
    }
    if !capabilities.exact_sharp_renderer {
        reasons.push("exact manifest-bound Sharp application is unproven");
    }
    if !capabilities.thinking {
        reasons.push("thinking-enabled package execution is unproven");
    }
    if !capabilities.unlimited_think_budget {
        reasons.push("unlimited thinking budget is unsupported/unproven");
    }
    if !capabilities.temperature_top_p {
        reasons.push("temperature/top-p package defaults are unsupported/unproven");
    }
    if !capabilities.top_k_min_p {
        reasons.push("top-k/min-p package defaults are unsupported/unproven");
    }
    if !capabilities.mtp_environment {
        reasons.push("Q27_MAXD/Q27_PMIN/Q27_SUFFIX package policy is unproven");
    }
    if !capabilities.fast_head_control {
        reasons.push("fast-head package control is unproven");
    }
    if !capabilities.bounded_startup_observation {
        reasons.push("served context/KV startup observation is unproven");
    }
    if capabilities.compiled_w_max.is_none() {
        reasons.push("numeric compiled W_MAX is unproven");
    }
    if capabilities.supported_kv_modes.is_empty() {
        reasons.push("no package-policy KV mode is proven for this executable");
    }
    reasons
}

fn evaluate_q27_package_runtime(
    capabilities: Q27PackageRuntimeCapabilities,
) -> RuntimeCompatibility {
    match validate_q27_package_prelaunch(capabilities) {
        Ok(()) => RuntimeCompatibility::NeedsAttention(
            "pre-launch q27 package capabilities are proven; actual served context, KV mode, and W_MAX still require bounded startup observation"
                .to_owned(),
        ),
        Err(reason) => RuntimeCompatibility::Incompatible(format!(
            "{reason}; actual served context and selected KV mode are not yet proven because startup cannot proceed"
        )),
    }
}

fn validate_q27_package_prelaunch(
    capabilities: Q27PackageRuntimeCapabilities,
) -> Result<(), String> {
    if !capabilities.trustworthy_identity {
        return Err(
            "the exact q27 executable has no trustworthy package-capability observation; external binaries are not credited from filenames or upstream version assumptions"
                .to_owned(),
        );
    }
    let failures = q27_package_prelaunch_failures(capabilities);
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("; "))
    }
}

fn q27_package_kv_attempt_modes(
    capabilities: Q27PackageRuntimeCapabilities,
    policy: &Q27PackagePolicy,
) -> Vec<Q27PackageKvMode> {
    Q27PackageKvMode::QUALITY_ORDER
        .into_iter()
        .filter(|mode| capabilities.supported_kv_modes.contains(mode))
        .filter(|mode| {
            policy
                .kv_preference
                .iter()
                .any(|declared| declared == mode.as_str())
        })
        .collect()
}

#[derive(Debug, Clone)]
struct Q27DeviceEvaluation {
    compatibility: RuntimeCompatibility,
    accelerator: Option<AcceleratorDevice>,
}

fn q27_device_evaluation(
    platform: &str,
    architecture: &str,
    requirements: &RuntimeRequirements,
    tier: Option<Q27Tier>,
    host: &HostCapabilities,
    external_build: bool,
) -> Q27DeviceEvaluation {
    let devices = match q27_visible_devices(host) {
        Ok(devices) => devices,
        Err(compatibility) => {
            return Q27DeviceEvaluation {
                compatibility,
                accelerator: None,
            };
        }
    };
    let mut effective_requirements = requirements.clone();
    effective_requirements.requires_nvidia_gpu = true;
    if external_build {
        effective_requirements.unverified_requirements.push(
            "external q27 build variant, runtime-specific VRAM floor, and CUDA targets are unverified"
                .to_owned(),
        );
    }
    let unknown_tier = tier.is_none().then(|| {
        RuntimeCompatibility::NeedsAttention(
            "Q27 architecture is supported, but the exact published model tier cannot be proven from quant_policy/q4_head/q8_extra metadata"
                .to_owned(),
        )
    });
    let mut evaluated = devices
        .into_iter()
        .map(|device| {
            let device_host = HostCapabilities {
                platform: host.platform.clone(),
                architecture: host.architecture.clone(),
                accelerators: vec![device.clone()],
                cuda_visible_devices: None,
                observations: Vec::new(),
            };
            let runtime = compatibility_for(
                platform,
                architecture,
                "cuda",
                &effective_requirements,
                &device_host,
            );
            let model = tier.map_or_else(
                || unknown_tier.clone().expect("missing tier compatibility"),
                |tier| q27_tier_compatibility(tier, device),
            );
            (device, combine_q27_compatibility(runtime, model))
        })
        .collect::<Vec<_>>();
    evaluated.sort_by(|left, right| {
        left.1
            .preference_rank()
            .cmp(&right.1.preference_rank())
            .then_with(|| {
                right
                    .0
                    .vram_bytes
                    .unwrap_or(0)
                    .cmp(&left.0.vram_bytes.unwrap_or(0))
            })
            .then_with(|| left.0.stable_id.cmp(&right.0.stable_id))
    });
    let Some((device, compatibility)) = evaluated.into_iter().next() else {
        return Q27DeviceEvaluation {
            compatibility: RuntimeCompatibility::NeedsAttention(
                "q27 requires a stable NVIDIA GPU UUID, but no observed CUDA device had one"
                    .to_owned(),
            ),
            accelerator: None,
        };
    };
    Q27DeviceEvaluation {
        compatibility,
        accelerator: Some(device.clone()),
    }
}

fn q27_visible_devices(
    host: &HostCapabilities,
) -> Result<Vec<&AcceleratorDevice>, RuntimeCompatibility> {
    visible_nvidia_devices(host, "q27")
}

fn combine_q27_compatibility(
    runtime: RuntimeCompatibility,
    model: RuntimeCompatibility,
) -> RuntimeCompatibility {
    match (runtime, model) {
        (RuntimeCompatibility::Incompatible(reason), _)
        | (_, RuntimeCompatibility::Incompatible(reason)) => {
            RuntimeCompatibility::Incompatible(reason)
        }
        (RuntimeCompatibility::NeedsAttention(reason), _)
        | (_, RuntimeCompatibility::NeedsAttention(reason)) => {
            RuntimeCompatibility::NeedsAttention(reason)
        }
        (RuntimeCompatibility::Recommended, RuntimeCompatibility::Recommended) => {
            RuntimeCompatibility::Recommended
        }
        _ => RuntimeCompatibility::Compatible,
    }
}

fn qualify_tier_compatibility(
    tier: Option<Q27Tier>,
    compatibility: RuntimeCompatibility,
) -> RuntimeCompatibility {
    let Some(tier) = tier else {
        return compatibility;
    };
    match compatibility {
        RuntimeCompatibility::Recommended | RuntimeCompatibility::Compatible => {
            RuntimeCompatibility::Recommended
        }
        RuntimeCompatibility::NeedsAttention(reason) => RuntimeCompatibility::NeedsAttention(
            format!("{} tier requirement is uncertain: {reason}", tier.label()),
        ),
        RuntimeCompatibility::Incompatible(reason) => RuntimeCompatibility::Incompatible(format!(
            "{} tier is incompatible: {reason}",
            tier.label()
        )),
    }
}

fn version_at_least(version: &str, major: u64, minor: u64, patch: u64) -> bool {
    compare_versions(version, &format!("{major}.{minor}.{patch}")) != Ordering::Less
}

fn exact_commit(value: &str) -> Option<String> {
    let value = value.trim();
    (value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .then(|| value.to_ascii_lowercase())
}

fn parse_github_timestamp(value: &str) -> Option<i64> {
    if value.len() != 20 || !value.ends_with('Z') {
        return None;
    }
    let bytes = value.as_bytes();
    if bytes[4] != b'-'
        || bytes[7] != b'-'
        || bytes[10] != b'T'
        || bytes[13] != b':'
        || bytes[16] != b':'
    {
        return None;
    }
    let number = |start: usize, end: usize| value.get(start..end)?.parse::<i64>().ok();
    let year = number(0, 4)?;
    let month = number(5, 7)?;
    let day = number(8, 10)?;
    let hour = number(11, 13)?;
    let minute = number(14, 16)?;
    let second = number(17, 19)?;
    if !(1..=12).contains(&month)
        || day < 1
        || day > days_in_month(year, month)
        || !(0..=23).contains(&hour)
        || !(0..=59).contains(&minute)
        || !(0..=60).contains(&second)
    {
        return None;
    }
    let adjusted_year = year - i64::from(month <= 2);
    let era = adjusted_year.div_euclid(400);
    let year_of_era = adjusted_year - era * 400;
    let adjusted_month = month + if month > 2 { -3 } else { 9 };
    let day_of_year = (153 * adjusted_month + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days_since_epoch = era * 146_097 + day_of_era - 719_468;
    days_since_epoch
        .checked_mul(86_400)?
        .checked_add(hour * 3_600 + minute * 60 + second)
}

fn days_in_month(year: i64, month: i64) -> i64 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        _ => 0,
    }
}

pub struct Q27Adapter {
    enabled: bool,
    binary_path: Option<PathBuf>,
    native_arguments: Vec<String>,
    environment: BTreeMap<String, String>,
    configuration_error: Option<String>,
    client: reqwest::Client,
    capability_cache: tokio::sync::RwLock<BTreeMap<String, String>>,
    package_executions: tokio::sync::RwLock<BTreeMap<String, Q27PackageExecution>>,
}

#[derive(Debug, Clone)]
struct Q27PackageExecution {
    sharp_template: String,
    policy: Q27PackagePolicy,
    compiled_w_max: u64,
    expected_fast_head: bool,
    selected_kv_mode: Q27PackageKvMode,
}

#[derive(Debug)]
struct Q27PackageLaunchPolicy {
    arguments: Vec<OsString>,
    environment: BTreeMap<String, String>,
    normalized_settings: BTreeMap<String, Value>,
}

fn q27_package_policy(package: &NortedPackageBinding) -> Result<&Q27PackagePolicy, EngineError> {
    match &package.policy {
        NortedPackagePolicy::Q27(policy) => Ok(policy),
        _ => Err(EngineError::InvalidConfiguration(
            "q27 received a non-q27 Norted package policy".to_owned(),
        )),
    }
}

fn q27_selected_fast_head(
    settings: &norted_core::ResolvedLoadSettings,
    policy: &Q27PackagePolicy,
) -> Result<bool, EngineError> {
    match settings.value("q27.fast_head") {
        Some(LoadSettingValue::Toggle(value)) => Ok(*value),
        None => Ok(policy.fast_head_default),
        Some(_) => Err(EngineError::InvalidConfiguration(
            "q27.fast_head has an invalid package-policy value".to_owned(),
        )),
    }
}

fn q27_package_launch_policy(
    package: &NortedPackageBinding,
    compiled_w_max: u64,
    fast_head: bool,
    kv_mode: Q27PackageKvMode,
) -> Result<Q27PackageLaunchPolicy, EngineError> {
    let policy = q27_package_policy(package)?;
    let arguments = [
        "--think".to_owned(),
        "--think-budget".to_owned(),
        "0".to_owned(),
        "--temp".to_owned(),
        policy.temperature.to_string(),
        "--top-p".to_owned(),
        policy.top_p.to_string(),
        "--top-k".to_owned(),
        policy.top_k.to_string(),
        "--min-p".to_owned(),
        policy.min_p.to_string(),
    ]
    .into_iter()
    .map(OsString::from)
    .collect();
    let environment = BTreeMap::from([
        ("Q27_KV".to_owned(), kv_mode.as_str().to_owned()),
        ("Q27_MAXD".to_owned(), policy.maximum_mtp_depth.clone()),
        (
            "Q27_PMIN".to_owned(),
            policy.mtp_minimum_probability.to_string(),
        ),
        (
            "Q27_SUFFIX".to_owned(),
            u8::from(policy.suffix_drafting).to_string(),
        ),
        ("Q27_SUFFIX_W".to_owned(), compiled_w_max.to_string()),
    ]);
    let normalized_settings = BTreeMap::from([
        ("temperature".to_owned(), json!(policy.temperature)),
        ("top_p".to_owned(), json!(policy.top_p)),
        ("top_k".to_owned(), json!(policy.top_k)),
        ("min_p".to_owned(), json!(policy.min_p)),
        ("thinking".to_owned(), json!(policy.thinking_enabled)),
        ("thinking_budget".to_owned(), json!("unlimited")),
        (
            "default_reasoning_effort".to_owned(),
            json!(policy.default_reasoning_effort),
        ),
        ("requested_context".to_owned(), json!("auto")),
        (
            "minimum_context_tokens".to_owned(),
            json!(policy.minimum_context_tokens),
        ),
        ("requested_kv_mode".to_owned(), json!(kv_mode.as_str())),
        ("compiled_w_max".to_owned(), json!(compiled_w_max)),
        ("fast_head".to_owned(), json!(fast_head)),
    ]);
    Ok(Q27PackageLaunchPolicy {
        arguments,
        environment,
        normalized_settings,
    })
}

impl Q27Adapter {
    pub fn from_config(config: Option<&EngineConfig>, config_directory: &Path) -> Self {
        let mut enabled = config.is_none();
        let mut binary_path = None;
        let mut native_arguments = Vec::new();
        let mut environment = BTreeMap::new();
        let mut configuration_error = None;

        if let Some(config) = config {
            enabled = config.enabled;
            environment = config.env.clone();
            if let Some(name) = environment
                .keys()
                .find(|name| conflicts_with_managed_environment(name))
            {
                configuration_error = Some(format!(
                    "environment variable `{name}` conflicts with the Norted-managed q27 backend contract"
                ));
            }
            for key in config.settings.keys() {
                if key != "binary_path" {
                    configuration_error = Some(format!(
                        "unsupported q27 setting `{key}`; only `binary_path` is supported"
                    ));
                    break;
                }
            }
            if configuration_error.is_none()
                && let Some(value) = config.settings.get("binary_path")
            {
                match value.as_str() {
                    Some(value) if !value.trim().is_empty() => {
                        let configured = PathBuf::from(value);
                        binary_path = Some(if configured.is_absolute() {
                            configured
                        } else {
                            config_directory.join(configured)
                        });
                    }
                    _ => {
                        configuration_error =
                            Some("q27 `binary_path` must be a non-empty string".to_owned());
                    }
                }
            }
            for key in config.native.keys() {
                if key != "arguments" {
                    configuration_error = Some(format!(
                        "unsupported q27 native setting `{key}`; use `arguments = [...]`"
                    ));
                    break;
                }
            }
            if configuration_error.is_none()
                && let Some(value) = config.native.get("arguments")
            {
                match value.as_array() {
                    Some(values) => {
                        for value in values {
                            match value.as_str() {
                                Some(value) if !value.contains('\0') => {
                                    native_arguments.push(value.to_owned())
                                }
                                Some(_) => {
                                    configuration_error = Some(
                                        "q27 native arguments cannot contain NUL bytes".to_owned(),
                                    );
                                    break;
                                }
                                None => {
                                    configuration_error =
                                        Some("q27 native arguments must all be strings".to_owned());
                                    break;
                                }
                            }
                        }
                    }
                    None => {
                        configuration_error =
                            Some("q27 native `arguments` must be an array of strings".to_owned());
                    }
                }
            }
            if configuration_error.is_none()
                && let Some(argument) = native_arguments
                    .iter()
                    .find(|argument| conflicts_with_managed_argument(argument))
            {
                configuration_error = Some(format!(
                    "native argument `{argument}` conflicts with the Norted-managed q27 backend contract"
                ));
            }
            if configuration_error.is_none()
                && let Some(argument) = invalid_native_argument(&native_arguments)
            {
                configuration_error = Some(format!(
                    "native argument `{argument}` is unsupported or has invalid arity; Norted accepts only known q27 v0.6.2 tuning options and owns the model/tokenizer positions"
                ));
            }
        }

        let client = reqwest::Client::builder()
            .redirect(Policy::none())
            .build()
            .unwrap_or_else(|error| {
                configuration_error.get_or_insert_with(|| {
                    format!("could not create the private q27 HTTP client: {error}")
                });
                reqwest::Client::new()
            });
        Self {
            enabled,
            binary_path,
            native_arguments,
            environment,
            configuration_error,
            client,
            capability_cache: tokio::sync::RwLock::new(BTreeMap::new()),
            package_executions: tokio::sync::RwLock::new(BTreeMap::new()),
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    async fn probe_uncached(&self) -> EngineProbe {
        if !self.enabled {
            return EngineProbe {
                installation: InstallationState::NotInstalled,
                update: UpdateState::Unknown,
                healthy: false,
                detail: "q27 adapter is disabled in configuration".to_owned(),
            };
        }
        if let Some(error) = &self.configuration_error {
            return invalid_probe(error.clone());
        }
        let Some(configured_path) = &self.binary_path else {
            return EngineProbe {
                installation: InstallationState::NotInstalled,
                update: UpdateState::Unknown,
                healthy: false,
                detail: "no external q27-server is configured; managed q27 runtime packs remain available"
                    .to_owned(),
            };
        };
        let (binary_path, binary_sha256, observation) =
            match self.inspect_binary(configured_path, None).await {
                Ok(observation) => observation,
                Err(error) => return invalid_probe(error.to_string()),
            };
        EngineProbe {
            installation: InstallationState::Installed {
                installation: Box::new(EngineInstallation {
                    engine: EngineRevision {
                        engine_id: ENGINE_ID.to_owned(),
                        version: None,
                        revision: None,
                    },
                    source_repository: None,
                    acquisition_method: AcquisitionMethod::ExternalBinary,
                    binary_path,
                    binary_sha256: Some(binary_sha256),
                    build: None,
                    platform: std::env::consts::OS.to_owned(),
                    architecture: std::env::consts::ARCH.to_owned(),
                    runtime_variant: Some("external-binary".to_owned()),
                    acquired_at_unix: None,
                    observed_at_unix: observation.observed_at_unix,
                }),
            },
            update: UpdateState::Unknown,
            healthy: true,
            detail: format!(
                "external configured q27-server binary; {}",
                observation.detail
            ),
        }
    }

    async fn inspect_binary(
        &self,
        configured_path: &Path,
        expected_sha256: Option<&str>,
    ) -> Result<(PathBuf, String, RuntimeProbeObservation), EngineError> {
        let binary_path = tokio::fs::canonicalize(configured_path)
            .await
            .map_err(|error| {
                EngineError::InvalidConfiguration(format!(
                    "q27-server entrypoint {} could not be resolved: {error}",
                    configured_path.display()
                ))
            })?;
        let metadata = tokio::fs::metadata(&binary_path).await.map_err(|error| {
            EngineError::InvalidConfiguration(format!(
                "q27-server entrypoint {} could not be inspected: {error}",
                binary_path.display()
            ))
        })?;
        if !metadata.is_file() {
            return Err(EngineError::InvalidConfiguration(
                "q27-server entrypoint is not a regular file".to_owned(),
            ));
        }
        let binary_sha256 = hash_file(&binary_path).await.map_err(|error| {
            EngineError::Operation(format!("could not hash entrypoint: {error}"))
        })?;
        if let Some(expected) = expected_sha256
            && !binary_sha256.eq_ignore_ascii_case(expected)
        {
            return Err(EngineError::InvalidConfiguration(format!(
                "runtime entrypoint SHA-256 mismatch: expected {expected}, observed {binary_sha256}"
            )));
        }
        let usage_output = capture_command(
            &binary_path,
            &[],
            &self.environment,
            &managed_environment_removals(),
            PROBE_TIMEOUT,
        )
        .await?;
        let usage = command_detail(&usage_output.stdout, &usage_output.stderr);
        let usage_contract_error = q27_usage_contract_error(&usage, &self.native_arguments);
        if usage_output.success || usage_contract_error.is_some() {
            return Err(EngineError::InvalidConfiguration(format!(
                "entrypoint does not satisfy the q27-server launch contract ({}): {usage}",
                usage_contract_error.unwrap_or_else(|| {
                    "invocation without positional model arguments unexpectedly succeeded"
                        .to_owned()
                })
            )));
        }
        self.capability_cache
            .write()
            .await
            .insert(binary_sha256.clone(), usage.clone());
        let observed_at_unix = unix_timestamp();
        Ok((
            binary_path,
            binary_sha256,
            RuntimeProbeObservation {
                compatible: true,
                observed_engine_id: ENGINE_ID.to_owned(),
                // q27-server has no --version contract. The release version is
                // retained in RuntimeIdentity, never invented as an observation.
                observed_version: None,
                observed_revision: None,
                detail: "q27-server positional usage signature recognized; executable reports no version"
                    .to_owned(),
                observed_at_unix,
            },
        ))
    }

    fn backend_request(&self, request: &InferenceRequest, stream: bool) -> Value {
        let mut body = json!({
            "model": request.model_id.0,
            "messages": request.messages.iter().map(message_json).collect::<Vec<_>>(),
            "temperature": request.generation_settings.temperature.unwrap_or(0.0),
            "top_p": request.generation_settings.top_p.unwrap_or(1.0),
            "stream": stream,
        });
        if let Some(maximum) = request.max_output_tokens {
            body["max_tokens"] = json!(maximum);
        }
        if stream {
            body["stream_options"] = json!({ "include_usage": true });
        }
        body
    }

    fn package_backend_request(
        execution: &Q27PackageExecution,
        request: &InferenceRequest,
        stream: bool,
    ) -> Result<Value, EngineError> {
        let mut body = json!({
            "model": request.model_id.0,
            "prompt": render_sharp_template(&execution.sharp_template, &request.messages)?,
            "temperature": request
                .generation_settings
                .temperature
                .unwrap_or(execution.policy.temperature),
            "top_p": request
                .generation_settings
                .top_p
                .unwrap_or(execution.policy.top_p),
            "top_k": execution.policy.top_k,
            "min_p": execution.policy.min_p,
            "stream": stream,
        });
        if let Some(maximum) = request.max_output_tokens {
            body["max_tokens"] = json!(maximum);
        }
        if stream {
            body["stream_options"] = json!({ "include_usage": true });
        }
        Ok(body)
    }
}

#[async_trait]
impl EngineAdapter for Q27Adapter {
    fn identity(&self) -> EngineIdentity {
        EngineIdentity {
            id: ENGINE_ID.to_owned(),
            display_name: "q27".to_owned(),
            upstream_repository: UPSTREAM_REPOSITORY.to_owned(),
        }
    }

    fn capabilities(&self) -> EngineCapabilities {
        EngineCapabilities {
            artifact_formats: vec![ArtifactFormat::Q27],
            api: vec![ApiCapability::ChatCompletions],
            features: vec![EngineFeature::TextGeneration],
        }
    }

    fn validate_generation_settings(
        &self,
        settings: &GenerationSettingsPatch,
        backend_defaults: &EffectiveGenerationSettings,
    ) -> Result<(), EngineError> {
        if let Some(temperature) = settings.temperature
            && (!temperature.is_finite() || !(0.0..=2.0).contains(&temperature))
        {
            return Err(EngineError::InvalidGenerationSettings(
                "q27 temperature must be finite and in the range 0..=2".to_owned(),
            ));
        }
        if let Some(top_p) = settings.top_p
            && !(top_p.is_finite() && 0.0 < top_p && top_p <= 1.0)
        {
            return Err(EngineError::InvalidGenerationSettings(
                "q27 top_p must be finite and in the range 0 < top_p <= 1; q27 reinterprets other values as 1"
                    .to_owned(),
            ));
        }
        let effective_temperature = settings.temperature.unwrap_or(backend_defaults.temperature);
        if settings.top_p.is_some_and(|top_p| top_p < 1.0) && effective_temperature <= 0.0 {
            return Err(EngineError::InvalidGenerationSettings(
                "q27 top_p below 1 requires a positive effective temperature; q27 uses greedy decoding otherwise"
                    .to_owned(),
            ));
        }
        Ok(())
    }

    fn runtime_management_compatibility(&self) -> CompatibilityDecision {
        if !self.enabled {
            CompatibilityDecision::Unsupported {
                reason: "q27 adapter is disabled in configuration".to_owned(),
            }
        } else if let Some(error) = &self.configuration_error {
            CompatibilityDecision::Unsupported {
                reason: error.clone(),
            }
        } else {
            CompatibilityDecision::Supported
        }
    }

    fn runtime_compatibility(&self, runtime: &InstalledRuntime) -> CompatibilityDecision {
        if matches!(
            runtime.manifest.acquisition_method,
            RuntimeAcquisitionMethod::ExternalBinary
        ) {
            return CompatibilityDecision::Supported;
        }
        match unsupported_native_argument_for_version(
            &self.native_arguments,
            &runtime.manifest.identity.version,
        ) {
            Some(argument) => CompatibilityDecision::Unsupported {
                reason: format!(
                    "q27 runtime {} does not support configured native option `{argument}`",
                    runtime.manifest.identity.version
                ),
            },
            None => CompatibilityDecision::Supported,
        }
    }

    fn available_runtime_compatibility(&self, runtime: &AvailableRuntime) -> CompatibilityDecision {
        match unsupported_native_argument_for_version(
            &self.native_arguments,
            &runtime.identity.version,
        ) {
            Some(argument) => CompatibilityDecision::Unsupported {
                reason: format!(
                    "q27 runtime {} does not support configured native option `{argument}`",
                    runtime.identity.version
                ),
            },
            None => CompatibilityDecision::Supported,
        }
    }

    fn compatibility(&self, model: &ModelArtifact) -> CompatibilityDecision {
        if let CompatibilityDecision::Unsupported { reason } =
            self.runtime_management_compatibility()
        {
            return CompatibilityDecision::Unsupported { reason };
        }
        if model.format != ArtifactFormat::Q27 {
            return CompatibilityDecision::Unsupported {
                reason: format!(
                    "q27 requires a Q27 artifact, not `{}`",
                    model.format.as_str()
                ),
            };
        }
        if let Err(reason) = inspect_q27_model(&model.path) {
            return CompatibilityDecision::Unsupported { reason };
        }
        match tokenizer_candidate(model) {
            Ok(path) => match validate_tokenizer_sync(&path) {
                Ok(()) => CompatibilityDecision::Supported,
                Err(reason) => CompatibilityDecision::Unsupported { reason },
            },
            Err(reason) => CompatibilityDecision::Unsupported { reason },
        }
    }

    fn runtime_model_compatibility(
        &self,
        runtime: &InstalledRuntime,
        model: &ModelArtifact,
        host: &HostCapabilities,
    ) -> RuntimeCompatibility {
        let facts = match inspect_q27_model(&model.path) {
            Ok(facts) => facts,
            Err(reason) => return RuntimeCompatibility::Incompatible(reason),
        };
        let evaluation = q27_device_evaluation(
            &runtime.manifest.identity.platform,
            &runtime.manifest.identity.architecture,
            &runtime.manifest.requirements,
            facts.tier,
            host,
            runtime.manifest.acquisition_method == RuntimeAcquisitionMethod::ExternalBinary,
        );
        let artifact_and_device = qualify_tier_compatibility(facts.tier, evaluation.compatibility);
        if model.norted_package.is_some() {
            combine_q27_compatibility(
                artifact_and_device,
                evaluate_q27_package_runtime(q27_package_capabilities(
                    &runtime.manifest.identity,
                    &runtime.manifest.acquisition_method,
                )),
            )
        } else {
            artifact_and_device
        }
    }

    fn runtime_model_preference(
        &self,
        runtime: &InstalledRuntime,
        model: &ModelArtifact,
        host: &HostCapabilities,
    ) -> u16 {
        let accelerator = inspect_q27_model(&model.path).ok().and_then(|facts| {
            q27_device_evaluation(
                &runtime.manifest.identity.platform,
                &runtime.manifest.identity.architecture,
                &runtime.manifest.requirements,
                facts.tier,
                host,
                runtime.manifest.acquisition_method == RuntimeAcquisitionMethod::ExternalBinary,
            )
            .accelerator
        });
        q27_runtime_preference(&runtime.manifest.identity.variant, accelerator.as_ref())
    }

    fn runtime_package_sharp_compatibility(
        &self,
        runtime: &InstalledRuntime,
        model: &ModelArtifact,
    ) -> Option<RuntimeCompatibility> {
        model.norted_package.as_ref()?;
        let capabilities = q27_package_capabilities(
            &runtime.manifest.identity,
            &runtime.manifest.acquisition_method,
        );
        Some(if !capabilities.trustworthy_identity {
            RuntimeCompatibility::Incompatible(
                "external Sharp/raw-prompt capability is unproven for this exact q27 executable"
                    .to_owned(),
            )
        } else if capabilities.exact_sharp_renderer && capabilities.raw_completions {
            RuntimeCompatibility::Compatible
        } else {
            RuntimeCompatibility::Incompatible(
                "manifest-bound Sharp rendering into q27 raw completions is unsupported/unproven"
                    .to_owned(),
            )
        })
    }

    fn available_runtime_model_compatibility(
        &self,
        runtime: &AvailableRuntime,
        model: &ModelArtifact,
        host: &HostCapabilities,
    ) -> RuntimeCompatibility {
        if let CompatibilityDecision::Unsupported { reason } = self.compatibility(model) {
            return RuntimeCompatibility::Incompatible(reason);
        }
        let facts = match inspect_q27_model(&model.path) {
            Ok(facts) => facts,
            Err(reason) => return RuntimeCompatibility::Incompatible(reason),
        };
        let evaluation = q27_device_evaluation(
            &runtime.identity.platform,
            &runtime.identity.architecture,
            &runtime.requirements,
            facts.tier,
            host,
            false,
        );
        let artifact_and_device = qualify_tier_compatibility(facts.tier, evaluation.compatibility);
        if model.norted_package.is_some() {
            let acquisition = match runtime.acquisition {
                RuntimeAcquisitionPlan::ReleaseAsset { .. } => {
                    RuntimeAcquisitionMethod::OfficialReleaseAsset
                }
                RuntimeAcquisitionPlan::SourceBuild(_) => RuntimeAcquisitionMethod::SourceBuild,
            };
            combine_q27_compatibility(
                artifact_and_device,
                evaluate_q27_package_runtime(q27_package_capabilities(
                    &runtime.identity,
                    &acquisition,
                )),
            )
        } else {
            artifact_and_device
        }
    }

    fn available_runtime_model_preference(
        &self,
        runtime: &AvailableRuntime,
        model: &ModelArtifact,
        host: &HostCapabilities,
    ) -> u16 {
        let accelerator = inspect_q27_model(&model.path).ok().and_then(|facts| {
            q27_device_evaluation(
                &runtime.identity.platform,
                &runtime.identity.architecture,
                &runtime.requirements,
                facts.tier,
                host,
                false,
            )
            .accelerator
        });
        q27_runtime_preference(&runtime.identity.variant, accelerator.as_ref())
    }

    fn runtime_model_accelerator(
        &self,
        runtime: &InstalledRuntime,
        model: &ModelArtifact,
        host: &HostCapabilities,
    ) -> Option<AcceleratorDevice> {
        inspect_q27_model(&model.path).ok().and_then(|facts| {
            q27_device_evaluation(
                &runtime.manifest.identity.platform,
                &runtime.manifest.identity.architecture,
                &runtime.manifest.requirements,
                facts.tier,
                host,
                runtime.manifest.acquisition_method == RuntimeAcquisitionMethod::ExternalBinary,
            )
            .accelerator
        })
    }

    async fn prepare_model_input(
        &self,
        model: &ModelArtifact,
    ) -> Result<PreparedModelInput, EngineError> {
        if model.format != ArtifactFormat::Q27 {
            return Err(EngineError::InvalidConfiguration(
                "q27 can only prepare Q27 model artifacts".to_owned(),
            ));
        }
        inspect_q27_model(&model.path).map_err(EngineError::InvalidConfiguration)?;
        if model.norted_package.is_some() {
            let prepared = prepare_norted_package_input(model).await?;
            let tokenizer = prepared
                .auxiliary
                .iter()
                .find(|artifact| artifact.role == AuxiliaryArtifactRole::Tokenizer)
                .ok_or_else(|| {
                    EngineError::InvalidConfiguration(
                        "Norted q27 package has no prepared manifest-bound tokenizer".to_owned(),
                    )
                })?;
            validate_tokenizer_path(&tokenizer.path).await?;
            return Ok(prepared);
        }
        let mut primary = model.clone();
        primary.path = canonical_regular_file(&model.path, "q27 model").await?;
        let candidate = tokenizer_candidate(model).map_err(EngineError::InvalidConfiguration)?;
        let tokenizer = canonical_regular_file(&candidate, "q27 tokenizer companion").await?;
        validate_tokenizer_path(&tokenizer).await?;
        let metadata = tokio::fs::metadata(&tokenizer).await.map_err(|error| {
            EngineError::InvalidConfiguration(format!(
                "could not inspect q27 tokenizer {}: {error}",
                tokenizer.display()
            ))
        })?;
        let content_sha256 = hash_file(&tokenizer).await.map_err(|error| {
            EngineError::InvalidConfiguration(format!(
                "could not hash q27 tokenizer {}: {error}",
                tokenizer.display()
            ))
        })?;
        Ok(PreparedModelInput {
            primary,
            auxiliary: vec![PreparedAuxiliaryArtifact {
                role: AuxiliaryArtifactRole::Tokenizer,
                path: tokenizer,
                size_bytes: metadata.len(),
                content_sha256,
            }],
            primary_file_identity: None,
        })
    }

    fn native_options(&self) -> Vec<NativeOption> {
        vec![NativeOption {
            name: "arguments".to_owned(),
            description: "Allowlisted q27 v0.6.2 tuning options appended after Norted's positional model/tokenizer and managed bind/thinking flags"
                .to_owned(),
            value_kind: OptionValueKind::String,
            repeatable: true,
        }]
    }

    fn load_setting_definitions(&self) -> Vec<LoadSettingDefinition> {
        q27_load_setting_definitions()
    }

    async fn load_settings_schema(
        &self,
        runtime: &InstalledRuntime,
        _model: &ModelArtifact,
        _host: &HostCapabilities,
    ) -> Result<LoadSettingsSchema, EngineError> {
        self.probe_runtime(runtime).await?;
        let usage = self
            .capability_cache
            .read()
            .await
            .get(&runtime.manifest.entrypoint_sha256.to_ascii_lowercase())
            .cloned()
            .ok_or_else(|| {
                EngineError::Operation("q27 usage observation was not cached".to_owned())
            })?;
        let managed = !matches!(
            runtime.manifest.acquisition_method,
            RuntimeAcquisitionMethod::ExternalBinary
        );
        let version = runtime.manifest.identity.version.as_str();
        let usage = usage.to_ascii_lowercase();
        let mut definitions = q27_load_setting_definitions();
        apply_q27_runtime_bounds(&mut definitions, managed, version);
        for definition in &mut definitions {
            let option = q27_setting_option(definition.id.as_str());
            let unavailable_by_version =
                q27_setting_unavailable_by_version(managed, version, option);
            let missing_from_contract = !usage_has_token(&usage, option)
                || (definition.id.as_str() == "q27.fast_head"
                    && !usage_has_token(&usage, "--no-fast-head"));
            if unavailable_by_version || missing_from_contract {
                definition.supported = false;
                definition.unsupported_reason = Some(if unavailable_by_version {
                    format!("q27 runtime {version} predates `{option}`")
                } else {
                    format!("the exact q27-server usage contract does not advertise `{option}`")
                });
            }
        }
        Ok(LoadSettingsSchema {
            engine_id: ENGINE_ID.to_owned(),
            runtime_id: Some(runtime.manifest.runtime_id.clone()),
            definitions,
        })
    }

    async fn probe(&self) -> Result<EngineProbe, EngineError> {
        Ok(self.probe_uncached().await)
    }

    async fn probe_runtime(
        &self,
        runtime: &InstalledRuntime,
    ) -> Result<RuntimeProbeObservation, EngineError> {
        if !self.enabled {
            return Err(EngineError::InvalidConfiguration(
                "q27 adapter is disabled in configuration".to_owned(),
            ));
        }
        if let Some(error) = &self.configuration_error {
            return Err(EngineError::InvalidConfiguration(error.clone()));
        }
        runtime
            .manifest
            .identity
            .validate()
            .map_err(|error| EngineError::InvalidConfiguration(error.to_string()))?;
        if runtime.manifest.runtime_id != RuntimeId::from_identity(&runtime.manifest.identity) {
            return Err(EngineError::InvalidConfiguration(
                "runtime ID does not match its structured identity".to_owned(),
            ));
        }
        if runtime.manifest.identity.engine_id != ENGINE_ID {
            return Err(EngineError::InvalidConfiguration(format!(
                "runtime `{}` belongs to engine `{}`, not `{ENGINE_ID}`",
                runtime.manifest.runtime_id, runtime.manifest.identity.engine_id
            )));
        }
        if let CompatibilityDecision::Unsupported { reason } = self.runtime_compatibility(runtime) {
            return Err(EngineError::InvalidConfiguration(reason));
        }
        if !runtime
            .manifest
            .supported_formats
            .contains(&ArtifactFormat::Q27)
        {
            return Err(EngineError::InvalidConfiguration(
                "q27 runtime does not declare Q27 artifact support".to_owned(),
            ));
        }
        let configured_path = runtime.entrypoint_path();
        if !matches!(
            runtime.manifest.acquisition_method,
            RuntimeAcquisitionMethod::ExternalBinary
        ) {
            let root = tokio::fs::canonicalize(&runtime.installation_root)
                .await
                .map_err(|error| {
                    EngineError::InvalidConfiguration(format!(
                        "runtime installation root could not be resolved: {error}"
                    ))
                })?;
            let entrypoint = tokio::fs::canonicalize(&configured_path)
                .await
                .map_err(|error| {
                    EngineError::InvalidConfiguration(format!(
                        "runtime entrypoint could not be resolved: {error}"
                    ))
                })?;
            if !entrypoint.starts_with(&root) {
                return Err(EngineError::InvalidConfiguration(
                    "managed runtime entrypoint escapes its installation root".to_owned(),
                ));
            }
        }
        let (_, _, observation) = self
            .inspect_binary(&configured_path, Some(&runtime.manifest.entrypoint_sha256))
            .await?;
        Ok(observation)
    }

    async fn build_launch_spec(&self, request: LaunchRequest) -> Result<LaunchSpec, EngineError> {
        let endpoint = http_endpoint(request.backend_address);
        self.package_executions.write().await.remove(&endpoint);
        if !request.backend_address.ip().is_loopback() {
            return Err(EngineError::InvalidConfiguration(
                "q27 backend address must be loopback".to_owned(),
            ));
        }
        if request.model.primary.format != ArtifactFormat::Q27 {
            return Err(EngineError::InvalidConfiguration(
                "q27 can only launch Q27 model artifacts".to_owned(),
            ));
        }
        let package_capabilities = request.model.primary.norted_package.as_ref().map(|_| {
            q27_package_capabilities(
                &request.runtime.manifest.identity,
                &request.runtime.manifest.acquisition_method,
            )
        });
        if let Some(capabilities) = package_capabilities {
            if let Err(reason) = validate_q27_package_prelaunch(capabilities) {
                return Err(EngineError::InvalidConfiguration(format!(
                    "selected q27 runtime cannot launch the Norted package: {reason}"
                )));
            }
        }
        request
            .load_settings_schema
            .validate(&request.load_settings)
            .map_err(|error| EngineError::InvalidConfiguration(error.to_string()))?;
        revalidate_norted_package_before_launch(&request.model).await?;
        let mut structured = translate_q27_load_settings(
            &request.load_settings,
            &self.native_arguments,
            &self.environment,
        )?;
        if !matches!(
            request.runtime.manifest.acquisition_method,
            RuntimeAcquisitionMethod::ExternalBinary
        ) {
            if let Some(argument) = unsupported_native_argument_for_version(
                &self.native_arguments,
                &request.runtime.manifest.identity.version,
            ) {
                return Err(EngineError::InvalidConfiguration(format!(
                    "q27 runtime {} does not support configured native option `{argument}`",
                    request.runtime.manifest.identity.version
                )));
            }
        }
        let model_path = canonical_regular_file(&request.model.primary.path, "q27 model").await?;
        if model_path != request.model.primary.path {
            return Err(EngineError::InvalidConfiguration(
                "prepared q27 model path changed before launch".to_owned(),
            ));
        }
        inspect_q27_model(&model_path).map_err(EngineError::InvalidConfiguration)?;
        let tokenizers = request
            .model
            .auxiliary
            .iter()
            .filter(|artifact| artifact.role == AuxiliaryArtifactRole::Tokenizer)
            .collect::<Vec<_>>();
        let tokenizer = match tokenizers.as_slice() {
            [tokenizer] => *tokenizer,
            _ => {
                return Err(EngineError::InvalidConfiguration(
                    "prepared q27 input must contain exactly one tokenizer".to_owned(),
                ));
            }
        };
        let tokenizer_path = revalidate_prepared_tokenizer(tokenizer).await?;
        let observation = self.probe_runtime(&request.runtime).await?;
        let package_launch = if let Some(package) = request.model.primary.norted_package.as_ref() {
            let capabilities = package_capabilities.expect("package capabilities");
            let compiled_w_max = capabilities.compiled_w_max.ok_or_else(|| {
                EngineError::InvalidConfiguration(
                    "q27 package launch has no proven numeric compiled W_MAX".to_owned(),
                )
            })?;
            remove_q27_value_argument(&mut structured.arguments, "--ctx")?;
            structured
                .arguments
                .extend([OsString::from("--ctx"), OsString::from("auto")]);
            let sharp_template = read_prepared_sharp_template(&request.model).await?;
            validate_sharp_template(&sharp_template)?;
            let policy = q27_package_policy(package)?;
            let selected_kv_mode = q27_package_kv_attempt_modes(capabilities, policy)
                .into_iter()
                .next()
                .ok_or_else(|| {
                    EngineError::InvalidConfiguration(
                        "selected q27 runtime proves no package-declared KV mode".to_owned(),
                    )
                })?;
            Some(q27_package_launch_policy(
                package,
                compiled_w_max,
                q27_selected_fast_head(&request.load_settings, policy)?,
                selected_kv_mode,
            )?)
        } else {
            None
        };
        let binary_path = request.runtime.entrypoint_path();
        let manifest = &request.runtime.manifest;
        let installation = EngineInstallation {
            engine: EngineRevision {
                engine_id: ENGINE_ID.to_owned(),
                version: observation.observed_version.clone(),
                revision: observation.observed_revision.clone(),
            },
            source_repository: manifest.identity.package.repository.clone(),
            acquisition_method: match manifest.acquisition_method {
                RuntimeAcquisitionMethod::OfficialReleaseAsset
                | RuntimeAcquisitionMethod::PreseededOfficialPack => {
                    AcquisitionMethod::OfficialBinary
                }
                RuntimeAcquisitionMethod::SourceBuild => AcquisitionMethod::SourceBuild,
                RuntimeAcquisitionMethod::ExternalBinary => AcquisitionMethod::ExternalBinary,
            },
            binary_path: binary_path.clone(),
            binary_sha256: Some(manifest.entrypoint_sha256.clone()),
            build: None,
            platform: manifest.identity.platform.clone(),
            architecture: manifest.identity.architecture.clone(),
            runtime_variant: Some(manifest.identity.variant.clone()),
            acquired_at_unix: manifest.installed_at_unix,
            observed_at_unix: observation.observed_at_unix,
        };
        let mut arguments = vec![
            model_path.as_os_str().to_owned(),
            tokenizer_path.as_os_str().to_owned(),
            OsString::from("--host"),
            OsString::from(request.backend_address.ip().to_string()),
            OsString::from("--port"),
            OsString::from(request.backend_address.port().to_string()),
        ];
        if let Some(package_launch) = package_launch.as_ref() {
            arguments.extend(package_launch.arguments.iter().cloned());
        } else {
            arguments.push(OsString::from("--no-think"));
        }
        arguments.extend(structured.arguments);
        arguments.extend(self.native_arguments.iter().map(OsString::from));
        let accelerator = request.accelerator.ok_or_else(|| {
            EngineError::InvalidConfiguration(
                "q27 launch has no exact NVIDIA GPU selected by compatibility evaluation"
                    .to_owned(),
            )
        })?;
        let mut environment = q27_launch_environment(&self.environment, &accelerator)?;
        if let Some(package_launch) = package_launch.as_ref() {
            environment.extend(package_launch.environment.clone());
        }
        let mut environment_remove = managed_environment_removals();
        environment_remove.extend(structured.environment_remove);
        let normalized_settings = if let Some(package_launch) = package_launch {
            package_launch.normalized_settings
        } else {
            BTreeMap::from([
                ("temperature".to_owned(), json!(0.0)),
                ("top_p".to_owned(), json!(1.0)),
                ("thinking".to_owned(), json!(false)),
            ])
        };
        Ok(LaunchSpec {
            executable: binary_path,
            arguments,
            environment,
            environment_remove,
            inherits_parent_environment: true,
            working_directory: None,
            temporary_files: Vec::new(),
            endpoint: Some(endpoint),
            normalized_settings,
            load_settings: request.load_settings,
            native_arguments: self.native_arguments.clone(),
            installation,
            runtime: request.runtime,
            model: request.model,
            accelerator: Some(accelerator),
        })
    }

    async fn build_launch_attempts(
        &self,
        request: LaunchRequest,
    ) -> Result<Vec<LaunchSpec>, EngineError> {
        let package = request.model.primary.norted_package.clone();
        let capabilities = package.as_ref().map(|_| {
            q27_package_capabilities(
                &request.runtime.manifest.identity,
                &request.runtime.manifest.acquisition_method,
            )
        });
        let first = self.build_launch_spec(request).await?;
        let Some(package) = package else {
            return Ok(vec![first]);
        };
        let capabilities = capabilities.expect("package capabilities");
        let policy = q27_package_policy(&package)?;
        let modes = q27_package_kv_attempt_modes(capabilities, policy);
        if modes.is_empty() {
            return Err(EngineError::InvalidConfiguration(
                "selected q27 runtime proves no package-declared KV mode".to_owned(),
            ));
        }
        Ok(modes
            .into_iter()
            .map(|mode| {
                let mut attempt = first.clone();
                attempt
                    .environment
                    .insert("Q27_KV".to_owned(), mode.as_str().to_owned());
                attempt
                    .normalized_settings
                    .insert("requested_kv_mode".to_owned(), json!(mode.as_str()));
                attempt
            })
            .collect())
    }

    async fn prepare_launch_attempt(&self, spec: &LaunchSpec) -> Result<(), EngineError> {
        let endpoint = spec.endpoint.as_deref().ok_or_else(|| {
            EngineError::InvalidConfiguration("q27 launch has no private endpoint".to_owned())
        })?;
        self.package_executions.write().await.remove(endpoint);
        let Some(package) = spec.model.primary.norted_package.as_ref() else {
            return Ok(());
        };
        revalidate_norted_package_before_launch(&spec.model).await?;
        let capabilities = q27_package_capabilities(
            &spec.runtime.manifest.identity,
            &spec.runtime.manifest.acquisition_method,
        );
        validate_q27_package_prelaunch(capabilities).map_err(|reason| {
            EngineError::InvalidConfiguration(format!(
                "selected q27 runtime cannot launch the Norted package: {reason}"
            ))
        })?;
        let selected_kv_mode = Q27PackageKvMode::QUALITY_ORDER
            .into_iter()
            .find(|mode| spec.environment.get("Q27_KV").map(String::as_str) == Some(mode.as_str()))
            .ok_or_else(|| {
                EngineError::InvalidConfiguration(
                    "q27 package launch did not select a recognized policy KV mode".to_owned(),
                )
            })?;
        if !capabilities.supported_kv_modes.contains(&selected_kv_mode)
            || !q27_package_policy(package)?
                .kv_preference
                .iter()
                .any(|mode| mode == selected_kv_mode.as_str())
        {
            return Err(EngineError::InvalidConfiguration(format!(
                "q27 package KV mode `{}` is not proven and package-declared for this executable",
                selected_kv_mode.as_str()
            )));
        }
        let sharp_template = read_prepared_sharp_template(&spec.model).await?;
        validate_sharp_template(&sharp_template)?;
        let compiled_w_max = capabilities.compiled_w_max.ok_or_else(|| {
            EngineError::InvalidConfiguration(
                "q27 package launch has no proven numeric compiled W_MAX".to_owned(),
            )
        })?;
        self.package_executions.write().await.insert(
            endpoint.to_owned(),
            Q27PackageExecution {
                sharp_template,
                policy: q27_package_policy(package)?.clone(),
                compiled_w_max,
                expected_fast_head: q27_selected_fast_head(
                    &spec.load_settings,
                    q27_package_policy(package)?,
                )?,
                selected_kv_mode,
            },
        );
        Ok(())
    }

    async fn clear_launch_state(&self, endpoint: Option<&str>) {
        if let Some(endpoint) = endpoint {
            self.package_executions.write().await.remove(endpoint);
        }
    }

    async fn health(&self, process: &ProcessDescriptor) -> Result<bool, EngineError> {
        let endpoint = process.endpoint.as_deref().ok_or_else(|| {
            EngineError::Operation("q27 process has no backend endpoint".to_owned())
        })?;
        let response = self
            .client
            .get(format!("{endpoint}/health"))
            .timeout(HEALTH_TIMEOUT)
            .send()
            .await
            .map_err(|error| EngineError::BackendUnavailable(error.to_string()))?;
        if response.status() == reqwest::StatusCode::SERVICE_UNAVAILABLE {
            return Ok(false);
        }
        if !response.status().is_success() {
            return Err(EngineError::BackendUnavailable(format!(
                "q27 health endpoint returned HTTP {}",
                response.status()
            )));
        }
        let health = response.json::<HealthResponse>().await.map_err(|error| {
            EngineError::Operation(format!("invalid q27 health response: {error}"))
        })?;
        Ok(health.status == "ok")
    }

    async fn startup_observation(
        &self,
        process: &ProcessDescriptor,
        stderr_tail: &[String],
    ) -> Result<StartupObservation, EngineError> {
        let endpoint = process.endpoint.as_deref().ok_or_else(|| {
            EngineError::Operation("q27 process has no backend endpoint".to_owned())
        })?;
        let executions = self.package_executions.read().await;
        let Some(execution) = executions.get(endpoint) else {
            return Ok(StartupObservation::Ready(BTreeMap::new()));
        };
        let observed = parse_q27_startup_observation(stderr_tail)?;
        if observed.kv_mode != execution.selected_kv_mode.as_str() {
            return Err(EngineError::Operation(format!(
                "q27 startup selected KV mode `{}` instead of requested package mode `{}`",
                observed.kv_mode,
                execution.selected_kv_mode.as_str()
            )));
        }
        if observed.compiled_w_max != execution.compiled_w_max
            || observed.suffix_width != execution.compiled_w_max
        {
            return Err(EngineError::Operation(format!(
                "q27 startup W_MAX/suffix proof ({}/{}) disagrees with the pre-launch numeric W_MAX {}",
                observed.compiled_w_max, observed.suffix_width, execution.compiled_w_max
            )));
        }
        if !observed.thinking
            || observed.fast_head != execution.expected_fast_head
            || observed.maximum_mtp_depth != execution.policy.maximum_mtp_depth
            || !approximately_equal(
                observed.mtp_minimum_probability(),
                execution.policy.mtp_minimum_probability,
            )
            || observed.suffix_drafting != execution.policy.suffix_drafting
        {
            return Err(EngineError::Operation(
                "q27 startup profile disagrees with the Norted thinking/MTP/fast-head policy"
                    .to_owned(),
            ));
        }
        if observed.served_context < execution.policy.minimum_context_tokens {
            return Ok(StartupObservation::RetryContextCapacity {
                kv_mode: observed.kv_mode,
                observed_context: observed.served_context,
                minimum_context: execution.policy.minimum_context_tokens,
            });
        }
        Ok(StartupObservation::Ready(BTreeMap::from([
            (
                "observed_served_context".to_owned(),
                json!(observed.served_context),
            ),
            ("observed_kv_mode".to_owned(), json!(observed.kv_mode)),
            (
                "observed_compiled_w_max".to_owned(),
                json!(observed.compiled_w_max),
            ),
            (
                "sharp_application".to_owned(),
                json!("pretokenized-raw-prompt"),
            ),
        ])))
    }

    async fn effective_generation_settings(
        &self,
        process: &ProcessDescriptor,
    ) -> Result<EffectiveGenerationSettings, EngineError> {
        // q27 exposes no effective-config endpoint. These values are owned by
        // this adapter, sent explicitly on every request, and force env vars
        // are removed from the child environment.
        let package = if let Some(endpoint) = process.endpoint.as_deref() {
            self.package_executions.read().await.get(endpoint).cloned()
        } else {
            None
        };
        Ok(package.map_or(
            EffectiveGenerationSettings {
                temperature: 0.0,
                top_p: 1.0,
            },
            |execution| EffectiveGenerationSettings {
                temperature: execution.policy.temperature,
                top_p: execution.policy.top_p,
            },
        ))
    }

    async fn infer(
        &self,
        endpoint: &str,
        request: InferenceRequest,
    ) -> Result<InferenceOutput, EngineError> {
        let package = self.package_executions.read().await.get(endpoint).cloned();
        let (route, body) = if let Some(execution) = package.as_ref() {
            (
                "/v1/completions",
                Self::package_backend_request(execution, &request, false)?,
            )
        } else {
            (
                "/v1/chat/completions",
                self.backend_request(&request, false),
            )
        };
        let response = self
            .client
            .post(format!("{endpoint}{route}"))
            .timeout(INFERENCE_TIMEOUT)
            .json(&body)
            .send()
            .await
            .map_err(map_transport_error)?;
        let status = response.status();
        let body = response
            .bytes()
            .await
            .map_err(|error| EngineError::BackendUnavailable(error.to_string()))?;
        if !status.is_success() {
            return Err(backend_http_error(status, &body));
        }
        let response: ChatCompletionResponse = serde_json::from_slice(&body).map_err(|error| {
            EngineError::Operation(format!("invalid q27 completion response: {error}"))
        })?;
        let choice = response.choices.into_iter().next().ok_or_else(|| {
            EngineError::Operation("q27 response contained no completion choice".to_owned())
        })?;
        let text = choice
            .text
            .or_else(|| choice.message.and_then(|message| message.content))
            .ok_or_else(|| {
                EngineError::Operation("q27 response contained no assistant text".to_owned())
            })?;
        let text = if package.is_some() {
            filter_q27_package_output(&text)
        } else {
            text
        };
        Ok(InferenceOutput {
            text,
            usage: response.usage.map(Into::into),
            finish_reason: map_finish_reason(choice.finish_reason.as_deref())?,
        })
    }

    async fn infer_stream(
        &self,
        endpoint: &str,
        request: InferenceRequest,
    ) -> Result<InferenceStream, EngineError> {
        let package = self.package_executions.read().await.get(endpoint).cloned();
        let (route, body) = if let Some(execution) = package.as_ref() {
            (
                "/v1/completions",
                Self::package_backend_request(execution, &request, true)?,
            )
        } else {
            ("/v1/chat/completions", self.backend_request(&request, true))
        };
        let response = self
            .client
            .post(format!("{endpoint}{route}"))
            .timeout(INFERENCE_TIMEOUT)
            .json(&body)
            .send()
            .await
            .map_err(map_transport_error)?;
        let status = response.status();
        if !status.is_success() {
            let body = response
                .bytes()
                .await
                .map_err(|error| EngineError::BackendUnavailable(error.to_string()))?;
            return Err(backend_http_error(status, &body));
        }
        Ok(q27_sse_stream(
            response.bytes_stream().boxed(),
            package.is_some(),
        ))
    }
}

fn remove_q27_value_argument(
    arguments: &mut Vec<OsString>,
    option: &str,
) -> Result<(), EngineError> {
    let positions = arguments
        .iter()
        .enumerate()
        .filter_map(|(index, argument)| (argument == option).then_some(index))
        .collect::<Vec<_>>();
    let [index] = positions.as_slice() else {
        return Err(EngineError::InvalidConfiguration(format!(
            "q27 package policy expected exactly one structured `{option}` argument"
        )));
    };
    if *index + 1 >= arguments.len() {
        return Err(EngineError::InvalidConfiguration(format!(
            "q27 structured `{option}` argument has no value"
        )));
    }
    arguments.drain(*index..=*index + 1);
    Ok(())
}

async fn read_prepared_sharp_template(model: &PreparedModelInput) -> Result<String, EngineError> {
    let sharp = model
        .auxiliary
        .iter()
        .filter(|artifact| artifact.role == AuxiliaryArtifactRole::Sharp)
        .collect::<Vec<_>>();
    let [sharp] = sharp.as_slice() else {
        return Err(EngineError::InvalidConfiguration(
            "prepared q27 Norted package must contain exactly one manifest-bound Sharp template"
                .to_owned(),
        ));
    };
    let file = tokio::fs::File::open(&sharp.path).await.map_err(|error| {
        EngineError::InvalidConfiguration(format!(
            "could not open prepared Sharp template {}: {error}",
            sharp.path.display()
        ))
    })?;
    let mut bytes = Vec::new();
    file.take(SHARP_TEMPLATE_LIMIT + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|error| {
            EngineError::InvalidConfiguration(format!(
                "could not read prepared Sharp template {}: {error}",
                sharp.path.display()
            ))
        })?;
    if bytes.len() as u64 > SHARP_TEMPLATE_LIMIT {
        return Err(EngineError::InvalidConfiguration(
            "prepared Sharp template exceeded the local size limit".to_owned(),
        ));
    }
    String::from_utf8(bytes).map_err(|_| {
        EngineError::InvalidConfiguration("prepared Sharp template is not UTF-8".to_owned())
    })
}

fn sharp_environment<'source>() -> Environment<'source> {
    let mut environment = Environment::new();
    environment.set_unknown_method_callback(minijinja_contrib::pycompat::unknown_method_callback);
    environment.add_function(
        "raise_exception",
        |message: String| -> Result<String, minijinja::Error> {
            Err(minijinja::Error::new(ErrorKind::InvalidOperation, message))
        },
    );
    environment
}

fn render_sharp_template(
    template_source: &str,
    messages: &[InferenceMessage],
) -> Result<String, EngineError> {
    let mut environment = sharp_environment();
    environment
        .add_template("sharp", template_source)
        .map_err(|error| {
            EngineError::InvalidConfiguration(format!(
                "manifest-bound Sharp template is not supported by the exact Jinja renderer: {error}"
            ))
        })?;
    let template = environment.get_template("sharp").map_err(|error| {
        EngineError::InvalidConfiguration(format!(
            "manifest-bound Sharp template could not be loaded: {error}"
        ))
    })?;
    let messages = messages.iter().map(sharp_message_json).collect::<Vec<_>>();
    template
        .render(context! {
            messages => messages,
            add_generation_prompt => true,
            enable_thinking => true,
            reasoning_effort => "medium",
            tools => Vec::<Value>::new(),
        })
        .map_err(|error| {
            EngineError::InvalidConfiguration(format!(
                "manifest-bound Sharp rendering failed: {error}"
            ))
        })
}

fn validate_sharp_template(template_source: &str) -> Result<(), EngineError> {
    let rendered = render_sharp_template(
        template_source,
        &[InferenceMessage {
            role: InferenceRole::User,
            text: "Sharp startup validation".to_owned(),
        }],
    )?;
    if !rendered.contains("<|im_start|>user\nSharp startup validation<|im_end|>")
        || !rendered.ends_with("<|im_start|>assistant\n<think>\n")
    {
        return Err(EngineError::InvalidConfiguration(
            "manifest-bound Sharp template did not produce the required raw user/assistant token framing"
                .to_owned(),
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct Q27StartupObservation {
    served_context: u64,
    kv_mode: String,
    compiled_w_max: u64,
    suffix_width: u64,
    maximum_mtp_depth: String,
    mtp_minimum_probability_bits: u64,
    suffix_drafting: bool,
    fast_head: bool,
    thinking: bool,
}

impl Q27StartupObservation {
    fn mtp_minimum_probability(&self) -> f64 {
        f64::from_bits(self.mtp_minimum_probability_bits)
    }
}

fn parse_q27_startup_observation(
    stderr_tail: &[String],
) -> Result<Q27StartupObservation, EngineError> {
    if stderr_tail.len() > 80 || stderr_tail.iter().any(|line| line.len() > 4_096) {
        return Err(EngineError::Operation(
            "q27 startup observation exceeded the bounded supervisor log contract".to_owned(),
        ));
    }
    let profile = stderr_tail
        .iter()
        .rev()
        .find(|line| line.starts_with("profile:"))
        .ok_or_else(|| {
            EngineError::BackendUnavailable(
                "q27 startup profile line has not been observed yet".to_owned(),
            )
        })?;
    let auto = stderr_tail
        .iter()
        .rev()
        .find(|line| line.starts_with("--ctx auto:"))
        .ok_or_else(|| {
            EngineError::BackendUnavailable(
                "q27 auto-context resolution has not been observed yet".to_owned(),
            )
        })?;
    let slot = stderr_tail
        .iter()
        .rev()
        .find(|line| line.starts_with("slot 0 ready: ctx="))
        .ok_or_else(|| {
            EngineError::BackendUnavailable(
                "q27 served slot context has not been observed yet".to_owned(),
            )
        })?;

    let profile_kv = startup_token(profile, "kv=")?;
    let maximum_mtp_depth = startup_token(profile, "maxd=")?.to_owned();
    let pmin = startup_token(profile, "pmin=")?
        .parse::<f64>()
        .map_err(|_| EngineError::Operation("q27 startup P_MIN was not numeric".to_owned()))?;
    let suffix = startup_token(profile, "suffix=")?;
    let (suffix_enabled, suffix_width) = suffix.split_once("/w").ok_or_else(|| {
        EngineError::Operation("q27 startup suffix profile had no numeric width".to_owned())
    })?;
    let suffix_width = suffix_width.parse::<u64>().map_err(|_| {
        EngineError::Operation("q27 startup suffix width was not numeric".to_owned())
    })?;
    let fast_head = parse_startup_bool(startup_token(profile, "fast-head=")?, "fast-head")?;
    let thinking = parse_startup_bool(startup_token(profile, "think=")?, "thinking")?;

    let auto_context = auto
        .strip_prefix("--ctx auto:")
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|value| value.parse::<u64>().ok())
        .ok_or_else(|| {
            EngineError::Operation("q27 resolved auto context was not numeric".to_owned())
        })?;
    let served_context = slot
        .strip_prefix("slot 0 ready: ctx=")
        .and_then(|value| value.trim().parse::<u64>().ok())
        .ok_or_else(|| EngineError::Operation("q27 served context was not numeric".to_owned()))?;
    if served_context != auto_context {
        return Err(EngineError::Operation(format!(
            "q27 served context {served_context} disagrees with auto-context resolution {auto_context}"
        )));
    }
    let compiled_w_max = startup_parenthesized_value(auto, "W_MAX=")?
        .parse::<u64>()
        .map_err(|_| EngineError::Operation("q27 startup W_MAX was not numeric".to_owned()))?;
    let kv_mode = ["fp8", "turbo5k", "turbo3", "turbo3v", "fp16"]
        .into_iter()
        .find(|mode| auto.contains(&format!("{mode} KV")))
        .ok_or_else(|| EngineError::Operation("q27 startup KV mode was not recognized".to_owned()))?
        .to_owned();
    if kv_mode != profile_kv {
        return Err(EngineError::Operation(format!(
            "q27 startup KV profile `{profile_kv}` disagrees with auto-context mode `{kv_mode}`"
        )));
    }
    Ok(Q27StartupObservation {
        served_context,
        kv_mode,
        compiled_w_max,
        suffix_width,
        maximum_mtp_depth,
        mtp_minimum_probability_bits: pmin.to_bits(),
        suffix_drafting: suffix_enabled == "1",
        fast_head,
        thinking,
    })
}

fn startup_token<'a>(line: &'a str, prefix: &str) -> Result<&'a str, EngineError> {
    line.split_whitespace()
        .find_map(|token| token.strip_prefix(prefix))
        .ok_or_else(|| EngineError::Operation(format!("q27 startup profile omitted `{prefix}`")))
}

fn startup_parenthesized_value<'a>(line: &'a str, prefix: &str) -> Result<&'a str, EngineError> {
    line.split(|character: char| character.is_whitespace() || matches!(character, ',' | ')'))
        .find_map(|token| token.strip_prefix(prefix))
        .ok_or_else(|| EngineError::Operation(format!("q27 startup output omitted `{prefix}`")))
}

fn parse_startup_bool(value: &str, label: &str) -> Result<bool, EngineError> {
    match value {
        "0" => Ok(false),
        "1" => Ok(true),
        _ => Err(EngineError::Operation(format!(
            "q27 startup {label} value was not boolean"
        ))),
    }
}

fn approximately_equal(left: f64, right: f64) -> bool {
    (left - right).abs() <= f64::EPSILON * left.abs().max(right.abs()).max(1.0) * 8.0
}

async fn canonical_regular_file(path: &Path, description: &str) -> Result<PathBuf, EngineError> {
    let canonical = tokio::fs::canonicalize(path).await.map_err(|error| {
        EngineError::InvalidConfiguration(format!(
            "{description} {} could not be resolved: {error}",
            path.display()
        ))
    })?;
    let metadata = tokio::fs::metadata(&canonical).await.map_err(|error| {
        EngineError::InvalidConfiguration(format!(
            "{description} {} could not be inspected: {error}",
            canonical.display()
        ))
    })?;
    if !metadata.is_file() {
        return Err(EngineError::InvalidConfiguration(format!(
            "{description} is not a regular file"
        )));
    }
    Ok(canonical)
}

async fn validate_tokenizer_path(tokenizer: &Path) -> Result<(), EngineError> {
    if tokenizer
        .extension()
        .and_then(|value| value.to_str())
        .map(str::to_ascii_lowercase)
        != Some("tok".to_owned())
    {
        return Err(EngineError::InvalidConfiguration(
            "q27 tokenizer companion must use the `.tok` extension".to_owned(),
        ));
    }
    validate_tokenizer(tokenizer).await
}

async fn revalidate_prepared_tokenizer(
    tokenizer: &PreparedAuxiliaryArtifact,
) -> Result<PathBuf, EngineError> {
    let tokenizer_path = canonical_regular_file(&tokenizer.path, "q27 tokenizer companion").await?;
    if tokenizer_path != tokenizer.path {
        return Err(EngineError::InvalidConfiguration(
            "prepared q27 tokenizer path changed before launch".to_owned(),
        ));
    }
    validate_tokenizer_path(&tokenizer_path).await?;
    let tokenizer_size = tokio::fs::metadata(&tokenizer_path)
        .await
        .map_err(|error| {
            EngineError::InvalidConfiguration(format!(
                "could not inspect q27 tokenizer {} before launch: {error}",
                tokenizer_path.display()
            ))
        })?
        .len();
    let tokenizer_sha256 = hash_file(&tokenizer_path).await.map_err(|error| {
        EngineError::InvalidConfiguration(format!(
            "could not revalidate q27 tokenizer {} before launch: {error}",
            tokenizer_path.display()
        ))
    })?;
    if tokenizer_size != tokenizer.size_bytes || tokenizer_sha256 != tokenizer.content_sha256 {
        return Err(EngineError::InvalidConfiguration(
            "q27 tokenizer changed after prepared-input provenance was recorded".to_owned(),
        ));
    }
    Ok(tokenizer_path)
}

fn tokenizer_candidate(model: &ModelArtifact) -> Result<PathBuf, String> {
    let declared = model
        .auxiliary_artifacts
        .iter()
        .filter(|artifact| artifact.role == AuxiliaryArtifactRole::Tokenizer)
        .collect::<Vec<_>>();
    match declared.as_slice() {
        [tokenizer] => Ok(tokenizer.path.clone()),
        [] => Err(format!(
            "q27 model {} has no declared tokenizer companion; rediscover models before loading",
            model.path.display()
        )),
        _ => Err(format!(
            "q27 model {} has multiple tokenizer companions; exactly one is required",
            model.path.display()
        )),
    }
}

async fn validate_tokenizer(path: &Path) -> Result<(), EngineError> {
    let mut file = tokio::fs::File::open(path).await.map_err(|error| {
        EngineError::InvalidConfiguration(format!(
            "could not open q27 tokenizer {}: {error}",
            path.display()
        ))
    })?;
    let mut header = [0_u8; 8];
    file.read_exact(&mut header).await.map_err(|error| {
        EngineError::InvalidConfiguration(format!(
            "could not read Q27T header from {}: {error}",
            path.display()
        ))
    })?;
    validate_tokenizer_header(&header).map_err(EngineError::InvalidConfiguration)
}

fn validate_tokenizer_sync(path: &Path) -> Result<(), String> {
    use std::io::Read;

    let tokenizer = path.canonicalize().map_err(|error| {
        format!(
            "could not resolve q27 tokenizer companion {}: {error}",
            path.display()
        )
    })?;
    if tokenizer
        .extension()
        .and_then(|extension| extension.to_str())
        .is_none_or(|extension| !extension.eq_ignore_ascii_case("tok"))
    {
        return Err("q27 tokenizer companion must use the `.tok` extension".to_owned());
    }
    let metadata = std::fs::metadata(&tokenizer).map_err(|error| {
        format!(
            "could not inspect q27 tokenizer companion {}: {error}",
            tokenizer.display()
        )
    })?;
    if !metadata.is_file() {
        return Err("q27 tokenizer companion is not a regular file".to_owned());
    }
    let mut file = std::fs::File::open(&tokenizer).map_err(|error| {
        format!(
            "could not open q27 tokenizer {}: {error}",
            tokenizer.display()
        )
    })?;
    let mut header = [0_u8; 8];
    file.read_exact(&mut header).map_err(|error| {
        format!(
            "could not read Q27T header from {}: {error}",
            tokenizer.display()
        )
    })?;
    validate_tokenizer_header(&header)
}

fn validate_tokenizer_header(header: &[u8; 8]) -> Result<(), String> {
    if &header[..4] != b"Q27T" {
        return Err("q27 tokenizer companion does not contain Q27T magic".to_owned());
    }
    let version = u32::from_le_bytes(header[4..8].try_into().expect("four-byte version"));
    if version != 1 {
        return Err(format!(
            "q27 tokenizer companion has unsupported version {version}; expected version 1"
        ));
    }
    Ok(())
}

#[derive(Deserialize)]
struct HealthResponse {
    status: String,
}

#[derive(Deserialize)]
struct ChatCompletionResponse {
    choices: Vec<ChatChoice>,
    usage: Option<ChatUsage>,
}

#[derive(Deserialize)]
struct ChatChoice {
    message: Option<ChatMessage>,
    text: Option<String>,
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct ChatMessage {
    content: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct ChatUsage {
    prompt_tokens: u64,
    completion_tokens: u64,
    total_tokens: u64,
}

impl From<ChatUsage> for InferenceUsage {
    fn from(usage: ChatUsage) -> Self {
        Self {
            input_tokens: usage.prompt_tokens,
            output_tokens: usage.completion_tokens,
            total_tokens: usage.total_tokens,
            cached_input_tokens: None,
            cache_write_input_tokens: None,
            reasoning_output_tokens: None,
        }
    }
}

const THINK_OPEN: &str = "<think>";
const THINK_CLOSE: &str = "</think>";

#[derive(Debug, Default)]
struct Q27PackageOutputFilter {
    reasoning_closed: bool,
    pending: String,
}

impl Q27PackageOutputFilter {
    fn push(&mut self, input: &str) -> String {
        self.pending.push_str(input);
        if !self.reasoning_closed {
            let Some(close) = self.pending.find(THINK_CLOSE) else {
                self.retain_possible_marker_suffix(&[THINK_CLOSE]);
                return String::new();
            };
            self.pending.drain(..close + THINK_CLOSE.len());
            self.reasoning_closed = true;
        }
        self.drain_public()
    }

    fn finish(&mut self) -> String {
        if !self.reasoning_closed {
            self.pending.clear();
            return String::new();
        }
        // `pending` can only be a prefix of a control delimiter. Dropping it
        // prevents a truncated tag from becoming ordinary assistant text.
        self.pending.clear();
        String::new()
    }

    fn drain_public(&mut self) -> String {
        let mut output = String::new();
        loop {
            let marker = [THINK_OPEN, THINK_CLOSE]
                .into_iter()
                .filter_map(|marker| self.pending.find(marker).map(|index| (index, marker)))
                .min_by_key(|(index, _)| *index);
            let Some((index, marker)) = marker else {
                let retained =
                    possible_marker_suffix_len(&self.pending, &[THINK_OPEN, THINK_CLOSE]);
                let emit = self.pending.len() - retained;
                output.push_str(&self.pending[..emit]);
                self.pending.drain(..emit);
                break;
            };
            output.push_str(&self.pending[..index]);
            self.pending.drain(..index + marker.len());
        }
        output
    }

    fn retain_possible_marker_suffix(&mut self, markers: &[&str]) {
        let retained = possible_marker_suffix_len(&self.pending, markers);
        if retained == 0 {
            self.pending.clear();
        } else {
            self.pending.drain(..self.pending.len() - retained);
        }
    }
}

fn filter_q27_package_output(text: &str) -> String {
    let mut filter = Q27PackageOutputFilter::default();
    let mut public = filter.push(text);
    public.push_str(&filter.finish());
    public
}

fn possible_marker_suffix_len(value: &str, markers: &[&str]) -> usize {
    markers
        .iter()
        .flat_map(|marker| 1..marker.len().min(value.len() + 1))
        .filter(|length| {
            value
                .get(value.len().saturating_sub(*length)..)
                .is_some_and(|suffix| markers.iter().any(|marker| marker.starts_with(suffix)))
        })
        .max()
        .unwrap_or(0)
}

struct SseState {
    source: BoxStream<'static, Result<Bytes, reqwest::Error>>,
    buffer: Vec<u8>,
    queued: VecDeque<Result<InferenceEvent, EngineError>>,
    usage: Option<InferenceUsage>,
    finish_reason: Option<InferenceFinishReason>,
    package_output_filter: Option<Q27PackageOutputFilter>,
    finished: bool,
}

fn q27_sse_stream(
    source: BoxStream<'static, Result<Bytes, reqwest::Error>>,
    filter_package_output: bool,
) -> InferenceStream {
    let state = SseState {
        source,
        buffer: Vec::new(),
        queued: VecDeque::new(),
        usage: None,
        finish_reason: None,
        package_output_filter: filter_package_output.then(Q27PackageOutputFilter::default),
        finished: false,
    };
    Box::pin(stream::unfold(state, |mut state| async move {
        loop {
            if let Some(event) = state.queued.pop_front() {
                return Some((event, state));
            }
            if state.finished {
                return None;
            }
            match state.source.next().await {
                Some(Ok(chunk)) => {
                    state.buffer.extend_from_slice(&chunk);
                    parse_sse_frames(&mut state);
                    if !state.finished && state.buffer.len() > SSE_FRAME_LIMIT {
                        state.buffer.clear();
                        state.queued.push_back(Err(EngineError::Operation(
                            "q27 SSE frame exceeded the local size limit".to_owned(),
                        )));
                        state.finished = true;
                    }
                }
                Some(Err(error)) => {
                    state.finished = true;
                    return Some((Err(map_transport_error(error)), state));
                }
                None => {
                    state.finished = true;
                    return Some((
                        Err(EngineError::BackendUnavailable(
                            "q27 stream ended before the [DONE] marker".to_owned(),
                        )),
                        state,
                    ));
                }
            }
        }
    }))
}

fn parse_sse_frames(state: &mut SseState) {
    while let Some((boundary, boundary_length)) = find_sse_boundary(&state.buffer) {
        let frame = state.buffer.drain(..boundary).collect::<Vec<_>>();
        state.buffer.drain(..boundary_length);
        let frame = String::from_utf8_lossy(&frame);
        let data = frame
            .lines()
            .filter_map(|line| line.strip_prefix("data:"))
            .map(str::trim_start)
            .collect::<Vec<_>>()
            .join("\n");
        if data.is_empty() {
            continue;
        }
        if data == "[DONE]" {
            if let Some(filter) = state.package_output_filter.as_mut() {
                let delta = filter.finish();
                if !delta.is_empty() {
                    state
                        .queued
                        .push_back(Ok(InferenceEvent::TextDelta { delta }));
                }
            }
            match state.finish_reason.take() {
                Some(finish_reason) => {
                    state.queued.push_back(Ok(InferenceEvent::Completed {
                        usage: state.usage.take(),
                        finish_reason,
                    }));
                }
                None => state.queued.push_back(Err(EngineError::BackendUnavailable(
                    "q27 stream reached [DONE] without a terminal finish reason".to_owned(),
                ))),
            }
            state.finished = true;
            return;
        }
        let value: Value = match serde_json::from_str(&data) {
            Ok(value) => value,
            Err(error) => {
                state.queued.push_back(Err(EngineError::Operation(format!(
                    "invalid q27 SSE payload: {error}"
                ))));
                state.finished = true;
                return;
            }
        };
        if let Some(error) = value.get("error") {
            state.queued.push_back(Err(EngineError::Operation(format!(
                "q27 streaming error: {}",
                backend_error_message(error)
            ))));
            state.finished = true;
            return;
        }
        if let Some(usage) = value.get("usage").filter(|usage| !usage.is_null()) {
            match serde_json::from_value::<ChatUsage>(usage.clone()) {
                Ok(usage) => state.usage = Some(usage.into()),
                Err(error) => {
                    state.queued.push_back(Err(EngineError::Operation(format!(
                        "invalid q27 streaming usage: {error}"
                    ))));
                    state.finished = true;
                    return;
                }
            }
        }
        if let Some(finish_reason) = value
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|choices| choices.first())
            .and_then(|choice| choice.get("finish_reason"))
            .filter(|reason| !reason.is_null())
        {
            let Some(finish_reason) = finish_reason.as_str() else {
                state.queued.push_back(Err(EngineError::Operation(
                    "q27 stream returned a non-string finish reason".to_owned(),
                )));
                state.finished = true;
                return;
            };
            match map_finish_reason(Some(finish_reason)) {
                Ok(finish_reason) => state.finish_reason = Some(finish_reason),
                Err(error) => {
                    state.queued.push_back(Err(error));
                    state.finished = true;
                    return;
                }
            }
        }
        if let Some(delta) = value
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|choices| choices.first())
            .and_then(|choice| {
                choice
                    .get("text")
                    .or_else(|| choice.get("delta").and_then(|delta| delta.get("content")))
            })
            .and_then(Value::as_str)
            .filter(|delta| !delta.is_empty())
        {
            let delta = state
                .package_output_filter
                .as_mut()
                .map_or_else(|| delta.to_owned(), |filter| filter.push(delta));
            if !delta.is_empty() {
                state
                    .queued
                    .push_back(Ok(InferenceEvent::TextDelta { delta }));
            }
        }
    }
}

fn find_sse_boundary(buffer: &[u8]) -> Option<(usize, usize)> {
    for index in 0..buffer.len().saturating_sub(1) {
        if buffer[index..].starts_with(b"\r\n\r\n") {
            return Some((index, 4));
        }
        if buffer[index..].starts_with(b"\n\n") {
            return Some((index, 2));
        }
    }
    None
}

fn message_json(message: &InferenceMessage) -> Value {
    json!({
        "role": match message.role {
            InferenceRole::System | InferenceRole::Developer => "system",
            InferenceRole::User => "user",
            InferenceRole::Assistant => "assistant",
        },
        "content": message.text,
    })
}

fn sharp_message_json(message: &InferenceMessage) -> Value {
    json!({
        "role": match message.role {
            InferenceRole::System => "system",
            InferenceRole::Developer => "developer",
            InferenceRole::User => "user",
            InferenceRole::Assistant => "assistant",
        },
        "content": message.text,
    })
}

fn invalid_probe(reason: String) -> EngineProbe {
    EngineProbe {
        installation: InstallationState::Invalid {
            reason: reason.clone(),
        },
        update: UpdateState::Unknown,
        healthy: false,
        detail: reason,
    }
}

fn q27_load_setting_definitions() -> Vec<LoadSettingDefinition> {
    let mut definitions = common_load_setting_definitions();
    definitions.extend([
        q27_definition(
            "q27.kv_fp16",
            "FP16 KV cache",
            "Opt in to q27's FP16 KV cache",
            LoadSettingKind::OneWayFlag,
            Some("runtime/profile-selected KV format"),
        ),
        q27_definition(
            "q27.fast_head",
            "Fast head",
            "Explicitly enable or disable q27 fast-head behavior",
            LoadSettingKind::Toggle,
            Some("runtime profile-selected"),
        ),
        q27_definition(
            "q27.prefix_cache_path",
            "Prefix cache path",
            "Directory for q27's persistent prefix cache",
            LoadSettingKind::Path,
            Some("disabled"),
        ),
        q27_definition(
            "q27.prefix_cache_max_gb",
            "Prefix cache disk budget",
            "Persistent prefix-cache LRU disk budget",
            LoadSettingKind::Float {
                minimum: Some(0.0),
                maximum: None,
            },
            Some("20 GB in current runtimes"),
        ),
        q27_definition(
            "q27.prefix_cache_min_tokens",
            "Prefix cache minimum",
            "Shortest prefix eligible for persistence",
            LoadSettingKind::UnsignedInteger {
                minimum: Some(1),
                maximum: None,
            },
            Some("4096 tokens in current runtimes"),
        ),
        q27_definition(
            "q27.prefix_cache_max_tokens",
            "Prefix cache maximum",
            "Largest prefix staged for persistence",
            LoadSettingKind::UnsignedInteger {
                minimum: Some(1),
                maximum: None,
            },
            Some("32768 tokens in current runtimes"),
        ),
        q27_definition(
            "q27.prefix_cache_step_tokens",
            "Prefix cache step",
            "Token growth required before re-persisting a conversation",
            LoadSettingKind::UnsignedInteger {
                minimum: Some(1),
                maximum: None,
            },
            Some("8192 tokens in current runtimes"),
        ),
        q27_definition(
            "q27.prefix_cache_ram_gb",
            "Prefix cache RAM budget",
            "Pinned host-RAM prefix-cache tier budget",
            LoadSettingKind::Float {
                minimum: Some(0.0),
                maximum: None,
            },
            Some("disabled"),
        ),
    ]);
    definitions
}

fn q27_definition(
    id: &str,
    label: &str,
    description: &str,
    kind: LoadSettingKind,
    upstream_default: Option<&str>,
) -> LoadSettingDefinition {
    LoadSettingDefinition {
        id: LoadSettingId::new(id).expect("static q27 setting ID"),
        label: label.to_owned(),
        description: description.to_owned(),
        kind,
        scope: LoadSettingScope::Engine {
            engine_id: ENGINE_ID.to_owned(),
        },
        supported: true,
        unsupported_reason: None,
        unit: None,
        upstream_default: upstream_default.map(str::to_owned),
        recommendation: None,
    }
}

fn q27_setting_option(id: &str) -> &'static str {
    match id {
        "context_length" => "--ctx",
        "parallel_requests" => "--slots",
        "q27.kv_fp16" => "--kv-fp16",
        "q27.fast_head" => "--fast-head",
        "q27.prefix_cache_path" => "--prefix-cache",
        "q27.prefix_cache_max_gb" => "--prefix-cache-max-gb",
        "q27.prefix_cache_min_tokens" => "--prefix-cache-min",
        "q27.prefix_cache_max_tokens" => "--prefix-cache-max-tokens",
        "q27.prefix_cache_step_tokens" => "--prefix-cache-step",
        "q27.prefix_cache_ram_gb" => "--prefix-cache-ram-gb",
        _ => "",
    }
}

fn q27_setting_unavailable_by_version(managed: bool, version: &str, option: &str) -> bool {
    managed
        && ((option == "--prefix-cache-ram-gb" && !version_at_least(version, 0, 6, 1))
            || (option.starts_with("--prefix-cache") && !version_at_least(version, 0, 6, 0)))
}

fn apply_q27_runtime_bounds(
    definitions: &mut [LoadSettingDefinition],
    managed: bool,
    version: &str,
) {
    let maximum = managed.then(|| {
        if version_at_least(version, 0, 3, 1) {
            8
        } else {
            4
        }
    });
    if let Some(definition) = definitions
        .iter_mut()
        .find(|definition| definition.id.as_str() == "parallel_requests")
    {
        definition.kind = LoadSettingKind::UnsignedInteger {
            minimum: Some(1),
            maximum,
        };
    }
}

#[derive(Debug)]
struct Q27StructuredArguments {
    arguments: Vec<OsString>,
    environment_remove: Vec<OsString>,
}

fn translate_q27_load_settings(
    settings: &norted_core::ResolvedLoadSettings,
    native_arguments: &[String],
    configured_environment: &BTreeMap<String, String>,
) -> Result<Q27StructuredArguments, EngineError> {
    let has_prefix_path = settings.value("q27.prefix_cache_path").is_some();
    if !has_prefix_path
        && settings.effective.keys().any(|id| {
            id.as_str().starts_with("q27.prefix_cache_") && id.as_str() != "q27.prefix_cache_path"
        })
    {
        return Err(EngineError::InvalidConfiguration(
            "q27 prefix-cache tuning settings require `q27.prefix_cache_path` so the runtime cannot silently ignore them"
                .to_owned(),
        ));
    }

    let mut arguments = Vec::new();
    let mut environment_remove = Vec::new();
    for (id, resolved) in &settings.effective {
        let aliases = match id.as_str() {
            "q27.fast_head" => vec!["--fast-head", "--no-fast-head"],
            value => vec![q27_setting_option(value)],
        };
        if let Some(argument) = find_q27_native_option(native_arguments, &aliases) {
            return Err(EngineError::InvalidConfiguration(format!(
                "structured load setting `{id}` conflicts with native q27 argument `{argument}`"
            )));
        }
        if id.as_str() == "q27.kv_fp16" {
            if let Some(name) = configured_environment
                .keys()
                .find(|name| name.eq_ignore_ascii_case("Q27_KV"))
            {
                return Err(EngineError::InvalidConfiguration(format!(
                    "structured load setting `{id}` conflicts with configured q27 environment variable `{name}`"
                )));
            }
            environment_remove.push(OsString::from("Q27_KV"));
        }
        match (id.as_str(), &resolved.value) {
            ("context_length", LoadSettingValue::UnsignedInteger(value)) => {
                push_q27_value_argument(&mut arguments, "--ctx", value);
            }
            ("parallel_requests", LoadSettingValue::UnsignedInteger(value)) => {
                push_q27_value_argument(&mut arguments, "--slots", value);
            }
            ("q27.kv_fp16", LoadSettingValue::FlagEnabled) => {
                arguments.push(OsString::from("--kv-fp16"));
            }
            ("q27.fast_head", LoadSettingValue::Toggle(value)) => {
                arguments.push(OsString::from(if *value {
                    "--fast-head"
                } else {
                    "--no-fast-head"
                }));
            }
            ("q27.prefix_cache_path", LoadSettingValue::Path(value)) => {
                arguments.push(OsString::from("--prefix-cache"));
                arguments.push(value.as_os_str().to_owned());
            }
            ("q27.prefix_cache_max_gb", LoadSettingValue::Float(value)) => {
                push_q27_value_argument(&mut arguments, "--prefix-cache-max-gb", value);
            }
            ("q27.prefix_cache_min_tokens", LoadSettingValue::UnsignedInteger(value)) => {
                push_q27_value_argument(&mut arguments, "--prefix-cache-min", value);
            }
            ("q27.prefix_cache_max_tokens", LoadSettingValue::UnsignedInteger(value)) => {
                push_q27_value_argument(&mut arguments, "--prefix-cache-max-tokens", value);
            }
            ("q27.prefix_cache_step_tokens", LoadSettingValue::UnsignedInteger(value)) => {
                push_q27_value_argument(&mut arguments, "--prefix-cache-step", value);
            }
            ("q27.prefix_cache_ram_gb", LoadSettingValue::Float(value)) => {
                push_q27_value_argument(&mut arguments, "--prefix-cache-ram-gb", value);
            }
            _ => {
                return Err(EngineError::InvalidConfiguration(format!(
                    "load setting `{id}` has an invalid value for q27"
                )));
            }
        }
    }
    Ok(Q27StructuredArguments {
        arguments,
        environment_remove,
    })
}

fn find_q27_native_option<'a>(arguments: &'a [String], aliases: &[&str]) -> Option<&'a str> {
    arguments.iter().find_map(|argument| {
        aliases
            .iter()
            .any(|alias| {
                argument == alias
                    || argument
                        .strip_prefix(alias)
                        .is_some_and(|suffix| suffix.starts_with('='))
            })
            .then_some(argument.as_str())
    })
}

fn push_q27_value_argument(arguments: &mut Vec<OsString>, option: &str, value: impl ToString) {
    arguments.push(OsString::from(option));
    arguments.push(OsString::from(value.to_string()));
}

fn q27_usage_contract_error(output: &str, native_arguments: &[String]) -> Option<String> {
    let output = output.to_ascii_lowercase();
    let positional_contract = output.contains("usage:")
        && output.contains("model.q27")
        && output.contains("model.tok")
        && usage_has_token(&output, "--host")
        && usage_has_token(&output, "--port");
    if !positional_contract {
        return Some(
            "positional model/tokenizer and private bind options were not advertised".to_owned(),
        );
    }
    if !usage_has_token(&output, "no-think") && !usage_has_token(&output, "--no-think") {
        return Some("the required no-think launch behavior was not advertised".to_owned());
    }
    native_arguments
        .iter()
        .filter(|argument| argument.starts_with("--"))
        .find(|argument| !usage_has_token(&output, argument))
        .map(|argument| format!("configured native option `{argument}` was not advertised"))
}

fn usage_has_token(output: &str, expected: &str) -> bool {
    output.split_whitespace().any(|token| {
        token.trim_matches(|character: char| {
            matches!(
                character,
                '[' | ']' | '(' | ')' | '{' | '}' | '<' | '>' | ',' | '.' | ':' | ';' | '`'
            )
        }) == expected
    })
}

fn conflicts_with_managed_argument(argument: &str) -> bool {
    let argument = argument.to_ascii_lowercase().replace('_', "-");
    MANAGED_NATIVE_ARGUMENTS.iter().any(|managed| {
        argument == *managed
            || argument
                .strip_prefix(managed)
                .is_some_and(|suffix| suffix.starts_with('='))
    })
}

fn invalid_native_argument(arguments: &[String]) -> Option<&str> {
    let mut index = 0;
    while let Some(argument) = arguments.get(index) {
        if ALLOWED_BOOLEAN_NATIVE_ARGUMENTS.contains(&argument.as_str()) {
            index += 1;
            continue;
        }
        if ALLOWED_VALUE_NATIVE_ARGUMENTS.contains(&argument.as_str()) {
            let Some(value) = arguments.get(index + 1) else {
                return Some(argument);
            };
            if value.starts_with('-') {
                return Some(value);
            }
            index += 2;
            continue;
        }
        return Some(argument);
    }
    None
}

fn unsupported_native_argument_for_version<'a>(
    arguments: &'a [String],
    version: &str,
) -> Option<&'a str> {
    arguments.iter().find_map(|argument| {
        let unavailable = (argument == "--prefix-cache-ram-gb"
            && !version_at_least(version, 0, 6, 1))
            || (argument.starts_with("--prefix-cache") && !version_at_least(version, 0, 6, 0));
        unavailable.then_some(argument.as_str())
    })
}

fn conflicts_with_managed_environment(name: &str) -> bool {
    MANAGED_ENVIRONMENT_VARIABLES
        .iter()
        .any(|managed| name.eq_ignore_ascii_case(managed))
}

fn q27_launch_environment(
    configured: &BTreeMap<String, String>,
    accelerator: &AcceleratorDevice,
) -> Result<BTreeMap<String, String>, EngineError> {
    isolated_cuda_environment(configured, accelerator, "q27")
        .map_err(EngineError::InvalidConfiguration)
}

fn managed_environment_removals() -> Vec<OsString> {
    MANAGED_ENVIRONMENT_VARIABLES
        .iter()
        .map(OsString::from)
        .collect()
}

fn map_finish_reason(reason: Option<&str>) -> Result<InferenceFinishReason, EngineError> {
    match reason {
        Some("stop") => Ok(InferenceFinishReason::Stop),
        Some("length") => Ok(InferenceFinishReason::MaxOutputTokens),
        Some(reason) => Err(EngineError::Operation(format!(
            "q27 returned unsupported finish reason `{reason}`"
        ))),
        None => Err(EngineError::Operation(
            "q27 response omitted its terminal finish reason".to_owned(),
        )),
    }
}

async fn hash_file(path: &Path) -> std::io::Result<String> {
    let mut file = tokio::fs::File::open(path).await?;
    let mut hash = Sha256::new();
    let mut buffer = vec![0_u8; 1024 * 1024];
    loop {
        let read = file.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        hash.update(&buffer[..read]);
    }
    Ok(hex_digest(hash.finalize()))
}

fn hex_digest(bytes: impl AsRef<[u8]>) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let bytes = bytes.as_ref();
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

fn map_transport_error(error: reqwest::Error) -> EngineError {
    if error.is_timeout() {
        EngineError::TimedOut("q27 inference request timed out".to_owned())
    } else {
        EngineError::BackendUnavailable(error.to_string())
    }
}

fn backend_http_error(status: reqwest::StatusCode, body: &[u8]) -> EngineError {
    let message = serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|value| value.get("error").cloned())
        .map_or_else(
            || {
                status
                    .canonical_reason()
                    .unwrap_or("backend request failed")
                    .to_owned()
            },
            |error| backend_error_message(&error),
        );
    EngineError::Operation(format!("q27 returned HTTP {status}: {message}"))
}

fn backend_error_message(error: &Value) -> String {
    error
        .get("message")
        .and_then(Value::as_str)
        .or_else(|| error.as_str())
        .unwrap_or("unknown backend error")
        .to_owned()
}

fn command_detail(stdout: &str, stderr: &str) -> String {
    match (stdout.trim(), stderr.trim()) {
        ("", "") => "no output".to_owned(),
        ("", stderr) => stderr.to_owned(),
        (stdout, "") => stdout.to_owned(),
        (stdout, stderr) => format!("{stdout}\n{stderr}"),
    }
}

fn http_endpoint(address: SocketAddr) -> String {
    if address.is_ipv6() {
        format!("http://[{}]:{}", address.ip(), address.port())
    } else {
        format!("http://{address}")
    }
}

fn unix_timestamp() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_secs()).ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use norted_core::{
        AcceleratorDevice, ArtifactFormat, AuxiliaryArtifact, LoadSettingSource,
        LoadSettingsProvenance, ModelArtifact, ModelId, ResolvedLoadSetting, ResolvedLoadSettings,
    };
    use serde_json::json;

    use super::*;

    fn resolved_load_settings(values: &[(&str, LoadSettingValue)]) -> ResolvedLoadSettings {
        let effective = values
            .iter()
            .map(|(id, value)| {
                (
                    LoadSettingId::new(*id).expect("setting ID"),
                    ResolvedLoadSetting {
                        value: value.clone(),
                        source: LoadSettingSource::Invocation,
                    },
                )
            })
            .collect();
        ResolvedLoadSettings {
            engine_id: ENGINE_ID.to_owned(),
            selected_profile: None,
            effective,
        }
    }

    fn argument_strings(arguments: Vec<OsString>) -> Vec<String> {
        arguments
            .into_iter()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect()
    }

    fn q27_runtime_identity(variant: &str) -> RuntimeIdentity {
        RuntimeIdentity {
            engine_id: ENGINE_ID.to_owned(),
            package_family: PACKAGE_FAMILY.to_owned(),
            version: "0.6.2".to_owned(),
            platform: "linux".to_owned(),
            architecture: "x86_64".to_owned(),
            accelerator: "cuda".to_owned(),
            variant: variant.to_owned(),
            upstream_revision: None,
            package: RuntimePackageIdentity {
                provider_id: PROVIDER_ID.to_owned(),
                repository: Some(GITHUB_REPOSITORY.to_owned()),
                release_tag: Some("v0.6.2".to_owned()),
                asset_id: Some("1".to_owned()),
                asset_name: Some("q27-v0.6.2-linux-x86_64.tar.gz".to_owned()),
                additional_assets: Vec::new(),
            },
        }
    }

    fn q27_package_fixture() -> NortedPackageBinding {
        NortedPackageBinding {
            kind: norted_core::NortedPackageKind::Q27,
            manifest_schema: "norted.q27-package".to_owned(),
            manifest_version: 1,
            package_root: PathBuf::new(),
            manifest_path: PathBuf::new(),
            manifest_sha256: "00".repeat(32),
            output_key: "q6".to_owned(),
            expected_primary_size: 1,
            expected_primary_sha256: "11".repeat(32),
            build_key: None,
            master_id: None,
            quant_recipe_key: None,
            canonical_source_lineage_key: Some("22".repeat(32)),
            runtime_policy: None,
            runtime_policy_id: None,
            runtime_policy_profile: Some("quality".to_owned()),
            sharp: None,
            sharp_revision: Some("sharp".to_owned()),
            sharp_version: Some("1".to_owned()),
            tokenizer: None,
            projector: None,
            policy: NortedPackagePolicy::Q27(Q27PackagePolicy {
                profile: "quality".to_owned(),
                target: "qwen3.8-27b".to_owned(),
                thinking_enabled: true,
                default_reasoning_effort: "medium".to_owned(),
                unlimited_thinking_budget: true,
                temperature: 1.0,
                top_p: 0.95,
                top_k: 20,
                min_p: 0.05,
                maximum_mtp_depth: "auto7".to_owned(),
                mtp_minimum_probability: 0.5,
                suffix_drafting: true,
                suffix_width_from_runtime_w_max: true,
                fast_head_default: false,
                fast_head_override_allowed: true,
                preferred_context_tokens: 262_144,
                minimum_context_tokens: 200_000,
                kv_preference: vec!["fp8".to_owned(), "turbo5k".to_owned(), "turbo3".to_owned()],
            }),
            allowed_user_overrides: vec!["temperature".to_owned(), "top_p".to_owned()],
            status: norted_core::NortedPackageStatus::NeedsRuntimeCapability {
                requirements: vec!["runtime".to_owned()],
            },
        }
    }

    #[test]
    fn official_q27_variants_have_source_proven_compiled_widths() {
        let mut identity = RuntimeIdentity {
            engine_id: ENGINE_ID.to_owned(),
            package_family: PACKAGE_FAMILY.to_owned(),
            version: "0.6.2".to_owned(),
            platform: "linux".to_owned(),
            architecture: "x86_64".to_owned(),
            accelerator: "cuda".to_owned(),
            variant: "w8".to_owned(),
            upstream_revision: None,
            package: RuntimePackageIdentity {
                provider_id: PROVIDER_ID.to_owned(),
                repository: Some(GITHUB_REPOSITORY.to_owned()),
                release_tag: Some("v0.6.2".to_owned()),
                asset_id: Some("1".to_owned()),
                asset_name: Some("q27-v0.6.2-linux-x86_64.tar.gz".to_owned()),
                additional_assets: Vec::new(),
            },
        };
        assert_eq!(q27_compiled_w_max(&identity, false), Some(8));
        identity.variant = "w12".to_owned();
        assert_eq!(q27_compiled_w_max(&identity, false), Some(12));
        identity.variant = "w16".to_owned();
        assert_eq!(q27_compiled_w_max(&identity, false), Some(16));
        assert_eq!(q27_compiled_w_max(&identity, true), None);
    }

    #[test]
    fn exact_old_runtime_has_precise_package_capability_failures() {
        let identity = q27_runtime_identity("w12");
        let compatibility = evaluate_q27_package_runtime(q27_package_capabilities(
            &identity,
            &RuntimeAcquisitionMethod::OfficialReleaseAsset,
        ));
        assert!(matches!(
            compatibility,
            RuntimeCompatibility::Incompatible(ref reason)
                if reason.contains("top-k/min-p")
                    && !reason.contains("all Norted packages")
        ));

        let synthetic = evaluate_q27_package_runtime(Q27PackageRuntimeCapabilities {
            trustworthy_identity: true,
            raw_completions: true,
            exact_sharp_renderer: true,
            thinking: true,
            unlimited_think_budget: true,
            temperature_top_p: true,
            top_k_min_p: true,
            mtp_environment: true,
            fast_head_control: true,
            bounded_startup_observation: true,
            compiled_w_max: Some(12),
            supported_kv_modes: ALL_PACKAGE_KV_MODES,
        });
        assert!(matches!(synthetic, RuntimeCompatibility::NeedsAttention(_)));
        assert!(
            validate_q27_package_prelaunch(Q27PackageRuntimeCapabilities {
                trustworthy_identity: true,
                raw_completions: true,
                exact_sharp_renderer: true,
                thinking: true,
                unlimited_think_budget: true,
                temperature_top_p: true,
                top_k_min_p: true,
                mtp_environment: true,
                fast_head_control: true,
                bounded_startup_observation: true,
                compiled_w_max: Some(12),
                supported_kv_modes: ALL_PACKAGE_KV_MODES,
            })
            .is_ok()
        );

        let untrusted = q27_package_capabilities(
            &RuntimeIdentity {
                version: "future-by-name-only".to_owned(),
                ..identity
            },
            &RuntimeAcquisitionMethod::ExternalBinary,
        );
        let untrusted_compatibility = evaluate_q27_package_runtime(untrusted);
        assert!(matches!(
            untrusted_compatibility,
            RuntimeCompatibility::Incompatible(ref reason)
                if reason.contains("no trustworthy package-capability observation")
        ));
        assert!(!untrusted_compatibility.is_usable());
        assert!(validate_q27_package_prelaunch(untrusted).is_err());
    }

    #[test]
    fn fully_capable_package_policy_reaches_q27_arguments_and_environment() {
        let package = q27_package_fixture();
        let translated = q27_package_launch_policy(&package, 12, false, Q27PackageKvMode::Fp8)
            .expect("package launch policy");
        let arguments = argument_strings(translated.arguments);
        assert!(arguments.windows(2).any(|pair| pair == ["--top-k", "20"]));
        assert!(arguments.windows(2).any(|pair| pair == ["--min-p", "0.05"]));
        assert!(
            arguments
                .windows(2)
                .any(|pair| pair == ["--think-budget", "0"])
        );
        assert_eq!(translated.environment["Q27_MAXD"], "auto7");
        assert_eq!(translated.environment["Q27_PMIN"], "0.5");
        assert_eq!(translated.environment["Q27_SUFFIX"], "1");
        assert_eq!(translated.environment["Q27_SUFFIX_W"], "12");
        assert_eq!(translated.normalized_settings["top_k"], json!(20));
        assert_eq!(translated.normalized_settings["min_p"], json!(0.05));
    }

    #[test]
    fn exact_runtime_kv_modes_follow_package_quality_order_without_inventing_turbo5k() {
        let package = q27_package_fixture();
        let policy = q27_package_policy(&package).expect("q27 policy");
        let exact = q27_package_capabilities(
            &q27_runtime_identity("w12"),
            &RuntimeAcquisitionMethod::OfficialReleaseAsset,
        );
        assert_eq!(
            q27_package_kv_attempt_modes(exact, policy),
            [Q27PackageKvMode::Fp8, Q27PackageKvMode::Turbo3]
        );

        let future = Q27PackageRuntimeCapabilities {
            top_k_min_p: true,
            supported_kv_modes: ALL_PACKAGE_KV_MODES,
            ..exact
        };
        assert_eq!(
            q27_package_kv_attempt_modes(future, policy),
            Q27PackageKvMode::QUALITY_ORDER
        );
        assert!(matches!(
            evaluate_q27_package_runtime(future),
            RuntimeCompatibility::NeedsAttention(_)
        ));
        assert!(validate_q27_package_prelaunch(future).is_ok());
    }

    #[test]
    fn bounded_startup_parser_proves_context_kv_and_numeric_wmax() {
        let observation = parse_q27_startup_observation(&[
            "profile: cc (sm_120) | kv=fp8 fd=mma pmin=0.5 maxd=auto7 suffix=1/w12 fast-head=0 think=1".to_owned(),
            "--ctx auto: 262144 (free 30.0GB post-weights, fp8 KV, W_MAX=12)".to_owned(),
            "slot 0 ready: ctx=262144".to_owned(),
        ])
        .expect("startup observation");
        assert_eq!(observation.served_context, 262_144);
        assert_eq!(observation.kv_mode, "fp8");
        assert_eq!(observation.compiled_w_max, 12);
        assert_eq!(observation.suffix_width, 12);
        assert!(observation.thinking);
    }

    #[tokio::test]
    async fn package_startup_retries_only_a_fully_valid_low_context_observation() {
        let adapter = Q27Adapter::from_config(None, Path::new("."));
        let endpoint = "http://127.0.0.1:43127";
        let package = q27_package_fixture();
        adapter.package_executions.write().await.insert(
            endpoint.to_owned(),
            Q27PackageExecution {
                sharp_template: "sharp".to_owned(),
                policy: q27_package_policy(&package).expect("policy").clone(),
                compiled_w_max: 12,
                expected_fast_head: false,
                selected_kv_mode: Q27PackageKvMode::Fp8,
            },
        );
        let process = ProcessDescriptor {
            supervisor_id: "test".to_owned(),
            process_id: 1,
            engine: EngineRevision {
                engine_id: ENGINE_ID.to_owned(),
                version: Some("0.6.2".to_owned()),
                revision: None,
            },
            runtime_id: RuntimeId("runtime".to_owned()),
            runtime_version: "0.6.2".to_owned(),
            runtime_variant: "w12".to_owned(),
            runtime_executable_sha256: "00".repeat(32),
            model_id: ModelId("model".to_owned()),
            endpoint: Some(endpoint.to_owned()),
            launched_at_unix: 0,
        };
        let low_context = [
            "profile: cc (sm_120) | kv=fp8 fd=mma pmin=0.5 maxd=auto7 suffix=1/w12 fast-head=0 think=1".to_owned(),
            "--ctx auto: 180000 (free 30.0GB post-weights, fp8 KV, W_MAX=12)".to_owned(),
            "slot 0 ready: ctx=180000".to_owned(),
        ];
        assert!(matches!(
            adapter
                .startup_observation(&process, &low_context)
                .await
                .expect("classified observation"),
            StartupObservation::RetryContextCapacity {
                ref kv_mode,
                observed_context: 180_000,
                minimum_context: 200_000,
            } if kv_mode == "fp8"
        ));

        let wrong_wmax = [
            low_context[0].clone(),
            "--ctx auto: 180000 (free 30.0GB post-weights, fp8 KV, W_MAX=8)".to_owned(),
            low_context[2].clone(),
        ];
        assert!(matches!(
            adapter.startup_observation(&process, &wrong_wmax).await,
            Err(EngineError::Operation(reason)) if reason.contains("W_MAX")
        ));
    }

    #[tokio::test]
    async fn clearing_reused_endpoint_forces_a_later_raw_launch_to_raw_state() {
        let adapter = Q27Adapter::from_config(None, Path::new("."));
        let endpoint = "http://127.0.0.1:43128";
        let package = q27_package_fixture();
        adapter.package_executions.write().await.insert(
            endpoint.to_owned(),
            Q27PackageExecution {
                sharp_template: "stale".to_owned(),
                policy: q27_package_policy(&package).expect("policy").clone(),
                compiled_w_max: 12,
                expected_fast_head: false,
                selected_kv_mode: Q27PackageKvMode::Fp8,
            },
        );
        adapter.clear_launch_state(Some(endpoint)).await;
        assert!(
            adapter
                .package_executions
                .read()
                .await
                .get(endpoint)
                .is_none()
        );
    }

    #[test]
    fn sharp_renderer_preserves_developer_and_message_order() {
        let template = concat!(
            "{%- for message in messages -%}",
            "{{ message.role }}={{ message.content }};",
            "{%- endfor -%}",
            "{% if add_generation_prompt %}assistant={{ reasoning_effort }}{% endif %}"
        );
        let rendered = render_sharp_template(
            template,
            &[
                InferenceMessage {
                    role: InferenceRole::System,
                    text: "s".to_owned(),
                },
                InferenceMessage {
                    role: InferenceRole::Developer,
                    text: "d".to_owned(),
                },
                InferenceMessage {
                    role: InferenceRole::User,
                    text: "u".to_owned(),
                },
                InferenceMessage {
                    role: InferenceRole::Assistant,
                    text: "a".to_owned(),
                },
            ],
        )
        .expect("Sharp rendering");
        assert_eq!(
            rendered,
            "system=s;developer=d;user=u;assistant=a;assistant=medium"
        );

        let package = q27_package_fixture();
        let NortedPackagePolicy::Q27(policy) = package.policy else {
            unreachable!()
        };
        let body = Q27Adapter::package_backend_request(
            &Q27PackageExecution {
                sharp_template: template.to_owned(),
                policy,
                compiled_w_max: 12,
                expected_fast_head: false,
                selected_kv_mode: Q27PackageKvMode::Fp8,
            },
            &InferenceRequest {
                model_id: ModelId("package-model".to_owned()),
                messages: vec![InferenceMessage {
                    role: InferenceRole::Developer,
                    text: "bound Sharp".to_owned(),
                }],
                generation_settings: GenerationSettingsPatch::default(),
                max_output_tokens: Some(32),
                stream: false,
            },
            false,
        )
        .expect("raw package request");
        assert!(body.get("messages").is_none());
        assert_eq!(body["prompt"], "developer=bound Sharp;assistant=medium");
        assert_eq!(body["top_k"], 20);
        assert_eq!(body["min_p"], 0.05);
    }

    #[test]
    fn omitted_settings_emit_no_q27_arguments() {
        let translated =
            translate_q27_load_settings(&resolved_load_settings(&[]), &[], &BTreeMap::new())
                .expect("empty translation");
        assert!(translated.arguments.is_empty());
        assert!(translated.environment_remove.is_empty());
    }

    #[test]
    fn q27_common_settings_translate_to_ctx_and_slots() {
        let translated = translate_q27_load_settings(
            &resolved_load_settings(&[
                ("context_length", LoadSettingValue::UnsignedInteger(65_536)),
                ("parallel_requests", LoadSettingValue::UnsignedInteger(2)),
                ("q27.kv_fp16", LoadSettingValue::FlagEnabled),
            ]),
            &[],
            &BTreeMap::new(),
        )
        .expect("q27 translation");
        assert_eq!(
            argument_strings(translated.arguments),
            ["--ctx", "65536", "--slots", "2", "--kv-fp16"]
        );
        assert_eq!(translated.environment_remove, [OsString::from("Q27_KV")]);
    }

    fn q27_schema_with_bounds(managed: bool, version: &str) -> LoadSettingsSchema {
        let mut definitions = q27_load_setting_definitions();
        apply_q27_runtime_bounds(&mut definitions, managed, version);
        LoadSettingsSchema {
            engine_id: ENGINE_ID.to_owned(),
            runtime_id: None,
            definitions,
        }
    }

    #[test]
    fn managed_parallel_request_bound_changes_at_v031() {
        for (version, expected) in [("0.2.0", 4), ("0.3.0", 4), ("0.3.1", 8), ("0.6.2", 8)] {
            let schema = q27_schema_with_bounds(true, version);
            let definition = schema
                .definitions
                .iter()
                .find(|definition| definition.id.as_str() == "parallel_requests")
                .expect("parallel definition");
            assert_eq!(
                definition.kind,
                LoadSettingKind::UnsignedInteger {
                    minimum: Some(1),
                    maximum: Some(expected),
                }
            );
        }
    }

    #[test]
    fn managed_v030_rejects_eight_slots_before_translation() {
        let schema = q27_schema_with_bounds(true, "0.3.0");
        let settings =
            resolved_load_settings(&[("parallel_requests", LoadSettingValue::UnsignedInteger(8))]);
        assert!(schema.validate(&settings).is_err());
    }

    #[test]
    fn managed_v031_accepts_eight_slots() {
        let schema = q27_schema_with_bounds(true, "0.3.1");
        let settings =
            resolved_load_settings(&[("parallel_requests", LoadSettingValue::UnsignedInteger(8))]);
        schema.validate(&settings).expect("v0.3.1 eight slots");
    }

    #[test]
    fn external_parallel_requests_has_no_invented_maximum() {
        let schema = q27_schema_with_bounds(false, "external");
        let settings =
            resolved_load_settings(&[("parallel_requests", LoadSettingValue::UnsignedInteger(64))]);
        schema
            .validate(&settings)
            .expect("external usage does not prove a maximum");
        let translated =
            translate_q27_load_settings(&settings, &[], &BTreeMap::new()).expect("translation");
        assert_eq!(argument_strings(translated.arguments), ["--slots", "64"]);
    }

    #[test]
    fn explicit_positive_context_below_32_remains_valid() {
        let schema = q27_schema_with_bounds(true, "0.6.2");
        let settings =
            resolved_load_settings(&[("context_length", LoadSettingValue::UnsignedInteger(8))]);
        schema
            .validate(&settings)
            .expect("explicit positive q27 context");
        let translated =
            translate_q27_load_settings(&settings, &[], &BTreeMap::new()).expect("translation");
        assert_eq!(argument_strings(translated.arguments), ["--ctx", "8"]);
    }

    #[test]
    fn prefix_cache_launch_argument_matches_the_effective_path() {
        let path = std::env::temp_dir().join("norted-data/cache/q27");
        let settings = resolved_load_settings(&[(
            "q27.prefix_cache_path",
            LoadSettingValue::Path(path.clone()),
        )]);
        let translated =
            translate_q27_load_settings(&settings, &[], &BTreeMap::new()).expect("translation");
        assert_eq!(
            translated.arguments,
            [
                OsString::from("--prefix-cache"),
                path.clone().into_os_string()
            ]
        );
        let provenance = LoadSettingsProvenance {
            effective: settings.effective.clone(),
        };
        assert_eq!(
            provenance
                .effective
                .get(&LoadSettingId::new("q27.prefix_cache_path").expect("setting ID"))
                .map(|setting| &setting.value),
            Some(&LoadSettingValue::Path(path))
        );
    }

    #[test]
    fn q27_structured_native_and_environment_conflicts_are_rejected() {
        let context =
            resolved_load_settings(&[("context_length", LoadSettingValue::UnsignedInteger(8192))]);
        assert!(
            translate_q27_load_settings(&context, &["--ctx=4096".to_owned()], &BTreeMap::new())
                .expect_err("native collision")
                .to_string()
                .contains("conflicts")
        );
        let kv = resolved_load_settings(&[("q27.kv_fp16", LoadSettingValue::FlagEnabled)]);
        assert!(
            translate_q27_load_settings(
                &kv,
                &[],
                &BTreeMap::from([("Q27_KV".to_owned(), "turbo5k".to_owned())])
            )
            .expect_err("environment collision")
            .to_string()
            .contains("Q27_KV")
        );
    }

    #[test]
    fn q27_prefix_tuning_without_a_cache_path_is_rejected() {
        let settings = resolved_load_settings(&[(
            "q27.prefix_cache_min_tokens",
            LoadSettingValue::UnsignedInteger(4096),
        )]);
        assert!(
            translate_q27_load_settings(&settings, &[], &BTreeMap::new())
                .expect_err("ignored prefix tuning")
                .to_string()
                .contains("require `q27.prefix_cache_path`")
        );
    }

    #[test]
    fn structured_prefix_cache_settings_keep_the_q27_version_gates() {
        assert!(q27_setting_unavailable_by_version(
            true,
            "0.5.9",
            "--prefix-cache"
        ));
        assert!(q27_setting_unavailable_by_version(
            true,
            "0.6.0",
            "--prefix-cache-ram-gb"
        ));
        assert!(!q27_setting_unavailable_by_version(
            true,
            "0.6.1",
            "--prefix-cache-ram-gb"
        ));
        assert!(!q27_setting_unavailable_by_version(
            false,
            "external",
            "--prefix-cache-ram-gb"
        ));
    }

    #[test]
    fn exact_q27_reference_is_recovered_from_ids_and_queries() {
        assert_eq!(q27_tag_from_reference("v0.6.2"), Some("v0.6.2".to_owned()));
        assert_eq!(q27_tag_from_reference("0.6.2"), Some("v0.6.2".to_owned()));
        assert_eq!(
            q27_tag_from_reference("q27-0-6-2-linux-x86-64-cuda-w12-deadbeef"),
            Some("v0.6.2".to_owned())
        );
        assert_eq!(q27_tag_from_reference("q27 cuda"), None);
    }

    #[test]
    fn usage_probe_requires_managed_contract_and_configured_options() {
        let current = "Usage: q27-server model.q27 model.tok --host HOST --port PORT \
                       defaults: fast-head + no-think + phase stats --ctx N --kv-fp16";
        assert_eq!(
            q27_usage_contract_error(
                current,
                &[
                    "--ctx".to_owned(),
                    "8192".to_owned(),
                    "--kv-fp16".to_owned()
                ]
            ),
            None
        );
        assert!(
            q27_usage_contract_error(
                "Usage: q27-server model.q27 model.tok --host HOST --port PORT",
                &[]
            )
            .is_some_and(|error| error.contains("no-think"))
        );
        assert!(
            q27_usage_contract_error(current, &["--prefix-cache".to_owned(), "cache".to_owned()])
                .is_some_and(|error| error.contains("--prefix-cache"))
        );
    }

    fn release(
        id: u64,
        tag: &str,
        published_at: &str,
        prerelease: bool,
        assets: Vec<GitHubReleaseAsset>,
    ) -> GitHubRelease {
        GitHubRelease {
            id,
            tag_name: tag.to_owned(),
            name: Some(tag.to_owned()),
            html_url: format!("https://github.com/signalnine/q27/releases/tag/{tag}"),
            target_commitish: "master".to_owned(),
            draft: false,
            prerelease,
            published_at: Some(published_at.to_owned()),
            assets,
        }
    }

    fn asset(id: u64, tag: &str, digest: Option<&str>) -> GitHubReleaseAsset {
        GitHubReleaseAsset {
            id,
            name: format!("q27-{tag}-linux-x86_64.tar.gz"),
            size: 14_689_716,
            browser_download_url: format!(
                "https://github.com/signalnine/q27/releases/download/{tag}/q27-{tag}-linux-x86_64.tar.gz"
            ),
            digest: digest.map(str::to_owned),
            state: "uploaded".to_owned(),
        }
    }

    #[test]
    fn catalog_ignores_source_only_newer_releases_and_marks_installable_channels() {
        let releases = vec![
            release(10, "v0.10.0", "2026-08-26T01:43:22Z", false, vec![]),
            release(
                6,
                "v0.6.2",
                "2026-07-25T05:30:39Z",
                false,
                vec![asset(
                    600,
                    "v0.6.2",
                    Some("sha256:d0b7bd5abc4c2e84ee8bf5ff3936161a2285dc1d8e3d408f121770838d2c6d11"),
                )],
            ),
        ];
        let runtimes = catalog_runtimes(&releases).expect("valid catalog");
        assert_eq!(runtimes.len(), 3);
        assert!(runtimes.iter().all(|runtime| {
            let Some((download, _)) = runtime.release_assets() else {
                return false;
            };
            runtime.identity.version == "0.6.2"
                && runtime.channels.contains(&RuntimeReleaseChannel::Stable)
                && runtime.channels.contains(&RuntimeReleaseChannel::Latest)
                && download.archive_format == RuntimeArchiveFormat::TarGz
                && download.digest.is_some()
        }));
        assert_eq!(
            runtimes
                .iter()
                .map(|runtime| runtime.identity.variant.as_str())
                .collect::<Vec<_>>(),
            ["w8", "w12", "w16"]
        );
        let w8 = runtimes
            .iter()
            .find(|runtime| runtime.identity.variant == "w8")
            .expect("W8");
        let w12 = runtimes
            .iter()
            .find(|runtime| runtime.identity.variant == "w12")
            .expect("W12");
        let w16 = runtimes
            .iter()
            .find(|runtime| runtime.identity.variant == "w16")
            .expect("W16");
        assert_eq!(w8.requirements.minimum_vram_class_gib, Some(24));
        assert_eq!(w12.requirements.minimum_vram_class_gib, Some(32));
        assert_eq!(w16.requirements.minimum_vram_class_gib, None);
        assert_eq!(w16.requirements.minimum_vram_exclusive_class_gib, Some(24));
        assert_eq!(
            w8.requirements.minimum_nvidia_driver.as_deref(),
            Some("580")
        );
        assert_eq!(
            w8.requirements.supported_cuda_compute_capabilities,
            vec![
                ComputeCapability::new(8, 6),
                ComputeCapability::new(8, 9),
                ComputeCapability::new(12, 0)
            ]
        );
        assert!(w16.requirements.advisories.iter().any(|note| {
            note.contains("specialist") && note.contains("no separate W16 VRAM floor")
        }));
    }

    #[test]
    fn catalog_rejects_missing_digests_and_non_linux_archive_shapes() {
        let mut wrong = asset(2, "v0.6.2", Some(&format!("sha256:{}", "a".repeat(64))));
        wrong.name = "q27-v0.6.2-windows-x86_64.zip".to_owned();
        let releases = vec![release(
            6,
            "v0.6.2",
            "2026-07-25T05:30:39Z",
            false,
            vec![asset(1, "v0.6.2", None), wrong],
        )];
        assert!(catalog_runtimes(&releases).expect("catalog").is_empty());
    }

    #[test]
    fn v01_archives_are_excluded_without_terminal_stream_finish_reasons() {
        let releases = vec![release(
            1,
            "v0.1.2",
            "2026-07-12T23:27:39Z",
            false,
            vec![asset(
                1,
                "v0.1.2",
                Some(&format!("sha256:{}", "a".repeat(64))),
            )],
        )];
        let runtimes = catalog_runtimes(&releases).expect("catalog");
        assert!(runtimes.is_empty());
    }

    #[test]
    fn absent_configuration_participates_and_explicit_false_disables() {
        let absent = Q27Adapter::from_config(None, Path::new("."));
        assert!(absent.is_enabled());
        let disabled = Q27Adapter::from_config(Some(&EngineConfig::default()), Path::new("."));
        assert!(!disabled.is_enabled());
        assert!(
            !disabled
                .compatibility(&model_artifact(PathBuf::from("model.q27")))
                .is_supported()
        );
    }

    #[test]
    fn managed_arguments_and_environment_are_reserved() {
        for argument in ["--host", "--PORT=9", "--no_think", "--temp=1"] {
            assert!(
                conflicts_with_managed_argument(argument),
                "accepted {argument}"
            );
        }
        for environment in [
            "Q27_API_KEY",
            "q27_force_temp",
            "Q27_BARE",
            "CUDA_VISIBLE_DEVICES",
        ] {
            assert!(
                conflicts_with_managed_environment(environment),
                "accepted {environment}"
            );
        }
        assert_eq!(
            invalid_native_argument(&["another-model.q27".to_owned()]),
            Some("another-model.q27")
        );
        assert_eq!(
            invalid_native_argument(&["--threads".to_owned(), "8".to_owned()]),
            Some("--threads")
        );
        assert_eq!(invalid_native_argument(&["--".to_owned()]), Some("--"));
        assert_eq!(
            invalid_native_argument(&[
                "--ctx".to_owned(),
                "8192".to_owned(),
                "--kv-fp16".to_owned(),
                "--slots".to_owned(),
                "2".to_owned(),
            ]),
            None
        );
        assert_eq!(
            invalid_native_argument(&["--kv-fp16".to_owned(), "other.q27".to_owned()]),
            Some("other.q27")
        );
        assert_eq!(
            invalid_native_argument(&["--ctx".to_owned()]),
            Some("--ctx")
        );
        assert_eq!(
            invalid_native_argument(&["--ctx=8192".to_owned()]),
            Some("--ctx=8192")
        );
    }

    #[test]
    fn native_cache_options_follow_the_selected_runtime_version() {
        let disk = vec!["--prefix-cache".to_owned(), "cache".to_owned()];
        let ram = vec!["--prefix-cache-ram-gb".to_owned(), "4".to_owned()];
        assert_eq!(
            unsupported_native_argument_for_version(&disk, "0.5.0"),
            Some("--prefix-cache")
        );
        assert_eq!(
            unsupported_native_argument_for_version(&disk, "0.6.0"),
            None
        );
        assert_eq!(
            unsupported_native_argument_for_version(&ram, "0.6.0"),
            Some("--prefix-cache-ram-gb")
        );
        assert_eq!(unsupported_native_argument_for_version(&ram, "0.6.1"), None);
    }

    #[test]
    fn adapter_requires_the_tokenizer_discovered_by_the_model_registry() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let model_path = directory.path().join("example-family-q4s.q27");
        fs::write(&model_path, b"model").expect("model");
        let expected = directory.path().join("example-family.tok");
        fs::write(&expected, b"Q27T\x01\0\0\0").expect("tokenizer");
        let mut model = model_artifact(model_path);
        assert!(tokenizer_candidate(&model).is_err());
        model.auxiliary_artifacts.push(AuxiliaryArtifact {
            role: AuxiliaryArtifactRole::Tokenizer,
            path: expected.clone(),
            size_bytes: 8,
            hash: None,
        });
        assert_eq!(tokenizer_candidate(&model).expect("candidate"), expected);
    }

    fn accelerator(uuid: &str, vram_gib: Option<u64>) -> AcceleratorDevice {
        accelerator_with_compute(uuid, vram_gib, Some(ComputeCapability::new(12, 0)))
    }

    fn accelerator_with_compute(
        uuid: &str,
        vram_gib: Option<u64>,
        compute_capability: Option<ComputeCapability>,
    ) -> AcceleratorDevice {
        AcceleratorDevice {
            accelerator: "cuda".to_owned(),
            stable_id: Some(uuid.to_owned()),
            name: Some("fixture".to_owned()),
            vram_bytes: vram_gib.map(|gib| gib * 1024 * 1024 * 1024),
            driver_version: Some("600".to_owned()),
            compute_capability,
        }
    }

    fn host_with_vram(gib: u64) -> HostCapabilities {
        HostCapabilities {
            platform: "linux".to_owned(),
            architecture: "x86_64".to_owned(),
            accelerators: vec![accelerator(
                "GPU-11111111-1111-1111-1111-111111111111",
                Some(gib),
            )],
            cuda_visible_devices: None,
            observations: Vec::new(),
        }
    }

    #[test]
    fn model_tiers_reject_clearly_insufficient_vram_classes() {
        let host_24 = host_with_vram(24);
        let device = &host_24.accelerators[0];
        assert!(matches!(
            q27_tier_compatibility(Q27Tier::Qwen36Q6, device),
            RuntimeCompatibility::Incompatible(_)
        ));
        assert!(matches!(
            q27_tier_compatibility(Q27Tier::Qwen36Q8, device),
            RuntimeCompatibility::Incompatible(_)
        ));
        assert!(matches!(
            q27_tier_compatibility(Q27Tier::Qwen38Q6, device),
            RuntimeCompatibility::Recommended
        ));
    }

    #[test]
    fn w8_and_w12_are_semantically_preferred_while_w16_is_specialist() {
        let host_24 = host_with_vram(24);
        let device_24 = &host_24.accelerators[0];
        assert!(
            q27_runtime_preference("w8", Some(device_24))
                < q27_runtime_preference("w12", Some(device_24))
        );
        assert!(
            q27_runtime_preference("w8", Some(device_24))
                < q27_runtime_preference("w16", Some(device_24))
        );

        let host_32 = host_with_vram(32);
        let device_32 = &host_32.accelerators[0];
        assert!(
            q27_runtime_preference("w12", Some(device_32))
                < q27_runtime_preference("w8", Some(device_32))
        );
        assert!(
            q27_runtime_preference("w12", Some(device_32))
                < q27_runtime_preference("w16", Some(device_32))
        );

        let unknown = accelerator("GPU-22222222-2222-2222-2222-222222222222", None);
        assert!(
            q27_runtime_preference("w8", Some(&unknown))
                < q27_runtime_preference("w12", Some(&unknown))
        );
    }

    #[test]
    fn exact_gpu_selection_and_uuid_binding_are_one_decision() {
        let host = HostCapabilities {
            platform: "linux".to_owned(),
            architecture: "x86_64".to_owned(),
            accelerators: vec![
                accelerator("GPU-aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa", Some(24)),
                accelerator("GPU-bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb", Some(32)),
            ],
            cuda_visible_devices: None,
            observations: Vec::new(),
        };
        let w12 = Q27_VARIANTS
            .iter()
            .find(|variant| variant.id == "w12")
            .expect("W12");
        let evaluation = q27_device_evaluation(
            "linux",
            "x86_64",
            &requirements_for("0.6.2", w12),
            Some(Q27Tier::Qwen36Q4s),
            &host,
            false,
        );
        let selected = evaluation.accelerator.expect("selected GPU");
        assert_eq!(
            selected.stable_id.as_deref(),
            Some("GPU-bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb")
        );
        let environment =
            q27_launch_environment(&BTreeMap::new(), &selected).expect("launch binding");
        assert_eq!(
            environment.get("CUDA_VISIBLE_DEVICES").map(String::as_str),
            selected.stable_id.as_deref()
        );
        assert!(
            q27_launch_environment(&BTreeMap::new(), &accelerator("GPU-bbbbbbbb", Some(32)),)
                .is_err()
        );
    }

    #[test]
    fn managed_cuda_targets_are_version_specific_and_checked_before_vram() {
        let w8 = Q27_VARIANTS
            .iter()
            .find(|variant| variant.id == "w8")
            .expect("W8");
        let v030 = requirements_for("0.3.0", w8);
        let v031 = requirements_for("0.3.1", w8);
        let ada = accelerator_with_compute(
            "GPU-adaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa",
            Some(24),
            Some(ComputeCapability::new(8, 9)),
        );
        assert!(matches!(
            compatibility_for_nvidia_device(&v030, &ada),
            RuntimeCompatibility::Incompatible(_)
        ));
        assert!(matches!(
            compatibility_for_nvidia_device(&v031, &ada),
            RuntimeCompatibility::Recommended
        ));

        let unknown =
            accelerator_with_compute("GPU-unknownn-aaaa-aaaa-aaaa-aaaaaaaaaaaa", Some(24), None);
        assert!(matches!(
            compatibility_for_nvidia_device(&v031, &unknown),
            RuntimeCompatibility::NeedsAttention(_)
        ));
    }

    #[test]
    fn supported_lower_vram_gpu_beats_unsupported_higher_vram_gpu_and_keeps_uuid() {
        let unsupported = accelerator_with_compute(
            "GPU-aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa",
            Some(40),
            Some(ComputeCapability::new(8, 0)),
        );
        let supported = accelerator_with_compute(
            "GPU-bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb",
            Some(32),
            Some(ComputeCapability::new(12, 0)),
        );
        let host = HostCapabilities {
            platform: "linux".to_owned(),
            architecture: "x86_64".to_owned(),
            accelerators: vec![unsupported, supported],
            cuda_visible_devices: None,
            observations: Vec::new(),
        };
        let w12 = Q27_VARIANTS
            .iter()
            .find(|variant| variant.id == "w12")
            .expect("W12");
        let evaluation = q27_device_evaluation(
            "linux",
            "x86_64",
            &requirements_for("0.3.1", w12),
            Some(Q27Tier::Qwen36Q4s),
            &host,
            false,
        );
        assert!(matches!(
            evaluation.compatibility,
            RuntimeCompatibility::Recommended
        ));
        let selected = evaluation.accelerator.expect("supported GPU");
        assert_eq!(
            selected.stable_id.as_deref(),
            Some("GPU-bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb")
        );
        let environment =
            q27_launch_environment(&BTreeMap::new(), &selected).expect("launch binding");
        assert_eq!(
            environment.get("CUDA_VISIBLE_DEVICES").map(String::as_str),
            selected.stable_id.as_deref()
        );
    }

    #[test]
    fn external_q27_uncertainty_is_usable_but_managed_w8_wins_fallback_ranking() {
        let host = host_with_vram(24);
        let device = &host.accelerators[0];
        let managed = q27_device_evaluation(
            "linux",
            "x86_64",
            &requirements_for("0.3.1", &Q27_VARIANTS[0]),
            Some(Q27Tier::Qwen36Q4s),
            &host,
            false,
        );
        let external = q27_device_evaluation(
            "linux",
            "x86_64",
            &RuntimeRequirements::default(),
            Some(Q27Tier::Qwen36Q4s),
            &host,
            true,
        );
        let external_with_oversized_model = q27_device_evaluation(
            "linux",
            "x86_64",
            &RuntimeRequirements::default(),
            Some(Q27Tier::Qwen36Q6),
            &host,
            true,
        );
        assert!(matches!(
            managed.compatibility,
            RuntimeCompatibility::Recommended
        ));
        assert!(matches!(
            &external.compatibility,
            RuntimeCompatibility::NeedsAttention(reason)
                if reason.contains("external q27 build variant")
        ));
        assert!(external.compatibility.is_usable());
        assert!(matches!(
            external_with_oversized_model.compatibility,
            RuntimeCompatibility::Incompatible(_)
        ));
        assert!(managed.compatibility.preference_rank() < external.compatibility.preference_rank());
        assert_eq!(external.accelerator.as_ref(), Some(device));
    }

    #[test]
    fn parent_cuda_visibility_is_reconciled_only_by_uuid() {
        let mut host = HostCapabilities {
            platform: "linux".to_owned(),
            architecture: "x86_64".to_owned(),
            accelerators: vec![
                accelerator("GPU-aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa", Some(24)),
                accelerator("GPU-bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb", Some(48)),
            ],
            cuda_visible_devices: Some("GPU-aaaaaaaa".to_owned()),
            observations: Vec::new(),
        };
        let selected = q27_visible_devices(&host).expect("unique UUID prefix");
        assert_eq!(selected.len(), 1);
        assert_eq!(
            selected[0].stable_id.as_deref(),
            Some("GPU-aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa")
        );

        host.cuda_visible_devices = Some("1".to_owned());
        assert!(matches!(
            q27_visible_devices(&host),
            Err(RuntimeCompatibility::Incompatible(reason))
                if reason.contains("numeric index")
        ));
        host.cuda_visible_devices = Some("GPU-aaaaaaaa,GPU-bbbbbbbb".to_owned());
        assert!(matches!(
            q27_visible_devices(&host),
            Err(RuntimeCompatibility::Incompatible(_))
        ));
    }

    #[test]
    fn w16_rejects_24_gib_and_stays_uncertain_on_larger_devices() {
        let w16 = Q27_VARIANTS
            .iter()
            .find(|variant| variant.id == "w16")
            .expect("W16");
        let requirements = requirements_for("0.6.2", w16);
        let host_24 = host_with_vram(24);
        assert!(matches!(
            q27_device_evaluation(
                "linux",
                "x86_64",
                &requirements,
                Some(Q27Tier::Qwen36Q4s),
                &host_24,
                false,
            )
            .compatibility,
            RuntimeCompatibility::Incompatible(_)
        ));
        let host_48 = host_with_vram(48);
        assert!(matches!(
            q27_device_evaluation(
                "linux",
                "x86_64",
                &requirements,
                Some(Q27Tier::Qwen36Q4s),
                &host_48,
                false,
            )
            .compatibility,
            RuntimeCompatibility::NeedsAttention(_)
        ));
    }

    #[test]
    fn available_runtime_model_admission_uses_metadata_and_selected_device() {
        let releases = vec![release(
            6,
            "v0.6.2",
            "2026-07-25T05:30:39Z",
            false,
            vec![asset(
                600,
                "v0.6.2",
                Some("sha256:d0b7bd5abc4c2e84ee8bf5ff3936161a2285dc1d8e3d408f121770838d2c6d11"),
            )],
        )];
        let runtimes = catalog_runtimes(&releases).expect("catalog");
        let w8 = runtimes
            .iter()
            .find(|runtime| runtime.identity.variant == "w8")
            .expect("W8");
        let w12 = runtimes
            .iter()
            .find(|runtime| runtime.identity.variant == "w12")
            .expect("W12");
        let adapter = Q27Adapter::from_config(None, Path::new("."));
        let host_24 = host_with_vram(24);

        let (_q8_dir, q8) = q27_model_fixture("q8-v1", None, Some(".*"));
        assert!(matches!(
            adapter.available_runtime_model_compatibility(w8, &q8, &host_24),
            RuntimeCompatibility::Incompatible(_)
        ));

        let (_q6_dir, q6) =
            q27_model_fixture("q6-v1", None, Some("(ssm_out|attn_output|ffn_down)\\."));
        assert!(matches!(
            adapter.available_runtime_model_compatibility(w8, &q6, &host_24),
            RuntimeCompatibility::Incompatible(_)
        ));

        let (_q4_dir, q4) = q27_model_fixture("q4s-v1", Some(true), None);
        assert!(
            adapter
                .available_runtime_model_compatibility(w8, &q4, &host_24)
                .is_usable()
        );
        assert!(matches!(
            adapter.available_runtime_model_compatibility(w12, &q4, &host_24),
            RuntimeCompatibility::Incompatible(_)
        ));
    }

    #[tokio::test]
    async fn changed_tokenizer_is_rejected_before_launch() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("model.tok");
        fs::write(&path, b"Q27T\x01\0\0\0original").expect("tokenizer");
        let canonical = path.canonicalize().expect("canonical tokenizer");
        let prepared = PreparedAuxiliaryArtifact {
            role: AuxiliaryArtifactRole::Tokenizer,
            path: canonical.clone(),
            size_bytes: fs::metadata(&canonical).expect("metadata").len(),
            content_sha256: hash_file(&canonical).await.expect("hash"),
        };
        assert_eq!(
            revalidate_prepared_tokenizer(&prepared)
                .await
                .expect("unchanged tokenizer"),
            canonical
        );
        fs::write(&prepared.path, b"Q27T\x01\0\0\0changed!").expect("changed tokenizer");
        assert!(
            revalidate_prepared_tokenizer(&prepared)
                .await
                .unwrap_err()
                .to_string()
                .contains("changed")
        );
    }

    #[test]
    fn tokenizer_header_is_format_generic_but_fail_closed() {
        assert!(validate_tokenizer_header(b"Q27T\x01\0\0\0").is_ok());
        assert!(validate_tokenizer_header(b"GGUF\x01\0\0\0").is_err());
        assert!(validate_tokenizer_header(b"Q27T\x02\0\0\0").is_err());
    }

    #[test]
    fn github_timestamp_conversion_has_a_known_epoch() {
        assert_eq!(parse_github_timestamp("1970-01-01T00:00:00Z"), Some(0));
        assert!(parse_github_timestamp("2026-02-29T00:00:00Z").is_none());
    }

    #[test]
    fn chat_translation_owns_sampler_values_and_normalizes_developer_role() {
        let adapter = Q27Adapter::from_config(None, Path::new("."));
        let body = adapter.backend_request(
            &InferenceRequest {
                model_id: ModelId("model".to_owned()),
                messages: vec![InferenceMessage {
                    role: InferenceRole::Developer,
                    text: "instruction".to_owned(),
                }],
                generation_settings: GenerationSettingsPatch::default(),
                max_output_tokens: Some(123),
                stream: false,
            },
            false,
        );
        assert_eq!(body["temperature"], 0.0);
        assert_eq!(body["top_p"], 1.0);
        assert_eq!(body["max_tokens"], 123);
        assert_eq!(body["messages"][0]["role"], "system");
    }

    #[test]
    fn chat_translation_applies_overrides_and_retains_each_omitted_default() {
        let adapter = Q27Adapter::from_config(None, Path::new("."));
        let request = |generation_settings| InferenceRequest {
            model_id: ModelId("model".to_owned()),
            messages: Vec::new(),
            generation_settings,
            max_output_tokens: None,
            stream: false,
        };

        let temperature = adapter.backend_request(
            &request(GenerationSettingsPatch {
                temperature: Some(0.6),
                top_p: None,
            }),
            false,
        );
        assert_eq!(temperature["temperature"], 0.6);
        assert_eq!(temperature["top_p"], 1.0);

        let sampled = adapter.backend_request(
            &request(GenerationSettingsPatch {
                temperature: Some(0.6),
                top_p: Some(0.75),
            }),
            false,
        );
        assert_eq!(sampled["temperature"], 0.6);
        assert_eq!(sampled["top_p"], 0.75);
    }

    #[test]
    fn sampler_validation_requires_sampling_before_restricting_top_p() {
        let adapter = Q27Adapter::from_config(None, Path::new("."));
        let defaults = EffectiveGenerationSettings {
            temperature: 0.0,
            top_p: 1.0,
        };
        for valid in [
            GenerationSettingsPatch::default(),
            GenerationSettingsPatch {
                temperature: Some(0.0),
                top_p: None,
            },
            GenerationSettingsPatch {
                temperature: Some(0.6),
                top_p: None,
            },
            GenerationSettingsPatch {
                temperature: None,
                top_p: Some(1.0),
            },
            GenerationSettingsPatch {
                temperature: Some(0.0),
                top_p: Some(1.0),
            },
            GenerationSettingsPatch {
                temperature: Some(0.6),
                top_p: Some(0.75),
            },
        ] {
            adapter
                .validate_generation_settings(&valid, &defaults)
                .expect("q27 can honor sampler patch");
        }

        for ignored_top_p in [
            GenerationSettingsPatch {
                temperature: None,
                top_p: Some(0.75),
            },
            GenerationSettingsPatch {
                temperature: Some(0.0),
                top_p: Some(0.75),
            },
        ] {
            assert!(matches!(
                adapter.validate_generation_settings(&ignored_top_p, &defaults),
                Err(EngineError::InvalidGenerationSettings(_))
            ));
        }

        for invalid in [
            GenerationSettingsPatch {
                temperature: Some(-0.1),
                top_p: None,
            },
            GenerationSettingsPatch {
                temperature: Some(2.1),
                top_p: None,
            },
            GenerationSettingsPatch {
                temperature: None,
                top_p: Some(0.0),
            },
            GenerationSettingsPatch {
                temperature: None,
                top_p: Some(1.1),
            },
            GenerationSettingsPatch {
                temperature: Some(f64::INFINITY),
                top_p: None,
            },
        ] {
            assert!(matches!(
                adapter.validate_generation_settings(&invalid, &defaults),
                Err(EngineError::InvalidGenerationSettings(_))
            ));
        }
    }

    #[test]
    fn accepted_sampler_patch_matches_q27_execution_and_public_effective_values() {
        let adapter = Q27Adapter::from_config(None, Path::new("."));
        let defaults = EffectiveGenerationSettings {
            temperature: 0.0,
            top_p: 1.0,
        };
        let request = |generation_settings| InferenceRequest {
            model_id: ModelId("model".to_owned()),
            messages: Vec::new(),
            generation_settings,
            max_output_tokens: None,
            stream: false,
        };

        for patch in [
            GenerationSettingsPatch::default(),
            GenerationSettingsPatch {
                temperature: None,
                top_p: Some(1.0),
            },
            GenerationSettingsPatch {
                temperature: Some(0.6),
                top_p: Some(0.75),
            },
        ] {
            adapter
                .validate_generation_settings(&patch, &defaults)
                .expect("q27 can honor sampler patch");
            let effective = defaults.merged(&patch);
            let body = adapter.backend_request(&request(patch), false);
            assert_eq!(body["temperature"], effective.temperature);
            assert_eq!(body["top_p"], effective.top_p);
        }
    }

    #[tokio::test]
    async fn chat_sse_translation_emits_text_usage_and_length_completion() {
        let bytes = Bytes::from_static(
            b"data: {\"choices\":[{\"delta\":{\"content\":\"hello\"},\"finish_reason\":null}]}\n\n\
              data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"length\"}]}\n\n\
              data: {\"choices\":[],\"usage\":{\"prompt_tokens\":4,\"completion_tokens\":2,\"total_tokens\":6}}\n\n\
              data: [DONE]\n\n",
        );
        let source = stream::iter(vec![Ok::<Bytes, reqwest::Error>(bytes)]).boxed();
        let mut translated = q27_sse_stream(source, false);
        let first = translated.next().await.expect("text event").expect("text");
        assert!(matches!(
            first,
            InferenceEvent::TextDelta { ref delta } if delta == "hello"
        ));
        let completed = translated
            .next()
            .await
            .expect("completion event")
            .expect("completion");
        match completed {
            InferenceEvent::Completed {
                usage: Some(usage),
                finish_reason,
            } => {
                assert_eq!(usage.input_tokens, 4);
                assert_eq!(usage.output_tokens, 2);
                assert_eq!(usage.total_tokens, 6);
                assert_eq!(finish_reason, InferenceFinishReason::MaxOutputTokens);
            }
            _ => panic!("unexpected completion event"),
        }
        assert!(translated.next().await.is_none());
    }

    #[tokio::test]
    async fn chat_sse_rejects_done_without_a_terminal_finish_reason() {
        let bytes = Bytes::from_static(
            b"data: {\"choices\":[{\"delta\":{\"content\":\"hello\"},\"finish_reason\":null}]}\n\n\
              data: [DONE]\n\n",
        );
        let source = stream::iter(vec![Ok::<Bytes, reqwest::Error>(bytes)]).boxed();
        let mut translated = q27_sse_stream(source, false);
        assert!(matches!(
            translated.next().await.expect("text event"),
            Ok(InferenceEvent::TextDelta { ref delta }) if delta == "hello"
        ));
        assert!(matches!(
            translated.next().await.expect("terminal error"),
            Err(EngineError::BackendUnavailable(message)) if message.contains("finish reason")
        ));
        assert!(translated.next().await.is_none());
    }

    #[test]
    fn package_non_stream_output_suppresses_reasoning_and_control_transition() {
        assert_eq!(
            filter_q27_package_output(
                "private chain of thought\nmore private\n</think>\nPublic answer"
            ),
            "\nPublic answer"
        );
        assert_eq!(filter_q27_package_output("reasoning only"), "");
        assert_eq!(
            filter_q27_package_output("private</think>answer<think>"),
            "answer"
        );
    }

    #[tokio::test]
    async fn package_stream_filter_handles_every_reasoning_close_boundary() {
        for boundary in 0..=THINK_CLOSE.len() {
            let first = format!("private{}", &THINK_CLOSE[..boundary]);
            let second = format!("{}answer", &THINK_CLOSE[boundary..]);
            let payload = format!(
                "data: {}\n\ndata: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
                json!({"choices": [{"text": first, "finish_reason": null}]}),
                json!({"choices": [{"text": second, "finish_reason": null}]}),
                json!({"choices": [{"text": "", "finish_reason": "stop"}], "usage": {"prompt_tokens": 4, "completion_tokens": 3, "total_tokens": 7}}),
            );
            let source =
                stream::iter(vec![Ok::<Bytes, reqwest::Error>(Bytes::from(payload))]).boxed();
            let events = q27_sse_stream(source, true)
                .collect::<Vec<_>>()
                .await
                .into_iter()
                .collect::<Result<Vec<_>, _>>()
                .expect("filtered events");
            assert!(matches!(
                events.as_slice(),
                [InferenceEvent::TextDelta { delta }, InferenceEvent::Completed { usage: Some(usage), finish_reason: InferenceFinishReason::Stop }]
                    if delta == "answer"
                        && usage.input_tokens == 4
                        && usage.output_tokens == 3
                        && usage.total_tokens == 7
            ));
        }
    }

    #[tokio::test]
    async fn package_reasoning_only_length_stream_emits_no_public_text() {
        let payload = concat!(
            "data: {\"choices\":[{\"text\":\"private reasoning\",\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"text\":\"\",\"finish_reason\":\"length\"}],\"usage\":{\"prompt_tokens\":8,\"completion_tokens\":16,\"total_tokens\":24}}\n\n",
            "data: [DONE]\n\n"
        );
        let source = stream::iter(vec![Ok::<Bytes, reqwest::Error>(Bytes::from_static(
            payload.as_bytes(),
        ))])
        .boxed();
        let events = q27_sse_stream(source, true)
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .expect("filtered events");
        assert!(matches!(
            events.as_slice(),
            [InferenceEvent::Completed { usage: Some(usage), finish_reason: InferenceFinishReason::MaxOutputTokens }]
                if usage.input_tokens == 8
                    && usage.output_tokens == 16
                    && usage.total_tokens == 24
        ));
    }

    #[tokio::test]
    async fn package_stream_emits_normal_answer_deltas_only_after_close() {
        let payload = concat!(
            "data: {\"choices\":[{\"text\":\"private</thi\",\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"text\":\"nk>first \" ,\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"text\":\"second\",\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"text\":\"\",\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\n\n"
        );
        let source = stream::iter(vec![Ok::<Bytes, reqwest::Error>(Bytes::from_static(
            payload.as_bytes(),
        ))])
        .boxed();
        let events = q27_sse_stream(source, true)
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .expect("filtered events");
        assert!(matches!(
            events.as_slice(),
            [InferenceEvent::TextDelta { delta: first }, InferenceEvent::TextDelta { delta: second }, InferenceEvent::Completed { finish_reason: InferenceFinishReason::Stop, .. }]
                if first == "first " && second == "second"
        ));
    }

    #[tokio::test]
    async fn raw_q27_stream_keeps_reasoning_like_text_unchanged() {
        let payload = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"<think>raw</think>answer\"},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\n\n"
        );
        let source = stream::iter(vec![Ok::<Bytes, reqwest::Error>(Bytes::from_static(
            payload.as_bytes(),
        ))])
        .boxed();
        let events = q27_sse_stream(source, false)
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .expect("raw events");
        assert!(matches!(
            events.first(),
            Some(InferenceEvent::TextDelta { delta }) if delta == "<think>raw</think>answer"
        ));
    }

    fn model_artifact(path: PathBuf) -> ModelArtifact {
        ModelArtifact {
            id: ModelId("model".to_owned()),
            display_name: "model".to_owned(),
            path,
            format: ArtifactFormat::Q27,
            size_bytes: 1,
            created: 0,
            hash: None,
            architecture: None,
            context_length: None,
            provenance: None,
            native_identity: None,
            auxiliary_artifacts: Vec::<AuxiliaryArtifact>::new(),
            norted_package: None,
        }
    }

    fn q27_model_fixture(
        quant_policy: &str,
        q4_head: Option<bool>,
        q8_extra: Option<&str>,
    ) -> (tempfile::TempDir, ModelArtifact) {
        let directory = tempfile::tempdir().expect("temporary directory");
        let model_path = directory.path().join("model.q27");
        let tokenizer_path = directory.path().join("model.tok");
        let mut metadata = json!({
            "general.architecture": "qwen35",
            "qwen35.block_count": 65,
            "qwen35.nextn_predict_layers": 1,
            "qwen35.embedding_length": 5120,
            "qwen35.feed_forward_length": 17408,
            "qwen35.attention.head_count": 24,
            "qwen35.attention.head_count_kv": 4,
            "qwen35.attention.key_length": 256,
            "qwen35.attention.value_length": 256,
            "qwen35.rope.dimension_count": 64,
            "qwen35.ssm.state_size": 128,
            "qwen35.ssm.group_count": 16,
            "qwen35.ssm.inner_size": 6144,
            "qwen35.ssm.time_step_rank": 48,
            "qwen35.ssm.conv_kernel": 4,
            "qwen35.attention.layer_norm_rms_epsilon": 0.000001,
            "qwen35.rope.freq_base": 10000000.0,
            "group_q4": 64,
            "group_q8": 128,
            "nibble_order": "even=low",
            "quant_policy": quant_policy,
        });
        let object = metadata.as_object_mut().expect("metadata object");
        if let Some(q4_head) = q4_head {
            object.insert("q4_head".to_owned(), json!(q4_head));
        }
        if let Some(q8_extra) = q8_extra {
            object.insert("q8_extra".to_owned(), json!(q8_extra));
        }
        let encoded = serde_json::to_vec(&metadata).expect("metadata JSON");
        let mut bytes = Vec::with_capacity(16 + encoded.len());
        bytes.extend_from_slice(b"Q27F");
        bytes.extend_from_slice(&1_u32.to_le_bytes());
        bytes.extend_from_slice(&0_u32.to_le_bytes());
        bytes.extend_from_slice(&(encoded.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&encoded);
        fs::write(&model_path, bytes).expect("model fixture");
        fs::write(&tokenizer_path, b"Q27T\x01\0\0\0").expect("tokenizer fixture");
        let mut model = model_artifact(model_path);
        model.auxiliary_artifacts.push(AuxiliaryArtifact {
            role: AuxiliaryArtifactRole::Tokenizer,
            path: tokenizer_path,
            size_bytes: 8,
            hash: None,
        });
        (directory, model)
    }
}
