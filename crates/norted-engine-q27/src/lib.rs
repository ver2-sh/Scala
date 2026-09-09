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
    AcceleratorBinding, AcceleratorDevice, AcquisitionMethod, ArtifactFormat,
    AuxiliaryArtifactRole, AvailableRuntime, ComputeCapability, EngineConfig, EngineInstallation,
    EngineRevision, HostCapabilities, InstalledRuntime, ModelArtifact, RuntimeAcquisitionMethod,
    RuntimeAcquisitionPlan, RuntimeArchiveFormat, RuntimeCompatibility, RuntimeDigest,
    RuntimeDownload, RuntimeId, RuntimeIdentity, RuntimePackageIdentity, RuntimeProbeObservation,
    RuntimeReleaseChannel, RuntimeRequirements, RuntimeSourceBuildPlan,
    RuntimeSourceBuildPrerequisites, RuntimeSourceBuildProvenance, RuntimeSourceBuildRecipe,
    RuntimeSourceBuildSystem, RuntimeSourceSnapshot, SettingCategory, SettingDefaultPreview,
    SettingDefaultSource, SettingDefinition, SettingId, SettingKind, SettingScope, SettingValue,
    SettingsSchema,
};
use norted_engine::{
    ApiCapability, BackendLoadPhase, BackendLoadProgress, CatalogError, CompatibilityDecision,
    EffectiveGenerationSettings, EngineAdapter, EngineCapabilities, EngineError, EngineFeature,
    EngineIdentity, EngineProbe, GenerationSettingsPatch, GitHubCommit, GitHubRelease,
    GitHubReleaseAsset, GitHubReleaseClient, InferenceActivityReporter, InferenceEvent,
    InferenceFinishReason, InferenceMessage, InferenceOutput, InferenceRequest, InferenceRole,
    InferenceStream, InferenceToolCall, InferenceToolChoice, InferenceUsage, InstallationState,
    LaunchRequest, LaunchSpec, LoadProgressReporter, NativeOption, OutputFormat,
    PreparedAuxiliaryArtifact, PreparedModelInput, ProcessDescriptor, RuntimeCatalogProvider,
    StartupObservation, UpdateState, capture_command, common_setting_definitions_for,
    compatibility_for, compatibility_for_nvidia_device, isolated_cuda_environment,
    prepare_norted_package_input, prepare_norted_package_input_with_progress,
    revalidate_norted_package_before_launch, revalidate_norted_package_before_launch_with_progress,
    visible_nvidia_devices,
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
pub const SOURCE_PACKAGE_FAMILY: &str = "q27-official-source";
pub const SOURCE_RECIPE_VERSION: &str = "q27-upstream-make-v2-4770e05";
const SOURCE_RUNTIME_ONLY_RECIPE_VERSION: &str = "q27-upstream-make-v2-runtime-only";

// Provider-reviewed immutable source contract for q27 v0.10.0. Discovery
// re-reads the bounded files from this exact commit, installation rechecks the
// exact commit/tree and Makefile digest, and the resulting facts persist in
// RuntimeSourceBuildProvenance. A new upstream tree must receive a new audited
// contract before it can gain exact-runtime capabilities.
// Grep audit: this tree's api_common.h + tools/test_template_golden.cpp bind
// Qwen3.8 native tool rendering. src/toolconstrain.h/toolgram.h only constrain
// tool bodies after their opener, not a no-tools ranges response. server.cu
// has no response_format/schema/grammar request route. See docs/q27-grep-contract.md.
// StructuredOutput must remain absent even for this exact reviewed tree.
const PACKAGE_SOURCE_COMMIT: &str = "4770e053656af9aababdc49c81f280ad21b74986";
const PACKAGE_SOURCE_TREE: &str = "ff712f78fd17b5fe12149679114b6def003f16a6";
const PACKAGE_MAKEFILE_SHA256: &str =
    "f68397c2fdedc6e28ec22b0d50b04d9e4dc0fa7b569fa38366e63f742b250dc2";
const PACKAGE_README_SHA256: &str =
    "7f5a25c3a87ef49c152ef2304e6afc905477c61053e3b46a71057bc131a50bdc";
const PACKAGE_SERVER_SHA256: &str =
    "a49aa5780e3b54ef97246c565799a68b043e1907dbd6427fb4b5d12d0ff90ee2";
const PACKAGE_ENGINE_SHA256: &str =
    "5005f5926f24855b31c3bb3d9d5adf9e211a87c201d833dd54a194074e493aec";

const SOURCE_METADATA_LIMIT: usize = 512 * 1024;

mod model;

use model::{Q27Tier, inspect_q27_model};

/// Performs the adapter-owned bounded Q27 architecture and capability
/// inspection used before admitting an artifact to the managed model library.
pub fn validate_model_artifact(path: &Path) -> Result<(), String> {
    inspect_q27_model(path).map(|_| ())
}

const PROBE_TIMEOUT: Duration = Duration::from_secs(15);
const HEALTH_TIMEOUT: Duration = Duration::from_secs(2);
const INFERENCE_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const SSE_FRAME_LIMIT: usize = 1024 * 1024;
const SHARP_TEMPLATE_LIMIT: u64 = 4 * 1024 * 1024;

// These values can change authentication, sampling, prompt semantics, or the
// final context/slot contract behind Norted's back. They are removed from the
// inherited environment and rejected in the adapter-specific environment map.
const MANAGED_ENVIRONMENT_VARIABLES: &[&str] = &[
    "Q27_API_KEY",
    "Q27_FORCE_TEMP",
    "Q27_FORCE_TOP_P",
    "Q27_BATCH",
    "Q27_SAMPLED",
    "Q27_BARE",
    "Q27_KV",
    "Q27_MAXD",
    "Q27_PMIN",
    "Q27_SUFFIX",
    "Q27_SUFFIX_W",
    "Q27_PROFILE",
    "Q27_KV_POOL",
    "Q27_PF_ARENA",
    "Q27_BATCH_GRAPH",
    "Q27_BATCH_GRAPH_CAP",
    "Q27_BATCH_GEMM",
    "Q27_DEXIT",
    "Q27_SAMPLE_PLAIN",
    "Q27_TOOL_SPLIT",
    "Q27_NO_INTERLEAVE",
    "Q27_PHASE_STATS",
    "Q27_READY_FLOOR_MB",
    "Q27_THINK_BUDGET_FRAC",
    "Q27_REASONING_EFFORT",
    "Q27_SUFFIX_L",
    "Q27_PF_BATCH_MIN",
    "Q27_GEMM_MIN",
    "Q27_GEMM_SPLITK",
    "Q27_PREFILL",
    "Q27_PF_XG",
    "Q27_PF_NT",
    "Q27_FD",
    "Q27_DS_MODE",
    "Q27_DS_SPLIT",
    "Q27_CKPT_INTERVAL",
    "Q27_CKPT_SLOTS",
    "Q27_MAXD_RESET",
    "Q27_MAXD_EMA",
    "Q27_MAXD_HI",
    "Q27_MAXD_HI6",
    "Q27_MAXD_HI7",
    "Q27_MAXD_FLO6",
    "Q27_MAXD_FLO7",
    "Q27_MAXD_LO",
    "Q27_TOOL_DIALECT",
    "Q27_TOOL_STRICT",
    "Q27_TOOL_ERROR_WARNINGS",
    "Q27_MAX_TOOL_ARG_CHARS",
    "Q27_MAX_TOOL_RESPONSE_CHARS",
    "CUDA_VISIBLE_DEVICES",
];

// q27 parses MODEL and TOKENIZER positionally. Norted owns those arguments,
// its private bind, authentication state, thinking mode, sampler defaults, and
// the structured context/slot process contract.
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
    "--ctx",
    "--slots",
    "--slot1-ctx",
];

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
        fetch_catalog_runtimes(github, &releases).await
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
        let mut runtimes = fetch_catalog_runtimes(github, &[release]).await?;
        for runtime in &mut runtimes {
            runtime
                .channels
                .retain(|channel| matches!(channel, RuntimeReleaseChannel::Prerelease));
        }
        Ok(runtimes)
    }

    async fn verify_candidate(
        &self,
        github: &GitHubReleaseClient,
        candidate: &AvailableRuntime,
    ) -> Result<Option<AvailableRuntime>, CatalogError> {
        if candidate.identity.engine_id != ENGINE_ID
            || candidate.identity.package.provider_id != PROVIDER_ID
            || candidate.identity.package.repository.as_deref() != Some(GITHUB_REPOSITORY)
        {
            return Ok(None);
        }
        let Some(tag) = candidate.identity.package.release_tag.as_deref() else {
            return Ok(None);
        };
        let Some(release) = github.release_by_tag(GITHUB_REPOSITORY, tag).await? else {
            return Ok(None);
        };
        let live = fetch_catalog_runtimes(github, &[release])
            .await?
            .into_iter()
            .find(|runtime| runtime.runtime_id == candidate.runtime_id);
        Ok(live)
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

struct QualifiedRelease<'a> {
    release: &'a GitHubRelease,
    version: String,
    binary: Option<(&'a GitHubReleaseAsset, RuntimeDigest)>,
    source: Option<Q27SourceCapability>,
    published_at_unix: Option<i64>,
    ordinal: usize,
}

#[derive(Clone)]
struct Q27SourceCapability {
    commit: GitHubCommit,
    makefile_sha256: String,
    package_contract: bool,
    minimum_cuda_version: String,
    supported_variant_ids: Vec<&'static str>,
    supported_cuda_compute_capabilities: Vec<ComputeCapability>,
    accelerator_target: String,
}

async fn fetch_catalog_runtimes(
    github: &GitHubReleaseClient,
    releases: &[GitHubRelease],
) -> Result<Vec<AvailableRuntime>, CatalogError> {
    let mut qualified = Vec::new();
    for (ordinal, release) in releases.iter().enumerate() {
        if release.draft {
            continue;
        }
        let Some(version) = release_version(release) else {
            continue;
        };
        let binary = release.assets.iter().find_map(|asset| {
            qualify_release_asset(release, asset)
                .map(|(asset_version, digest)| (asset, asset_version, digest))
        });
        let (version, binary, source) = if let Some((asset, asset_version, digest)) = binary {
            (asset_version, Some((asset, digest)), None)
        } else {
            let source = inspect_source_capability(github, release).await?;
            (version, None, source)
        };
        if binary.is_some() || source.is_some() {
            qualified.push(QualifiedRelease {
                release,
                version,
                binary,
                source,
                published_at_unix: release
                    .published_at
                    .as_deref()
                    .and_then(parse_github_timestamp),
                ordinal,
            });
        }
    }

    materialize_qualified_releases(qualified)
}

fn materialize_qualified_releases(
    qualified: Vec<QualifiedRelease<'_>>,
) -> Result<Vec<AvailableRuntime>, CatalogError> {
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
            if qualified.binary.is_none()
                && !qualified
                    .source
                    .as_ref()
                    .is_some_and(|source| source.supported_variant_ids.contains(&variant.id))
            {
                continue;
            }
            let source_build = qualified.source.as_ref();
            let source_recipe_version = source_build.map(|source| {
                if source.package_contract {
                    SOURCE_RECIPE_VERSION
                } else {
                    SOURCE_RUNTIME_ONLY_RECIPE_VERSION
                }
            });
            let identity = RuntimeIdentity {
                engine_id: ENGINE_ID.to_owned(),
                package_family: if source_build.is_some() {
                    format!(
                        "{SOURCE_PACKAGE_FAMILY}-{}",
                        source_recipe_version.expect("source recipe version")
                    )
                } else {
                    PACKAGE_FAMILY.to_owned()
                },
                version: qualified.version.clone(),
                upstream_revision: source_build
                    .map(|source| source.commit.sha.clone())
                    .or_else(|| exact_commit(&qualified.release.target_commitish)),
                platform: "linux".to_owned(),
                architecture: "x86_64".to_owned(),
                accelerator: "cuda".to_owned(),
                variant: variant.id.to_owned(),
                package: RuntimePackageIdentity {
                    provider_id: PROVIDER_ID.to_owned(),
                    repository: Some(GITHUB_REPOSITORY.to_owned()),
                    release_tag: Some(qualified.release.tag_name.clone()),
                    asset_id: qualified
                        .binary
                        .as_ref()
                        .map(|(asset, _)| asset.id.to_string()),
                    asset_name: qualified
                        .binary
                        .as_ref()
                        .map(|(asset, _)| asset.name.clone()),
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
                    "q27 {} Linux x86_64 CUDA {} {}",
                    qualified.version,
                    variant.label,
                    if source_build.is_some() {
                        "source build"
                    } else {
                        "upstream binary"
                    }
                ),
                supported_formats: vec![ArtifactFormat::Q27],
                source_url: source_build.map_or_else(
                    || qualified.release.html_url.clone(),
                    |source| source.commit.html_url.clone(),
                ),
                published_at_unix: qualified.published_at_unix,
                channels,
                prerelease: qualified.release.prerelease,
                acquisition: if let Some((asset, digest)) = &qualified.binary {
                    RuntimeAcquisitionPlan::ReleaseAsset {
                        download: RuntimeDownload {
                            url: asset.browser_download_url.clone(),
                            size_bytes: asset.size,
                            digest: Some(digest.clone()),
                            archive_format: RuntimeArchiveFormat::TarGz,
                            entrypoint_names: vec![variant.entrypoint.to_owned()],
                        },
                        additional_downloads: Vec::new(),
                    }
                } else {
                    RuntimeAcquisitionPlan::SourceBuild(Box::new(q27_source_build_plan(
                        qualified.release,
                        source_build.expect("qualified source release has source evidence"),
                        source_recipe_version.expect("source recipe version"),
                        variant,
                    )?))
                },
                supported_native_identities: Vec::new(),
                requirements: source_build.map_or_else(
                    || requirements_for(&qualified.version, variant),
                    |source| source_requirements_for(&qualified.version, variant, source),
                ),
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

fn release_version(release: &GitHubRelease) -> Option<String> {
    let version = release.tag_name.strip_prefix('v')?;
    let numeric = version.split('.').collect::<Vec<_>>();
    (numeric.len() == 3
        && numeric
            .iter()
            .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
        && version_at_least(version, 0, 2, 0))
    .then(|| version.to_owned())
}

async fn inspect_source_capability(
    github: &GitHubReleaseClient,
    release: &GitHubRelease,
) -> Result<Option<Q27SourceCapability>, CatalogError> {
    let commit = github.commit(GITHUB_REPOSITORY, &release.tag_name).await?;
    if !norted_core::is_full_git_sha(&commit.sha)
        || !norted_core::is_full_git_sha(&commit.commit.tree.sha)
        || commit.html_url
            != format!(
                "https://github.com/{GITHUB_REPOSITORY}/commit/{}",
                commit.sha
            )
    {
        return Err(q27_provider_error(format!(
            "release `{}` did not resolve to a canonical full commit/tree identity",
            release.tag_name
        )));
    }
    let raw_root = format!(
        "https://raw.githubusercontent.com/{GITHUB_REPOSITORY}/{}",
        commit.sha
    );
    let makefile = match github
        .fetch_small_text(&format!("{raw_root}/Makefile"), SOURCE_METADATA_LIMIT)
        .await
    {
        Ok(makefile) => makefile,
        Err(CatalogError::Http { status: 404, .. }) => return Ok(None),
        Err(error) => return Err(error),
    };
    let readme = match github
        .fetch_small_text(&format!("{raw_root}/README.md"), SOURCE_METADATA_LIMIT)
        .await
    {
        Ok(readme) => readme,
        Err(CatalogError::Http { status: 404, .. }) => return Ok(None),
        Err(error) => return Err(error),
    };
    let Some(mut capability) = q27_source_capability_from_files(commit, &makefile, &readme) else {
        return Ok(None);
    };
    if capability.makefile_sha256 != PACKAGE_MAKEFILE_SHA256 {
        // The Make dependency/command closure is deliberately provider-audited
        // rather than guessed with a partial GNU Make parser.
        return Ok(None);
    }
    let server = github
        .fetch_small_text(&format!("{raw_root}/src/server.cu"), SOURCE_METADATA_LIMIT)
        .await?;
    let engine = github
        .fetch_small_text(&format!("{raw_root}/src/engine.cuh"), SOURCE_METADATA_LIMIT)
        .await?;
    capability.package_contract =
        q27_source_package_contract(&capability.commit, &makefile, &readme, &server, &engine);
    Ok(Some(capability))
}

fn q27_source_capability_from_files(
    commit: GitHubCommit,
    makefile: &str,
    readme: &str,
) -> Option<Q27SourceCapability> {
    let cxx = make_variable_value(makefile, "CXX")?;
    let nvcc = make_variable_value(makefile, "NVCC")?;
    if cxx != "g++" || nvcc != "/usr/local/cuda/bin/nvcc" || !makefile.contains("-std=c++17") {
        return None;
    }
    let supported_variant_ids = Q27_VARIANTS
        .iter()
        .filter(|variant| makefile_declares_target(makefile, variant.build_target))
        .map(|variant| variant.id)
        .collect::<Vec<_>>();
    if supported_variant_ids.is_empty() {
        return None;
    }
    let supported_cuda_compute_capabilities = [
        ("code=sm_86", ComputeCapability::new(8, 6)),
        ("code=sm_89", ComputeCapability::new(8, 9)),
        ("code=sm_120", ComputeCapability::new(12, 0)),
    ]
    .into_iter()
    .filter_map(|(evidence, capability)| makefile.contains(evidence).then_some(capability))
    .collect::<Vec<_>>();
    if supported_cuda_compute_capabilities.is_empty() {
        return None;
    }
    let accelerator_target = supported_cuda_compute_capabilities
        .iter()
        .map(|capability| format!("sm_{}{}", capability.major, capability.minor))
        .collect::<Vec<_>>()
        .join("+");
    Some(Q27SourceCapability {
        commit,
        makefile_sha256: sha256_text(makefile),
        package_contract: false,
        minimum_cuda_version: readme_cuda_floor(readme)?,
        supported_variant_ids,
        supported_cuda_compute_capabilities,
        accelerator_target,
    })
}

fn sha256_text(contents: &str) -> String {
    let digest = Sha256::digest(contents.as_bytes());
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn q27_source_package_contract(
    commit: &GitHubCommit,
    makefile: &str,
    readme: &str,
    server: &str,
    engine: &str,
) -> bool {
    q27_source_package_contract_from_digests(
        commit,
        &sha256_text(makefile),
        &sha256_text(readme),
        &sha256_text(server),
        &sha256_text(engine),
    )
}

fn q27_source_package_contract_from_digests(
    commit: &GitHubCommit,
    makefile_sha256: &str,
    readme_sha256: &str,
    server_sha256: &str,
    engine_sha256: &str,
) -> bool {
    commit.sha == PACKAGE_SOURCE_COMMIT
        && commit.commit.tree.sha == PACKAGE_SOURCE_TREE
        && makefile_sha256 == PACKAGE_MAKEFILE_SHA256
        && readme_sha256 == PACKAGE_README_SHA256
        && server_sha256 == PACKAGE_SERVER_SHA256
        && engine_sha256 == PACKAGE_ENGINE_SHA256
}

fn make_variable_value<'a>(makefile: &'a str, name: &str) -> Option<&'a str> {
    makefile.lines().find_map(|line| {
        let (left, right) = line.split_once("?=")?;
        (left.trim() == name).then(|| right.trim())
    })
}

fn makefile_declares_target(makefile: &str, expected: &str) -> bool {
    makefile.lines().any(|line| {
        let line = line.trim_start();
        !line.starts_with('#')
            && line.split_once(':').is_some_and(|(targets, _)| {
                targets.split_whitespace().any(|target| target == expected)
            })
    })
}

fn readme_cuda_floor(readme: &str) -> Option<String> {
    let (_, tail) = readme.split_once("CUDA toolkit ")?;
    let version = tail
        .chars()
        .take_while(|character| character.is_ascii_digit() || *character == '.')
        .collect::<String>();
    (!version.is_empty() && tail.get(version.len()..)?.starts_with('+')).then_some(version)
}

fn q27_source_build_plan(
    release: &GitHubRelease,
    source: &Q27SourceCapability,
    recipe_version: &str,
    variant: &RuntimeVariant,
) -> Result<RuntimeSourceBuildPlan, CatalogError> {
    let commit_timestamp_unix = parse_github_timestamp(&source.commit.commit.committer.date)
        .ok_or_else(|| {
            q27_provider_error(format!(
                "release `{}` resolved to a commit with an invalid timestamp",
                release.tag_name
            ))
        })?;
    Ok(RuntimeSourceBuildPlan {
        source: RuntimeSourceSnapshot {
            repository: GITHUB_REPOSITORY.to_owned(),
            repository_url: format!("{UPSTREAM_REPOSITORY}.git"),
            source_branch: release.tag_name.clone(),
            commit_sha: source.commit.sha.clone(),
            tree_sha: source.commit.commit.tree.sha.clone(),
            commit_timestamp_unix,
            source_provider: PROVIDER_ID.to_owned(),
        },
        recipe: RuntimeSourceBuildRecipe {
            recipe_version: recipe_version.to_owned(),
            build_system: RuntimeSourceBuildSystem::Make,
            build_definition_sha256: Some(source.makefile_sha256.clone()),
            source_overlay_sha256: None,
            cmake_configuration_arguments: Vec::new(),
            build_target: variant.build_target.to_owned(),
            entrypoint: variant.build_target.into(),
            accelerator_target: source.accelerator_target.clone(),
            rejected_build_environment: vec![
                "CC".to_owned(),
                "CXX".to_owned(),
                "CUDACXX".to_owned(),
                "CUDAHOSTCXX".to_owned(),
                "CFLAGS".to_owned(),
                "CPPFLAGS".to_owned(),
                "CXXFLAGS".to_owned(),
                "CUDAFLAGS".to_owned(),
                "LDFLAGS".to_owned(),
                "NVCC".to_owned(),
                "NVCCFLAGS".to_owned(),
                "NVCC_PREPEND_FLAGS".to_owned(),
                "NVCC_APPEND_FLAGS".to_owned(),
                "MAKEFLAGS".to_owned(),
                "GNUMAKEFLAGS".to_owned(),
                "MAKEFILES".to_owned(),
                "MAKEOVERRIDES".to_owned(),
                "MFLAGS".to_owned(),
                "SHELL".to_owned(),
            ],
        },
        prerequisites: RuntimeSourceBuildPrerequisites {
            minimum_cmake_version: String::new(),
            minimum_cuda_version: Some(source.minimum_cuda_version.clone()),
            maximum_cuda_version_exclusive: None,
            requires_ninja: false,
            requires_cpp20_compiler: false,
            requires_make: true,
            minimum_cpp_standard: Some(17),
            cpp_compiler: Some("g++".to_owned()),
            cuda_compiler: Some("/usr/local/cuda/bin/nvcc".into()),
            requires_pkg_config: false,
            pkg_config_modules: BTreeMap::new(),
        },
    })
}

fn source_requirements_for(
    version: &str,
    variant: &RuntimeVariant,
    source: &Q27SourceCapability,
) -> RuntimeRequirements {
    let mut requirements = requirements_for(version, variant);
    requirements.minimum_nvidia_driver = None;
    requirements.supported_cuda_compute_capabilities =
        source.supported_cuda_compute_capabilities.clone();
    requirements
        .advisories
        .retain(|note| !note.contains("Prebuilt binaries"));
    requirements
        .unverified_requirements
        .retain(|note| !note.contains("ELF requires"));
    requirements.advisories.push(format!(
        "Built locally from the exact upstream commit with CUDA toolkit {}+; this is not an upstream binary",
        source.minimum_cuda_version
    ));
    requirements
}

fn q27_provider_error(message: String) -> CatalogError {
    CatalogError::Provider {
        provider: PROVIDER_ID.to_owned(),
        message,
    }
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

fn compare_qualified(left: &QualifiedRelease<'_>, right: &QualifiedRelease<'_>) -> Ordering {
    compare_versions(&left.version, &right.version)
        .then_with(|| left.published_at_unix.cmp(&right.published_at_unix))
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
    build_target: &'static str,
    minimum_vram_class_gib: Option<u16>,
    minimum_vram_exclusive_class_gib: Option<u16>,
}

const Q27_VARIANTS: &[RuntimeVariant] = &[
    RuntimeVariant {
        id: "w8",
        label: "W8",
        entrypoint: "q27-server-w8",
        build_target: "build/q27-server-w8",
        minimum_vram_class_gib: Some(24),
        minimum_vram_exclusive_class_gib: None,
    },
    RuntimeVariant {
        id: "w12",
        label: "W12",
        entrypoint: "q27-server",
        build_target: "build/q27-server",
        minimum_vram_class_gib: Some(32),
        minimum_vram_exclusive_class_gib: None,
    },
    RuntimeVariant {
        id: "w16",
        label: "W16",
        entrypoint: "q27-server-w16",
        build_target: "build/q27-server-w16",
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

#[derive(Debug, Clone, Copy)]
enum Q27SourceBuildEvidence<'a> {
    Provenance(&'a RuntimeSourceBuildProvenance),
}

impl<'a> Q27SourceBuildEvidence<'a> {
    fn source(self) -> &'a RuntimeSourceSnapshot {
        match self {
            Self::Provenance(provenance) => &provenance.source,
        }
    }

    fn recipe_version(self) -> &'a str {
        match self {
            Self::Provenance(provenance) => &provenance.recipe_version,
        }
    }

    fn build_system(self) -> RuntimeSourceBuildSystem {
        match self {
            Self::Provenance(provenance) => provenance.build_system,
        }
    }

    fn build_definition_sha256(self) -> Option<&'a str> {
        match self {
            Self::Provenance(provenance) => provenance.build_definition_sha256.as_deref(),
        }
    }

    fn build_target(self) -> &'a str {
        match self {
            Self::Provenance(provenance) => &provenance.build_target,
        }
    }

    fn entrypoint_matches_target(self) -> bool {
        match self {
            Self::Provenance(provenance) => {
                provenance.entrypoint == Path::new("source").join(&provenance.build_target)
            }
        }
    }
}

fn q27_source_package_contract_is_trusted(
    identity: &RuntimeIdentity,
    evidence: Q27SourceBuildEvidence<'_>,
) -> bool {
    let source = evidence.source();
    identity.engine_id == ENGINE_ID
        && identity.platform == "linux"
        && identity.architecture == "x86_64"
        && identity.accelerator == "cuda"
        && identity.package_family == format!("{SOURCE_PACKAGE_FAMILY}-{SOURCE_RECIPE_VERSION}")
        && identity.package.provider_id == PROVIDER_ID
        && identity.package.repository.as_deref() == Some(GITHUB_REPOSITORY)
        && identity.package.release_tag.as_deref() == Some(source.source_branch.as_str())
        && identity.upstream_revision.as_deref() == Some(source.commit_sha.as_str())
        && source.repository == GITHUB_REPOSITORY
        && source.source_provider == PROVIDER_ID
        && source.commit_sha == PACKAGE_SOURCE_COMMIT
        && source.tree_sha == PACKAGE_SOURCE_TREE
        && evidence.recipe_version() == SOURCE_RECIPE_VERSION
        && evidence.build_system() == RuntimeSourceBuildSystem::Make
        && evidence.build_definition_sha256() == Some(PACKAGE_MAKEFILE_SHA256)
        && evidence.entrypoint_matches_target()
}

/// Binary W_MAX is historical release evidence. Source W_MAX comes only from
/// the exact selected build target inside the immutable provider-reviewed
/// source recipe; the variant is checked as a consistency constraint rather
/// than used as the proof itself.
fn q27_compiled_w_max(
    identity: &RuntimeIdentity,
    acquisition: &RuntimeAcquisitionMethod,
    source_evidence: Option<Q27SourceBuildEvidence<'_>>,
) -> Option<u64> {
    let official_v062 = matches!(
        acquisition,
        RuntimeAcquisitionMethod::OfficialReleaseAsset
            | RuntimeAcquisitionMethod::PreseededOfficialPack
    ) && identity.engine_id == ENGINE_ID
        && identity.platform == "linux"
        && identity.architecture == "x86_64"
        && identity.accelerator == "cuda"
        && identity.package_family == PACKAGE_FAMILY
        && identity.package.provider_id == PROVIDER_ID
        && identity.package.repository.as_deref() == Some(GITHUB_REPOSITORY)
        && identity.package.release_tag.as_deref() == Some("v0.6.2")
        && identity.version == "0.6.2";
    if official_v062 {
        return match identity.variant.as_str() {
            "w8" => Some(8),
            "w12" => Some(12),
            "w16" => Some(16),
            _ => None,
        };
    }

    let evidence = source_evidence?;
    if acquisition != &RuntimeAcquisitionMethod::SourceBuild
        || !q27_source_package_contract_is_trusted(identity, evidence)
    {
        return None;
    }
    match (evidence.build_target(), identity.variant.as_str()) {
        ("build/q27-server-w8", "w8") => Some(8),
        ("build/q27-server", "w12") => Some(12),
        ("build/q27-server-w16", "w16") => Some(16),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum Q27KvMode {
    Fp8,
    Turbo5k,
    Turbo3,
    Fp16,
}

impl Q27KvMode {
    const QUALITY_ORDER: [Self; 3] = [Self::Fp8, Self::Turbo5k, Self::Turbo3];

    const fn as_str(self) -> &'static str {
        match self {
            Self::Fp8 => "fp8",
            Self::Turbo5k => "turbo5k",
            Self::Turbo3 => "turbo3",
            Self::Fp16 => "fp16",
        }
    }
}

const Q27_V062_KV_MODES: &[Q27KvMode] = &[Q27KvMode::Fp8, Q27KvMode::Turbo3, Q27KvMode::Fp16];
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
struct Q27RuntimeCapabilities {
    trustworthy_identity: bool,
    raw_completions: bool,
    exact_sharp_renderer: bool,
    thinking: bool,
    unlimited_think_budget: bool,
    temperature_top_p: bool,
    top_k_min_p: bool,
    request_seed: bool,
    request_thinking: bool,
    reasoning_effort: bool,
    tool_calling: bool,
    stable_serving_environment: bool,
    mtp_environment: bool,
    mtp_disable_control: bool,
    fast_head_control: bool,
    bounded_startup_observation: bool,
    compiled_w_max: Option<u64>,
    supported_kv_modes: &'static [Q27KvMode],
}

fn q27_runtime_capabilities(
    identity: &RuntimeIdentity,
    acquisition: &RuntimeAcquisitionMethod,
    source_evidence: Option<Q27SourceBuildEvidence<'_>>,
) -> Q27RuntimeCapabilities {
    let exact_managed_v062 = matches!(
        acquisition,
        RuntimeAcquisitionMethod::OfficialReleaseAsset
            | RuntimeAcquisitionMethod::PreseededOfficialPack
    ) && identity.engine_id == ENGINE_ID
        && identity.platform == "linux"
        && identity.architecture == "x86_64"
        && identity.accelerator == "cuda"
        && identity.package_family == PACKAGE_FAMILY
        && identity.package.provider_id == PROVIDER_ID
        && identity.package.repository.as_deref() == Some(GITHUB_REPOSITORY)
        && identity.package.release_tag.as_deref() == Some("v0.6.2")
        && identity.version == "0.6.2";
    let exact_source_contract = acquisition == &RuntimeAcquisitionMethod::SourceBuild
        && source_evidence
            .is_some_and(|evidence| q27_source_package_contract_is_trusted(identity, evidence));
    let trustworthy_identity = exact_managed_v062 || exact_source_contract;
    Q27RuntimeCapabilities {
        trustworthy_identity,
        // Sharp rendering is Server-owned. The runtime facts below are proven
        // by either the historical binary contract or the immutable v2 source
        // contract admitted from the exact source file fingerprints.
        raw_completions: trustworthy_identity,
        exact_sharp_renderer: trustworthy_identity,
        thinking: trustworthy_identity,
        // v0.6.2 has no separate thinking-budget limiter: the ordinary
        // max-token limit is the only generation bound on its raw route.
        unlimited_think_budget: trustworthy_identity,
        temperature_top_p: trustworthy_identity,
        top_k_min_p: exact_source_contract,
        request_seed: exact_source_contract,
        request_thinking: exact_source_contract,
        reasoning_effort: exact_source_contract,
        tool_calling: exact_source_contract,
        stable_serving_environment: exact_source_contract,
        mtp_environment: trustworthy_identity,
        // The exact q27 engine always performs NextN/MTP speculative decode.
        // Q27_SUFFIX=0 disables only suffix drafting; it is not an MTP-off switch.
        mtp_disable_control: false,
        fast_head_control: trustworthy_identity,
        bounded_startup_observation: trustworthy_identity,
        compiled_w_max: q27_compiled_w_max(identity, acquisition, source_evidence),
        // q27 v0.6.2 source contains fp8 and turbo3. It does not contain the
        // automatic quality sequence's intermediate turbo5k mode.
        supported_kv_modes: if exact_source_contract {
            &Q27KvMode::QUALITY_ORDER
        } else if exact_managed_v062 {
            Q27_V062_KV_MODES
        } else {
            &[]
        },
    }
}

fn validate_q27_settings_prelaunch(
    settings: &norted_core::ResolvedSettings,
    capabilities: Q27RuntimeCapabilities,
) -> Result<(), String> {
    if let Some(SettingValue::UnsignedIntegerOrChoice(
        norted_core::UnsignedIntegerOrChoiceValue::Choice(value),
    )) = settings.value("q27.seed")
    {
        return Err(format!(
            "q27 does not implement `seed={value}`; configure an explicit numeric seed or leave it unset"
        ));
    }
    if !capabilities.trustworthy_identity {
        return Err(
            "the exact q27 executable has no trustworthy capability observation; external binaries are not credited from filenames or upstream version assumptions"
                .to_owned(),
        );
    }
    if setting_float(settings, "q27.top_p").is_some_and(|value| value <= 0.0) {
        return Err(
            "q27.top_p must be greater than zero and at most 1; the runtime reinterprets zero as 1"
                .to_owned(),
        );
    }
    let mut failures = Vec::new();
    for (force, regular, active) in [
        (
            "q27.force_temperature",
            "q27.temperature",
            setting_float(settings, "q27.force_temperature").is_some_and(|value| value > 0.0),
        ),
        (
            "q27.force_top_p",
            "q27.top_p",
            setting_float(settings, "q27.force_top_p").is_some_and(|value| value < 1.0),
        ),
    ] {
        if active && settings.value(regular).is_some() {
            return Err(format!(
                "conflicting settings `{force}` and `{regular}` both define an omitted-request default; inherit one of them"
            ));
        }
    }

    if setting_toggle(settings, "q27.mtp") == Some(false) && !capabilities.mtp_disable_control {
        failures.push(
            "the exact q27 runtime always uses its NextN/MTP speculative engine and exposes no proven MTP-disable control",
        );
    }
    let prompt_mode = setting_choice(settings, "q27.prompt_mode").unwrap_or("runtime_default");
    let delivery = setting_choice(settings, "q27.prompt_delivery").unwrap_or("runtime_chat");
    if !matches!(
        (prompt_mode, delivery),
        ("runtime_default", "runtime_chat") | ("external_template", "raw_completions")
    ) {
        failures.push("the q27 adapter cannot apply the selected prompt mode/delivery combination");
    }
    if setting_choice(settings, "q27.response_filter").is_some_and(|value| value != "none")
        && prompt_mode != "external_template"
    {
        failures.push("q27 response filtering requires an externally rendered raw prompt");
    }
    if prompt_mode == "external_template"
        && (!capabilities.raw_completions || !capabilities.exact_sharp_renderer)
    {
        failures.push("external template rendering/raw completion delivery is unproven");
    }
    if settings.value("q27.thinking").is_some() && !capabilities.thinking {
        failures.push("thinking control is unproven");
    }
    if settings.value("q27.thinking_budget").is_some() && !capabilities.unlimited_think_budget {
        failures.push("thinking budget control is unproven");
    }
    if (settings.value("q27.temperature").is_some() || settings.value("q27.top_p").is_some())
        && !capabilities.temperature_top_p
    {
        failures.push("temperature/top-p controls are unproven");
    }
    if (settings.value("q27.top_k").is_some() || settings.value("q27.min_p").is_some())
        && !capabilities.top_k_min_p
    {
        failures.push("top-k/min-p controls are unproven");
    }
    if settings.value("q27.seed").is_some() && !capabilities.request_seed {
        failures.push("request-default seed control is unproven");
    }
    if (settings.value("q27.reasoning").is_some()
        || settings.value("q27.reasoning_budget").is_some())
        && (!capabilities.request_thinking
            || setting_toggle(settings, "q27.request_thinking") != Some(true))
    {
        failures.push(
            "q27 reasoning request defaults require the proven `q27.request_thinking` launch control",
        );
    }
    if settings.value("q27.request_thinking").is_some() && !capabilities.request_thinking {
        failures.push("per-request thinking control is unproven");
    }
    if settings.value("q27.reasoning_effort").is_some() && !capabilities.reasoning_effort {
        failures.push("reasoning-effort process default is unproven");
    }
    if settings.value("q27.constrain_tools").is_some() && !capabilities.tool_calling {
        failures.push("tool grammar constraint control is unproven");
    }
    if (settings.value("q27.continuous_batching").is_some()
        || settings.value("q27.sampled_graphs").is_some())
        && !capabilities.stable_serving_environment
    {
        failures.push("stable batching/sampled-graph controls are unproven");
    }
    if setting_toggle(settings, "q27.sampled_graphs") == Some(false)
        && setting_float(settings, "q27.temperature").is_some_and(|temperature| temperature > 0.0)
    {
        failures.push("q27 sampled graphs cannot be disabled with a sampled temperature default");
    }
    if [
        "q27.mtp",
        "q27.mtp_max_depth",
        "q27.mtp_min_probability",
        "q27.suffix_drafting",
        "q27.suffix_width_mode",
    ]
    .into_iter()
    .any(|id| settings.value(id).is_some())
        && !capabilities.mtp_environment
    {
        failures.push("MTP/suffix controls are unproven");
    }
    if setting_choice(settings, "q27.suffix_width_mode") == Some("runtime_w_max")
        && capabilities.compiled_w_max.is_none()
    {
        failures.push("numeric compiled W_MAX is unproven");
    }
    if settings.value("q27.fast_head").is_some() && !capabilities.fast_head_control {
        failures.push("fast-head control is unproven");
    }
    if settings.value("q27.kv_mode").is_some() && capabilities.supported_kv_modes.is_empty() {
        failures.push("no configurable KV mode is proven for this executable");
    }
    if settings.value("q27.slot1_context_length").is_some()
        && setting_unsigned(settings, "q27.parallel_requests").unwrap_or(1) < 2
    {
        failures.push("q27 background-slot context requires at least two configured slots");
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("; "))
    }
}

fn q27_configured_kv_attempt_modes(
    capabilities: Q27RuntimeCapabilities,
    settings: &norted_core::ResolvedSettings,
) -> Vec<Q27KvMode> {
    match setting_choice(settings, "q27.kv_mode") {
        None | Some("runtime_default") => Vec::new(),
        Some("auto") => Q27KvMode::QUALITY_ORDER
            .into_iter()
            .filter(|mode| capabilities.supported_kv_modes.contains(mode))
            .collect(),
        Some(selected) => capabilities
            .supported_kv_modes
            .iter()
            .copied()
            .filter(|mode| mode.as_str() == selected)
            .collect(),
    }
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
                nvidia_gpu_absence_confirmed: false,
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
    environment: BTreeMap<String, String>,
    configuration_error: Option<String>,
    client: reqwest::Client,
    capability_cache: tokio::sync::RwLock<BTreeMap<String, String>>,
    configured_executions: tokio::sync::RwLock<BTreeMap<String, Q27ConfiguredExecution>>,
}

#[derive(Debug, Clone)]
struct Q27ConfiguredExecution {
    settings: norted_core::ResolvedSettings,
    sharp_template: Option<String>,
    compiled_w_max: Option<u64>,
    selected_kv_mode: Option<Q27KvMode>,
    capabilities: Q27RuntimeCapabilities,
}

#[derive(Debug)]
struct Q27ConfiguredLaunch {
    arguments: Vec<OsString>,
    environment: BTreeMap<String, String>,
    normalized_settings: BTreeMap<String, Value>,
}

fn setting_toggle(settings: &norted_core::ResolvedSettings, id: &str) -> Option<bool> {
    match settings.value(id) {
        Some(SettingValue::Toggle(value)) => Some(*value),
        _ => None,
    }
}

fn setting_unsigned(settings: &norted_core::ResolvedSettings, id: &str) -> Option<u64> {
    match settings.value(id) {
        Some(SettingValue::UnsignedInteger(value)) => Some(*value),
        _ => None,
    }
}

fn setting_float(settings: &norted_core::ResolvedSettings, id: &str) -> Option<f64> {
    match settings.value(id) {
        Some(SettingValue::Float(value)) => Some(*value),
        _ => None,
    }
}

fn setting_choice<'a>(settings: &'a norted_core::ResolvedSettings, id: &str) -> Option<&'a str> {
    match settings.value(id) {
        Some(SettingValue::Choice(value)) => Some(value),
        _ => None,
    }
}

