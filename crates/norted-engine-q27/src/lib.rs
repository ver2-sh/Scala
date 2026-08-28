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
use norted_core::{
    AcquisitionMethod, ArtifactFormat, AuxiliaryArtifactRole, AvailableRuntime, EngineConfig,
    EngineInstallation, EngineRevision, InstalledRuntime, ModelArtifact, RuntimeAcquisitionMethod,
    RuntimeArchiveFormat, RuntimeDigest, RuntimeDownload, RuntimeId, RuntimeIdentity,
    RuntimePackageIdentity, RuntimeProbeObservation, RuntimeReleaseChannel, RuntimeRequirements,
};
use norted_engine::{
    ApiCapability, CatalogError, CompatibilityDecision, EffectiveGenerationSettings, EngineAdapter,
    EngineCapabilities, EngineError, EngineFeature, EngineIdentity, EngineProbe, GitHubRelease,
    GitHubReleaseAsset, GitHubReleaseClient, InferenceEvent, InferenceFinishReason,
    InferenceMessage, InferenceOutput, InferenceRequest, InferenceRole, InferenceStream,
    InferenceUsage, InstallationState, LaunchRequest, LaunchSpec, NativeOption, OptionValueKind,
    ProcessDescriptor, RuntimeCatalogProvider, UpdateState, capture_command,
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

const PROBE_TIMEOUT: Duration = Duration::from_secs(15);
const HEALTH_TIMEOUT: Duration = Duration::from_secs(2);
const INFERENCE_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const SSE_FRAME_LIMIT: usize = 1024 * 1024;
const GIB: u64 = 1024 * 1024 * 1024;

// These values can change authentication, sampling, or prompt semantics behind
// Norted's back. They are removed from the inherited environment and rejected
// in the adapter-specific environment map.
const MANAGED_ENVIRONMENT_VARIABLES: &[&str] = &[
    "Q27_API_KEY",
    "Q27_FORCE_TEMP",
    "Q27_FORCE_TOP_P",
    "Q27_SAMPLED",
    "Q27_BARE",
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
                download: RuntimeDownload {
                    url: qualified.asset.browser_download_url.clone(),
                    size_bytes: qualified.asset.size,
                    digest: Some(qualified.digest.clone()),
                    archive_format: RuntimeArchiveFormat::TarGz,
                    entrypoint_names: vec![variant.entrypoint.to_owned()],
                },
                additional_downloads: Vec::new(),
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
    minimum_vram_bytes: u64,
}

const Q27_VARIANTS: &[RuntimeVariant] = &[
    RuntimeVariant {
        id: "w8",
        label: "W8",
        entrypoint: "q27-server-w8",
        minimum_vram_bytes: 24 * GIB,
    },
    RuntimeVariant {
        id: "w12",
        label: "W12",
        entrypoint: "q27-server",
        minimum_vram_bytes: 32 * GIB,
    },
    RuntimeVariant {
        id: "w16",
        label: "W16",
        entrypoint: "q27-server-w16",
        // Upstream publishes no separate W16 memory floor. Preserve q27's
        // general 24 GiB minimum and describe this specialist build below.
        minimum_vram_bytes: 24 * GIB,
    },
];

fn variants_for(_version: &str) -> &'static [RuntimeVariant] {
    Q27_VARIANTS
}