fn q27_request_reasoning_effort(effort: norted_engine::ReasoningEffort) -> &'static str {
    match effort {
        norted_engine::ReasoningEffort::None => "none",
        norted_engine::ReasoningEffort::Minimal | norted_engine::ReasoningEffort::Low => "low",
        norted_engine::ReasoningEffort::Medium => "medium",
        norted_engine::ReasoningEffort::High
        | norted_engine::ReasoningEffort::Xhigh
        | norted_engine::ReasoningEffort::Max => "xhigh",
    }
}

fn setting_unsigned_with_source<'a>(
    settings: &'a norted_core::ResolvedSettings,
    id: &str,
) -> Option<(u64, &'a norted_core::SettingSource)> {
    settings.configured.iter().find_map(|(candidate, setting)| {
        if candidate.as_str() != id {
            return None;
        }
        match &setting.value {
            SettingValue::UnsignedInteger(value) => Some((*value, &setting.source)),
            _ => None,
        }
    })
}

fn setting_source_label(source: &norted_core::SettingSource) -> &'static str {
    match source {
        norted_core::SettingSource::SettingsOverride => "Settings override",
        norted_core::SettingSource::RuntimeDefault => "Runtime Default",
        norted_core::SettingSource::ModelProfile { .. } => "Model Profile",
        norted_core::SettingSource::Invocation => "invocation",
    }
}

fn q27_has_configured_execution(settings: &norted_core::ResolvedSettings) -> bool {
    settings.configured.keys().any(|id| {
        q27_environment_name(id.as_str()).is_some()
            || matches!(
                id.as_str(),
                "q27.context_length"
                    | "q27.parallel_requests"
                    | "q27.temperature"
                    | "q27.top_p"
                    | "q27.top_k"
                    | "q27.min_p"
                    | "q27.seed"
                    | "q27.max_output_tokens"
                    | "q27.system_prompt"
                    | "q27.reasoning"
                    | "q27.reasoning_budget"
                    | "q27.reasoning_effort"
                    | "q27.thinking"
                    | "q27.thinking_budget"
                    | "q27.request_thinking"
                    | "q27.constrain_tools"
                    | "q27.continuous_batching"
                    | "q27.sampled_graphs"
                    | "q27.slot1_context_length"
                    | "q27.fast_head"
                    | "q27.kv_mode"
                    | "q27.mtp"
                    | "q27.mtp_max_depth"
                    | "q27.mtp_min_probability"
                    | "q27.suffix_drafting"
                    | "q27.suffix_width_mode"
                    | "q27.prompt_mode"
                    | "q27.prompt_delivery"
                    | "q27.template_path"
                    | "q27.template_sha256"
                    | "q27.render_generation_prompt"
                    | "q27.template_thinking"
                    | "q27.response_filter"
            )
    })
}

fn settings_require_selected_kv(settings: &norted_core::ResolvedSettings) -> bool {
    setting_choice(settings, "q27.kv_mode").is_some_and(|value| value != "runtime_default")
}

fn q27_configured_launch(
    settings: &norted_core::ResolvedSettings,
    compiled_w_max: Option<u64>,
    selected_kv_mode: Option<Q27KvMode>,
) -> Result<Q27ConfiguredLaunch, EngineError> {
    let mut arguments = Vec::new();
    if let Some(thinking) = setting_toggle(settings, "q27.thinking") {
        arguments.push(OsString::from(if thinking {
            "--think"
        } else {
            "--no-think"
        }));
    }
    if let Some(budget) = setting_unsigned(settings, "q27.thinking_budget") {
        arguments.extend([
            OsString::from("--think-budget"),
            OsString::from(budget.to_string()),
        ]);
    }
    if setting_toggle(settings, "q27.request_thinking") == Some(true) {
        arguments.push(OsString::from("--request-think"));
    }
    if setting_toggle(settings, "q27.constrain_tools") == Some(true) {
        arguments.push(OsString::from("--constrain-tools"));
    }
    for (option, value) in [
        (
            "--temp",
            setting_float(settings, "q27.temperature").map(|value| value.to_string()),
        ),
        (
            "--top-p",
            setting_float(settings, "q27.top_p").map(|value| value.to_string()),
        ),
        (
            "--top-k",
            setting_unsigned(settings, "q27.top_k").map(|value| value.to_string()),
        ),
        (
            "--min-p",
            setting_float(settings, "q27.min_p").map(|value| value.to_string()),
        ),
    ] {
        if let Some(value) = value {
            arguments.extend([OsString::from(option), OsString::from(value)]);
        }
    }
    let mut environment = BTreeMap::new();
    if let Some(enabled) = setting_toggle(settings, "q27.continuous_batching") {
        environment.insert("Q27_BATCH".to_owned(), u8::from(enabled).to_string());
    }
    if let Some(enabled) = setting_toggle(settings, "q27.sampled_graphs") {
        environment.insert("Q27_SAMPLED".to_owned(), u8::from(enabled).to_string());
    }
    if let Some(kv_mode) = selected_kv_mode {
        if kv_mode == Q27KvMode::Fp16 {
            arguments.push(OsString::from("--kv-fp16"));
        } else {
            environment.insert("Q27_KV".to_owned(), kv_mode.as_str().to_owned());
        }
    }
    if setting_toggle(settings, "q27.mtp") != Some(false) {
        if let Some(depth) = setting_unsigned(settings, "q27.mtp_max_depth") {
            environment.insert("Q27_MAXD".to_owned(), depth.to_string());
        }
        if let Some(probability) = setting_float(settings, "q27.mtp_min_probability") {
            environment.insert("Q27_PMIN".to_owned(), probability.to_string());
        }
        if let Some(suffix) = setting_toggle(settings, "q27.suffix_drafting") {
            environment.insert("Q27_SUFFIX".to_owned(), u8::from(suffix).to_string());
        }
        if setting_choice(settings, "q27.suffix_width_mode") == Some("runtime_w_max") {
            let compiled_w_max = compiled_w_max.ok_or_else(|| {
                EngineError::InvalidConfiguration(
                    "q27 suffix drafting has no proven numeric compiled W_MAX".to_owned(),
                )
            })?;
            environment.insert("Q27_SUFFIX_W".to_owned(), compiled_w_max.to_string());
        }
    }
    for (id, setting) in &settings.configured {
        let Some(name) = q27_environment_name(id.as_str()) else {
            continue;
        };
        let value = match (id.as_str(), &setting.value) {
            ("q27.serving_profile", SettingValue::Choice(value)) if value == "cc" => continue,
            ("q27.reasoning_effort", SettingValue::Choice(value)) => match value.as_str() {
                "low" | "medium" | "xhigh" => value.clone(),
                _ => {
                    return Err(EngineError::InvalidConfiguration(
                        "q27.reasoning_effort must be low, medium, or xhigh".to_owned(),
                    ));
                }
            },
            (
                "q27.tool_dialect"
                | "q27.decode_attention"
                | "q27.prefill_token_tile"
                | "q27.delta_scan_split",
                SettingValue::Choice(value),
            ) if value == "auto" => continue,
            ("q27.batch_gemm", SettingValue::Choice(value))
            | ("q27.prefill_split_k", SettingValue::Choice(value)) => match value.as_str() {
                "auto" => "auto".to_owned(),
                "on" => "1".to_owned(),
                "off" => "0".to_owned(),
                _ => {
                    return Err(EngineError::InvalidConfiguration(format!(
                        "setting `{id}` has an invalid q27 environment value"
                    )));
                }
            },
            ("q27.gpu_interleaving", SettingValue::Toggle(true)) => continue,
            ("q27.gpu_interleaving", SettingValue::Toggle(false)) => "1".to_owned(),
            (
                "q27.plain_sampling" | "q27.tool_split" | "q27.bare_system_prompt",
                SettingValue::Toggle(false),
            ) => continue,
            (_, SettingValue::Toggle(value)) => u8::from(*value).to_string(),
            (_, SettingValue::UnsignedInteger(value)) => value.to_string(),
            (_, SettingValue::Float(value)) => value.to_string(),
            (_, SettingValue::Choice(value)) => value.clone(),
            _ => {
                return Err(EngineError::InvalidConfiguration(format!(
                    "setting `{id}` has an invalid q27 environment value"
                )));
            }
        };
        environment.insert(name.to_owned(), value);
    }
    let mut normalized_settings = BTreeMap::new();
    for (id, setting) in &settings.configured {
        normalized_settings.insert(
            id.to_string(),
            serde_json::to_value(&setting.value).map_err(|error| {
                EngineError::InvalidConfiguration(format!(
                    "could not record setting `{id}`: {error}"
                ))
            })?,
        );
    }
    Ok(Q27ConfiguredLaunch {
        arguments,
        environment,
        normalized_settings,
    })
}