fn requirements_for(version: &str, variant: &RuntimeVariant) -> RuntimeRequirements {
    let tri_arch = version_at_least(version, 0, 3, 1);
    let architecture_note = if tri_arch {
        "Official fat binary targets NVIDIA compute capabilities 8.6, 8.9, and 12.0".to_owned()
    } else if version == "0.3.0" {
        "Official fat binary targets NVIDIA compute capabilities 8.6 and 12.0; v0.3.0 does not contain an Ada/8.9 target"
            .to_owned()
    } else {
        "Official fat binary targets NVIDIA compute capabilities 8.6 and 12.0".to_owned()
    };
    let mut notes = vec![
        architecture_note,
        match variant.id {
            "w8" => "W8 is the q27 build intended for 24 GiB cards".to_owned(),
            "w12" => {
                "W12 is q27's default server build and needs a 32 GiB-class card"
                    .to_owned()
            }
            _ => "W16 is a specialist repetition-heavy/file-re-emission build, not q27's recommended live-traffic default; upstream publishes no separate W16 VRAM floor"
                .to_owned(),
        },
        "Model tier also matters: q6/q6f/q6k need 32 GiB and q8 needs 48 GiB"
            .to_owned(),
    ];
    if tri_arch {
        notes.push(
            "Prebuilt binaries statically link CUDA 13.2; q27 documents NVIDIA driver branch r580 or newer"
                .to_owned(),
        );
    }
    if version == "0.6.2" {
        notes.push(
            "The v0.6.2 ELF requires glibc 2.38 and libstdc++ with GLIBCXX_3.4.32".to_owned(),
        );
    }
    RuntimeRequirements {
        requires_nvidia_gpu: true,
        minimum_nvidia_driver: tri_arch.then(|| "580".to_owned()),
        minimum_vram_bytes: Some(variant.minimum_vram_bytes),
        notes,
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
            "temperature": 0.0,
            "top_p": 1.0,
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
        match tokenizer_candidate(model) {
            Ok(path) => match validate_tokenizer_sync(&path) {
                Ok(()) => CompatibilityDecision::Supported,
                Err(reason) => CompatibilityDecision::Unsupported { reason },
            },
            Err(reason) => CompatibilityDecision::Unsupported { reason },
        }
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
        if !request.backend_address.ip().is_loopback() {
            return Err(EngineError::InvalidConfiguration(
                "q27 backend address must be loopback".to_owned(),
            ));
        }
        if request.model.format != ArtifactFormat::Q27 {
            return Err(EngineError::InvalidConfiguration(
                "q27 can only launch Q27 model artifacts".to_owned(),
            ));
        }
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
        let model_path = canonical_regular_file(&request.model.path, "q27 model").await?;
        let tokenizer_path = resolve_tokenizer(&request.model).await?;
        let observation = self.probe_runtime(&request.runtime).await?;
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
        let arguments = vec![
            model_path.as_os_str().to_owned(),
            tokenizer_path.as_os_str().to_owned(),
            OsString::from("--host"),
            OsString::from(request.backend_address.ip().to_string()),
            OsString::from("--port"),
            OsString::from(request.backend_address.port().to_string()),
            OsString::from("--no-think"),
        ]
        .into_iter()
        .chain(self.native_arguments.iter().map(OsString::from))
        .collect();
        Ok(LaunchSpec {
            executable: binary_path,
            arguments,
            environment: self.environment.clone(),
            environment_remove: managed_environment_removals(),
            inherits_parent_environment: true,
            working_directory: None,
            endpoint: Some(http_endpoint(request.backend_address)),
            normalized_settings: BTreeMap::from([
                ("temperature".to_owned(), json!(0.0)),
                ("top_p".to_owned(), json!(1.0)),
                ("thinking".to_owned(), json!(false)),
            ]),
            native_arguments: self.native_arguments.clone(),
            installation,
            runtime: request.runtime,
        })
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

    async fn effective_generation_settings(
        &self,
        _process: &ProcessDescriptor,
    ) -> Result<EffectiveGenerationSettings, EngineError> {
        // q27 exposes no effective-config endpoint. These values are owned by
        // this adapter, sent explicitly on every request, and force env vars
        // are removed from the child environment.
        Ok(EffectiveGenerationSettings {
            temperature: 0.0,
            top_p: 1.0,
        })
    }

    async fn infer(
        &self,
        endpoint: &str,
        request: InferenceRequest,
    ) -> Result<InferenceOutput, EngineError> {
        let response = self
            .client
            .post(format!("{endpoint}/v1/chat/completions"))
            .timeout(INFERENCE_TIMEOUT)
            .json(&self.backend_request(&request, false))
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
        let text = choice.message.content.ok_or_else(|| {
            EngineError::Operation("q27 response contained no assistant text".to_owned())
        })?;
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
        let response = self
            .client
            .post(format!("{endpoint}/v1/chat/completions"))
            .timeout(INFERENCE_TIMEOUT)
            .json(&self.backend_request(&request, true))
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
        Ok(q27_sse_stream(response.bytes_stream().boxed()))
    }
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

async fn resolve_tokenizer(model: &ModelArtifact) -> Result<PathBuf, EngineError> {
    let candidate = tokenizer_candidate(model).map_err(EngineError::InvalidConfiguration)?;
    let tokenizer = canonical_regular_file(&candidate, "q27 tokenizer companion").await?;
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
    validate_tokenizer(&tokenizer).await?;
    Ok(tokenizer)
}

fn tokenizer_candidate(model: &ModelArtifact) -> Result<PathBuf, String> {
    let declared = model
        .auxiliary_artifacts
        .iter()
        .filter(|artifact| artifact.role == AuxiliaryArtifactRole::Tokenizer)
        .collect::<Vec<_>>();
    match declared.as_slice() {
        [tokenizer] => return Ok(tokenizer.path.clone()),
        [] => {}
        _ => {
            return Err(format!(
                "q27 model {} has multiple tokenizer companions; exactly one is required",
                model.path.display()
            ));
        }
    }
    discover_tokenizer_beside(&model.path)
}

fn discover_tokenizer_beside(model_path: &Path) -> Result<PathBuf, String> {
    let exact = model_path.with_extension("tok");
    if exact.is_file() {
        return Ok(exact);
    }
    let model_stem = model_path
        .file_stem()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            format!(
                "q27 model path has no UTF-8 file stem: {}",
                model_path.display()
            )
        })?;
    let parent = model_path.parent().ok_or_else(|| {
        format!(
            "q27 model path has no parent directory: {}",
            model_path.display()
        )
    })?;
    let entries = std::fs::read_dir(parent).map_err(|error| {
        format!(
            "could not inspect tokenizer companions beside {}: {error}",
            model_path.display()
        )
    })?;
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
    candidates.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.cmp(&right.1)));
    let Some(longest) = candidates.first().map(|candidate| candidate.0) else {
        return Err(format!(
            "q27 model {} requires a companion `.tok` tokenizer",
            model_path.display()
        ));
    };
    let mut longest_candidates = candidates
        .into_iter()
        .take_while(|candidate| candidate.0 == longest)
        .map(|candidate| candidate.1);
    let candidate = longest_candidates.next().expect("candidate was present");
    if longest_candidates.next().is_some() {
        return Err(format!(
            "q27 tokenizer companion is ambiguous for {}",
            model_path.display()
        ));
    }
    Ok(candidate)
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
    message: ChatMessage,
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