fn verify_q27_process_setting_arguments(
    settings: &norted_core::ResolvedSettings,
    arguments: &[OsString],
) -> Result<(), EngineError> {
    for (id, option) in [
        ("q27.context_length", "--ctx"),
        ("q27.parallel_requests", "--slots"),
        ("q27.slot1_context_length", "--slot1-ctx"),
    ] {
        let expected = setting_unsigned(settings, id).map(|value| value.to_string());
        let mut observed = Vec::new();
        let mut index = 0;
        while let Some(argument) = arguments.get(index) {
            if argument == option {
                let value = arguments.get(index + 1).ok_or_else(|| {
                    EngineError::InvalidConfiguration(format!(
                        "q27 launch option `{option}` omitted its value"
                    ))
                })?;
                observed.push(value.to_string_lossy().into_owned());
                index += 2;
                continue;
            }
            if argument
                .to_string_lossy()
                .strip_prefix(option)
                .is_some_and(|suffix| suffix.starts_with('='))
            {
                return Err(EngineError::InvalidConfiguration(format!(
                    "q27 launch option `{option}` did not come from structured settings"
                )));
            }
            index += 1;
        }
        let matches = match expected.as_deref() {
            Some(expected) => observed.as_slice() == [expected],
            None => observed.is_empty(),
        };
        if !matches {
            return Err(EngineError::InvalidConfiguration(format!(
                "q27 structured setting `{id}` disagrees with the launch arguments"
            )));
        }
    }
    Ok(())
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
            if configuration_error.is_none() && !native_arguments.is_empty() {
                configuration_error = Some(
                    "q27 native arguments are disabled; use typed `q27.*` settings".to_owned(),
                );
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
            environment,
            configuration_error,
            client,
            capability_cache: tokio::sync::RwLock::new(BTreeMap::new()),
            configured_executions: tokio::sync::RwLock::new(BTreeMap::new()),
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
        let cached = self
            .capability_cache
            .read()
            .await
            .contains_key(&binary_sha256);
        if !cached {
            let usage_output = capture_command(
                &binary_path,
                &[],
                &self.environment,
                &managed_environment_removals(),
                PROBE_TIMEOUT,
            )
            .await?;
            let usage = command_detail(&usage_output.stdout, &usage_output.stderr);
            let usage_contract_error = q27_usage_contract_error(&usage);
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

    async fn prepare_launch_attempt_inner(
        &self,
        spec: &LaunchSpec,
        progress: Option<&LoadProgressReporter>,
    ) -> Result<(), EngineError> {
        let endpoint = spec.endpoint.as_deref().ok_or_else(|| {
            EngineError::InvalidConfiguration("q27 launch has no private endpoint".to_owned())
        })?;
        self.configured_executions.write().await.remove(endpoint);
        if spec.model.primary.norted_package.is_some() {
            match progress {
                Some(progress) => {
                    revalidate_norted_package_before_launch_with_progress(&spec.model, progress)
                        .await?
                }
                None => revalidate_norted_package_before_launch(&spec.model).await?,
            }
        }
        let capabilities = q27_runtime_capabilities(
            &spec.runtime.manifest.identity,
            &spec.runtime.manifest.acquisition_method,
            spec.runtime
                .manifest
                .source_build
                .as_ref()
                .map(Q27SourceBuildEvidence::Provenance),
        );
        if !capabilities.trustworthy_identity && !q27_has_configured_execution(&spec.settings) {
            return Ok(());
        }
        validate_q27_settings_prelaunch(&spec.settings, capabilities).map_err(|reason| {
            EngineError::InvalidConfiguration(format!(
                "selected q27 runtime cannot satisfy the effective settings: {reason}"
            ))
        })?;
        let selected_kv_mode = [
            Q27KvMode::Fp8,
            Q27KvMode::Turbo5k,
            Q27KvMode::Turbo3,
            Q27KvMode::Fp16,
        ]
        .into_iter()
        .find(|mode| {
            spec.environment.get("Q27_KV").map(String::as_str) == Some(mode.as_str())
                || (*mode == Q27KvMode::Fp16
                    && spec
                        .arguments
                        .iter()
                        .any(|argument| argument == "--kv-fp16"))
        });
        if selected_kv_mode.is_some_and(|mode| !capabilities.supported_kv_modes.contains(&mode)) {
            return Err(EngineError::InvalidConfiguration(format!(
                "q27 KV mode `{}` is not proven for this executable",
                selected_kv_mode.expect("checked").as_str()
            )));
        }
        let sharp_template =
            if setting_choice(&spec.settings, "q27.prompt_mode") == Some("external_template") {
                let template = read_configured_template(&spec.settings).await?;
                validate_configured_template(&spec.settings, &template)?;
                Some(template)
            } else {
                None
            };
        let compiled_w_max = capabilities.compiled_w_max;
        if setting_choice(&spec.settings, "q27.suffix_width_mode") == Some("runtime_w_max")
            && compiled_w_max.is_none()
        {
            return Err(EngineError::InvalidConfiguration(
                "q27 launch has no proven numeric compiled W_MAX".to_owned(),
            ));
        }
        verify_q27_process_setting_arguments(&spec.settings, &spec.arguments)?;
        self.configured_executions.write().await.insert(
            endpoint.to_owned(),
            Q27ConfiguredExecution {
                settings: spec.settings.clone(),
                sharp_template,
                compiled_w_max,
                selected_kv_mode,
                capabilities,
            },
        );
        Ok(())
    }

    fn backend_request(
        &self,
        request: &InferenceRequest,
        stream: bool,
    ) -> Result<Value, EngineError> {
        if request.messages.iter().any(InferenceMessage::has_media)
            || !request.tools.is_empty()
            || request
                .messages
                .iter()
                .any(|message| !message.tool_calls.is_empty() || message.tool_call_id.is_some())
            || request.generation_settings.seed.is_some()
            || request.generation_settings.top_k.is_some()
            || request.generation_settings.min_p.is_some()
            || request.generation_settings.reasoning_enabled.is_some()
            || request.generation_settings.reasoning_budget.is_some()
            || request.generation_settings.reasoning_effort.is_some()
        {
            return Err(EngineError::InvalidGenerationSettings(
                "the unverified external q27 contract supports only text, temperature, top_p, and max-token translation"
                    .to_owned(),
            ));
        }
        let mut body = json!({
            "model": request.model_profile_id.as_str(),
            "messages": request.messages.iter().map(message_json).collect::<Vec<_>>(),
            "stream": stream,
        });
        if let Some(value) = request.generation_settings.temperature {
            body["temperature"] = json!(value);
        }
        if let Some(value) = request.generation_settings.top_p {
            body["top_p"] = json!(value);
        }
        if let Some(maximum) = request.max_output_tokens {
            body["max_tokens"] = json!(maximum);
        }
        if stream {
            body["stream_options"] = json!({ "include_usage": true });
        }
        Ok(body)
    }

    fn configured_backend_request(
        execution: &Q27ConfiguredExecution,
        request: &InferenceRequest,
        stream: bool,
    ) -> Result<(&'static str, Value), EngineError> {
        if request.messages.iter().any(InferenceMessage::has_media) {
            return Err(EngineError::InvalidGenerationSettings(
                "q27 does not support media content".to_owned(),
            ));
        }
        if (request.generation_settings.seed.is_some() && !execution.capabilities.request_seed)
            || ((request.generation_settings.top_k.is_some()
                || request.generation_settings.min_p.is_some())
                && !execution.capabilities.top_k_min_p)
        {
            return Err(EngineError::InvalidGenerationSettings(
                "this exact q27 runtime does not prove the requested seed/top_k/min_p controls"
                    .to_owned(),
            ));
        }
        if (!request.tools.is_empty()
            || request
                .messages
                .iter()
                .any(|message| !message.tool_calls.is_empty() || message.tool_call_id.is_some()))
            && !execution.capabilities.tool_calling
        {
            return Err(EngineError::InvalidGenerationSettings(
                "this exact q27 runtime does not prove tool calling".to_owned(),
            ));
        }
        if execution.sharp_template.is_some()
            && (!request.tools.is_empty()
                || request.messages.iter().any(|message| {
                    !message.tool_calls.is_empty() || message.tool_call_id.is_some()
                }))
        {
            return Err(EngineError::InvalidGenerationSettings(
                "q27 tool calling requires the runtime chat route, not external raw-prompt delivery"
                    .to_owned(),
            ));
        }
        if (request.generation_settings.reasoning_enabled.is_some()
            || request.generation_settings.reasoning_budget.is_some()
            || matches!(
                request.generation_settings.reasoning_effort,
                Some(norted_engine::ReasoningEffort::None)
            ))
            && (!execution.capabilities.request_thinking
                || setting_toggle(&execution.settings, "q27.request_thinking") != Some(true))
        {
            return Err(EngineError::InvalidGenerationSettings(
                "q27 per-request thinking enable, disable, and budget require `q27.request_thinking` to be enabled"
                    .to_owned(),
            ));
        }
        if request.generation_settings.reasoning_effort.is_some()
            && !execution.capabilities.reasoning_effort
        {
            return Err(EngineError::InvalidGenerationSettings(
                "this exact q27 runtime does not prove reasoning-effort request semantics"
                    .to_owned(),
            ));
        }
        if request
            .generation_settings
            .reasoning_budget
            .is_some_and(|budget| budget < 0)
        {
            return Err(EngineError::InvalidGenerationSettings(
                "q27 per-request thinking budget must be zero or positive".to_owned(),
            ));
        }
        let effective_temperature = request
            .generation_settings
            .temperature
            .or_else(|| setting_float(&execution.settings, "q27.temperature"))
            .or_else(|| {
                setting_float(&execution.settings, "q27.force_temperature")
                    .filter(|value| *value > 0.0)
            })
            .unwrap_or(0.0);
        if setting_toggle(&execution.settings, "q27.sampled_graphs") == Some(false)
            && effective_temperature > 0.0
        {
            return Err(EngineError::InvalidGenerationSettings(
                "q27 sampled requests are unavailable because `q27.sampled_graphs` is disabled"
                    .to_owned(),
            ));
        }
        let request_effort = request
            .generation_settings
            .reasoning_effort
            .map(q27_request_reasoning_effort);
        let request_reasoning_enabled = match request.generation_settings.reasoning_effort {
            Some(norted_engine::ReasoningEffort::None) => Some(false),
            _ => request.generation_settings.reasoning_enabled,
        };
        let mut body = json!({"model": request.model_profile_id.as_str(), "stream": stream});
        let route = if let Some(template) = execution.sharp_template.as_deref() {
            body["prompt"] = json!(render_sharp_template(
                template,
                &request.messages,
                setting_toggle(&execution.settings, "q27.render_generation_prompt").unwrap_or(true),
                request_reasoning_enabled
                    .or_else(|| setting_toggle(&execution.settings, "q27.template_thinking"))
                    .or_else(|| setting_toggle(&execution.settings, "q27.thinking"))
                    .unwrap_or(false),
                request_effort
                    .or_else(|| setting_choice(&execution.settings, "q27.reasoning_effort")),
            )?);
            "/v1/completions"
        } else {
            body["messages"] = json!(
                request
                    .messages
                    .iter()
                    .map(message_json)
                    .collect::<Vec<_>>()
            );
            "/v1/chat/completions"
        };
        for (name, value) in [
            (
                "temperature",
                request
                    .generation_settings
                    .temperature
                    .or_else(|| setting_float(&execution.settings, "q27.temperature"))
                    .map(Value::from),
            ),
            (
                "top_p",
                request
                    .generation_settings
                    .top_p
                    .or_else(|| setting_float(&execution.settings, "q27.top_p"))
                    .map(Value::from),
            ),
            (
                "top_k",
                request
                    .generation_settings
                    .top_k
                    .or_else(|| setting_unsigned(&execution.settings, "q27.top_k"))
                    .map(Value::from),
            ),
            (
                "min_p",
                request
                    .generation_settings
                    .min_p
                    .or_else(|| setting_float(&execution.settings, "q27.min_p"))
                    .map(Value::from),
            ),
        ] {
            if let Some(value) = value {
                body[name] = value;
            }
        }
        if let Some(seed) = request.generation_settings.seed {
            body["seed"] = json!(seed);
        }
        if let Some(enabled) = request_reasoning_enabled {
            body["enable_thinking"] = json!(enabled);
        }
        if let Some(effort) = request_effort {
            body["reasoning_effort"] = json!(effort);
        }
        if let Some(budget) = request.generation_settings.reasoning_budget {
            body["thinking_token_budget"] = json!(budget);
        }
        if !request.tools.is_empty() {
            body["tools"] = Value::Array(
                request
                    .tools
                    .iter()
                    .map(|tool| {
                        json!({
                            "type": "function",
                            "function": {
                                "name": tool.name,
                                "description": tool.description,
                                "parameters": tool.parameters,
                            }
                        })
                    })
                    .collect(),
            );
            if let Some(choice) = request.tool_choice.as_ref() {
                body["tool_choice"] = match choice {
                    InferenceToolChoice::Auto => json!("auto"),
                    InferenceToolChoice::None => json!("none"),
                    InferenceToolChoice::Required => json!("required"),
                    InferenceToolChoice::Function { name } => {
                        json!({"type": "function", "function": {"name": name}})
                    }
                };
            }
            if let Some(parallel) = request.parallel_tool_calls {
                body["parallel_tool_calls"] = json!(parallel);
            }
        }
        if let Some(maximum) = request.max_output_tokens {
            body["max_tokens"] = json!(maximum);
        }
        if stream {
            body["stream_options"] = json!({ "include_usage": true });
        }
        Ok((route, body))
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
            api: vec![
                ApiCapability::Responses,
                ApiCapability::ChatCompletions,
                ApiCapability::Completions,
            ],
            features: vec![EngineFeature::TextGeneration, EngineFeature::ToolCalling],
        }
    }

    fn serving_features(
        &self,
        runtime: &InstalledRuntime,
        _model: &ModelArtifact,
        settings: Option<&norted_core::ResolvedSettings>,
    ) -> Vec<EngineFeature> {
        let capabilities = q27_runtime_capabilities(
            &runtime.manifest.identity,
            &runtime.manifest.acquisition_method,
            runtime
                .manifest
                .source_build
                .as_ref()
                .map(Q27SourceBuildEvidence::Provenance),
        );
        let mut features = vec![EngineFeature::TextGeneration];
        if capabilities.tool_calling
            && settings.is_none_or(|settings| {
                setting_choice(settings, "q27.prompt_mode") != Some("external_template")
            })
        {
            features.push(EngineFeature::ToolCalling);
        }
        features
    }

    fn uses_setting_as_request_default(&self, id: &str) -> bool {
        // q27.reasoning_effort is a process-level trained-template default
        // translated to Q27_REASONING_EFFORT. Explicit request effort remains
        // ephemeral and is translated separately in configured_backend_request.
        id != "reasoning_effort"
    }

    fn validate_generation_settings(
        &self,
        settings: &GenerationSettingsPatch,
        _backend_defaults: &EffectiveGenerationSettings,
    ) -> Result<(), EngineError> {
        settings.validate_stop_token_ids()?;
        if settings.stop_token_ids.is_some() {
            return Err(EngineError::InvalidGenerationSettings(
                "this runtime has no reviewed exact token-stop request contract".to_owned(),
            ));
        }
        if settings.repeat_penalty.is_some()
            || settings.presence_penalty.is_some()
            || settings.frequency_penalty.is_some()
            || settings.stop.is_some()
        {
            return Err(EngineError::InvalidGenerationSettings(
                "q27 does not prove repetition/presence/frequency penalties or stop-string request controls"
                    .to_owned(),
            ));
        }
        if matches!(
            (settings.reasoning_enabled, settings.reasoning_effort),
            (Some(true), Some(norted_engine::ReasoningEffort::None))
                | (
                    Some(false),
                    Some(
                        norted_engine::ReasoningEffort::Minimal
                            | norted_engine::ReasoningEffort::Low
                            | norted_engine::ReasoningEffort::Medium
                            | norted_engine::ReasoningEffort::High
                            | norted_engine::ReasoningEffort::Xhigh
                            | norted_engine::ReasoningEffort::Max
                    )
                )
        ) {
            return Err(EngineError::InvalidGenerationSettings(
                "q27 request reasoning enable/disable conflicts with the requested reasoning effort"
                    .to_owned(),
            ));
        }
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
        if settings.top_k.is_some_and(|top_k| top_k >= 1_000_000_000) {
            return Err(EngineError::InvalidGenerationSettings(
                "q27 top_k must be below 1000000000; zero disables it".to_owned(),
            ));
        }
        if let Some(min_p) = settings.min_p
            && (!min_p.is_finite() || !(0.0..=1.0).contains(&min_p))
        {
            return Err(EngineError::InvalidGenerationSettings(
                "q27 min_p must be finite and in the range 0..=1; zero disables it".to_owned(),
            ));
        }
        // Sampling controls remain configured while greedy decoding is active.
        // They become active when a later request supplies positive temperature.
        Ok(())
    }

    fn validate_inference_request(
        &self,
        request: &InferenceRequest,
        backend_defaults: &EffectiveGenerationSettings,
        settings_schema: &SettingsSchema,
    ) -> Result<(), EngineError> {
        self.validate_generation_settings(&request.generation_settings, backend_defaults)?;
        if request.generation_settings.reasoning_effort.is_some() {
            let id = SettingId::new("q27.reasoning_effort").expect("static setting ID");
            if settings_schema.definition(&id).is_none() {
                return Err(EngineError::InvalidGenerationSettings(
                    "the exact q27 runtime/model contract does not support reasoning effort"
                        .to_owned(),
                ));
            }
        }
        if request.messages.iter().any(InferenceMessage::has_media) {
            return Err(EngineError::InvalidGenerationSettings(
                "q27 does not support media content".to_owned(),
            ));
        }
        if matches!(
            request.output_format.as_ref(),
            Some(OutputFormat::JsonObject | OutputFormat::JsonSchema { .. })
        ) {
            return Err(EngineError::InvalidGenerationSettings(
                "q27 does not support structured output: exact reviewed runtime only constrains tool-call bodies, not no-tools JSON finalization".to_owned(),
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
        CompatibilityDecision::Supported
    }

    fn available_runtime_compatibility(&self, runtime: &AvailableRuntime) -> CompatibilityDecision {
        let _ = runtime;
        CompatibilityDecision::Supported
    }

    fn compatibility(&self, model: &ModelArtifact) -> CompatibilityDecision {
        if !model.generation_contract.required_stop_token_ids.is_empty() {
            return CompatibilityDecision::Unsupported { reason: "artifact requires exact token stops; q27 block generation does not prove this contract".to_owned() };
        }
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
        _settings: Option<&norted_core::ResolvedSettings>,
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
        qualify_tier_compatibility(facts.tier, evaluation.compatibility)
    }

    fn validate_configuration(
        &self,
        runtime: &InstalledRuntime,
        model: Option<&ModelArtifact>,
        _host: &HostCapabilities,
        settings: &norted_core::ResolvedSettings,
    ) -> Result<(), EngineError> {
        if let Some(model) = model {
            let facts =
                inspect_q27_model(&model.path).map_err(EngineError::InvalidConfiguration)?;
            validate_q27_model_settings(&facts, settings)
                .map_err(EngineError::InvalidConfiguration)?;
        }
        if q27_has_configured_execution(settings) {
            validate_q27_settings_prelaunch(
                settings,
                q27_runtime_capabilities(
                    &runtime.manifest.identity,
                    &runtime.manifest.acquisition_method,
                    runtime
                        .manifest
                        .source_build
                        .as_ref()
                        .map(Q27SourceBuildEvidence::Provenance),
                ),
            )
            .map_err(EngineError::InvalidConfiguration)?;
        }
        Ok(())
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

    fn available_runtime_model_compatibility(
        &self,
        runtime: &AvailableRuntime,
        model: &ModelArtifact,
        host: &HostCapabilities,
        _settings: Option<&norted_core::ResolvedSettings>,
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
        qualify_tier_compatibility(facts.tier, evaluation.compatibility)
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

    fn runtime_model_accelerator_binding(
        &self,
        runtime: &InstalledRuntime,
        model: &ModelArtifact,
        host: &HostCapabilities,
        _settings: Option<&norted_core::ResolvedSettings>,
    ) -> Option<AcceleratorBinding> {
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
            .map(AcceleratorBinding::single)
        })
    }

    async fn prepare_model_input(
        &self,
        model: &ModelArtifact,
    ) -> Result<PreparedModelInput, EngineError> {
        prepare_q27_model_input(model, None).await
    }

    async fn prepare_model_input_with_progress(
        &self,
        model: &ModelArtifact,
        progress: LoadProgressReporter,
    ) -> Result<PreparedModelInput, EngineError> {
        prepare_q27_model_input(model, Some(&progress)).await
    }

    fn native_options(&self) -> Vec<NativeOption> {
        Vec::new()
    }

    fn setting_definitions(&self) -> Vec<SettingDefinition> {
        q27_setting_definitions()
    }

    fn model_setting_definitions(
        &self,
        model: &ModelArtifact,
    ) -> Result<Vec<SettingDefinition>, EngineError> {
        q27_model_setting_definitions(model)
    }

    async fn runtime_settings_schema(
        &self,
        runtime: &InstalledRuntime,
        _host: &HostCapabilities,
        settings: Option<&norted_core::ResolvedSettings>,
    ) -> Result<SettingsSchema, EngineError> {
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
        let mut schema = q27_settings_schema_from_usage(
            Some(runtime.manifest.runtime_id.clone()),
            &runtime.manifest.identity,
            &runtime.manifest.acquisition_method,
            runtime
                .manifest
                .source_build
                .as_ref()
                .map(Q27SourceBuildEvidence::Provenance),
            &usage,
            false,
        );
        apply_q27_sampler_context(&mut schema.definitions, settings);
        Ok(schema)
    }

    async fn settings_schema(
        &self,
        runtime: &InstalledRuntime,
        model: &ModelArtifact,
        host: &HostCapabilities,
        settings: Option<&norted_core::ResolvedSettings>,
    ) -> Result<SettingsSchema, EngineError> {
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
        let mut schema = q27_settings_schema_from_usage(
            Some(runtime.manifest.runtime_id.clone()),
            &runtime.manifest.identity,
            &runtime.manifest.acquisition_method,
            runtime
                .manifest
                .source_build
                .as_ref()
                .map(Q27SourceBuildEvidence::Provenance),
            &usage,
            true,
        );
        let facts = inspect_q27_model(&model.path).map_err(EngineError::InvalidConfiguration)?;
        apply_q27_model_capabilities(&mut schema.definitions, &facts);
        apply_q27_profile_defaults(&mut schema.definitions, settings);
        apply_q27_sampler_context(&mut schema.definitions, settings);
        apply_q27_context_defaults(&mut schema.definitions, runtime, model, host, settings);
        Ok(schema)
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
        self.configured_executions.write().await.remove(&endpoint);
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
        let capabilities = q27_runtime_capabilities(
            &request.runtime.manifest.identity,
            &request.runtime.manifest.acquisition_method,
            request
                .runtime
                .manifest
                .source_build
                .as_ref()
                .map(Q27SourceBuildEvidence::Provenance),
        );
        if q27_has_configured_execution(&request.settings) {
            validate_q27_settings_prelaunch(&request.settings, capabilities).map_err(|reason| {
                EngineError::InvalidConfiguration(format!(
                    "selected q27 runtime cannot satisfy the effective settings: {reason}"
                ))
            })?;
        }
        request
            .settings_schema
            .validate(&request.settings)
            .map_err(|error| EngineError::InvalidConfiguration(error.to_string()))?;
        let structured = translate_q27_settings(&request.settings, &[], &self.environment)?;
        let model_path = canonical_regular_file(&request.model.primary.path, "q27 model").await?;
        if model_path != request.model.primary.path {
            return Err(EngineError::InvalidConfiguration(
                "prepared q27 model path changed before launch".to_owned(),
            ));
        }
        let model_facts =
            inspect_q27_model(&model_path).map_err(EngineError::InvalidConfiguration)?;
        validate_q27_model_settings(&model_facts, &request.settings)
            .map_err(EngineError::InvalidConfiguration)?;
        if setting_toggle(&request.settings, "q27.mtp") == Some(true)
            && !model_facts.capabilities.contains("mtp_layer_1")
        {
            return Err(EngineError::InvalidConfiguration(
                "q27 MTP was enabled but bounded artifact inspection did not prove an MTP layer"
                    .to_owned(),
            ));
        }
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
        let configured_launch = if q27_has_configured_execution(&request.settings) {
            let modes = q27_configured_kv_attempt_modes(capabilities, &request.settings);
            if settings_require_selected_kv(&request.settings) && modes.is_empty() {
                return Err(EngineError::InvalidConfiguration(
                    "selected q27 runtime proves none of the configured KV modes".to_owned(),
                ));
            }
            Some(q27_configured_launch(
                &request.settings,
                capabilities.compiled_w_max,
                modes.first().copied(),
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
        if let Some(configured_launch) = configured_launch.as_ref() {
            arguments.extend(configured_launch.arguments.iter().cloned());
        }
        arguments.extend(structured.arguments);
        let binding = request.accelerator_binding.ok_or_else(|| {
            EngineError::InvalidConfiguration(
                "q27 launch has no exact NVIDIA GPU selected by compatibility evaluation"
                    .to_owned(),
            )
        })?;
        let [accelerator] = binding.devices.as_slice() else {
            return Err(EngineError::InvalidConfiguration(
                "q27 requires an accelerator binding containing exactly one NVIDIA GPU".to_owned(),
            ));
        };
        let mut environment = q27_launch_environment(&self.environment, accelerator)?;
        if let Some(configured_launch) = configured_launch.as_ref() {
            environment.extend(configured_launch.environment.clone());
        }
        let mut environment_remove = managed_environment_removals();
        environment_remove.extend(structured.environment_remove);
        let normalized_settings = configured_launch
            .map(|launch| launch.normalized_settings)
            .unwrap_or_default();
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
            settings: request.settings,
            native_arguments: Vec::new(),
            installation,
            runtime: request.runtime,
            model: request.model,
            accelerator_binding: Some(binding),
        })
    }

    async fn build_launch_attempts(
        &self,
        request: LaunchRequest,
    ) -> Result<Vec<LaunchSpec>, EngineError> {
        let settings = request.settings.clone();
        let capabilities = q27_runtime_capabilities(
            &request.runtime.manifest.identity,
            &request.runtime.manifest.acquisition_method,
            request
                .runtime
                .manifest
                .source_build
                .as_ref()
                .map(Q27SourceBuildEvidence::Provenance),
        );
        let first = self.build_launch_spec(request).await?;
        if !settings_require_selected_kv(&settings) {
            return Ok(vec![first]);
        }
        let modes = q27_configured_kv_attempt_modes(capabilities, &settings);
        if modes.is_empty() {
            return Err(EngineError::InvalidConfiguration(
                "selected q27 runtime proves none of the configured KV modes".to_owned(),
            ));
        }
        Ok(modes
            .into_iter()
            .map(|mode| {
                let mut attempt = first.clone();
                attempt.environment.remove("Q27_KV");
                attempt.arguments.retain(|argument| argument != "--kv-fp16");
                if mode == Q27KvMode::Fp16 {
                    attempt.arguments.push(OsString::from("--kv-fp16"));
                } else {
                    attempt
                        .environment
                        .insert("Q27_KV".to_owned(), mode.as_str().to_owned());
                }
                attempt
                    .normalized_settings
                    .insert("requested_kv_mode".to_owned(), json!(mode.as_str()));
                attempt
            })
            .collect())
    }

    async fn prepare_launch_attempt(&self, spec: &LaunchSpec) -> Result<(), EngineError> {
        self.prepare_launch_attempt_inner(spec, None).await
    }

    async fn prepare_launch_attempt_with_progress(
        &self,
        spec: &LaunchSpec,
        progress: LoadProgressReporter,
    ) -> Result<(), EngineError> {
        self.prepare_launch_attempt_inner(spec, Some(&progress))
            .await
    }

    fn prepare_launch_progress(&self, spec: &LaunchSpec) -> Option<BackendLoadProgress> {
        spec.model.primary.norted_package.as_ref().map(|_| {
            BackendLoadProgress::with_message(
                BackendLoadPhase::PreparingLaunch,
                "Revalidating package before launch",
            )
        })
    }

    async fn clear_launch_state(&self, endpoint: Option<&str>) {
        if let Some(endpoint) = endpoint {
            self.configured_executions.write().await.remove(endpoint);
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
        let executions = self.configured_executions.read().await;
        let Some(execution) = executions.get(endpoint) else {
            return Ok(StartupObservation::Ready(BTreeMap::new()));
        };
        let observed = parse_q27_startup_observation(stderr_tail)?;
        if let Some(selected_kv_mode) = execution.selected_kv_mode
            && observed.kv_mode != selected_kv_mode.as_str()
        {
            return Err(EngineError::Operation(format!(
                "q27 startup selected KV mode `{}` instead of requested mode `{}`",
                observed.kv_mode,
                selected_kv_mode.as_str()
            )));
        }
        let compiled_w_max = execution.compiled_w_max.ok_or_else(|| {
            EngineError::Operation(
                "q27 startup has no immutable exact-runtime compiled W_MAX authority".to_owned(),
            )
        })?;
        if observed
            .auto_compiled_w_max
            .is_some_and(|observed| observed != compiled_w_max)
        {
            return Err(EngineError::Operation(format!(
                "q27 auto-context diagnostic W_MAX {} disagrees with exact-runtime compiled W_MAX {compiled_w_max}",
                observed.auto_compiled_w_max.expect("checked")
            )));
        }
        if (observed.suffix_drafting
            || setting_choice(&execution.settings, "q27.suffix_width_mode")
                == Some("runtime_w_max"))
            && observed.suffix_width != compiled_w_max
        {
            return Err(EngineError::Operation(format!(
                "q27 startup applied suffix width {} instead of exact-runtime compiled W_MAX {compiled_w_max}",
                observed.suffix_width
            )));
        }
        let thinking_mismatch = setting_toggle(&execution.settings, "q27.thinking")
            .is_some_and(|expected| observed.thinking != expected);
        let fast_head_mismatch = setting_toggle(&execution.settings, "q27.fast_head")
            .is_some_and(|expected| observed.fast_head != expected);
        let depth_mismatch = setting_unsigned(&execution.settings, "q27.mtp_max_depth")
            .is_some_and(|expected| observed.maximum_mtp_depth != expected.to_string());
        let probability_mismatch = setting_float(&execution.settings, "q27.mtp_min_probability")
            .is_some_and(|expected| {
                !approximately_equal(observed.mtp_minimum_probability(), expected)
            });
        let suffix_mismatch = setting_toggle(&execution.settings, "q27.suffix_drafting")
            .is_some_and(|expected| observed.suffix_drafting != expected);
        if thinking_mismatch
            || fast_head_mismatch
            || depth_mismatch
            || probability_mismatch
            || suffix_mismatch
        {
            return Err(EngineError::Operation(
                "q27 startup observation disagrees with the effective thinking/MTP/fast-head settings"
                    .to_owned(),
            ));
        }
        if let Some((requested_context, source)) =
            setting_unsigned_with_source(&execution.settings, "q27.context_length")
        {
            if observed.initial_auto_context.is_some() {
                return Err(EngineError::Operation(
                    "q27 emitted automatic context sizing for an explicit structured `--ctx` launch"
                        .to_owned(),
                ));
            }
            if observed.served_context != requested_context {
                let changed = if observed.served_context < requested_context {
                    "reduced"
                } else {
                    "changed"
                };
                return Err(EngineError::Operation(format!(
                    "q27 started with context {} but {} explicitly requested {requested_context}; the runtime {changed} the requested context during startup",
                    observed.served_context,
                    setting_source_label(source)
                )));
            }
        }
        let ready_slot_count = observed.slot_contexts.len() as u64;
        if let Some((requested_slots, source)) =
            setting_unsigned_with_source(&execution.settings, "q27.parallel_requests")
            && ready_slot_count != requested_slots
        {
            let changed = if ready_slot_count < requested_slots {
                "reduced"
            } else {
                "changed"
            };
            return Err(EngineError::Operation(format!(
                "q27 started {ready_slot_count} ready slots but {} explicitly requested {requested_slots}; the runtime {changed} the requested slot count during startup",
                setting_source_label(source)
            )));
        }
        if let Some((requested_background_context, source)) =
            setting_unsigned_with_source(&execution.settings, "q27.slot1_context_length")
        {
            if let Some((slot, final_context)) = observed
                .slot_contexts
                .iter()
                .find(|(slot, context)| **slot > 0 && **context != requested_background_context)
            {
                let changed = if *final_context < requested_background_context {
                    "reduced"
                } else {
                    "changed"
                };
                return Err(EngineError::Operation(format!(
                    "q27 started background slot {slot} with context {final_context} but {} explicitly requested {requested_background_context}; the runtime {changed} the requested background context during startup",
                    setting_source_label(source)
                )));
            }
        }
        let background_context = observed.slot_contexts.get(&1).copied();
        if let Some(expected) = background_context
            && let Some((slot, context)) = observed
                .slot_contexts
                .iter()
                .find(|(slot, context)| **slot > 1 && **context != expected)
        {
            return Err(EngineError::Operation(format!(
                "q27 background slot {slot} started with context {context} instead of the slot-1 context {expected}"
            )));
        }
        let mut proof = BTreeMap::from([
            (
                "observed_served_context".to_owned(),
                json!(observed.served_context),
            ),
            (
                "observed_kv_mode".to_owned(),
                json!(observed.kv_mode.clone()),
            ),
            ("observed_ready_slots".to_owned(), json!(ready_slot_count)),
            (
                "observed_slot_contexts".to_owned(),
                json!(observed.slot_contexts),
            ),
            (
                "observed_suffix_width".to_owned(),
                json!(observed.suffix_width),
            ),
            ("compiled_w_max".to_owned(), json!(compiled_w_max)),
            (
                "compiled_w_max_authority".to_owned(),
                json!("exact-runtime-capability"),
            ),
        ]);
        if let Some(initial_auto_context) = observed.initial_auto_context {
            proof.insert(
                "initial_auto_context_estimate".to_owned(),
                json!(initial_auto_context),
            );
        }
        if let Some(auto_kv_mode) = observed.auto_kv_mode.as_deref() {
            proof.insert("auto_context_kv_mode".to_owned(), json!(auto_kv_mode));
        }
        let mut resolved_settings = serde_json::Map::from_iter([
            (
                "q27.context_length".to_owned(),
                json!(observed.served_context),
            ),
            ("q27.parallel_requests".to_owned(), json!(ready_slot_count)),
            ("q27.kv_mode".to_owned(), json!(observed.kv_mode)),
            ("q27.fast_head".to_owned(), json!(observed.fast_head)),
            ("q27.thinking".to_owned(), json!(observed.thinking)),
            (
                "q27.mtp_max_depth".to_owned(),
                json!(observed.maximum_mtp_depth),
            ),
            (
                "q27.mtp_min_probability".to_owned(),
                json!(observed.mtp_minimum_probability()),
            ),
            (
                "q27.suffix_drafting".to_owned(),
                json!(observed.suffix_drafting),
            ),
            ("q27.suffix_width_mode".to_owned(), json!("runtime_w_max")),
        ]);
        if let Some(background_context) = background_context {
            resolved_settings.insert(
                "q27.slot1_context_length".to_owned(),
                json!(background_context),
            );
        }
        proof.insert(
            "resolved_settings".to_owned(),
            Value::Object(resolved_settings),
        );
        if execution.sharp_template.is_some() {
            proof.insert(
                "sharp_application".to_owned(),
                json!("pretokenized-raw-prompt"),
            );
        }
        Ok(StartupObservation::Ready(proof))
    }

    fn startup_progress(&self, stderr_tail: &[String]) -> Option<BackendLoadProgress> {
        parse_q27_startup_progress(stderr_tail)
    }

    async fn effective_generation_settings(
        &self,
        process: &ProcessDescriptor,
    ) -> Result<EffectiveGenerationSettings, EngineError> {
        // q27 has no effective-config endpoint. Use the reviewed runtime defaults
        // plus explicit process configuration; omitted request fields stay omitted.
        let configured = if let Some(endpoint) = process.endpoint.as_deref() {
            self.configured_executions
                .read()
                .await
                .get(endpoint)
                .cloned()
        } else {
            None
        };
        Ok(configured.map_or(
            EffectiveGenerationSettings {
                stop_token_ids: None,
                required_stop_token_ids: Vec::new(),
                temperature: 0.0,
                top_p: 1.0,
            },
            |execution| EffectiveGenerationSettings {
                stop_token_ids: None,
                required_stop_token_ids: Vec::new(),
                temperature: setting_float(&execution.settings, "q27.temperature")
                    .or_else(|| {
                        setting_float(&execution.settings, "q27.force_temperature")
                            .filter(|value| *value > 0.0)
                    })
                    .unwrap_or(0.0),
                top_p: setting_float(&execution.settings, "q27.top_p")
                    .or_else(|| {
                        setting_float(&execution.settings, "q27.force_top_p")
                            .filter(|value| *value < 1.0)
                    })
                    .unwrap_or(1.0),
            },
        ))
    }

    async fn complete(
        &self,
        endpoint: &str,
        request: norted_engine::CompletionRequest,
    ) -> Result<InferenceOutput, EngineError> {
        let body = self.raw_completion_body(endpoint, &request, false).await?;
        self.send_completion(endpoint, "/v1/completions", body, false)
            .await
    }
    async fn complete_stream(
        &self,
        endpoint: &str,
        request: norted_engine::CompletionRequest,
        _activity: InferenceActivityReporter,
    ) -> Result<InferenceStream, EngineError> {
        let body = self.raw_completion_body(endpoint, &request, true).await?;
        self.send_completion_stream(endpoint, "/v1/completions", body, false)
            .await
    }
    async fn infer(
        &self,
        endpoint: &str,
        request: InferenceRequest,
    ) -> Result<InferenceOutput, EngineError> {
        let configured = self
            .configured_executions
            .read()
            .await
            .get(endpoint)
            .cloned();
        let (route, body) = if let Some(execution) = configured.as_ref() {
            Self::configured_backend_request(execution, &request, false)?
        } else {
            (
                "/v1/chat/completions",
                self.backend_request(&request, false)?,
            )
        };
        self.send_completion(
            endpoint,
            route,
            body,
            configured.as_ref().is_some_and(|execution| {
                setting_choice(&execution.settings, "q27.response_filter")
                    == Some("strip_initial_reasoning")
            }),
        )
        .await
    }

    async fn infer_stream(
        &self,
        endpoint: &str,
        request: InferenceRequest,
        _activity: InferenceActivityReporter,
    ) -> Result<InferenceStream, EngineError> {
        let configured = self
            .configured_executions
            .read()
            .await
            .get(endpoint)
            .cloned();
        let (route, body) = if let Some(execution) = configured.as_ref() {
            Self::configured_backend_request(execution, &request, true)?
        } else {
            (
                "/v1/chat/completions",
                self.backend_request(&request, true)?,
            )
        };
        self.send_completion_stream(
            endpoint,
            route,
            body,
            configured.as_ref().is_some_and(|execution| {
                setting_choice(&execution.settings, "q27.response_filter")
                    == Some("strip_initial_reasoning")
            }),
        )
        .await
    }
}

async fn read_configured_template(
    settings: &norted_core::ResolvedSettings,
) -> Result<String, EngineError> {
    let Some(SettingValue::Path(path)) = settings.value("q27.template_path") else {
        return Err(EngineError::InvalidConfiguration(
            "q27 external template mode requires `q27.template_path`".to_owned(),
        ));
    };
    let Some(SettingValue::String(expected_sha256)) = settings.value("q27.template_sha256") else {
        return Err(EngineError::InvalidConfiguration(
            "q27 external template mode requires a recorded `q27.template_sha256`".to_owned(),
        ));
    };
    let file = tokio::fs::File::open(path).await.map_err(|error| {
        EngineError::InvalidConfiguration(format!(
            "could not open configured template {}: {error}",
            path.display()
        ))
    })?;
    let mut bytes = Vec::new();
    file.take(SHARP_TEMPLATE_LIMIT + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|error| {
            EngineError::InvalidConfiguration(format!(
                "could not read configured template {}: {error}",
                path.display()
            ))
        })?;
    if bytes.len() as u64 > SHARP_TEMPLATE_LIMIT {
        return Err(EngineError::InvalidConfiguration(
            "configured template exceeded the local size limit".to_owned(),
        ));
    }
    if format!("{:x}", Sha256::digest(&bytes)) != *expected_sha256 {
        return Err(EngineError::InvalidConfiguration(
            "configured template SHA-256 no longer matches the Model Profile".to_owned(),
        ));
    }
    String::from_utf8(bytes).map_err(|_| {
        EngineError::InvalidConfiguration("configured template is not UTF-8".to_owned())
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
    render_generation_prompt: bool,
    thinking_enabled: bool,
    reasoning_effort: Option<&str>,
) -> Result<String, EngineError> {
    let mut environment = sharp_environment();
    environment
        .add_template("sharp", template_source)
        .map_err(|error| {
            EngineError::InvalidConfiguration(format!(
                "configured Sharp template is not supported by the exact Jinja renderer: {error}"
            ))
        })?;
    let template = environment.get_template("sharp").map_err(|error| {
        EngineError::InvalidConfiguration(format!(
            "configured Sharp template could not be loaded: {error}"
        ))
    })?;
    let messages = messages.iter().map(sharp_message_json).collect::<Vec<_>>();
    template
        .render(context! {
            messages => messages,
            add_generation_prompt => render_generation_prompt,
            enable_thinking => thinking_enabled,
            reasoning_effort => reasoning_effort,
            tools => Vec::<Value>::new(),
        })
        .map_err(|error| {
            EngineError::InvalidConfiguration(format!("configured Sharp rendering failed: {error}"))
        })
}

fn validate_configured_template(
    settings: &norted_core::ResolvedSettings,
    template_source: &str,
) -> Result<(), EngineError> {
    let rendered = render_sharp_template(
        template_source,
        &[InferenceMessage::text(
            InferenceRole::User,
            "Sharp startup validation",
        )],
        setting_toggle(settings, "q27.render_generation_prompt").unwrap_or(true),
        setting_toggle(settings, "q27.template_thinking")
            .or_else(|| setting_toggle(settings, "q27.thinking"))
            .unwrap_or(false),
        setting_choice(settings, "q27.reasoning_effort"),
    )?;
    if rendered.is_empty() {
        return Err(EngineError::InvalidConfiguration(
            "configured template produced an empty raw prompt during bounded validation".to_owned(),
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct Q27StartupObservation {
    served_context: u64,
    slot_contexts: BTreeMap<u64, u64>,
    initial_auto_context: Option<u64>,
    auto_kv_mode: Option<String>,
    auto_compiled_w_max: Option<u64>,
    kv_mode: String,
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
        .find(|line| line.starts_with("--ctx auto:"));
    let mut slot_contexts = BTreeMap::new();
    for line in stderr_tail {
        let Some(rest) = line.strip_prefix("slot ") else {
            continue;
        };
        let Some((slot, context)) = rest.split_once(" ready: ctx=") else {
            continue;
        };
        let slot = slot.parse::<u64>().map_err(|_| {
            EngineError::Operation("q27 ready slot number was not numeric".to_owned())
        })?;
        let context = context.trim().parse::<u64>().map_err(|_| {
            EngineError::Operation("q27 ready slot context was not numeric".to_owned())
        })?;
        if slot_contexts.insert(slot, context).is_some() {
            return Err(EngineError::Operation(format!(
                "q27 startup reported ready slot {slot} more than once"
            )));
        }
    }
    let served_context = slot_contexts.get(&0).copied().ok_or_else(|| {
        EngineError::BackendUnavailable(
            "q27 served slot context has not been observed yet".to_owned(),
        )
    })?;
    for (expected, observed) in (0_u64..).zip(slot_contexts.keys().copied()) {
        if expected != observed {
            return Err(EngineError::Operation(format!(
                "q27 startup ready slots were not contiguous at slot {expected}"
            )));
        }
    }

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

    let (initial_auto_context, auto_kv_mode, auto_compiled_w_max) = if let Some(auto) = auto {
        let rest = auto.strip_prefix("--ctx auto:").expect("matched prefix");
        let mut words = rest.split_whitespace();
        let first = words.next();
        let context = match first {
            Some("only") => words.next(),
            value => value,
        }
        .and_then(|value| value.parse::<u64>().ok())
        .ok_or_else(|| {
            EngineError::Operation("q27 initial auto-context estimate was not numeric".to_owned())
        })?;
        let compiled_w_max = startup_parenthesized_value(auto, "W_MAX=")?
            .parse::<u64>()
            .map_err(|_| {
                EngineError::Operation("q27 auto-context W_MAX was not numeric".to_owned())
            })?;
        let kv_mode = ["fp8", "turbo5k", "turbo3", "turbo3v", "fp16"]
            .into_iter()
            .find(|mode| auto.contains(&format!("{mode} KV")))
            .ok_or_else(|| {
                EngineError::Operation("q27 auto-context KV mode was not recognized".to_owned())
            })?
            .to_owned();
        if kv_mode != profile_kv {
            return Err(EngineError::Operation(format!(
                "q27 startup KV profile `{profile_kv}` disagrees with auto-context mode `{kv_mode}`"
            )));
        }
        (Some(context), Some(kv_mode), Some(compiled_w_max))
    } else {
        (None, None, None)
    };
    Ok(Q27StartupObservation {
        served_context,
        slot_contexts,
        initial_auto_context,
        auto_kv_mode,
        auto_compiled_w_max,
        kv_mode: profile_kv.to_owned(),
        suffix_width,
        maximum_mtp_depth,
        mtp_minimum_probability_bits: pmin.to_bits(),
        suffix_drafting: suffix_enabled == "1",
        fast_head,
        thinking,
    })
}

/// Best-effort UX progress from the exact q27 v0.10.0 startup lines. q27 does
/// not expose a trustworthy numerator/denominator during model loading, so
/// every observation is an indeterminate phase; the strict correctness checks
/// remain in `parse_q27_startup_observation` and are unaffected by this.
fn parse_q27_startup_progress(stderr_tail: &[String]) -> Option<BackendLoadProgress> {
    if stderr_tail.len() > 80 || stderr_tail.iter().any(|line| line.len() > 4_096) {
        return None;
    }
    if let Some(slot) = stderr_tail
        .iter()
        .rev()
        .find(|line| line.starts_with("slot 0 ready: ctx="))
    {
        let message = slot
            .strip_prefix("slot 0 ready: ctx=")
            .and_then(|value| value.trim().parse::<u64>().ok())
            .map_or_else(
                || "Verifying q27 startup profile".to_owned(),
                |context| format!("Verifying q27 startup profile (context {context})"),
            );
        return Some(BackendLoadProgress::with_message(
            BackendLoadPhase::VerifyingStartup,
            message,
        ));
    }
    if let Some(auto) = stderr_tail
        .iter()
        .rev()
        .find(|line| line.starts_with("--ctx auto:"))
    {
        let message = auto
            .strip_prefix("--ctx auto:")
            .and_then(|rest| rest.split_whitespace().next())
            .and_then(|value| value.parse::<u64>().ok())
            .map_or_else(
                || "Preparing slots".to_owned(),
                |context| format!("Preparing slots (context {context})"),
            );
        return Some(BackendLoadProgress::with_message(
            BackendLoadPhase::AllocatingContext,
            message,
        ));
    }
    if let Some(profile) = stderr_tail
        .iter()
        .rev()
        .find(|line| line.starts_with("profile:"))
    {
        let message = startup_token(profile, "kv=").map_or_else(
            |_| "Resolving KV and context".to_owned(),
            |kv| format!("Resolving context ({kv} KV)"),
        );
        return Some(BackendLoadProgress::with_message(
            BackendLoadPhase::AllocatingContext,
            message,
        ));
    }
    if stderr_tail.is_empty() {
        None
    } else {
        Some(BackendLoadProgress::with_message(
            BackendLoadPhase::LoadingModel,
            "Loading model weights",
        ))
    }
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

async fn prepare_q27_model_input(
    model: &ModelArtifact,
    progress: Option<&LoadProgressReporter>,
) -> Result<PreparedModelInput, EngineError> {
    if model.format != ArtifactFormat::Q27 {
        return Err(EngineError::InvalidConfiguration(
            "q27 can only prepare Q27 model artifacts".to_owned(),
        ));
    }
    inspect_q27_model(&model.path).map_err(EngineError::InvalidConfiguration)?;
    if model.norted_package.is_some() {
        let prepared = match progress {
            Some(progress) => prepare_norted_package_input_with_progress(model, progress).await?,
            None => prepare_norted_package_input(model).await?,
        };
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
    #[serde(default)]
    tool_calls: Vec<ChatToolCall>,
}

#[derive(Deserialize)]
struct ChatToolCall {
    id: String,
    function: ChatToolFunction,
}

#[derive(Deserialize)]
struct ChatToolFunction {
    name: String,
    arguments: String,
}

#[derive(Debug, Clone, Deserialize)]
struct ChatUsage {
    prompt_tokens: u64,
    completion_tokens: u64,
    total_tokens: u64,
    // q27 0.10.0's routed chat usage reports this top-level field.
    // It is a subset of completion_tokens, never an additional population.
    reasoning_tokens: Option<u64>,
}

impl From<ChatUsage> for InferenceUsage {
    fn from(usage: ChatUsage) -> Self {
        Self {
            input_tokens: usage.prompt_tokens,
            output_tokens: usage.completion_tokens,
            total_tokens: usage.total_tokens,
            cached_input_tokens: None,
            cache_write_input_tokens: None,
            prompt_processing_tokens: None,
            prompt_processing_ms: None,
            reasoning_output_tokens: usage.reasoning_tokens,
        }
    }
}

const THINK_OPEN: &str = "<think>";
const THINK_CLOSE: &str = "</think>";

#[derive(Debug, Default)]
struct Q27InitialReasoningFilter {
    reasoning_closed: bool,
    pending: String,
}

impl Q27InitialReasoningFilter {
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

fn filter_q27_initial_reasoning(text: &str) -> String {
    let mut filter = Q27InitialReasoningFilter::default();
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
    initial_reasoning_filter: Option<Q27InitialReasoningFilter>,
    finished: bool,
}

fn q27_sse_stream(
    source: BoxStream<'static, Result<Bytes, reqwest::Error>>,
    filter_profile_output: bool,
) -> InferenceStream {
    let state = SseState {
        source,
        buffer: Vec::new(),
        queued: VecDeque::new(),
        usage: None,
        finish_reason: None,
        initial_reasoning_filter: filter_profile_output.then(Q27InitialReasoningFilter::default),
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
            if let Some(filter) = state.initial_reasoning_filter.as_mut() {
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
        if let Some(choice) = value
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|v| v.first())
        {
            let raw = choice.get("text").and_then(Value::as_str);
            let content = choice
                .get("delta")
                .and_then(|v| v.get("content"))
                .and_then(Value::as_str);
            if let Some(delta) = raw.or(content).filter(|v| !v.is_empty()) {
                // Chat content is already routed by q27's StreamSplitter;
                // waiting for a raw </think> here would discard the answer.
                let delta = if raw.is_some() {
                    state
                        .initial_reasoning_filter
                        .as_mut()
                        .map_or_else(|| delta.to_owned(), |filter| filter.push(delta))
                } else {
                    delta.to_owned()
                };
                if !delta.is_empty() {
                    state
                        .queued
                        .push_back(Ok(InferenceEvent::TextDelta { delta }));
                }
            }
        }
        if let Some(tool_calls) = value
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|choices| choices.first())
            .and_then(|choice| choice.get("delta"))
            .and_then(|delta| delta.get("tool_calls"))
            .and_then(Value::as_array)
        {
            for call in tool_calls {
                let Some(index) = call.get("index").and_then(Value::as_u64) else {
                    state.queued.push_back(Err(EngineError::Operation(
                        "q27 tool-call delta omitted its index".to_owned(),
                    )));
                    state.finished = true;
                    return;
                };
                let index = match u32::try_from(index) {
                    Ok(index) => index,
                    Err(_) => {
                        state.queued.push_back(Err(EngineError::Operation(
                            "q27 tool-call delta index exceeded u32".to_owned(),
                        )));
                        state.finished = true;
                        return;
                    }
                };
                let function = call.get("function");
                state.queued.push_back(Ok(InferenceEvent::ToolCallDelta {
                    index,
                    id: call.get("id").and_then(Value::as_str).map(str::to_owned),
                    name: function
                        .and_then(|function| function.get("name"))
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                    arguments_delta: function
                        .and_then(|function| function.get("arguments"))
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                }));
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
    let mut value = json!({
        "role": match message.role {
            InferenceRole::System | InferenceRole::Developer => "system",
            InferenceRole::User => "user",
            InferenceRole::Assistant => "assistant",
            InferenceRole::Tool => "tool",
        },
        "content": message.text_only().unwrap_or_default(),
    });
    if !message.tool_calls.is_empty() {
        value["tool_calls"] = Value::Array(
            message
                .tool_calls
                .iter()
                .map(|call| {
                    json!({
                        "id": call.id,
                        "type": "function",
                        "function": {"name": call.name, "arguments": call.arguments},
                    })
                })
                .collect(),
        );
        if message.content.is_empty() {
            value["content"] = Value::Null;
        }
    }
    if let Some(tool_call_id) = message.tool_call_id.as_deref() {
        value["tool_call_id"] = json!(tool_call_id);
    }
    value
}

fn sharp_message_json(message: &InferenceMessage) -> Value {
    json!({
        "role": match message.role {
            InferenceRole::System => "system",
            InferenceRole::Developer => "developer",
            InferenceRole::User => "user",
            InferenceRole::Assistant => "assistant",
            InferenceRole::Tool => "tool",
        },
        "content": message.text_only().unwrap_or_default(),
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

fn q27_settings_schema_from_usage(
    runtime_id: Option<RuntimeId>,
    identity: &RuntimeIdentity,
    acquisition: &RuntimeAcquisitionMethod,
    source_evidence: Option<Q27SourceBuildEvidence<'_>>,
    usage: &str,
    include_model_dependent_settings: bool,
) -> SettingsSchema {
    let managed = !matches!(acquisition, RuntimeAcquisitionMethod::ExternalBinary);
    let version = identity.version.as_str();
    let usage = usage.to_ascii_lowercase();
    let capabilities = q27_runtime_capabilities(identity, acquisition, source_evidence);
    let mut definitions = q27_setting_definitions();
    apply_q27_runtime_bounds(&mut definitions, managed, version);
    if capabilities.stable_serving_environment {
        apply_q27_reviewed_runtime_defaults(&mut definitions);
        if let Some(compiled_w_max) = capabilities.compiled_w_max {
            set_q27_default(
                &mut definitions,
                "q27.suffix_width_mode",
                SettingDefaultPreview::new(
                    compiled_w_max.to_string(),
                    SettingDefaultSource::Derived,
                )
                .with_detail("The exact reviewed runtime identity proves its compiled W_MAX"),
            );
        }
    }
    if capabilities.request_seed
        && let Some(seed) = definitions
            .iter_mut()
            .find(|definition| definition.id.as_str() == "q27.seed")
    {
        seed.description =
            "Configured numeric q27 request seed; q27 does not implement the `random` sentinel"
                .to_owned();
        seed.kind = SettingKind::UnsignedIntegerOrChoice {
            minimum: Some(0),
            maximum: Some(u64::from(u32::MAX)),
            choices: Vec::new(),
        };
        seed.default_preview = Some(SettingDefaultPreview::new(
            "0",
            SettingDefaultSource::Runtime,
        ));
    }
    for definition in &mut definitions {
        let option = q27_setting_option(definition.id.as_str());
        let unavailable_by_version = q27_setting_unavailable_by_version(managed, version, option);
        let observed = match definition.id.as_str() {
            "q27.fast_head" => {
                capabilities.fast_head_control
                    || (usage_has_token(&usage, "--fast-head")
                        && usage_has_token(&usage, "--no-fast-head"))
            }
            "q27.thinking" => capabilities.thinking,
            "q27.thinking_budget" => capabilities.unlimited_think_budget,
            "q27.request_thinking" => capabilities.request_thinking,
            "q27.constrain_tools" => capabilities.tool_calling,
            "q27.continuous_batching" | "q27.sampled_graphs" => {
                capabilities.stable_serving_environment
            }
            "q27.temperature" | "q27.top_p" => capabilities.temperature_top_p,
            "q27.top_k" | "q27.min_p" => capabilities.top_k_min_p,
            "q27.seed" => capabilities.request_seed,
            "q27.max_output_tokens" | "q27.system_prompt" => capabilities.trustworthy_identity,
            "q27.reasoning" | "q27.reasoning_budget" => capabilities.request_thinking,
            "q27.reasoning_effort" => {
                include_model_dependent_settings && capabilities.reasoning_effort
            }
            "q27.kv_mode" => !capabilities.supported_kv_modes.is_empty(),
            "q27.mtp" | "q27.mtp_max_depth" | "q27.mtp_min_probability" | "q27.suffix_drafting" => {
                capabilities.mtp_environment
            }
            "q27.suffix_width_mode" => capabilities.compiled_w_max.is_some(),
            "q27.prompt_mode"
            | "q27.prompt_delivery"
            | "q27.template_path"
            | "q27.template_sha256"
            | "q27.render_generation_prompt"
            | "q27.template_thinking"
            | "q27.response_filter" => {
                capabilities.raw_completions && capabilities.exact_sharp_renderer
            }
            id if q27_environment_name(id).is_some() => capabilities.stable_serving_environment,
            "q27.parallel_requests" => {
                capabilities.trustworthy_identity || usage_has_token(&usage, option)
            }
            _ => usage_has_token(&usage, option),
        };
        if unavailable_by_version || !observed {
            definition.supported = false;
            definition.unsupported_reason = Some(if unavailable_by_version {
                format!("q27 runtime {version} predates `{option}`")
            } else if definition.id.as_str() == "q27.fast_head" {
                "the exact q27-server usage contract does not advertise both `--fast-head` and `--no-fast-head`"
                    .to_owned()
            } else if option.is_empty() {
                "the exact q27 runtime does not prove this semantic control".to_owned()
            } else {
                format!("the exact q27-server usage contract does not advertise `{option}`")
            });
        }
    }
    SettingsSchema {
        engine_id: ENGINE_ID.to_owned(),
        runtime_id,
        definitions,
    }
}

fn q27_setting_definitions() -> Vec<SettingDefinition> {
    const COMMON_SETTINGS: &[&str] = &[
        "q27.context_length",
        "q27.parallel_requests",
        "q27.temperature",
        "q27.top_p",
        "q27.top_k",
        "q27.min_p",
        "q27.seed",
        "q27.max_output_tokens",
        "q27.stop_token_ids",
        "q27.system_prompt",
        "q27.reasoning",
        "q27.reasoning_budget",
    ];
    let mut definitions = common_setting_definitions_for(ENGINE_ID, COMMON_SETTINGS);
    if let Some(d) = definitions
        .iter_mut()
        .find(|d| d.id.as_str() == "q27.stop_token_ids")
    {
        d.supported = false;
        d.unsupported_reason = Some("q27 samples and commits blocks before its EOS emission check; exact per-token termination is not proven".to_owned());
    }
    definitions.extend([
        q27_definition(
            "q27.reasoning_effort",
            "Reasoning effort",
            "Qwen3.8 trained-template process default; explicit request effort may override it without changing this setting",
            SettingKind::Choice {
                choices: ["low", "medium", "xhigh"]
                    .into_iter()
                    .map(str::to_owned)
                    .collect(),
            },
            Some("exact Qwen3.8 runtime default"),
        ),
        q27_definition(
            "q27.slot1_context_length",
            "Background slot context length",
            "Context window for slots after slot 0; explicit `context_length` continues to own slot 0",
            SettingKind::UnsignedInteger {
                minimum: Some(1),
                maximum: Some(262_144),
            },
            Some("derived by exact runtime"),
        ),
        q27_definition(
            "q27.kv_mode",
            "KV mode",
            "Use the runtime default, Server-owned automatic quality fallback, or an exact q27 KV mode",
            SettingKind::Choice {
                choices: ["runtime_default", "auto", "fp8", "turbo5k", "turbo3", "fp16"]
                    .into_iter()
                    .map(str::to_owned)
                    .collect(),
            },
            Some("exact runtime default"),
        ),
        q27_definition(
            "q27.fast_head",
            "Fast head",
            "Explicitly enable or disable q27 fast-head behavior",
            SettingKind::Toggle,
            Some("exact runtime profile default"),
        ),
        q27_definition(
            "q27.thinking",
            "Thinking",
            "Explicitly enable or disable q27 thinking",
            SettingKind::Toggle,
            Some("exact runtime profile default"),
        ),
        q27_definition(
            "q27.thinking_budget",
            "Thinking budget",
            "Maximum q27 thinking budget; zero means unlimited where supported",
            SettingKind::UnsignedInteger { minimum: Some(0), maximum: None },
            Some("exact runtime automatic"),
        ),
        q27_definition(
            "q27.request_thinking",
            "Per-request thinking",
            "Allow explicit request thinking enable/disable and budget fields to override the server/profile default",
            SettingKind::Toggle,
            Some("exact runtime default"),
        ),
        q27_definition(
            "q27.constrain_tools",
            "Constrain tool calls",
            "Grammar-constrain eligible greedy automatic tool-call bodies; sampled and forced calls retain upstream behavior",
            SettingKind::Toggle,
            Some("exact runtime default"),
        ),
        q27_definition(
            "q27.continuous_batching",
            "Continuous batching",
            "Explicitly enable or disable q27's stable serving-time continuous batching path",
            SettingKind::Toggle,
            Some("exact runtime profile default"),
        ),
        q27_definition(
            "q27.sampled_graphs",
            "Sampled decoding graphs",
            "Capture sampled decoding graphs; disabling saves VRAM but rejects positive-temperature requests",
            SettingKind::Toggle,
            Some("exact runtime default"),
        ),
        q27_definition(
            "q27.mtp",
            "MTP",
            "Enable or disable q27 multi-token prediction where the exact runtime permits control",
            SettingKind::Toggle,
            Some("exact runtime default"),
        ),
        q27_definition(
            "q27.mtp_max_depth",
            "MTP maximum depth",
            "Maximum q27 MTP proposal depth",
            SettingKind::UnsignedInteger { minimum: Some(1), maximum: None },
            Some("exact runtime automatic"),
        ),
        q27_definition(
            "q27.mtp_min_probability",
            "MTP minimum probability",
            "Minimum probability accepted by q27 MTP",
            SettingKind::Float { minimum: Some(0.0), maximum: Some(1.0) },
            Some("exact runtime default"),
        ),
        q27_definition(
            "q27.suffix_drafting",
            "Suffix drafting",
            "Enable q27 suffix drafting",
            SettingKind::Toggle,
            Some("exact runtime default"),
        ),
        q27_definition(
            "q27.suffix_width_mode",
            "Suffix width",
            "Use the runtime default or the exact runtime's compiled W_MAX",
            SettingKind::Choice {
                choices: ["runtime_default", "runtime_w_max"]
                    .into_iter()
                    .map(str::to_owned)
                    .collect(),
            },
            Some("derived by exact runtime"),
        ),
        q27_definition(
            "q27.prompt_mode",
            "Template mode",
            "Use the runtime prompt path or render an explicitly selected local template",
            SettingKind::Choice {
                choices: ["runtime_default", "external_template"]
                    .into_iter()
                    .map(str::to_owned)
                    .collect(),
            },
            Some("Norted default: runtime template"),
        ),
        q27_definition(
            "q27.prompt_delivery",
            "Prompt delivery",
            "Deliver runtime chat messages or a Server-rendered raw completion prompt",
            SettingKind::Choice {
                choices: ["runtime_chat", "raw_completions"]
                    .into_iter()
                    .map(str::to_owned)
                    .collect(),
            },
            Some("Norted default: runtime chat"),
        ),
        q27_definition(
            "q27.template_path",
            "Template path",
            "Any compatible local Sharp/Jinja template file",
            SettingKind::Path,
            Some("Norted default: none"),
        ),
        q27_definition(
            "q27.template_sha256",
            "Template SHA-256",
            "Recorded content identity for the selected local template",
            SettingKind::String,
            Some("Norted default: none"),
        ),
        q27_definition(
            "q27.render_generation_prompt",
            "Render generation prompt",
            "Ask the external template to append its generation prompt",
            SettingKind::Toggle,
            Some("Norted default: enabled"),
        ),
        q27_definition(
            "q27.template_thinking",
            "Template thinking",
            "Pass positive thinking state to the external template",
            SettingKind::Toggle,
            Some("Norted default: disabled"),
        ),
        q27_definition(
            "q27.response_filter",
            "Response filter",
            "Optionally strip the initial reasoning block and transition from a raw completion",
            SettingKind::Choice {
                choices: ["none", "strip_initial_reasoning"]
                    .into_iter()
                    .map(str::to_owned)
                    .collect(),
            },
            Some("Norted default: none"),
        ),
        q27_definition(
            "q27.prefix_cache_path",
            "Prefix cache path",
            "Directory for q27's persistent prefix cache",
            SettingKind::Path,
            Some("exact runtime default"),
        ),
        q27_definition(
            "q27.prefix_cache_max_gb",
            "Prefix cache disk budget",
            "Persistent prefix-cache LRU disk budget",
            SettingKind::Float {
                minimum: Some(0.0),
                maximum: None,
            },
            Some("exact runtime default"),
        ),
        q27_definition(
            "q27.prefix_cache_min_tokens",
            "Prefix cache minimum",
            "Shortest prefix eligible for persistence",
            SettingKind::UnsignedInteger {
                minimum: Some(1),
                maximum: None,
            },
            Some("exact runtime default"),
        ),
        q27_definition(
            "q27.prefix_cache_max_tokens",
            "Prefix cache maximum",
            "Largest prefix staged for persistence",
            SettingKind::UnsignedInteger {
                minimum: Some(1),
                maximum: None,
            },
            Some("exact runtime default"),
        ),
        q27_definition(
            "q27.prefix_cache_step_tokens",
            "Prefix cache step",
            "Token growth required before re-persisting a conversation",
            SettingKind::UnsignedInteger {
                minimum: Some(1),
                maximum: None,
            },
            Some("exact runtime default"),
        ),
        q27_definition(
            "q27.prefix_cache_ram_gb",
            "Prefix cache RAM budget",
            "Pinned host-RAM prefix-cache tier budget",
            SettingKind::Float {
                minimum: Some(0.0),
                maximum: None,
            },
            Some("exact runtime default"),
        ),
    ]);
    definitions.extend(q27_reviewed_environment_definitions());
    definitions
}

fn q27_reviewed_environment_definitions() -> Vec<SettingDefinition> {
    let toggle = |id, label, description| {
        q27_definition(
            id,
            label,
            description,
            SettingKind::Toggle,
            Some("reviewed q27 environment"),
        )
    };
    let unsigned = |id, label, description, minimum| {
        q27_definition(
            id,
            label,
            description,
            SettingKind::UnsignedInteger {
                minimum: Some(minimum),
                maximum: None,
            },
            Some("reviewed q27 environment"),
        )
    };
    let float = |id, label, description, minimum, maximum| {
        q27_definition(
            id,
            label,
            description,
            SettingKind::Float { minimum, maximum },
            Some("reviewed q27 environment"),
        )
    };
    vec![
        q27_definition(
            "q27.serving_profile",
            "Serving profile",
            "q27 serving defaults profile",
            SettingKind::Choice {
                choices: vec!["cc".to_owned(), "ref".to_owned()],
            },
            Some("reviewed q27 environment"),
        ),
        float(
            "q27.force_temperature",
            "Forced temperature",
            "Force a temperature when a request omits it",
            Some(0.0),
            None,
        ),
        float(
            "q27.force_top_p",
            "Forced Top P",
            "Force Top P when a request omits it",
            Some(0.0),
            Some(1.0),
        ),
        toggle(
            "q27.batch_graphs",
            "Batch graphs",
            "Capture fused continuous-batching CUDA graphs",
        ),
        unsigned(
            "q27.batch_graph_capacity",
            "Batch graph capacity",
            "Maximum fused graph executable cache entries before headroom shrinking",
            1,
        ),
        q27_definition(
            "q27.batch_gemm",
            "Batch GEMM",
            "Fused batch GEMM policy",
            SettingKind::Choice {
                choices: vec!["auto".to_owned(), "on".to_owned(), "off".to_owned()],
            },
            Some("reviewed q27 environment"),
        ),
        toggle(
            "q27.kv_pooling",
            "KV pooling",
            "Use the process-wide paged KV pool",
        ),
        toggle(
            "q27.shared_prefill_arena",
            "Shared prefill arena",
            "Use one process-wide prefill scratch arena",
        ),
        toggle(
            "q27.draft_early_exit",
            "Draft early exit",
            "Stop confidence-gated drafting as soon as its margin gate trips",
        ),
        toggle(
            "q27.plain_sampling",
            "Plain sampling",
            "Use the non-speculative sampler path for sampled requests",
        ),
        toggle(
            "q27.tool_split",
            "Tool split",
            "Use the split constrained-tool decode path",
        ),
        toggle(
            "q27.gpu_interleaving",
            "GPU interleaving",
            "Interleave multi-slot work at round granularity",
        ),
        toggle(
            "q27.phase_stats",
            "Phase statistics",
            "Log per-request draft and verify phase statistics",
        ),
        unsigned(
            "q27.readiness_floor_mib",
            "Ready VRAM floor",
            "Warn when free VRAM at readiness is below this threshold",
            0,
        ),
        float(
            "q27.thinking_budget_fraction",
            "Thinking budget fraction",
            "Fraction of request max tokens used for the automatic thinking budget",
            Some(f64::MIN_POSITIVE),
            Some(1.0 - f64::EPSILON),
        ),
        unsigned(
            "q27.suffix_min_match",
            "Suffix minimum match",
            "Minimum matching suffix length required before suffix drafting",
            1,
        ),
        unsigned(
            "q27.prefill_batch_min",
            "Prefill batch threshold",
            "Minimum prompt length routed through chunked prefill",
            2,
        ),
        unsigned(
            "q27.gemm_min_width",
            "GEMM minimum width",
            "Verify width at which q27 changes from GEMV to GEMM",
            1,
        ),
        q27_definition(
            "q27.prefill_split_k",
            "Prefill split-K",
            "q27 split-K prefill GEMM policy",
            SettingKind::Choice {
                choices: vec!["auto".to_owned(), "on".to_owned(), "off".to_owned()],
            },
            Some("reviewed q27 environment"),
        ),
        q27_definition(
            "q27.prefill_kernel",
            "Prefill kernel",
            "q27 prefill GEMM implementation",
            SettingKind::Choice {
                choices: vec!["mma".to_owned(), "dp4a".to_owned(), "fp4".to_owned()],
            },
            Some("reviewed q27 environment"),
        ),
        q27_definition(
            "q27.prefill_activation_group",
            "Prefill activation group",
            "Activation regroup width for prefill",
            SettingKind::Choice {
                choices: vec!["32".to_owned(), "64".to_owned()],
            },
            Some("reviewed q27 environment"),
        ),
        q27_definition(
            "q27.prefill_token_tile",
            "Prefill token tile",
            "Fixed prefill GEMM token tile; zero keeps automatic dispatch",
            SettingKind::Choice {
                choices: vec![
                    "auto".to_owned(),
                    "16".to_owned(),
                    "32".to_owned(),
                    "64".to_owned(),
                    "128".to_owned(),
                ],
            },
            Some("reviewed q27 environment"),
        ),
        q27_definition(
            "q27.decode_attention",
            "Decode attention",
            "Speculative verify/decode attention implementation",
            SettingKind::Choice {
                choices: vec![
                    "auto".to_owned(),
                    "fd2".to_owned(),
                    "v1".to_owned(),
                    "mma".to_owned(),
                ],
            },
            Some("reviewed q27 environment"),
        ),
        q27_definition(
            "q27.delta_scan_mode",
            "Delta scan mode",
            "Recurrent delta-scan implementation",
            SettingKind::Choice {
                choices: vec!["wy".to_owned(), "seq".to_owned()],
            },
            Some("reviewed q27 environment"),
        ),
        q27_definition(
            "q27.delta_scan_split",
            "Delta scan split",
            "Delta-scan split count; one restores the exact reference path",
            SettingKind::Choice {
                choices: vec![
                    "auto".to_owned(),
                    "1".to_owned(),
                    "2".to_owned(),
                    "4".to_owned(),
                    "8".to_owned(),
                ],
            },
            Some("reviewed q27 environment"),
        ),
        unsigned(
            "q27.checkpoint_interval",
            "Checkpoint interval",
            "Token interval between q27 recurrent-state checkpoints",
            1,
        ),
        unsigned(
            "q27.checkpoint_slots",
            "Checkpoint slots",
            "Number of recurrent-state checkpoint slots",
            1,
        ),
        toggle(
            "q27.adaptive_depth_reset",
            "Adaptive-depth reset",
            "Reset the adaptive MTP-depth controller for every request",
        ),
        float(
            "q27.adaptive_depth_ema",
            "Adaptive-depth EMA",
            "Adaptive MTP-depth controller EMA coefficient",
            Some(0.0),
            Some(1.0),
        ),
        float(
            "q27.adaptive_depth_high",
            "Adaptive-depth high",
            "Adaptive MTP-depth high threshold",
            None,
            None,
        ),
        float(
            "q27.adaptive_depth_high6",
            "Adaptive-depth high 6",
            "Adaptive MTP depth-6 high threshold",
            None,
            None,
        ),
        float(
            "q27.adaptive_depth_high7",
            "Adaptive-depth high 7",
            "Adaptive MTP depth-7 high threshold",
            None,
            None,
        ),
        float(
            "q27.adaptive_depth_floor6",
            "Adaptive-depth floor 6",
            "Adaptive MTP depth-6 floor threshold",
            None,
            None,
        ),
        float(
            "q27.adaptive_depth_floor7",
            "Adaptive-depth floor 7",
            "Adaptive MTP depth-7 floor threshold",
            None,
            None,
        ),
        float(
            "q27.adaptive_depth_low",
            "Adaptive-depth low",
            "Adaptive MTP-depth low threshold",
            None,
            None,
        ),
        q27_definition(
            "q27.tool_dialect",
            "Tool dialect",
            "Use model-derived tool-call syntax or force JSON/XML",
            SettingKind::Choice {
                choices: vec!["auto".to_owned(), "json".to_owned(), "xml".to_owned()],
            },
            Some("reviewed q27 environment"),
        ),
        toggle(
            "q27.strict_tool_parser",
            "Strict tool parser",
            "Disable tolerant tool-call parser recovery",
        ),
        toggle(
            "q27.tool_error_warnings",
            "Tool error warnings",
            "Inject tool-error recovery warnings",
        ),
        unsigned(
            "q27.max_tool_argument_chars",
            "Tool argument limit",
            "Maximum rendered tool-call argument characters; zero is unlimited",
            0,
        ),
        unsigned(
            "q27.max_tool_response_chars",
            "Tool response limit",
            "Maximum rendered XML tool-response characters; zero is unlimited",
            0,
        ),
        toggle(
            "q27.bare_system_prompt",
            "Bare system prompt",
            "Do not inject q27's fallback system prompt when the request omits one",
        ),
    ]
}

fn q27_model_setting_definitions(
    model: &ModelArtifact,
) -> Result<Vec<SettingDefinition>, EngineError> {
    let facts = inspect_q27_model(&model.path).map_err(EngineError::InvalidConfiguration)?;
    let mut definitions = q27_setting_definitions();
    apply_q27_model_capabilities(&mut definitions, &facts);
    Ok(definitions)
}

fn apply_q27_reviewed_runtime_defaults(definitions: &mut [SettingDefinition]) {
    for (id, value) in [
        ("q27.context_length", "auto"),
        ("q27.parallel_requests", "1"),
        ("q27.temperature", "0.0"),
        ("q27.top_p", "1.0"),
        ("q27.top_k", "0"),
        ("q27.min_p", "0.0"),
        ("q27.max_output_tokens", "8192"),
        ("q27.reasoning", "off"),
        ("q27.reasoning_budget", "auto"),
        ("q27.reasoning_effort", "xhigh"),
        ("q27.slot1_context_length", "auto"),
        ("q27.kv_mode", "auto"),
        ("q27.fast_head", "enabled"),
        ("q27.thinking", "disabled"),
        ("q27.request_thinking", "disabled"),
        ("q27.constrain_tools", "disabled"),
        ("q27.continuous_batching", "enabled"),
        ("q27.sampled_graphs", "enabled"),
        ("q27.mtp", "enabled"),
        ("q27.mtp_max_depth", "auto"),
        ("q27.mtp_min_probability", "0.5"),
        ("q27.suffix_drafting", "enabled"),
        ("q27.prefix_cache_path", "disabled"),
        ("q27.prefix_cache_max_gb", "20 GB"),
        ("q27.prefix_cache_min_tokens", "4096"),
        ("q27.prefix_cache_max_tokens", "32768"),
        ("q27.prefix_cache_step_tokens", "8192"),
        ("q27.prefix_cache_ram_gb", "disabled"),
        ("q27.serving_profile", "cc"),
        ("q27.force_temperature", "0.0"),
        ("q27.force_top_p", "1.0"),
        ("q27.batch_graphs", "enabled"),
        ("q27.batch_graph_capacity", "64"),
        ("q27.batch_gemm", "auto"),
        ("q27.kv_pooling", "enabled"),
        ("q27.shared_prefill_arena", "enabled"),
        ("q27.draft_early_exit", "enabled"),
        ("q27.plain_sampling", "disabled"),
        ("q27.tool_split", "disabled"),
        ("q27.gpu_interleaving", "enabled"),
        ("q27.phase_stats", "enabled"),
        ("q27.readiness_floor_mib", "192 MiB"),
        ("q27.thinking_budget_fraction", "0.5"),
        ("q27.suffix_min_match", "12"),
        ("q27.prefill_batch_min", "2"),
        ("q27.gemm_min_width", "9"),
        ("q27.prefill_split_k", "auto"),
        ("q27.prefill_kernel", "mma"),
        ("q27.prefill_activation_group", "64"),
        ("q27.prefill_token_tile", "auto"),
        ("q27.decode_attention", "auto"),
        ("q27.delta_scan_mode", "wy"),
        ("q27.delta_scan_split", "auto"),
        ("q27.checkpoint_interval", "4096"),
        ("q27.checkpoint_slots", "16"),
        ("q27.adaptive_depth_reset", "disabled"),
        ("q27.adaptive_depth_ema", "0.0625"),
        ("q27.adaptive_depth_high", "0.5"),
        ("q27.adaptive_depth_high6", "0.6"),
        ("q27.adaptive_depth_high7", "0.6"),
        ("q27.adaptive_depth_floor6", "0.45"),
        ("q27.adaptive_depth_floor7", "0.45"),
        ("q27.adaptive_depth_low", "0.35"),
        ("q27.tool_dialect", "auto"),
        ("q27.strict_tool_parser", "disabled"),
        ("q27.tool_error_warnings", "disabled"),
        ("q27.max_tool_argument_chars", "unlimited"),
        ("q27.max_tool_response_chars", "unlimited"),
        ("q27.bare_system_prompt", "disabled"),
    ] {
        set_q27_default(
            definitions,
            id,
            SettingDefaultPreview::new(value, SettingDefaultSource::Runtime),
        );
    }
    set_q27_default(
        definitions,
        "q27.context_length",
        SettingDefaultPreview::new("auto", SettingDefaultSource::Runtime).with_detail(
            "q27 sizes context from live free VRAM and startup reservations after model weights load; compact KV is capped at 262144 and FP16 at 131072",
        ),
    );
    set_q27_default(
        definitions,
        "q27.slot1_context_length",
        SettingDefaultPreview::new("auto", SettingDefaultSource::Runtime).with_detail(
            "When context is automatic, q27 sizes later slots to the live-VRAM-derived slot-0 window",
        ),
    );
    set_q27_default(
        definitions,
        "q27.kv_mode",
        SettingDefaultPreview::new("auto", SettingDefaultSource::Runtime)
            .with_detail("The exact runtime selects KV format from the accelerator architecture"),
    );
    set_q27_default(
        definitions,
        "q27.continuous_batching",
        SettingDefaultPreview::new("enabled", SettingDefaultSource::Runtime).with_detail(
            "Enabled by the reviewed runtime profile; q27 may auto-disable it when runtime compatibility requires the solo path",
        ),
    );
    set_q27_default(
        definitions,
        "q27.mtp_max_depth",
        SettingDefaultPreview::new("auto", SettingDefaultSource::Runtime).with_detail(
            "The reviewed runtime chooses proposal depth automatically with a maximum of 7",
        ),
    );
    set_q27_default(
        definitions,
        "q27.thinking_budget",
        SettingDefaultPreview::new("auto", SettingDefaultSource::Runtime).with_detail(
            "For prompt-seeded thinking, q27 derives the budget from the request's maximum output at request time",
        ),
    );
    set_q27_default(
        definitions,
        "q27.reasoning_budget",
        SettingDefaultPreview::new("auto", SettingDefaultSource::Runtime).with_detail(
            "When request thinking is enabled, an omitted request budget remains request-derived",
        ),
    );
}

fn apply_q27_sampler_context(
    definitions: &mut [SettingDefinition],
    settings: Option<&norted_core::ResolvedSettings>,
) {
    let Some(settings) = settings else {
        return;
    };
    for (force, regular, active) in [
        (
            "q27.force_temperature",
            "q27.temperature",
            setting_float(settings, "q27.force_temperature").filter(|value| *value > 0.0),
        ),
        (
            "q27.force_top_p",
            "q27.top_p",
            setting_float(settings, "q27.force_top_p").filter(|value| *value < 1.0),
        ),
    ] {
        if let Some(value) = active {
            set_q27_default(
                definitions,
                regular,
                SettingDefaultPreview::new(value.to_string(), SettingDefaultSource::Derived)
                    .with_detail(format!(
                        "Runtime omitted-request behavior with `{force}` configured"
                    )),
            );
        }
    }
    if setting_float(settings, "q27.force_temperature").is_some_and(|value| value > 0.0) {
        set_q27_default(definitions, "q27.seed", SettingDefaultPreview::new("auto", SettingDefaultSource::Runtime)
            .with_detail("Runtime assigns a new counter seed per sampled request when the force-temperature policy is active"));
    }
}

fn apply_q27_profile_defaults(
    definitions: &mut [SettingDefinition],
    settings: Option<&norted_core::ResolvedSettings>,
) {
    if settings.and_then(|settings| setting_choice(settings, "q27.serving_profile")) != Some("ref")
    {
        return;
    }
    for (id, value) in [
        ("q27.fast_head", "disabled"),
        ("q27.thinking", "enabled"),
        ("q27.continuous_batching", "disabled"),
        ("q27.kv_mode", "fp16"),
        ("q27.suffix_drafting", "disabled"),
        ("q27.prefill_batch_min", "32"),
        ("q27.decode_attention", "fd2"),
    ] {
        set_q27_default(
            definitions,
            id,
            SettingDefaultPreview::new(value, SettingDefaultSource::Runtime)
                .with_detail("Selected q27 reference serving profile"),
        );
    }
}

fn set_q27_default(
    definitions: &mut [SettingDefinition],
    id: &str,
    preview: SettingDefaultPreview,
) {
    if let Some(definition) = definitions
        .iter_mut()
        .find(|definition| definition.id.as_str() == id)
    {
        definition.default_preview = Some(preview);
    }
}

fn apply_q27_context_defaults(
    definitions: &mut [SettingDefinition],
    runtime: &InstalledRuntime,
    model: &ModelArtifact,
    host: &HostCapabilities,
    settings: Option<&norted_core::ResolvedSettings>,
) {
    let capabilities = q27_runtime_capabilities(
        &runtime.manifest.identity,
        &runtime.manifest.acquisition_method,
        runtime
            .manifest
            .source_build
            .as_ref()
            .map(Q27SourceBuildEvidence::Provenance),
    );
    if !capabilities.stable_serving_environment {
        return;
    }

    let configured_mode = settings
        .and_then(|settings| setting_choice(settings, "q27.kv_mode"))
        .or_else(|| {
            (settings.and_then(|settings| setting_choice(settings, "q27.serving_profile"))
                == Some("ref"))
            .then_some("fp16")
        });
    let default_mode = if matches!(configured_mode, Some(mode) if mode != "runtime_default") {
        None
    } else {
        self_selected_q27_accelerator(runtime, model, host)
            .and_then(|accelerator| accelerator.compute_capability)
            .map(|capability| {
                let architecture = u32::from(capability.major) * 10 + u32::from(capability.minor);
                if architecture >= 89 {
                    Q27KvMode::Fp8
                } else if architecture >= 80 {
                    Q27KvMode::Turbo5k
                } else {
                    Q27KvMode::Fp16
                }
            })
    };
    if let Some(mode) = default_mode {
        set_q27_default(
            definitions,
            "q27.kv_mode",
            SettingDefaultPreview::new(mode.as_str(), SettingDefaultSource::Derived).with_detail(
                "Derived before launch from the reviewed q27 architecture contract and the selected accelerator's numeric compute capability",
            ),
        );
    } else if configured_mode.is_none() || configured_mode == Some("runtime_default") {
        set_q27_default(
            definitions,
            "q27.kv_mode",
            SettingDefaultPreview::new("auto", SettingDefaultSource::Runtime).with_detail(
                "The exact runtime selects KV format from accelerator architecture; no authoritative compute capability is available before launch",
            ),
        );
    }

    let effective_mode = configured_mode
        .and_then(|mode| {
            [
                Q27KvMode::Fp8,
                Q27KvMode::Turbo5k,
                Q27KvMode::Turbo3,
                Q27KvMode::Fp16,
            ]
            .into_iter()
            .find(|known| known.as_str() == mode)
        })
        .or(default_mode);
    let context_cap = if effective_mode == Some(Q27KvMode::Fp16) {
        131_072
    } else {
        262_144
    };
    let context = SettingDefaultPreview::new("auto", SettingDefaultSource::Runtime).with_detail(
        format!(
            "q27 sizes context from live free VRAM and startup reservations after model weights load; the selected KV policy caps it at {context_cap}"
        ),
    );
    set_q27_default(definitions, "q27.context_length", context.clone());
    set_q27_default(
        definitions,
        "q27.slot1_context_length",
        SettingDefaultPreview::new("auto", SettingDefaultSource::Runtime).with_detail(
            "When context is automatic, the reviewed runtime sizes later slots to the live-VRAM-derived slot-0 window",
        ),
    );
    if let Some(compiled_w_max) = capabilities.compiled_w_max {
        set_q27_default(
            definitions,
            "q27.suffix_width_mode",
            SettingDefaultPreview::new(compiled_w_max.to_string(), SettingDefaultSource::Derived)
                .with_detail("The exact reviewed runtime identity proves its compiled W_MAX"),
        );
    }
}

fn self_selected_q27_accelerator(
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

fn apply_q27_model_capabilities(
    definitions: &mut [SettingDefinition],
    facts: &model::Q27ModelFacts,
) {
    for definition in definitions {
        let (unsupported, reason) = if (definition.id.as_str().starts_with("q27.mtp")
            || definition.id.as_str().starts_with("q27.suffix"))
            && !facts.capabilities.contains("mtp_layer_1")
        {
            (
                true,
                "bounded q27 artifact inspection did not prove an MTP prediction layer",
            )
        } else if definition.id.as_str() == "q27.reasoning_effort"
            && !facts.capabilities.contains("qwen38_trained_template")
        {
            (
                true,
                "bounded q27 artifact metadata did not prove the Qwen3.8 trained-template contract",
            )
        } else {
            (false, "")
        };
        if unsupported {
            definition.supported = false;
            definition.unsupported_reason = Some(reason.to_owned());
        }
    }
}

fn validate_q27_model_settings(
    facts: &model::Q27ModelFacts,
    settings: &norted_core::ResolvedSettings,
) -> Result<(), String> {
    if settings.value("q27.reasoning_effort").is_some()
        && !facts.capabilities.contains("qwen38_trained_template")
    {
        return Err(
            "q27.reasoning_effort requires bounded artifact metadata proving the Qwen3.8 trained-template contract"
                .to_owned(),
        );
    }
    let contradicted = (!facts.capabilities.contains("mtp_layer_1"))
        .then(|| {
            settings.configured.keys().find(|id| {
                id.as_str().starts_with("q27.mtp") || id.as_str().starts_with("q27.suffix")
            })
        })
        .flatten();
    match contradicted {
        Some(id) => Err(format!(
            "q27 setting `{id}` requires an MTP prediction layer, but bounded artifact inspection did not prove one"
        )),
        None => Ok(()),
    }
}

fn q27_definition(
    id: &str,
    label: &str,
    description: &str,
    kind: SettingKind,
    _runtime_note: Option<&str>,
) -> SettingDefinition {
    SettingDefinition {
        id: SettingId::new(id).expect("static q27 setting ID"),
        label: label.to_owned(),
        description: description.to_owned(),
        kind,
        scope: SettingScope::Runtime {
            engine_id: ENGINE_ID.to_owned(),
        },
        category: q27_setting_category(id),
        supported: true,
        unsupported_reason: None,
        unit: None,
        default_preview: match id {
            "q27.prompt_mode" => Some(SettingDefaultPreview::new(
                "runtime_default",
                SettingDefaultSource::Norted,
            )),
            "q27.prompt_delivery" => Some(SettingDefaultPreview::new(
                "runtime_chat",
                SettingDefaultSource::Norted,
            )),
            "q27.template_path" | "q27.template_sha256" => Some(SettingDefaultPreview::new(
                "None",
                SettingDefaultSource::Norted,
            )),
            "q27.render_generation_prompt" => Some(SettingDefaultPreview::new(
                "enabled",
                SettingDefaultSource::Norted,
            )),
            "q27.template_thinking" => Some(SettingDefaultPreview::new(
                "disabled",
                SettingDefaultSource::Norted,
            )),
            "q27.response_filter" => Some(SettingDefaultPreview::new(
                "none",
                SettingDefaultSource::Norted,
            )),
            _ => None,
        },
    }
}

fn q27_setting_category(id: &str) -> SettingCategory {
    if id.contains("thinking") || id.contains("reasoning") {
        SettingCategory::Reasoning
    } else if id.contains("prompt") || id.contains("template") || id.contains("response_filter") {
        SettingCategory::Prompt
    } else if id.contains("mtp") || id.contains("suffix") {
        SettingCategory::Speculation
    } else if id.contains("kv_") || id.contains("q27.context_length") {
        SettingCategory::KvMemory
    } else if id.contains("cache") {
        SettingCategory::Cache
    } else {
        SettingCategory::Advanced
    }
}

fn q27_setting_option(id: &str) -> &'static str {
    match id {
        "q27.context_length" => "--ctx",
        "q27.parallel_requests" => "--slots",
        "q27.slot1_context_length" => "--slot1-ctx",
        "q27.fast_head" => "--fast-head",
        "q27.request_thinking" => "--request-think",
        "q27.constrain_tools" => "--constrain-tools",
        "q27.prefix_cache_path" => "--prefix-cache",
        "q27.prefix_cache_max_gb" => "--prefix-cache-max-gb",
        "q27.prefix_cache_min_tokens" => "--prefix-cache-min",
        "q27.prefix_cache_max_tokens" => "--prefix-cache-max-tokens",
        "q27.prefix_cache_step_tokens" => "--prefix-cache-step",
        "q27.prefix_cache_ram_gb" => "--prefix-cache-ram-gb",
        _ => "",
    }
}

fn q27_environment_name(id: &str) -> Option<&'static str> {
    Some(match id {
        "q27.serving_profile" => "Q27_PROFILE",
        "q27.force_temperature" => "Q27_FORCE_TEMP",
        "q27.force_top_p" => "Q27_FORCE_TOP_P",
        "q27.batch_graphs" => "Q27_BATCH_GRAPH",
        "q27.batch_graph_capacity" => "Q27_BATCH_GRAPH_CAP",
        "q27.batch_gemm" => "Q27_BATCH_GEMM",
        "q27.kv_pooling" => "Q27_KV_POOL",
        "q27.shared_prefill_arena" => "Q27_PF_ARENA",
        "q27.draft_early_exit" => "Q27_DEXIT",
        "q27.plain_sampling" => "Q27_SAMPLE_PLAIN",
        "q27.tool_split" => "Q27_TOOL_SPLIT",
        "q27.gpu_interleaving" => "Q27_NO_INTERLEAVE",
        "q27.phase_stats" => "Q27_PHASE_STATS",
        "q27.readiness_floor_mib" => "Q27_READY_FLOOR_MB",
        "q27.thinking_budget_fraction" => "Q27_THINK_BUDGET_FRAC",
        "q27.reasoning_effort" => "Q27_REASONING_EFFORT",
        "q27.suffix_min_match" => "Q27_SUFFIX_L",
        "q27.prefill_batch_min" => "Q27_PF_BATCH_MIN",
        "q27.gemm_min_width" => "Q27_GEMM_MIN",
        "q27.prefill_split_k" => "Q27_GEMM_SPLITK",
        "q27.prefill_kernel" => "Q27_PREFILL",
        "q27.prefill_activation_group" => "Q27_PF_XG",
        "q27.prefill_token_tile" => "Q27_PF_NT",
        "q27.decode_attention" => "Q27_FD",
        "q27.delta_scan_mode" => "Q27_DS_MODE",
        "q27.delta_scan_split" => "Q27_DS_SPLIT",
        "q27.checkpoint_interval" => "Q27_CKPT_INTERVAL",
        "q27.checkpoint_slots" => "Q27_CKPT_SLOTS",
        "q27.adaptive_depth_reset" => "Q27_MAXD_RESET",
        "q27.adaptive_depth_ema" => "Q27_MAXD_EMA",
        "q27.adaptive_depth_high" => "Q27_MAXD_HI",
        "q27.adaptive_depth_high6" => "Q27_MAXD_HI6",
        "q27.adaptive_depth_high7" => "Q27_MAXD_HI7",
        "q27.adaptive_depth_floor6" => "Q27_MAXD_FLO6",
        "q27.adaptive_depth_floor7" => "Q27_MAXD_FLO7",
        "q27.adaptive_depth_low" => "Q27_MAXD_LO",
        "q27.tool_dialect" => "Q27_TOOL_DIALECT",
        "q27.strict_tool_parser" => "Q27_TOOL_STRICT",
        "q27.tool_error_warnings" => "Q27_TOOL_ERROR_WARNINGS",
        "q27.max_tool_argument_chars" => "Q27_MAX_TOOL_ARG_CHARS",
        "q27.max_tool_response_chars" => "Q27_MAX_TOOL_RESPONSE_CHARS",
        "q27.bare_system_prompt" => "Q27_BARE",
        _ => return None,
    })
}

fn q27_setting_unavailable_by_version(managed: bool, version: &str, option: &str) -> bool {
    managed
        && ((option == "--prefix-cache-ram-gb" && !version_at_least(version, 0, 6, 1))
            || (option.starts_with("--prefix-cache") && !version_at_least(version, 0, 6, 0)))
}

fn apply_q27_runtime_bounds(definitions: &mut [SettingDefinition], managed: bool, version: &str) {
    let maximum = managed.then(|| {
        if version_at_least(version, 0, 3, 1) {
            8
        } else {
            4
        }
    });
    if let Some(definition) = definitions
        .iter_mut()
        .find(|definition| definition.id.as_str() == "q27.parallel_requests")
    {
        definition.kind = SettingKind::UnsignedInteger {
            minimum: Some(1),
            maximum,
        };
    }
    if let Some(definition) = definitions
        .iter_mut()
        .find(|definition| definition.id.as_str() == "q27.top_k")
    {
        definition.kind = SettingKind::UnsignedInteger {
            minimum: Some(0),
            maximum: Some(999_999_999),
        };
    }
}

#[derive(Debug)]
struct Q27StructuredArguments {
    arguments: Vec<OsString>,
    environment_remove: Vec<OsString>,
}

fn translate_q27_settings(
    settings: &norted_core::ResolvedSettings,
    native_arguments: &[String],
    _configured_environment: &BTreeMap<String, String>,
) -> Result<Q27StructuredArguments, EngineError> {
    let has_prefix_path = settings.value("q27.prefix_cache_path").is_some();
    if !has_prefix_path
        && settings.configured.keys().any(|id| {
            id.as_str().starts_with("q27.prefix_cache_") && id.as_str() != "q27.prefix_cache_path"
        })
    {
        return Err(EngineError::InvalidConfiguration(
            "q27 prefix-cache tuning settings require `q27.prefix_cache_path` so the runtime cannot silently ignore them"
                .to_owned(),
        ));
    }

    let mut arguments = Vec::new();
    let environment_remove = Vec::new();
    for (id, resolved) in &settings.configured {
        if matches!(
            id.as_str(),
            "q27.temperature"
                | "q27.top_p"
                | "q27.top_k"
                | "q27.min_p"
                | "q27.seed"
                | "q27.max_output_tokens"
                | "q27.system_prompt"
                | "q27.reasoning"
                | "q27.reasoning_budget"
                | "q27.reasoning_effort"
                | "q27.thinking"
                | "q27.thinking_budget"
                | "q27.request_thinking"
                | "q27.constrain_tools"
                | "q27.continuous_batching"
                | "q27.sampled_graphs"
                | "q27.kv_mode"
                | "q27.mtp"
                | "q27.mtp_max_depth"
                | "q27.mtp_min_probability"
                | "q27.suffix_drafting"
                | "q27.suffix_width_mode"
                | "q27.prompt_mode"
                | "q27.prompt_delivery"
                | "q27.template_path"
                | "q27.template_sha256"
                | "q27.render_generation_prompt"
                | "q27.template_thinking"
                | "q27.response_filter"
        ) {
            continue;
        }
        let aliases = match id.as_str() {
            "q27.fast_head" => vec!["--fast-head", "--no-fast-head"],
            value => vec![q27_setting_option(value)],
        };
        if let Some(argument) = find_q27_native_option(native_arguments, &aliases) {
            return Err(EngineError::InvalidConfiguration(format!(
                "structured setting `{id}` conflicts with native q27 argument `{argument}`"
            )));
        }
        match (id.as_str(), &resolved.value) {
            ("q27.context_length", SettingValue::UnsignedInteger(value)) => {
                push_q27_value_argument(&mut arguments, "--ctx", value);
            }
            ("q27.parallel_requests", SettingValue::UnsignedInteger(value)) => {
                push_q27_value_argument(&mut arguments, "--slots", value);
            }
            ("q27.slot1_context_length", SettingValue::UnsignedInteger(value)) => {
                push_q27_value_argument(&mut arguments, "--slot1-ctx", value);
            }
            ("q27.fast_head", SettingValue::Toggle(value)) => {
                arguments.push(OsString::from(if *value {
                    "--fast-head"
                } else {
                    "--no-fast-head"
                }));
            }
            ("q27.prefix_cache_path", SettingValue::Path(value)) => {
                arguments.push(OsString::from("--prefix-cache"));
                arguments.push(value.as_os_str().to_owned());
            }
            ("q27.prefix_cache_max_gb", SettingValue::Float(value)) => {
                push_q27_value_argument(&mut arguments, "--prefix-cache-max-gb", value);
            }
            ("q27.prefix_cache_min_tokens", SettingValue::UnsignedInteger(value)) => {
                push_q27_value_argument(&mut arguments, "--prefix-cache-min", value);
            }
            ("q27.prefix_cache_max_tokens", SettingValue::UnsignedInteger(value)) => {
                push_q27_value_argument(&mut arguments, "--prefix-cache-max-tokens", value);
            }
            ("q27.prefix_cache_step_tokens", SettingValue::UnsignedInteger(value)) => {
                push_q27_value_argument(&mut arguments, "--prefix-cache-step", value);
            }
            ("q27.prefix_cache_ram_gb", SettingValue::Float(value)) => {
                push_q27_value_argument(&mut arguments, "--prefix-cache-ram-gb", value);
            }
            _ => {
                return Err(EngineError::InvalidConfiguration(format!(
                    "setting `{id}` has an invalid value for q27"
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

fn q27_usage_contract_error(output: &str) -> Option<String> {
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
    None
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

fn conflicts_with_managed_environment(name: &str) -> bool {
    is_q27_engine_environment(name)
        || MANAGED_ENVIRONMENT_VARIABLES
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
    let mut names = MANAGED_ENVIRONMENT_VARIABLES
        .iter()
        .map(OsString::from)
        .collect::<Vec<_>>();
    names.extend(std::env::vars_os().filter_map(|(name, _)| {
        name.to_str()
            .is_some_and(is_q27_engine_environment)
            .then_some(name)
    }));
    names.sort_unstable();
    names.dedup();
    names
}

fn is_q27_engine_environment(name: &str) -> bool {
    name.to_ascii_uppercase().starts_with("Q27_")
}

fn map_finish_reason(reason: Option<&str>) -> Result<InferenceFinishReason, EngineError> {
    match reason {
        Some("stop") => Ok(InferenceFinishReason::Stop),
        Some("length") => Ok(InferenceFinishReason::MaxOutputTokens),
        Some("tool_calls") => Ok(InferenceFinishReason::ToolCalls),
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

impl Q27Adapter {
    async fn send_completion(
        &self,
        endpoint: &str,
        route: &str,
        body: Value,
        filter_reasoning: bool,
    ) -> Result<InferenceOutput, EngineError> {
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
        if route == "/v1/completions"
            && (choice.text.is_none() || choice.finish_reason.as_deref() == Some("tool_calls"))
        {
            return Err(EngineError::Operation(
                "raw completion returned an invalid choice".to_owned(),
            ));
        }

        let raw_text = choice.text.is_some();
        let (text, tool_calls) = match (choice.text, choice.message) {
            (Some(text), _) => (text, Vec::new()),
            (None, Some(message)) => (
                message.content.unwrap_or_default(),
                message
                    .tool_calls
                    .into_iter()
                    .map(|call| InferenceToolCall {
                        id: call.id,
                        name: call.function.name,
                        arguments: call.function.arguments,
                    })
                    .collect(),
            ),
            (None, None) => {
                return Err(EngineError::Operation(
                    "q27 response contained neither assistant text nor tool calls".to_owned(),
                ));
            }
        };
        let text = if filter_reasoning && raw_text {
            filter_q27_initial_reasoning(&text)
        } else {
            text
        };
        Ok(InferenceOutput {
            text,
            tool_calls,
            usage: response.usage.map(Into::into),
            finish_reason: map_finish_reason(choice.finish_reason.as_deref())?,
        })
    }

    async fn send_completion_stream(
        &self,
        endpoint: &str,
        route: &str,
        body: Value,
        filter_reasoning: bool,
    ) -> Result<InferenceStream, EngineError> {
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
            filter_reasoning,
        ))
    }
}

impl Q27Adapter {
    async fn raw_completion_body(
        &self,
        endpoint: &str,
        request: &norted_engine::CompletionRequest,
        stream: bool,
    ) -> Result<Value, EngineError> {
        let mut settings = request.generation_settings.clone();
        let executions = self.configured_executions.read().await;
        let execution = executions.get(endpoint).ok_or_else(|| {
            EngineError::Unsupported("unproven q27 raw completion runtime".to_owned())
        })?;
        settings.temperature = settings
            .temperature
            .or_else(|| setting_float(&execution.settings, "q27.temperature"));
        settings.top_p = settings
            .top_p
            .or_else(|| setting_float(&execution.settings, "q27.top_p"));
        settings.top_k = settings
            .top_k
            .or_else(|| setting_unsigned(&execution.settings, "q27.top_k"));
        settings.min_p = settings
            .min_p
            .or_else(|| setting_float(&execution.settings, "q27.min_p"));
        if request.prompt.is_empty() {
            return Err(EngineError::InvalidGenerationSettings(
                "q27 requires a non-empty raw prompt".to_owned(),
            ));
        }
        if !execution.capabilities.raw_completions {
            return Err(EngineError::Unsupported(
                "unproven q27 raw completion runtime".to_owned(),
            ));
        }
        if (settings.seed.is_some() && !execution.capabilities.request_seed)
            || ((settings.top_k.is_some() || settings.min_p.is_some())
                && !execution.capabilities.top_k_min_p)
            || (settings.temperature.is_some_and(|v| v > 0.0)
                && setting_toggle(&execution.settings, "q27.sampled_graphs") == Some(false))
        {
            return Err(EngineError::InvalidGenerationSettings(
                "unsupported q27 raw completion sampling controls".to_owned(),
            ));
        }
        if settings.reasoning_enabled.is_some()
            || settings.reasoning_effort.is_some()
            || settings.reasoning_budget.is_some()
        {
            return Err(EngineError::InvalidGenerationSettings(
                "chat reasoning controls do not apply to raw prompts".to_owned(),
            ));
        }
        let mut body = json!({"model": request.model_profile_id.as_str(), "prompt": request.prompt, "stream": stream});
        for (key, value) in serde_json::to_value(settings)
            .map_err(|e| EngineError::Operation(e.to_string()))?
            .as_object()
            .expect("generation settings object")
        {
            if !value.is_null() {
                body[key] = value.clone();
            }
        }
        if let Some(maximum) = request.max_output_tokens {
            body["max_tokens"] = json!(maximum);
        }
        if stream {
            body["stream_options"] = json!({"include_usage": true});
        }
        Ok(body)
    }
}

#[cfg(test)]
mod tests {
    use norted_core::{
        ModelProfileId, ResolvedSetting, ResolvedSettings, SettingId, SettingSource,
    };

    use super::*;

    fn resolved(values: &[(&str, SettingValue)]) -> ResolvedSettings {
        ResolvedSettings {
            engine_id: ENGINE_ID.to_owned(),
            model_profile_id: None,
            configured: values
                .iter()
                .map(|(id, value)| {
                    (
                        SettingId::new(*id).expect("setting ID"),
                        ResolvedSetting {
                            value: value.clone(),
                            source: SettingSource::Invocation,
                        },
                    )
                })
                .collect(),
            effective: BTreeMap::new(),
        }
    }

    fn exact_capabilities() -> Q27RuntimeCapabilities {
        Q27RuntimeCapabilities {
            trustworthy_identity: true,
            raw_completions: true,
            exact_sharp_renderer: true,
            thinking: true,
            unlimited_think_budget: true,
            temperature_top_p: true,
            top_k_min_p: true,
            request_seed: true,
            request_thinking: true,
            reasoning_effort: true,
            tool_calling: true,
            stable_serving_environment: true,
            mtp_environment: true,
            mtp_disable_control: false,
            fast_head_control: true,
            bounded_startup_observation: true,
            compiled_w_max: Some(12),
            supported_kv_modes: Q27_V062_KV_MODES,
        }
    }

    fn argument_strings(arguments: Vec<OsString>) -> Vec<String> {
        arguments
            .into_iter()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect()
    }

    fn inference_request(generation_settings: GenerationSettingsPatch) -> InferenceRequest {
        InferenceRequest {
            model_profile_id: ModelProfileId::new("profile-alias").expect("profile ID"),
            messages: Vec::new(),
            generation_settings,
            tools: Vec::new(),
            tool_choice: None,
            parallel_tool_calls: None,
            output_format: None,
            max_output_tokens: None,
            stream: false,
        }
    }

    #[test]
    fn ordinary_q27_settings_cover_formerly_hidden_execution_controls() {
        let ids = q27_setting_definitions()
            .into_iter()
            .map(|definition| definition.id.to_string())
            .collect::<std::collections::BTreeSet<_>>();
        for expected in [
            "q27.temperature",
            "q27.top_p",
            "q27.top_k",
            "q27.min_p",
            "q27.thinking",
            "q27.thinking_budget",
            "q27.reasoning_effort",
            "q27.fast_head",
            "q27.kv_mode",
            "q27.mtp",
            "q27.mtp_max_depth",
            "q27.mtp_min_probability",
            "q27.suffix_drafting",
            "q27.suffix_width_mode",
            "q27.prompt_mode",
            "q27.prompt_delivery",
            "q27.template_path",
            "q27.template_sha256",
            "q27.render_generation_prompt",
            "q27.template_thinking",
            "q27.response_filter",
        ] {
            assert!(ids.contains(expected), "missing q27 setting {expected}");
        }
        for absent in [
            "q27.frequency_penalty",
            "q27.presence_penalty",
            "q27.repeat_penalty",
            "q27.structured_output_schema",
            "q27.reasoning_budget_message",
            "q27.stop_strings",
            "q27.context_overflow",
        ] {
            assert!(
                !ids.contains(absent),
                "{absent} must not belong to the reviewed q27 settings surface"
            );
        }
    }

    #[test]
    fn mtp_configuration_directly_drives_capability_validation() {
        let settings = resolved(&[("q27.mtp", SettingValue::Toggle(true))]);
        let mut capabilities = exact_capabilities();
        capabilities.mtp_environment = false;
        assert!(
            validate_q27_settings_prelaunch(&settings, capabilities)
                .unwrap_err()
                .contains("MTP")
        );
        assert!(
            validate_q27_settings_prelaunch(&resolved(&[]), capabilities).is_ok(),
            "an omitted setting must not invent a capability requirement"
        );
    }

    #[test]
    fn no_mtp_model_disables_and_rejects_mtp_dependent_settings_before_runtime() {
        let facts = model::Q27ModelFacts {
            tier: None,
            capabilities: std::collections::BTreeSet::from(["text_only"]),
        };
        let mut definitions = q27_setting_definitions();
        apply_q27_model_capabilities(&mut definitions, &facts);
        for id in [
            "q27.mtp",
            "q27.mtp_max_depth",
            "q27.mtp_min_probability",
            "q27.suffix_drafting",
            "q27.suffix_width_mode",
        ] {
            let definition = definitions
                .iter()
                .find(|definition| definition.id.as_str() == id)
                .expect("MTP-dependent definition");
            assert!(!definition.supported, "{id} must be model-gated");
        }
        let settings = resolved(&[("q27.mtp", SettingValue::Toggle(true))]);
        assert!(
            validate_q27_model_settings(&facts, &settings)
                .unwrap_err()
                .contains("did not prove")
        );
        assert!(validate_q27_model_settings(&facts, &resolved(&[])).is_ok());

        let reasoning = resolved(&[(
            "q27.reasoning_effort",
            SettingValue::Choice("xhigh".to_owned()),
        )]);
        assert!(
            validate_q27_model_settings(&facts, &reasoning)
                .unwrap_err()
                .contains("Qwen3.8")
        );
    }

    #[test]
    fn automatic_kv_selection_is_server_owned_and_proven() {
        let automatic = resolved(&[("q27.kv_mode", SettingValue::Choice("auto".to_owned()))]);
        assert_eq!(
            q27_configured_kv_attempt_modes(exact_capabilities(), &automatic),
            [Q27KvMode::Fp8, Q27KvMode::Turbo3]
        );
        let fp16 = resolved(&[("q27.kv_mode", SettingValue::Choice("fp16".to_owned()))]);
        assert_eq!(
            q27_configured_kv_attempt_modes(exact_capabilities(), &fp16),
            [Q27KvMode::Fp16]
        );
    }

    #[test]
    fn configured_q27_values_translate_without_provenance_checks() {
        let settings = resolved(&[
            ("q27.temperature", SettingValue::Float(0.8)),
            ("q27.top_p", SettingValue::Float(0.9)),
            ("q27.top_k", SettingValue::UnsignedInteger(32)),
            ("q27.min_p", SettingValue::Float(0.05)),
            ("q27.thinking", SettingValue::Toggle(true)),
            ("q27.thinking_budget", SettingValue::UnsignedInteger(0)),
            (
                "q27.reasoning_effort",
                SettingValue::Choice("xhigh".to_owned()),
            ),
            ("q27.mtp", SettingValue::Toggle(true)),
            ("q27.mtp_max_depth", SettingValue::UnsignedInteger(7)),
            ("q27.mtp_min_probability", SettingValue::Float(0.5)),
            ("q27.suffix_drafting", SettingValue::Toggle(true)),
            (
                "q27.suffix_width_mode",
                SettingValue::Choice("runtime_w_max".to_owned()),
            ),
        ]);
        let launch = q27_configured_launch(&settings, Some(12), Some(Q27KvMode::Fp8))
            .expect("configured launch");
        let arguments = argument_strings(launch.arguments);
        assert!(arguments.iter().any(|argument| argument == "--think"));
        assert!(arguments.windows(2).any(|pair| pair == ["--top-k", "32"]));
        assert_eq!(launch.environment["Q27_KV"], "fp8");
        assert_eq!(launch.environment["Q27_REASONING_EFFORT"], "xhigh");
        assert_eq!(launch.environment["Q27_MAXD"], "7");
        assert_eq!(launch.environment["Q27_SUFFIX_W"], "12");

        let inherited_mtp = q27_configured_launch(
            &resolved(&[("q27.mtp_max_depth", SettingValue::UnsignedInteger(3))]),
            Some(12),
            None,
        )
        .expect("MTP subcontrol with runtime MTP default");
        assert_eq!(inherited_mtp.environment["Q27_MAXD"], "3");
    }

    #[tokio::test]
    async fn any_local_template_is_verified_by_content_hash() {
        let directory = tempfile::tempdir().expect("template directory");
        let path = directory.path().join("custom.jinja");
        let bytes = b"{{ messages[0].content }}";
        std::fs::write(&path, bytes).expect("template");
        let digest = format!("{:x}", Sha256::digest(bytes));
        let settings = resolved(&[
            ("q27.template_path", SettingValue::Path(path.clone())),
            ("q27.template_sha256", SettingValue::String(digest)),
        ]);
        assert_eq!(
            read_configured_template(&settings).await.expect("template"),
            String::from_utf8(bytes.to_vec()).unwrap()
        );

        std::fs::write(&path, b"changed").expect("changed template");
        assert!(read_configured_template(&settings).await.is_err());
    }

    #[test]
    fn an_unconfigured_artifact_never_auto_selects_a_template() {
        let settings = resolved(&[]);
        assert!(!q27_has_configured_execution(&settings));
        assert!(settings.value("q27.template_path").is_none());
    }

    #[test]
    fn response_filter_has_generic_semantics() {
        assert_eq!(
            filter_q27_initial_reasoning("private reasoning</think>public answer"),
            "public answer"
        );
        assert_eq!(filter_q27_initial_reasoning("reasoning only"), "");
    }

    #[test]
    fn request_generation_values_override_configured_defaults() {
        let execution = Q27ConfiguredExecution {
            settings: resolved(&[
                ("q27.temperature", SettingValue::Float(0.8)),
                ("q27.top_p", SettingValue::Float(0.9)),
                (
                    "q27.reasoning_effort",
                    SettingValue::Choice("xhigh".to_owned()),
                ),
            ]),
            sharp_template: None,
            compiled_w_max: Some(12),
            selected_kv_mode: None,
            capabilities: exact_capabilities(),
        };
        let (_, body) = Q27Adapter::configured_backend_request(
            &execution,
            &inference_request(GenerationSettingsPatch {
                temperature: Some(0.4),
                top_p: Some(0.7),
                reasoning_effort: Some(norted_engine::ReasoningEffort::Low),
                ..Default::default()
            }),
            false,
        )
        .expect("request body");
        assert_eq!(body["temperature"], 0.4);
        assert_eq!(body["top_p"], 0.7);
        assert_eq!(body["reasoning_effort"], "low");
        assert_eq!(body["model"], "profile-alias");

        let (_, later_body) = Q27Adapter::configured_backend_request(
            &execution,
            &inference_request(GenerationSettingsPatch::default()),
            false,
        )
        .expect("later request body");
        assert!(later_body.get("reasoning_effort").is_none());
        assert_eq!(
            setting_choice(&execution.settings, "q27.reasoning_effort"),
            Some("xhigh")
        );
    }

    #[test]
    fn request_reasoning_effort_is_deliberately_canonicalized() {
        for (effort, expected) in [
            (norted_engine::ReasoningEffort::None, "none"),
            (norted_engine::ReasoningEffort::Minimal, "low"),
            (norted_engine::ReasoningEffort::Low, "low"),
            (norted_engine::ReasoningEffort::Medium, "medium"),
            (norted_engine::ReasoningEffort::High, "xhigh"),
            (norted_engine::ReasoningEffort::Xhigh, "xhigh"),
            (norted_engine::ReasoningEffort::Max, "xhigh"),
        ] {
            assert_eq!(q27_request_reasoning_effort(effort), expected);
        }

        let execution = Q27ConfiguredExecution {
            settings: resolved(&[
                (
                    "q27.reasoning_effort",
                    SettingValue::Choice("xhigh".to_owned()),
                ),
                ("q27.request_thinking", SettingValue::Toggle(true)),
            ]),
            sharp_template: None,
            compiled_w_max: Some(12),
            selected_kv_mode: None,
            capabilities: exact_capabilities(),
        };
        let (_, body) = Q27Adapter::configured_backend_request(
            &execution,
            &inference_request(GenerationSettingsPatch {
                reasoning_effort: Some(norted_engine::ReasoningEffort::None),
                ..Default::default()
            }),
            false,
        )
        .expect("request body");
        assert_eq!(body["reasoning_effort"], "none");
        assert_eq!(body["enable_thinking"], false);
    }

    #[test]
    fn request_reasoning_effort_none_requires_request_thinking() {
        let execution = Q27ConfiguredExecution {
            settings: resolved(&[(
                "q27.reasoning_effort",
                SettingValue::Choice("xhigh".to_owned()),
            )]),
            sharp_template: None,
            compiled_w_max: Some(12),
            selected_kv_mode: None,
            capabilities: exact_capabilities(),
        };
        let error = Q27Adapter::configured_backend_request(
            &execution,
            &inference_request(GenerationSettingsPatch {
                reasoning_effort: Some(norted_engine::ReasoningEffort::None),
                ..Default::default()
            }),
            false,
        )
        .expect_err("q27 would ignore the engine-level disable without --request-think");
        assert!(matches!(error, EngineError::InvalidGenerationSettings(_)));
        assert!(error.to_string().contains("q27.request_thinking"));
    }

    #[test]
    fn configured_reasoning_off_does_not_synthesize_process_effort() {
        let adapter = Q27Adapter::from_config(None, Path::new("."));
        assert!(!adapter.uses_setting_as_request_default("reasoning_effort"));
        assert!(adapter.uses_setting_as_request_default("reasoning_budget"));

        let execution = Q27ConfiguredExecution {
            settings: resolved(&[
                ("q27.reasoning", SettingValue::Choice("off".to_owned())),
                (
                    "q27.reasoning_effort",
                    SettingValue::Choice("xhigh".to_owned()),
                ),
                ("q27.request_thinking", SettingValue::Toggle(true)),
            ]),
            sharp_template: None,
            compiled_w_max: Some(12),
            selected_kv_mode: None,
            capabilities: exact_capabilities(),
        };
        let (_, body) = Q27Adapter::configured_backend_request(
            &execution,
            &inference_request(GenerationSettingsPatch {
                reasoning_enabled: Some(false),
                ..Default::default()
            }),
            false,
        )
        .expect("request default body");
        assert_eq!(body["enable_thinking"], false);
        assert!(body.get("reasoning_effort").is_none());
    }

    #[test]
    fn bounded_startup_parser_uses_final_slots_and_profile_evidence() {
        let observation = parse_q27_startup_observation(&[
            "profile: cc (sm_120) | kv=fp8 fd=mma pmin=0.5 maxd=auto7 suffix=1/w12 fast-head=0 think=1".to_owned(),
            "--ctx auto: 262144 (free 30.0GB post-weights, fp8 KV, W_MAX=12)".to_owned(),
            "[pool] ctx 262144 -> 200704 (pool-entitlable window)".to_owned(),
            "slot 0 ready: ctx=200704".to_owned(),
        ])
        .expect("startup observation");
        assert_eq!(observation.initial_auto_context, Some(262_144));
        assert_eq!(observation.served_context, 200_704);
        assert_eq!(observation.kv_mode, "fp8");
        assert_eq!(observation.auto_compiled_w_max, Some(12));
        assert!(observation.thinking);

        let explicit = parse_q27_startup_observation(&[
            "profile: cc (sm_120) | kv=fp8 fd=mma pmin=0.5 maxd=auto7 suffix=1/w12 fast-head=0 think=1".to_owned(),
            "slot 0 ready: ctx=262144".to_owned(),
            "slot 1 ready: ctx=32768".to_owned(),
        ])
        .expect("explicit-context startup observation");
        assert_eq!(explicit.initial_auto_context, None);
        assert_eq!(explicit.slot_contexts.get(&1), Some(&32_768));
    }

    #[test]
    fn omitted_settings_emit_no_q27_arguments() {
        let translated = translate_q27_settings(&resolved(&[]), &[], &BTreeMap::new())
            .expect("empty translation");
        assert!(translated.arguments.is_empty());
        assert!(translated.environment_remove.is_empty());
    }

    #[test]
    fn raw_q27_controls_are_disabled_and_future_q27_environment_is_reserved() {
        let raw: EngineConfig = serde_json::from_value(json!({
            "enabled": true,
            "native": { "arguments": ["--unclassified"] }
        }))
        .expect("engine config");
        let adapter = Q27Adapter::from_config(Some(&raw), Path::new("."));
        assert!(
            adapter
                .configuration_error
                .as_deref()
                .is_some_and(|error| error.contains("native arguments are disabled"))
        );
        assert!(adapter.native_options().is_empty());

        let mut diagnostic = EngineConfig {
            enabled: true,
            ..Default::default()
        };
        diagnostic
            .env
            .insert("Q27_MPROBE".to_owned(), "1".to_owned());
        let adapter = Q27Adapter::from_config(Some(&diagnostic), Path::new("."));
        assert!(
            adapter
                .configuration_error
                .as_deref()
                .is_some_and(|error| error.contains("Q27_MPROBE"))
        );
        assert!(is_q27_engine_environment("q27_future_control"));
        assert!(!is_q27_engine_environment("ORDINARY_APPLICATION_VALUE"));
    }
}