struct SseState {
    source: BoxStream<'static, Result<Bytes, reqwest::Error>>,
    buffer: Vec<u8>,
    queued: VecDeque<Result<InferenceEvent, EngineError>>,
    usage: Option<InferenceUsage>,
    finish_reason: Option<InferenceFinishReason>,
    finished: bool,
}

fn q27_sse_stream(source: BoxStream<'static, Result<Bytes, reqwest::Error>>) -> InferenceStream {
    let state = SseState {
        source,
        buffer: Vec::new(),
        queued: VecDeque::new(),
        usage: None,
        finish_reason: None,
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
            .and_then(|choice| choice.get("delta"))
            .and_then(|delta| delta.get("content"))
            .and_then(Value::as_str)
            .filter(|delta| !delta.is_empty())
        {
            state.queued.push_back(Ok(InferenceEvent::TextDelta {
                delta: delta.to_owned(),
            }));
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

    use norted_core::{ArtifactFormat, AuxiliaryArtifact, ModelArtifact, ModelId};

    use super::*;

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
            runtime.identity.version == "0.6.2"
                && runtime.channels.contains(&RuntimeReleaseChannel::Stable)
                && runtime.channels.contains(&RuntimeReleaseChannel::Latest)
                && runtime.download.archive_format == RuntimeArchiveFormat::TarGz
                && runtime.download.digest.is_some()
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
        assert_eq!(w8.requirements.minimum_vram_bytes, Some(24 * GIB));
        assert_eq!(w12.requirements.minimum_vram_bytes, Some(32 * GIB));
        assert_eq!(
            w8.requirements.minimum_nvidia_driver.as_deref(),
            Some("580")
        );
        assert!(w16.requirements.notes.iter().any(|note| {
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
        for environment in ["Q27_API_KEY", "q27_force_temp", "Q27_BARE"] {
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
    fn generic_quant_suffix_uses_the_longest_unambiguous_tokenizer_stem() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let model_path = directory.path().join("example-family-q4s.q27");
        fs::write(&model_path, b"model").expect("model");
        fs::write(directory.path().join("example.tok"), b"Q27T\x01\0\0\0")
            .expect("short tokenizer");
        let expected = directory.path().join("example-family.tok");
        fs::write(&expected, b"Q27T\x01\0\0\0").expect("tokenizer");
        let model = model_artifact(model_path);
        assert_eq!(tokenizer_candidate(&model).expect("candidate"), expected);
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

    #[tokio::test]
    async fn chat_sse_translation_emits_text_usage_and_length_completion() {
        let bytes = Bytes::from_static(
            b"data: {\"choices\":[{\"delta\":{\"content\":\"hello\"},\"finish_reason\":null}]}\n\n\
              data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"length\"}]}\n\n\
              data: {\"choices\":[],\"usage\":{\"prompt_tokens\":4,\"completion_tokens\":2,\"total_tokens\":6}}\n\n\
              data: [DONE]\n\n",
        );
        let source = stream::iter(vec![Ok::<Bytes, reqwest::Error>(bytes)]).boxed();
        let mut translated = q27_sse_stream(source);
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
        let mut translated = q27_sse_stream(source);
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
            auxiliary_artifacts: Vec::<AuxiliaryArtifact>::new(),
        }
    }
}
