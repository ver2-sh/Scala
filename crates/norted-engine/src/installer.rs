use std::collections::BTreeMap;
use std::collections::HashSet;
use std::fs::OpenOptions;
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use flate2::read::GzDecoder;
use futures_util::StreamExt;
use norted_core::{
    AvailableRuntime, InstalledRuntime, RUNTIME_MANIFEST_SCHEMA_VERSION, RuntimeAcquisitionMethod,
    RuntimeAcquisitionPlan, RuntimeArchiveFormat, RuntimeCompatibility, RuntimeManifest,
    RuntimeOperationPhase, RuntimeOperationProgress, RuntimeProbeObservation,
    RuntimeSourceBuildPlan, RuntimeSourceBuildPrerequisites, RuntimeSourceBuildProvenance,
    RuntimeSourceBuildRecipe, RuntimeSourceBuildSystem, RuntimeSourceBuildToolchain,
    effective_cmake_configuration_arguments, is_safe_relative_path,
};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::sync::broadcast;

use crate::catalog::{
    GitHubReleaseAsset, GitHubReleaseClient, RuntimeProviderAuthority, is_allowed_github_host,
};
use crate::store::{RUNTIME_MANIFEST_FILE, RuntimeStaging, RuntimeStore, RuntimeStoreError};
use crate::{EngineError, EngineRegistry};

const MAX_ARCHIVE_ENTRIES: usize = 100_000;
const MAX_EXTRACTED_BYTES: u64 = 32 * 1024 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum RuntimeInstallError {
    #[error("runtime metadata is invalid: {0}")]
    InvalidMetadata(String),
    #[error(
        "runtime `{0}` has no trustworthy SHA-256 package digest; managed installation is refused"
    )]
    MissingDigest(String),
    #[error("runtime download URL is not an approved GitHub release host: {0}")]
    UntrustedUrl(String),
    #[error("runtime download failed: {0}")]
    Download(String),
    #[error("runtime release provenance changed or could not be verified: {0}")]
    SourceChanged(String),
    #[error("runtime source-build prerequisite is not satisfied: {0}")]
    Prerequisite(String),
    #[error("runtime source build failed: {0}")]
    SourceBuild(String),
    #[error("runtime package checksum mismatch: expected {expected}, observed {observed}")]
    ChecksumMismatch { expected: String, observed: String },
    #[error("runtime archive is unsafe or invalid: {0}")]
    UnsafeArchive(String),
    #[error("runtime archive did not contain exactly one expected entrypoint: {0}")]
    Entrypoint(String),
    #[error("runtime engine adapter is unavailable: {0}")]
    Adapter(String),
    #[error("runtime probe failed: {0}")]
    Probe(#[from] EngineError),
    #[error(transparent)]
    Store(#[from] RuntimeStoreError),
    #[error("runtime installation task failed: {0}")]
    Task(String),
}

#[derive(Clone)]
pub struct RuntimeInstaller {
    store: Arc<RuntimeStore>,
    registry: EngineRegistry,
    cache_root: PathBuf,
    github: GitHubReleaseClient,
    authorities: BTreeMap<String, RuntimeProviderAuthority>,
    progress: broadcast::Sender<RuntimeOperationProgress>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub(crate) struct SourceBuildPrerequisiteEvaluationKey {
    recipe: RuntimeSourceBuildRecipe,
    prerequisites: RuntimeSourceBuildPrerequisites,
}

struct CheckedSourceBuildPrerequisites {
    toolchain: RuntimeSourceBuildToolchain,
    effective_cmake_configuration_arguments: Option<Vec<String>>,
}

impl SourceBuildPrerequisiteEvaluationKey {
    pub(crate) fn from_plan(plan: &RuntimeSourceBuildPlan) -> Self {
        Self {
            recipe: plan.recipe.clone(),
            prerequisites: plan.prerequisites.clone(),
        }
    }
}

impl RuntimeInstaller {
    pub fn new(
        store: Arc<RuntimeStore>,
        registry: EngineRegistry,
        cache_root: PathBuf,
        authorities: impl IntoIterator<Item = RuntimeProviderAuthority>,
    ) -> Result<Self, RuntimeInstallError> {
        let github = GitHubReleaseClient::new()
            .map_err(|error| RuntimeInstallError::Download(error.to_string()))?;
        let authorities = authorities
            .into_iter()
            .map(|authority| (authority.provider_id.clone(), authority))
            .collect();
        let (progress, _) = broadcast::channel(64);
        Ok(Self {
            store,
            registry,
            cache_root,
            github,
            authorities,
            progress,
        })
    }

    pub fn subscribe(&self) -> broadcast::Receiver<RuntimeOperationProgress> {
        self.progress.subscribe()
    }

    pub async fn source_build_compatibility(
        &self,
        plan: &RuntimeSourceBuildPlan,
    ) -> RuntimeCompatibility {
        match check_source_build_prerequisites(plan).await {
            Ok(_) => RuntimeCompatibility::Recommended,
            Err(error) => RuntimeCompatibility::NeedsAttention(error.to_string()),
        }
    }

    pub async fn install(
        &self,
        available: &AvailableRuntime,
    ) -> Result<InstalledRuntime, RuntimeInstallError> {
        available
            .validate()
            .map_err(|error| RuntimeInstallError::InvalidMetadata(error.to_string()))?;
        let authority = self
            .authorities
            .get(&available.identity.package.provider_id)
            .ok_or_else(|| {
                RuntimeInstallError::InvalidMetadata(format!(
                    "runtime provider `{}` is not registered as an installation authority",
                    available.identity.package.provider_id
                ))
            })?;
        authority
            .validate(available)
            .map_err(|error| RuntimeInstallError::InvalidMetadata(error.to_string()))?;
        if let RuntimeAcquisitionPlan::SourceBuild(plan) = &available.acquisition {
            return self.install_source_build(available, authority, plan).await;
        }
        let (download, additional_downloads) = available.release_assets().ok_or_else(|| {
            RuntimeInstallError::InvalidMetadata(
                "release runtime is missing its release acquisition plan".to_owned(),
            )
        })?;
        let total_package_size = available.download_size_bytes().ok_or_else(|| {
            RuntimeInstallError::InvalidMetadata(
                "release runtime is missing a package size".to_owned(),
            )
        })?;
        let repository = authority.repository.clone();
        let expected = download
            .digest
            .as_ref()
            .ok_or_else(|| RuntimeInstallError::MissingDigest(available.runtime_id.to_string()))?;
        if !expected.algorithm.eq_ignore_ascii_case("sha256") {
            return Err(RuntimeInstallError::MissingDigest(
                available.runtime_id.to_string(),
            ));
        }
        validate_release_source(available)?;
        self.verify_published_assets(available).await?;
        let primary_asset_name = available
            .identity
            .package
            .asset_name
            .as_deref()
            .ok_or_else(|| {
                RuntimeInstallError::InvalidMetadata("primary asset name is missing".to_owned())
            })?;
        validate_release_asset_url(available, primary_asset_name, download)?;
        let primary_asset_id = parse_asset_id(
            available.identity.package.asset_id.as_deref(),
            primary_asset_name,
        )?;
        let adapter = self
            .registry
            .get(&available.identity.engine_id)
            .ok_or_else(|| RuntimeInstallError::Adapter(available.identity.engine_id.clone()))?;
        let archive = self
            .download(
                available,
                download,
                &repository,
                primary_asset_id,
                &expected.value,
                total_package_size,
            )
            .await?;
        let mut additional_archives = Vec::new();
        let mut additional_digests = Vec::new();
        for (asset, download) in available
            .identity
            .package
            .additional_assets
            .iter()
            .zip(additional_downloads)
        {
            let digest = download.digest.as_ref().ok_or_else(|| {
                RuntimeInstallError::MissingDigest(available.runtime_id.to_string())
            })?;
            if !digest.algorithm.eq_ignore_ascii_case("sha256") {
                return Err(RuntimeInstallError::MissingDigest(
                    available.runtime_id.to_string(),
                ));
            }
            validate_release_asset_url(available, &asset.asset_name, download)?;
            let asset_id = parse_asset_id(Some(&asset.asset_id), &asset.asset_name)?;
            additional_archives.push(
                self.download(
                    available,
                    download,
                    &repository,
                    asset_id,
                    &digest.value,
                    total_package_size,
                )
                .await?,
            );
            additional_digests.push(digest.value.clone());
        }
        self.emit(
            available,
            RuntimeOperationPhase::Extracting,
            None,
            Some(total_package_size),
            "Extracting verified package",
        );
        let staging = self.store.create_staging().await?;
        let result = async {
            let archive_path = archive.clone();
            let format = download.archive_format;
            let mut staging = extract_archive_owned(staging, archive_path, format).await?;
            for (download, archive) in additional_downloads.iter().zip(&additional_archives) {
                let archive_path = archive.clone();
                let format = download.archive_format;
                staging = extract_archive_owned(staging, archive_path, format).await?;
            }
            let entrypoint = locate_entrypoint(&staging, &download.entrypoint_names)
                .map_err(RuntimeInstallError::Entrypoint)?;
            let relative_entrypoint = entrypoint
                .strip_prefix(&staging)
                .map_err(|_| {
                    RuntimeInstallError::Entrypoint(
                        "entrypoint escaped the staging directory".to_owned(),
                    )
                })?
                .to_path_buf();
            if !is_safe_relative_path(&relative_entrypoint) {
                return Err(RuntimeInstallError::Entrypoint(
                    "entrypoint is not a contained relative path".to_owned(),
                ));
            }
            let entrypoint_sha256 = hash_file(&entrypoint)
                .await
                .map_err(|error| RuntimeInstallError::Entrypoint(error.to_string()))?;
            let mut manifest = RuntimeManifest {
                schema_version: RUNTIME_MANIFEST_SCHEMA_VERSION,
                runtime_id: available.runtime_id.clone(),
                identity: available.identity.clone(),
                supported_formats: available.supported_formats.clone(),
                supported_native_identities: available.supported_native_identities.clone(),
                requirements: available.requirements.clone(),
                acquisition_method: RuntimeAcquisitionMethod::OfficialReleaseAsset,
                source_url: Some(available.source_url.clone()),
                downloaded_archive_sha256: Some(expected.value.clone()),
                additional_downloaded_archive_sha256: additional_digests.clone(),
                source_build: None,
                entrypoint: relative_entrypoint,
                entrypoint_sha256,
                installed_at_unix: Some(unix_timestamp()),
                probe: RuntimeProbeObservation {
                    compatible: false,
                    observed_engine_id: available.identity.engine_id.clone(),
                    observed_version: None,
                    observed_revision: None,
                    detail: "probe pending".to_owned(),
                    observed_at_unix: unix_timestamp(),
                },
            };
            let candidate = InstalledRuntime {
                manifest: manifest.clone(),
                installation_root: staging.path().to_path_buf(),
            };
            self.emit(
                available,
                RuntimeOperationPhase::Probing,
                None,
                None,
                "Validating executable through the engine adapter",
            );
            let observation = adapter.probe_runtime(&candidate).await?;
            if !observation.compatible
                || observation.observed_engine_id != available.identity.engine_id
            {
                return Err(RuntimeInstallError::Probe(
                    EngineError::InvalidConfiguration(format!(
                        "runtime probe reported engine `{}` with compatible={}",
                        observation.observed_engine_id, observation.compatible
                    )),
                ));
            }
            let post_probe_sha256 = hash_file(&entrypoint)
                .await
                .map_err(|error| RuntimeInstallError::Entrypoint(error.to_string()))?;
            if post_probe_sha256 != manifest.entrypoint_sha256 {
                return Err(RuntimeInstallError::Probe(
                    EngineError::InvalidConfiguration(
                        "runtime entrypoint changed while its adapter probe was executing"
                            .to_owned(),
                    ),
                ));
            }
            manifest.probe = observation;
            manifest
                .validate()
                .map_err(|error| RuntimeInstallError::InvalidMetadata(error.to_string()))?;
            write_manifest(&staging.join(RUNTIME_MANIFEST_FILE), &manifest).await?;
            self.emit(
                available,
                RuntimeOperationPhase::Installing,
                None,
                None,
                "Atomically activating immutable runtime",
            );
            self.store
                .activate(&mut staging, &manifest)
                .await
                .map_err(Into::into)
        }
        .await;
        match result {
            Ok(runtime) => {
                self.emit(
                    available,
                    RuntimeOperationPhase::Installed,
                    Some(total_package_size),
                    Some(total_package_size),
                    "Runtime installed",
                );
                Ok(runtime)
            }
            Err(error) => {
                self.emit(
                    available,
                    RuntimeOperationPhase::Failed,
                    None,
                    None,
                    &error.to_string(),
                );
                Err(error)
            }
        }
    }

    async fn install_source_build(
        &self,
        available: &AvailableRuntime,
        authority: &RuntimeProviderAuthority,
        plan: &RuntimeSourceBuildPlan,
    ) -> Result<InstalledRuntime, RuntimeInstallError> {
        let result = self
            .install_source_build_inner(available, authority, plan)
            .await;
        if let Err(error) = &result {
            self.emit(
                available,
                RuntimeOperationPhase::Failed,
                None,
                None,
                &error.to_string(),
            );
        }
        result
    }

    async fn install_source_build_inner(
        &self,
        available: &AvailableRuntime,
        authority: &RuntimeProviderAuthority,
        plan: &RuntimeSourceBuildPlan,
    ) -> Result<InstalledRuntime, RuntimeInstallError> {
        self.emit(
            available,
            RuntimeOperationPhase::CheckingPrerequisites,
            None,
            None,
            "Checking source-build prerequisites",
        );
        let prerequisite_check = check_source_build_prerequisites(plan).await?;
        let toolchain = prerequisite_check.toolchain;
        let effective_cmake_configuration_arguments =
            prerequisite_check.effective_cmake_configuration_arguments;

        let repository = self
            .github
            .repository(&authority.repository)
            .await
            .map_err(|error| RuntimeInstallError::SourceChanged(error.to_string()))?;
        if repository.full_name != plan.source.repository
            || repository.html_url != format!("https://github.com/{}", plan.source.repository)
        {
            return Err(RuntimeInstallError::SourceChanged(
                "canonical repository no longer matches the selected source candidate".to_owned(),
            ));
        }
        let live_commit = self
            .github
            .commit(&authority.repository, &plan.source.commit_sha)
            .await
            .map_err(|error| RuntimeInstallError::SourceChanged(error.to_string()))?;
        if live_commit.sha != plan.source.commit_sha
            || live_commit.commit.tree.sha != plan.source.tree_sha
            || live_commit.html_url != available.source_url
        {
            return Err(RuntimeInstallError::SourceChanged(
                "selected commit or Git tree no longer matches the catalog candidate".to_owned(),
            ));
        }

        let adapter = self
            .registry
            .get(&available.identity.engine_id)
            .ok_or_else(|| RuntimeInstallError::Adapter(available.identity.engine_id.clone()))?;
        let mut staging = self.store.create_staging().await?;
        let result = async {
            let source_root = staging.join("source");
            tokio::fs::create_dir_all(&source_root)
                .await
                .map_err(|error| RuntimeInstallError::SourceBuild(error.to_string()))?;
            self.emit(
                available,
                RuntimeOperationPhase::FetchingSource,
                None,
                None,
                "Fetching the exact upstream commit",
            );
            run_source_command(
                "git init",
                "git",
                &["init", "--quiet"],
                Some(&source_root),
                &[],
            )
            .await?;
            run_source_command(
                "git remote configuration",
                "git",
                &["remote", "add", "origin", &plan.source.repository_url],
                Some(&source_root),
                &[],
            )
            .await?;
            run_source_command(
                "git fetch",
                "git",
                &[
                    "fetch",
                    "--quiet",
                    "--depth=1",
                    "origin",
                    &plan.source.commit_sha,
                ],
                Some(&source_root),
                &[],
            )
            .await?;
            run_source_command(
                "git checkout",
                "git",
                &["checkout", "--quiet", "--detach", "FETCH_HEAD"],
                Some(&source_root),
                &[],
            )
            .await?;

            self.emit(
                available,
                RuntimeOperationPhase::VerifyingSource,
                None,
                None,
                "Verifying exact source commit and Git tree",
            );
            let observed_commit =
                command_text("git", &["rev-parse", "HEAD"], Some(&source_root)).await?;
            let observed_tree =
                command_text("git", &["rev-parse", "HEAD^{tree}"], Some(&source_root)).await?;
            verify_source_checkout(
                &plan.source.commit_sha,
                &plan.source.tree_sha,
                &observed_commit,
                &observed_tree,
            )?;
            let audit_root = source_root.clone();
            let build_system = plan.recipe.build_system;
            let build_target = plan.recipe.build_target.clone();
            let build_definition_sha256 = plan.recipe.build_definition_sha256.clone();
            let cmake_configuration_arguments = plan.recipe.cmake_configuration_arguments.clone();
            tokio::task::spawn_blocking(move || {
                inspect_build_dependency_contract(
                    &audit_root,
                    build_system,
                    &build_target,
                    build_definition_sha256.as_deref(),
                    &cmake_configuration_arguments,
                )
            })
            .await
            .map_err(|error| RuntimeInstallError::Task(error.to_string()))??;
            let supported_native_identities = adapter.source_native_identities(&source_root)?;

            let rejected_environment = plan
                .recipe
                .rejected_build_environment
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>();
            match plan.recipe.build_system {
                RuntimeSourceBuildSystem::Cmake => {
                    let build_root = source_root.join("build");
                    self.emit(
                        available,
                        RuntimeOperationPhase::Configuring,
                        None,
                        None,
                        "Configuring the fixed Norted source-build recipe",
                    );
                    let source_text = source_root.to_string_lossy().into_owned();
                    let build_text = build_root.to_string_lossy().into_owned();
                    let mut configure_arguments = vec![
                        "-S".to_owned(),
                        source_text,
                        "-B".to_owned(),
                        build_text.clone(),
                    ];
                    configure_arguments.extend(
                        effective_cmake_configuration_arguments
                            .clone()
                            .ok_or_else(|| {
                                RuntimeInstallError::SourceBuild(
                                    "CMake source plan has no effective configuration".to_owned(),
                                )
                            })?,
                    );
                    let configure_refs = configure_arguments
                        .iter()
                        .map(String::as_str)
                        .collect::<Vec<_>>();
                    run_source_command(
                        "CMake configure",
                        "cmake",
                        &configure_refs,
                        None,
                        &rejected_environment,
                    )
                    .await?;

                    self.emit(
                        available,
                        RuntimeOperationPhase::Building,
                        None,
                        None,
                        "Building the required runtime target",
                    );
                    run_source_command(
                        "CMake build",
                        "cmake",
                        &[
                            "--build",
                            &build_text,
                            "--target",
                            &plan.recipe.build_target,
                        ],
                        None,
                        &rejected_environment,
                    )
                    .await?;
                }
                RuntimeSourceBuildSystem::Make => {
                    self.emit(
                        available,
                        RuntimeOperationPhase::Configuring,
                        None,
                        None,
                        "Validated the selected upstream Makefile target",
                    );
                    self.emit(
                        available,
                        RuntimeOperationPhase::Building,
                        None,
                        None,
                        "Building only the selected upstream Makefile target",
                    );
                    run_source_command(
                        "Make build",
                        "make",
                        &[
                            "--no-builtin-rules",
                            "--no-builtin-variables",
                            &plan.recipe.build_target,
                        ],
                        Some(&source_root),
                        &rejected_environment,
                    )
                    .await?;
                }
            }

            let relative_entrypoint = PathBuf::from("source").join(&plan.recipe.entrypoint);
            let entrypoint = staging.join(&relative_entrypoint);
            let metadata = tokio::fs::metadata(&entrypoint).await.map_err(|error| {
                RuntimeInstallError::Entrypoint(format!(
                    "expected source-built entrypoint `{}` is unavailable: {error}",
                    relative_entrypoint.display()
                ))
            })?;
            if !metadata.is_file() {
                return Err(RuntimeInstallError::Entrypoint(format!(
                    "expected source-built entrypoint `{}` is not a regular file",
                    relative_entrypoint.display()
                )));
            }
            ensure_executable(&entrypoint)?;
            let entrypoint_sha256 = hash_file(&entrypoint)
                .await
                .map_err(|error| RuntimeInstallError::Entrypoint(error.to_string()))?;
            let installed_at = unix_timestamp();
            let source_build = RuntimeSourceBuildProvenance {
                source: plan.source.clone(),
                recipe_version: plan.recipe.recipe_version.clone(),
                build_system: plan.recipe.build_system,
                build_definition_sha256: plan.recipe.build_definition_sha256.clone(),
                cmake_configuration_arguments: plan.recipe.cmake_configuration_arguments.clone(),
                effective_cmake_configuration_arguments,
                build_target: plan.recipe.build_target.clone(),
                toolchain,
                build_platform: available.identity.platform.clone(),
                build_architecture: available.identity.architecture.clone(),
                accelerator_target: plan.recipe.accelerator_target.clone(),
                built_at_unix: installed_at,
                entrypoint: relative_entrypoint.clone(),
                entrypoint_sha256: entrypoint_sha256.clone(),
            };
            let mut manifest = RuntimeManifest {
                schema_version: RUNTIME_MANIFEST_SCHEMA_VERSION,
                runtime_id: available.runtime_id.clone(),
                identity: available.identity.clone(),
                supported_formats: available.supported_formats.clone(),
                supported_native_identities,
                requirements: available.requirements.clone(),
                acquisition_method: RuntimeAcquisitionMethod::SourceBuild,
                source_url: Some(available.source_url.clone()),
                downloaded_archive_sha256: None,
                additional_downloaded_archive_sha256: Vec::new(),
                source_build: Some(source_build),
                entrypoint: relative_entrypoint,
                entrypoint_sha256,
                installed_at_unix: Some(installed_at),
                probe: RuntimeProbeObservation {
                    compatible: false,
                    observed_engine_id: available.identity.engine_id.clone(),
                    observed_version: None,
                    observed_revision: None,
                    detail: "probe pending".to_owned(),
                    observed_at_unix: installed_at,
                },
            };
            let candidate = InstalledRuntime {
                manifest: manifest.clone(),
                installation_root: staging.path().to_path_buf(),
            };
            self.emit(
                available,
                RuntimeOperationPhase::Probing,
                None,
                None,
                "Validating source-built executable through the engine adapter",
            );
            let observation = adapter.probe_runtime(&candidate).await?;
            if !observation.compatible
                || observation.observed_engine_id != available.identity.engine_id
            {
                return Err(RuntimeInstallError::Probe(
                    EngineError::InvalidConfiguration(format!(
                        "runtime probe reported engine `{}` with compatible={}",
                        observation.observed_engine_id, observation.compatible
                    )),
                ));
            }
            if hash_file(&entrypoint)
                .await
                .map_err(|error| RuntimeInstallError::Entrypoint(error.to_string()))?
                != manifest.entrypoint_sha256
            {
                return Err(RuntimeInstallError::Probe(
                    EngineError::InvalidConfiguration(
                        "runtime entrypoint changed while its adapter probe was executing"
                            .to_owned(),
                    ),
                ));
            }
            manifest.probe = observation;
            manifest
                .validate()
                .map_err(|error| RuntimeInstallError::InvalidMetadata(error.to_string()))?;
            write_manifest(&staging.join(RUNTIME_MANIFEST_FILE), &manifest).await?;
            self.emit(
                available,
                RuntimeOperationPhase::Installing,
                None,
                None,
                "Atomically activating immutable source-built runtime",
            );
            self.store
                .activate(&mut staging, &manifest)
                .await
                .map_err(Into::into)
        }
        .await;

        match result {
            Ok(runtime) => {
                self.emit(
                    available,
                    RuntimeOperationPhase::Installed,
                    None,
                    None,
                    "Source-built runtime installed",
                );
                Ok(runtime)
            }
            Err(error) => Err(error),
        }
    }

    async fn verify_published_assets(
        &self,
        available: &AvailableRuntime,
    ) -> Result<(), RuntimeInstallError> {
        let (download, additional_downloads) = available.release_assets().ok_or_else(|| {
            RuntimeInstallError::InvalidMetadata(
                "release verification requires a release acquisition plan".to_owned(),
            )
        })?;
        let package = &available.identity.package;
        let repository = package.repository.as_deref().ok_or_else(|| {
            RuntimeInstallError::InvalidMetadata(
                "official repository identity is missing".to_owned(),
            )
        })?;
        let release_tag = package.release_tag.as_deref().ok_or_else(|| {
            RuntimeInstallError::InvalidMetadata("official release tag is missing".to_owned())
        })?;
        let release = self
            .github
            .release_by_tag(repository, release_tag)
            .await
            .map_err(|error| RuntimeInstallError::SourceChanged(error.to_string()))?
            .ok_or_else(|| {
                RuntimeInstallError::SourceChanged(format!(
                    "release `{repository}@{release_tag}` is no longer published"
                ))
            })?;
        if release.draft
            || release.tag_name != release_tag
            || release.html_url != available.source_url
            || available
                .identity
                .upstream_revision
                .as_deref()
                .is_some_and(|revision| revision != release.target_commitish)
        {
            return Err(RuntimeInstallError::SourceChanged(format!(
                "release `{repository}@{release_tag}` no longer matches the catalog identity"
            )));
        }

        let primary_name = package.asset_name.as_deref().ok_or_else(|| {
            RuntimeInstallError::InvalidMetadata("primary asset name is missing".to_owned())
        })?;
        verify_release_asset(
            &release.assets,
            package.asset_id.as_deref(),
            primary_name,
            download,
        )?;
        for (asset, download) in package.additional_assets.iter().zip(additional_downloads) {
            verify_release_asset(
                &release.assets,
                Some(&asset.asset_id),
                &asset.asset_name,
                download,
            )?;
        }
        Ok(())
    }

    async fn download(
        &self,
        available: &AvailableRuntime,
        download: &norted_core::RuntimeDownload,
        repository: &str,
        asset_id: u64,
        expected: &str,
        total_package_size: u64,
    ) -> Result<PathBuf, RuntimeInstallError> {
        let downloads = self.cache_root.join("downloads");
        tokio::fs::create_dir_all(&downloads)
            .await
            .map_err(|error| RuntimeInstallError::Download(error.to_string()))?;
        let cached = downloads.join(format!(
            "{}.{}",
            expected,
            download.archive_format.extension()
        ));
        if tokio::fs::try_exists(&cached)
            .await
            .map_err(|error| RuntimeInstallError::Download(error.to_string()))?
        {
            let cached_size = tokio::fs::metadata(&cached)
                .await
                .map_err(|error| RuntimeInstallError::Download(error.to_string()))?
                .len();
            if cached_size != download.size_bytes {
                tokio::fs::remove_file(&cached)
                    .await
                    .map_err(|error| RuntimeInstallError::Download(error.to_string()))?;
            } else {
                self.emit(
                    available,
                    RuntimeOperationPhase::Verifying,
                    None,
                    Some(total_package_size),
                    "Verifying cached package",
                );
                let observed = hash_file(&cached)
                    .await
                    .map_err(|error| RuntimeInstallError::Download(error.to_string()))?;
                if observed == expected {
                    return Ok(cached);
                }
                tokio::fs::remove_file(&cached)
                    .await
                    .map_err(|error| RuntimeInstallError::Download(error.to_string()))?;
            }
        }
        let temporary = downloads.join(format!("{}.part", uuid::Uuid::new_v4()));
        let mut file = tokio::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)
            .await
            .map_err(|error| RuntimeInstallError::Download(error.to_string()))?;
        let result = async {
            let response = self
                .github
                .release_asset_request(repository, asset_id)
                .map_err(|error| RuntimeInstallError::Download(error.to_string()))?
                .send()
                .await
                .map_err(|error| RuntimeInstallError::Download(error.to_string()))?;
            if !response.status().is_success() {
                return Err(RuntimeInstallError::Download(format!(
                    "GitHub asset returned HTTP {}",
                    response.status()
                )));
            }
            let total = response.content_length().or(Some(download.size_bytes));
            let mut stream = response.bytes_stream();
            let mut hash = Sha256::new();
            let mut completed = 0_u64;
            while let Some(chunk) = stream.next().await {
                let chunk =
                    chunk.map_err(|error| RuntimeInstallError::Download(error.to_string()))?;
                let next_completed =
                    completed.checked_add(chunk.len() as u64).ok_or_else(|| {
                        RuntimeInstallError::Download("downloaded byte count overflowed".to_owned())
                    })?;
                if next_completed > download.size_bytes {
                    return Err(RuntimeInstallError::Download(format!(
                        "GitHub asset exceeded its advertised size of {} bytes",
                        download.size_bytes
                    )));
                }
                file.write_all(&chunk)
                    .await
                    .map_err(|error| RuntimeInstallError::Download(error.to_string()))?;
                hash.update(&chunk);
                completed = next_completed;
                self.emit(
                    available,
                    RuntimeOperationPhase::Downloading,
                    Some(completed),
                    total,
                    "Downloading official release asset",
                );
            }
            if completed != download.size_bytes {
                return Err(RuntimeInstallError::Download(format!(
                    "GitHub asset size mismatch: expected {} bytes, received {completed}",
                    download.size_bytes
                )));
            }
            file.flush()
                .await
                .map_err(|error| RuntimeInstallError::Download(error.to_string()))?;
            file.sync_all()
                .await
                .map_err(|error| RuntimeInstallError::Download(error.to_string()))?;
            self.emit(
                available,
                RuntimeOperationPhase::Verifying,
                Some(completed),
                total,
                "Verifying package SHA-256",
            );
            let observed = hex_digest(hash.finalize());
            verify_package_digest(expected, &observed)?;
            drop(file);
            match tokio::fs::rename(&temporary, &cached).await {
                Ok(()) => Ok(cached.clone()),
                Err(_) if tokio::fs::try_exists(&cached).await.unwrap_or(false) => {
                    let cached_hash = hash_file(&cached)
                        .await
                        .map_err(|error| RuntimeInstallError::Download(error.to_string()))?;
                    if cached_hash == expected {
                        Ok(cached.clone())
                    } else {
                        Err(RuntimeInstallError::ChecksumMismatch {
                            expected: expected.to_owned(),
                            observed: cached_hash,
                        })
                    }
                }
                Err(error) => Err(RuntimeInstallError::Download(error.to_string())),
            }
        }
        .await;
        if result.is_err() && tokio::fs::try_exists(&temporary).await.unwrap_or(false) {
            let _ = tokio::fs::remove_file(&temporary).await;
        }
        result
    }

    fn emit(
        &self,
        available: &AvailableRuntime,
        phase: RuntimeOperationPhase,
        bytes_completed: Option<u64>,
        bytes_total: Option<u64>,
        detail: &str,
    ) {
        let _ = self.progress.send(RuntimeOperationProgress {
            runtime_id: available.runtime_id.clone(),
            phase,
            bytes_completed,
            bytes_total,
            detail: detail.to_owned(),
        });
    }
}

const MAX_PROBE_OUTPUT_BYTES: usize = 64 * 1024;
const MAX_BUILD_DIAGNOSTIC_BYTES: usize = 64 * 1024;
const SOURCE_PROBE_TIMEOUT: Duration = Duration::from_secs(20);

#[cfg(unix)]
#[derive(Debug)]
struct SourceProcessGroup {
    process_group_id: Option<libc::pid_t>,
}

#[cfg(unix)]
impl SourceProcessGroup {
    fn for_child(child: &tokio::process::Child) -> Result<Self, RuntimeInstallError> {
        let process_group_id = child
            .id()
            .and_then(|id| libc::pid_t::try_from(id).ok())
            .ok_or_else(|| {
                RuntimeInstallError::SourceBuild(
                    "source command did not expose a valid process-group ID".to_owned(),
                )
            })?;
        Ok(Self {
            process_group_id: Some(process_group_id),
        })
    }

    fn disarm(&mut self) {
        self.process_group_id = None;
    }
}

#[cfg(unix)]
impl Drop for SourceProcessGroup {
    fn drop(&mut self) {
        let Some(process_group_id) = self.process_group_id.take() else {
            return;
        };
        // The child is created as its own process-group leader. A negative PID
        // targets that group only, never Norted's process group.
        let result = unsafe { libc::kill(-process_group_id, libc::SIGKILL) };
        if result != 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) {
                tracing::warn!(process_group_id, %error, "failed to kill owned source process group");
            }
        }
    }
}

#[cfg(unix)]
fn configure_source_process_group(command: &mut tokio::process::Command) {
    use std::os::unix::process::CommandExt;

    command.as_std_mut().process_group(0);
}

#[cfg(not(unix))]
fn configure_source_process_group(_command: &mut tokio::process::Command) {}
const MAX_CMAKE_CONTRACT_BYTES: u64 = 8 * 1024 * 1024;
const MAX_CMAKE_CONTRACT_FILES: usize = 4_096;

async fn check_source_build_prerequisites(
    plan: &RuntimeSourceBuildPlan,
) -> Result<CheckedSourceBuildPrerequisites, RuntimeInstallError> {
    if !cfg!(target_os = "linux") {
        return Err(RuntimeInstallError::Prerequisite(
            "the selected official source recipe requires Linux".to_owned(),
        ));
    }
    if std::env::consts::ARCH != "x86_64" {
        return Err(RuntimeInstallError::Prerequisite(format!(
            "the selected official source recipe requires x86_64, observed {}",
            std::env::consts::ARCH
        )));
    }

    // Construct the effective configuration before probing the compiler. This
    // exact value is subsequently supplied to CMake and persisted, so
    // admission, execution, and provenance cannot select different nvcc
    // programs through ambient PATH or CUDACXX.
    let effective_cmake_configuration_arguments = effective_cmake_configuration_arguments(plan)
        .map_err(|error| RuntimeInstallError::Prerequisite(error.to_string()))?;

    command_text("git", &["--version"], None).await?;
    let cmake_version = if plan.recipe.build_system == RuntimeSourceBuildSystem::Cmake {
        let identity = command_text("cmake", &["--version"], None).await?;
        let version = first_version(&identity).ok_or_else(|| {
            RuntimeInstallError::Prerequisite("could not parse `cmake --version`".to_owned())
        })?;
        require_minimum_version("CMake", &version, &plan.prerequisites.minimum_cmake_version)?;
        version
    } else {
        "not required".to_owned()
    };

    let ninja_identity = if plan.prerequisites.requires_ninja {
        command_text("ninja", &["--version"], None).await?
    } else {
        "not required".to_owned()
    };
    let make_identity = if plan.prerequisites.requires_make {
        command_text("make", &["--version"], None).await?
    } else {
        "not required".to_owned()
    };
    let cpp_standard = plan
        .prerequisites
        .minimum_cpp_standard
        .or(plan.prerequisites.requires_cpp20_compiler.then_some(20));
    let cpp_program = plan.prerequisites.cpp_compiler.as_deref().unwrap_or("c++");
    let compiler_identity = if let Some(standard) = cpp_standard {
        let identity = command_text(cpp_program, &["--version"], None).await?;
        probe_cpp_compiler(cpp_program, standard).await?;
        identity
    } else {
        "not required".to_owned()
    };
    let nvcc_program = plan
        .prerequisites
        .cuda_compiler
        .as_deref()
        .and_then(Path::to_str)
        .unwrap_or("nvcc");
    let nvcc_identity = command_text(nvcc_program, &["--version"], None).await?;
    let nvcc_version = version_after(&nvcc_identity, "release")
        .or_else(|| first_version(&nvcc_identity))
        .ok_or_else(|| {
            RuntimeInstallError::Prerequisite("could not parse `nvcc --version`".to_owned())
        })?;
    if let Some(minimum) = &plan.prerequisites.minimum_cuda_version {
        require_minimum_version("CUDA Toolkit", &nvcc_version, minimum)?;
    }
    if let Some(maximum) = &plan.prerequisites.maximum_cuda_version_exclusive {
        require_version_below("CUDA Toolkit", &nvcc_version, maximum)?;
    }
    if let Some(arguments) = &effective_cmake_configuration_arguments {
        verify_explicit_cuda_architectures(nvcc_program, arguments).await?;
    }

    let pkg_config_identity = if plan.prerequisites.requires_pkg_config {
        command_text("pkg-config", &["--version"], None).await?
    } else {
        "not required".to_owned()
    };
    let mut system_dependencies = BTreeMap::new();
    for (module, minimum) in &plan.prerequisites.pkg_config_modules {
        let constraint = format!("--atleast-version={minimum}");
        probe_status("pkg-config", &[constraint.as_str(), module])
            .await
            .map_err(|_| {
                RuntimeInstallError::Prerequisite(format!(
                    "pkg-config module `{module}` >= {minimum} is required"
                ))
            })?;
        let observed = command_text("pkg-config", &["--modversion", module], None).await?;
        system_dependencies.insert(module.clone(), observed);
    }

    Ok(CheckedSourceBuildPrerequisites {
        toolchain: RuntimeSourceBuildToolchain {
            cmake_version,
            ninja_version: first_line(&ninja_identity),
            make_version: first_line(&make_identity),
            cpp_compiler: compact_identity(&compiler_identity),
            nvcc_version,
            pkg_config_version: first_line(&pkg_config_identity),
            system_dependencies,
        },
        effective_cmake_configuration_arguments,
    })
}

#[derive(Debug, Default, Eq, PartialEq)]
struct RequiredCudaCompilerTargets {
    real: Vec<String>,
    virtual_targets: Vec<String>,
}

async fn verify_explicit_cuda_architectures(
    nvcc_program: &str,
    cmake_arguments: &[String],
) -> Result<(), RuntimeInstallError> {
    let Some(required) = required_cuda_compiler_targets(cmake_arguments)? else {
        return Ok(());
    };
    if !required.real.is_empty() {
        verify_reported_cuda_compiler_targets(
            nvcc_program,
            "real",
            &required.real,
            "--list-gpu-code",
        )
        .await?;
    }
    if !required.virtual_targets.is_empty() {
        verify_reported_cuda_compiler_targets(
            nvcc_program,
            "virtual",
            &required.virtual_targets,
            "--list-gpu-arch",
        )
        .await?;
    }
    Ok(())
}

async fn verify_reported_cuda_compiler_targets(
    nvcc_program: &str,
    kind: &str,
    required: &[String],
    list_argument: &str,
) -> Result<(), RuntimeInstallError> {
    let listed = command_text(nvcc_program, &[list_argument], None).await?;
    if require_cuda_compiler_targets(kind, required, &listed).is_ok() {
        return Ok(());
    }

    // CUDA 13's list actions report base SM/compute values but omit accepted
    // architecture-specific `a`/`f` values. Its own option help is the
    // compiler-authoritative enumeration for those targets.
    let help = command_text(nvcc_program, &["--help"], None).await?;
    require_cuda_compiler_targets(kind, required, &format!("{listed}\n{help}"))
}

fn required_cuda_compiler_targets(
    cmake_arguments: &[String],
) -> Result<Option<RequiredCudaCompilerTargets>, RuntimeInstallError> {
    const PREFIX: &str = "-DCMAKE_CUDA_ARCHITECTURES=";
    let configured = cmake_arguments
        .iter()
        .filter_map(|argument| argument.strip_prefix(PREFIX))
        .collect::<Vec<_>>();
    let [architectures] = configured.as_slice() else {
        return if configured.is_empty() {
            Ok(None)
        } else {
            Err(RuntimeInstallError::Prerequisite(
                "the source recipe contains multiple CMAKE_CUDA_ARCHITECTURES policies".to_owned(),
            ))
        };
    };
    if matches!(
        architectures.to_ascii_lowercase().as_str(),
        "all" | "all-major" | "native" | "off"
    ) {
        return Ok(None);
    }

    let mut required = RequiredCudaCompilerTargets::default();
    for configured in architectures.split(';') {
        let (architecture, real, virtual_target) =
            if let Some(architecture) = configured.strip_suffix("-real") {
                (architecture, true, false)
            } else if let Some(architecture) = configured.strip_suffix("-virtual") {
                (architecture, false, true)
            } else {
                (configured, true, true)
            };
        let numeric = architecture.trim_end_matches(['a', 'f']);
        if numeric.is_empty()
            || architecture.len().saturating_sub(numeric.len()) > 1
            || !numeric.bytes().all(|byte| byte.is_ascii_digit())
        {
            return Err(RuntimeInstallError::Prerequisite(format!(
                "source recipe has unsupported explicit CUDA architecture `{configured}`"
            )));
        }
        if real {
            required.real.push(format!("sm_{architecture}"));
        }
        if virtual_target {
            required
                .virtual_targets
                .push(format!("compute_{architecture}"));
        }
    }
    Ok(Some(required))
}

fn require_cuda_compiler_targets(
    kind: &str,
    required: &[String],
    supported: &str,
) -> Result<(), RuntimeInstallError> {
    let supported = supported
        .split(|character: char| !(character.is_ascii_alphanumeric() || character == '_'))
        .filter(|target| !target.is_empty())
        .collect::<HashSet<_>>();
    let missing = required
        .iter()
        .filter(|target| !supported.contains(target.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(RuntimeInstallError::Prerequisite(format!(
            "the installed CUDA compiler cannot build required {kind} target(s): {}",
            missing.join(", ")
        )))
    }
}

async fn probe_cpp_compiler(program: &str, standard: u16) -> Result<(), RuntimeInstallError> {
    let mut command = tokio::process::Command::new(program);
    command
        .args([
            format!("-std=c++{standard}"),
            "-x".to_owned(),
            "c++".to_owned(),
            "-fsyntax-only".to_owned(),
            "-".to_owned(),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    configure_source_process_group(&mut command);
    let mut child = command.spawn().map_err(|error| {
        RuntimeInstallError::Prerequisite(format!(
            "could not start C++{standard} compiler probe with `{program}`: {error}"
        ))
    })?;
    #[cfg(unix)]
    let mut process_group = SourceProcessGroup::for_child(&child)?;
    if let Some(mut stdin) = child.stdin.take() {
        let probe = if standard >= 20 {
            b"#include <span>\nint main(){int x[1]{}; std::span<int> s{x}; return int(s.size())-1;}\n".as_slice()
        } else {
            b"#include <optional>\nint main(){std::optional<int> value{1}; return *value-1;}\n"
                .as_slice()
        };
        stdin
            .write_all(probe)
            .await
            .map_err(|error| RuntimeInstallError::Prerequisite(error.to_string()))?;
    }
    let status = tokio::time::timeout(SOURCE_PROBE_TIMEOUT, child.wait())
        .await
        .map_err(|_| {
            RuntimeInstallError::Prerequisite(format!("C++{standard} compiler probe timed out"))
        })?
        .map_err(|error| RuntimeInstallError::Prerequisite(error.to_string()))?;
    // Once the compiler driver is fully reaped, disarm immediately so a
    // recycled PID can never cause an unrelated process group to be targeted.
    #[cfg(unix)]
    process_group.disarm();
    if !status.success() {
        return Err(RuntimeInstallError::Prerequisite(format!(
            "the host `{program}` compiler did not accept a C++{standard} probe"
        )));
    }
    Ok(())
}

async fn command_text(
    program: &str,
    arguments: &[&str],
    current_dir: Option<&Path>,
) -> Result<String, RuntimeInstallError> {
    let mut command = tokio::process::Command::new(program);
    command
        .args(arguments)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Some(current_dir) = current_dir {
        command.current_dir(current_dir);
    }
    let mut child = command.spawn().map_err(|error| {
        RuntimeInstallError::Prerequisite(format!("could not execute `{program}`: {error}"))
    })?;
    let stdout = child.stdout.take().ok_or_else(|| {
        RuntimeInstallError::Prerequisite(format!("`{program}` stdout is unavailable"))
    })?;
    let stderr = child.stderr.take().ok_or_else(|| {
        RuntimeInstallError::Prerequisite(format!("`{program}` stderr is unavailable"))
    })?;
    let stdout_task = tokio::spawn(read_bounded_probe_output(stdout));
    let stderr_task = tokio::spawn(read_bounded_probe_output(stderr));
    let status = tokio::time::timeout(SOURCE_PROBE_TIMEOUT, child.wait())
        .await
        .map_err(|_| RuntimeInstallError::Prerequisite(format!("`{program}` probe timed out")))?
        .map_err(|error| {
            RuntimeInstallError::Prerequisite(format!("could not execute `{program}`: {error}"))
        })?;
    let (stdout, stdout_exceeded) = stdout_task
        .await
        .map_err(|error| RuntimeInstallError::Task(error.to_string()))?
        .map_err(|error| RuntimeInstallError::Prerequisite(error.to_string()))?;
    let (stderr, stderr_exceeded) = stderr_task
        .await
        .map_err(|error| RuntimeInstallError::Task(error.to_string()))?
        .map_err(|error| RuntimeInstallError::Prerequisite(error.to_string()))?;
    if stdout_exceeded
        || stderr_exceeded
        || stdout.len().saturating_add(stderr.len()) > MAX_PROBE_OUTPUT_BYTES
    {
        return Err(RuntimeInstallError::Prerequisite(format!(
            "`{program}` probe output exceeded {MAX_PROBE_OUTPUT_BYTES} bytes"
        )));
    }
    if !status.success() {
        return Err(RuntimeInstallError::Prerequisite(format!(
            "`{program} {}` exited unsuccessfully: {}",
            arguments.join(" "),
            compact_identity(&String::from_utf8_lossy(&stderr))
        )));
    }
    let stdout = String::from_utf8_lossy(&stdout);
    let stderr = String::from_utf8_lossy(&stderr);
    let text = if stdout.trim().is_empty() {
        stderr.trim()
    } else {
        stdout.trim()
    };
    if text.is_empty() {
        return Err(RuntimeInstallError::Prerequisite(format!(
            "`{program}` returned no observable identity"
        )));
    }
    Ok(text.to_owned())
}

async fn read_bounded_probe_output<R>(mut reader: R) -> std::io::Result<(Vec<u8>, bool)>
where
    R: AsyncRead + Unpin,
{
    let mut output = Vec::with_capacity(MAX_PROBE_OUTPUT_BYTES);
    let mut exceeded = false;
    let mut buffer = [0_u8; 8 * 1024];
    loop {
        let read = reader.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        let retained = MAX_PROBE_OUTPUT_BYTES
            .saturating_sub(output.len())
            .min(read);
        output.extend_from_slice(&buffer[..retained]);
        exceeded |= retained != read;
    }
    Ok((output, exceeded))
}

async fn probe_status(program: &str, arguments: &[&str]) -> Result<(), RuntimeInstallError> {
    let mut command = tokio::process::Command::new(program);
    command
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let status = tokio::time::timeout(SOURCE_PROBE_TIMEOUT, command.status())
        .await
        .map_err(|_| RuntimeInstallError::Prerequisite(format!("`{program}` probe timed out")))?
        .map_err(|error| RuntimeInstallError::Prerequisite(error.to_string()))?;
    status.success().then_some(()).ok_or_else(|| {
        RuntimeInstallError::Prerequisite(format!("`{program}` probe exited unsuccessfully"))
    })
}

async fn run_source_command(
    label: &str,
    program: &str,
    arguments: &[&str],
    current_dir: Option<&Path>,
    rejected_environment: &[&str],
) -> Result<(), RuntimeInstallError> {
    let mut command = tokio::process::Command::new(program);
    command
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    configure_source_process_group(&mut command);
    if let Some(current_dir) = current_dir {
        command.current_dir(current_dir);
    }
    for name in rejected_environment {
        command.env_remove(name);
    }
    let mut child = command.spawn().map_err(|error| {
        RuntimeInstallError::SourceBuild(format!("{label} could not start: {error}"))
    })?;
    #[cfg(unix)]
    let mut process_group = SourceProcessGroup::for_child(&child)?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| RuntimeInstallError::SourceBuild(format!("{label} stdout unavailable")))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| RuntimeInstallError::SourceBuild(format!("{label} stderr unavailable")))?;
    let stdout_task = tokio::spawn(read_bounded_tail(stdout));
    let stderr_task = tokio::spawn(read_bounded_tail(stderr));
    let status = child.wait().await.map_err(|error| {
        RuntimeInstallError::SourceBuild(format!("{label} could not be observed: {error}"))
    })?;
    // Once the leader is fully reaped, disarm immediately so a recycled PID
    // can never cause an unrelated process group to be targeted.
    #[cfg(unix)]
    process_group.disarm();
    let stdout = stdout_task
        .await
        .map_err(|error| RuntimeInstallError::Task(error.to_string()))?
        .map_err(|error| RuntimeInstallError::SourceBuild(error.to_string()))?;
    let stderr = stderr_task
        .await
        .map_err(|error| RuntimeInstallError::Task(error.to_string()))?
        .map_err(|error| RuntimeInstallError::SourceBuild(error.to_string()))?;
    if !status.success() {
        let detail = if stderr.is_empty() { stdout } else { stderr };
        let detail = compact_identity(&String::from_utf8_lossy(&detail));
        return Err(RuntimeInstallError::SourceBuild(format!(
            "{label} exited with {status}: {detail}"
        )));
    }
    Ok(())
}

async fn read_bounded_tail<R>(mut reader: R) -> std::io::Result<Vec<u8>>
where
    R: AsyncRead + Unpin,
{
    let mut tail = Vec::with_capacity(MAX_BUILD_DIAGNOSTIC_BYTES);
    let mut buffer = [0_u8; 8 * 1024];
    loop {
        let read = reader.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        if read >= MAX_BUILD_DIAGNOSTIC_BYTES {
            tail.clear();
            tail.extend_from_slice(&buffer[read - MAX_BUILD_DIAGNOSTIC_BYTES..read]);
            continue;
        }
        let overflow = tail
            .len()
            .saturating_add(read)
            .saturating_sub(MAX_BUILD_DIAGNOSTIC_BYTES);
        if overflow > 0 {
            tail.drain(..overflow);
        }
        tail.extend_from_slice(&buffer[..read]);
    }
    Ok(tail)
}

fn verify_source_checkout(
    expected_commit: &str,
    expected_tree: &str,
    observed_commit: &str,
    observed_tree: &str,
) -> Result<(), RuntimeInstallError> {
    if observed_commit != expected_commit {
        return Err(RuntimeInstallError::SourceChanged(format!(
            "source checkout revision mismatch: expected {expected_commit}, observed {observed_commit}"
        )));
    }
    if observed_tree != expected_tree {
        return Err(RuntimeInstallError::SourceChanged(format!(
            "source checkout tree mismatch: expected {expected_tree}, observed {observed_tree}"
        )));
    }
    Ok(())
}

fn inspect_build_dependency_contract(
    source_root: &Path,
    build_system: RuntimeSourceBuildSystem,
    build_target: &str,
    build_definition_sha256: Option<&str>,
    cmake_configuration_arguments: &[String],
) -> Result<(), RuntimeInstallError> {
    if build_system == RuntimeSourceBuildSystem::Make {
        return inspect_make_build_contract(
            source_root,
            build_target,
            build_definition_sha256.ok_or_else(|| {
                RuntimeInstallError::SourceBuild(
                    "Make source build is missing its provider-audited build-definition digest"
                        .to_owned(),
                )
            })?,
        );
    }
    let fetch_content_disconnected = cmake_configuration_arguments
        .iter()
        .any(|argument| argument == "-DFETCHCONTENT_FULLY_DISCONNECTED=ON")
        && cmake_configuration_arguments
            .iter()
            .any(|argument| argument == "-DFETCHCONTENT_UPDATES_DISCONNECTED=ON");
    let mut files = 0_usize;
    let mut bytes = 0_u64;
    for entry in walkdir::WalkDir::new(source_root).follow_links(false) {
        let entry = entry.map_err(|error| RuntimeInstallError::SourceBuild(error.to_string()))?;
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        let selected = path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name == "CMakeLists.txt" || name.ends_with(".cmake"));
        if !selected {
            continue;
        }
        files = files.saturating_add(1);
        if files > MAX_CMAKE_CONTRACT_FILES {
            return Err(RuntimeInstallError::SourceBuild(
                "source CMake tree exceeds the bounded dependency audit file count".to_owned(),
            ));
        }
        let metadata = entry
            .metadata()
            .map_err(|error| RuntimeInstallError::SourceBuild(error.to_string()))?;
        bytes = bytes.saturating_add(metadata.len());
        if bytes > MAX_CMAKE_CONTRACT_BYTES || metadata.len() > MAX_CMAKE_CONTRACT_BYTES {
            return Err(RuntimeInstallError::SourceBuild(
                "source CMake tree exceeds the bounded dependency audit size".to_owned(),
            ));
        }
        let contents = std::fs::read_to_string(path)
            .map_err(|error| RuntimeInstallError::SourceBuild(error.to_string()))?;
        let normalized = contents.to_ascii_lowercase();
        if !fetch_content_disconnected {
            let forbidden = [
                "fetchcontent",
                "externalproject",
                "file(download",
                "git clone",
                "http://",
                "https://",
            ];
            if let Some(directive) = forbidden
                .iter()
                .find(|directive| normalized.contains(**directive))
            {
                return Err(RuntimeInstallError::SourceBuild(format!(
                    "source build dependency audit rejected `{}` in {}",
                    directive,
                    path.strip_prefix(source_root).unwrap_or(path).display()
                )));
            }
        }
    }
    if files == 0 {
        return Err(RuntimeInstallError::SourceBuild(
            "source snapshot contains no CMake build definition".to_owned(),
        ));
    }
    Ok(())
}

fn inspect_make_build_contract(
    source_root: &Path,
    build_target: &str,
    expected_makefile_sha256: &str,
) -> Result<(), RuntimeInstallError> {
    for alternate in ["GNUmakefile", "makefile"] {
        match std::fs::symlink_metadata(source_root.join(alternate)) {
            Ok(_) => {
                return Err(RuntimeInstallError::SourceBuild(format!(
                    "source tree contains alternate Make entrypoint `{alternate}` outside the provider-audited closure"
                )));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(RuntimeInstallError::SourceBuild(error.to_string())),
        }
    }
    let makefile = source_root.join("Makefile");
    let metadata = std::fs::symlink_metadata(&makefile)
        .map_err(|error| RuntimeInstallError::SourceBuild(error.to_string()))?;
    if !metadata.file_type().is_file() || metadata.len() > MAX_CMAKE_CONTRACT_BYTES {
        return Err(RuntimeInstallError::SourceBuild(
            "source Makefile is missing, non-regular, or exceeds the bounded audit size".to_owned(),
        ));
    }
    let contents = std::fs::read_to_string(&makefile)
        .map_err(|error| RuntimeInstallError::SourceBuild(error.to_string()))?;
    let observed_makefile_sha256 = hex_digest(Sha256::digest(contents.as_bytes()));
    if observed_makefile_sha256 != expected_makefile_sha256 {
        return Err(RuntimeInstallError::SourceBuild(format!(
            "source Makefile does not match the provider-audited dependency/command closure: expected {expected_makefile_sha256}, observed {observed_makefile_sha256}"
        )));
    }
    if !makefile_declares_target(&contents, build_target) {
        return Err(RuntimeInstallError::SourceBuild(format!(
            "source Makefile does not declare the selected target `{build_target}`"
        )));
    }
    for line in contents
        .lines()
        .map(str::trim_start)
        .filter(|line| !line.starts_with('#'))
    {
        let normalized = line.to_ascii_lowercase();
        if ["git clone", "curl ", "wget ", "http://", "https://"]
            .iter()
            .any(|directive| normalized.contains(directive))
        {
            return Err(RuntimeInstallError::SourceBuild(format!(
                "source Makefile dependency audit rejected a network command in target `{build_target}`"
            )));
        }
    }
    Ok(())
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

fn ensure_executable(path: &Path) -> Result<(), RuntimeInstallError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mode = std::fs::metadata(path)
            .map_err(|error| RuntimeInstallError::Entrypoint(error.to_string()))?
            .permissions()
            .mode();
        if mode & 0o111 == 0 {
            return Err(RuntimeInstallError::Entrypoint(format!(
                "source-built entrypoint `{}` is not executable",
                path.display()
            )));
        }
    }
    Ok(())
}

fn require_minimum_version(
    name: &str,
    observed: &str,
    minimum: &str,
) -> Result<(), RuntimeInstallError> {
    let observed_parts = numeric_version(observed);
    let minimum_parts = numeric_version(minimum);
    if observed_parts.is_empty() || minimum_parts.is_empty() || observed_parts < minimum_parts {
        return Err(RuntimeInstallError::Prerequisite(format!(
            "{name} >= {minimum} is required; observed {observed}"
        )));
    }
    Ok(())
}

fn require_version_below(
    name: &str,
    observed: &str,
    maximum_exclusive: &str,
) -> Result<(), RuntimeInstallError> {
    let observed_parts = numeric_version(observed);
    let maximum_parts = numeric_version(maximum_exclusive);
    if observed_parts.is_empty() || maximum_parts.is_empty() || observed_parts >= maximum_parts {
        return Err(RuntimeInstallError::Prerequisite(format!(
            "{name} < {maximum_exclusive} is required; observed {observed}"
        )));
    }
    Ok(())
}

fn numeric_version(value: &str) -> Vec<u64> {
    value
        .split('.')
        .map(|component| {
            component
                .chars()
                .take_while(char::is_ascii_digit)
                .collect::<String>()
        })
        .take_while(|component| !component.is_empty())
        .filter_map(|component| component.parse().ok())
        .collect()
}

fn first_version(value: &str) -> Option<String> {
    value.split_whitespace().find_map(|token| {
        let trimmed = token.trim_matches(|character: char| !character.is_ascii_digit());
        (trimmed.contains('.') && !numeric_version(trimmed).is_empty()).then(|| trimmed.to_owned())
    })
}

fn version_after(value: &str, marker: &str) -> Option<String> {
    let (_, tail) = value.split_once(marker)?;
    first_version(tail)
}

fn first_line(value: &str) -> String {
    value.lines().next().unwrap_or(value).trim().to_owned()
}

fn compact_identity(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn extract_archive(
    archive_path: &Path,
    staging: &Path,
    format: RuntimeArchiveFormat,
) -> Result<(), RuntimeInstallError> {
    match format {
        RuntimeArchiveFormat::Zip => extract_zip(archive_path, staging),
        RuntimeArchiveFormat::TarGz => extract_tar_gz(archive_path, staging),
    }?;
    validate_staging_limits(staging)
}

async fn extract_archive_owned(
    staging: RuntimeStaging,
    archive_path: PathBuf,
    format: RuntimeArchiveFormat,
) -> Result<RuntimeStaging, RuntimeInstallError> {
    run_owned_staging_operation(staging, move |staging_path| {
        extract_archive(&archive_path, staging_path, format)
    })
    .await
}

async fn run_owned_staging_operation<F>(
    staging: RuntimeStaging,
    operation: F,
) -> Result<RuntimeStaging, RuntimeInstallError>
where
    F: FnOnce(&Path) -> Result<(), RuntimeInstallError> + Send + 'static,
{
    tokio::task::spawn_blocking(move || {
        operation(staging.path())?;
        Ok(staging)
    })
    .await
    .map_err(|error| RuntimeInstallError::Task(error.to_string()))?
}

fn validate_staging_limits(staging: &Path) -> Result<(), RuntimeInstallError> {
    let mut entries = 0_usize;
    let mut bytes = 0_u64;
    for entry in walkdir::WalkDir::new(staging).follow_links(false) {
        let entry = entry.map_err(|error| RuntimeInstallError::UnsafeArchive(error.to_string()))?;
        if entry.path() == staging {
            continue;
        }
        entries = entries.checked_add(1).ok_or_else(|| {
            RuntimeInstallError::UnsafeArchive("extracted entry count overflow".to_owned())
        })?;
        if entries > MAX_ARCHIVE_ENTRIES {
            return Err(RuntimeInstallError::UnsafeArchive(format!(
                "combined package contains more than {MAX_ARCHIVE_ENTRIES} entries"
            )));
        }
        if entry.file_type().is_file() {
            bytes = bytes
                .checked_add(
                    entry
                        .metadata()
                        .map_err(|error| RuntimeInstallError::UnsafeArchive(error.to_string()))?
                        .len(),
                )
                .ok_or_else(|| {
                    RuntimeInstallError::UnsafeArchive("extracted size overflow".to_owned())
                })?;
            if bytes > MAX_EXTRACTED_BYTES {
                return Err(RuntimeInstallError::UnsafeArchive(format!(
                    "combined package expands beyond {MAX_EXTRACTED_BYTES} bytes"
                )));
            }
        }
    }
    Ok(())
}

fn validate_release_source(available: &AvailableRuntime) -> Result<(), RuntimeInstallError> {
    let package = &available.identity.package;
    let repository = package.repository.as_deref().ok_or_else(|| {
        RuntimeInstallError::InvalidMetadata("official repository identity is missing".to_owned())
    })?;
    let release_tag = package.release_tag.as_deref().ok_or_else(|| {
        RuntimeInstallError::InvalidMetadata("official release tag is missing".to_owned())
    })?;
    let expected = github_url(&[repository, "releases", "tag", release_tag])?;
    let observed = available
        .source_url
        .parse::<reqwest::Url>()
        .map_err(|error| RuntimeInstallError::InvalidMetadata(error.to_string()))?;
    if observed != expected {
        return Err(RuntimeInstallError::UntrustedUrl(observed.to_string()));
    }
    Ok(())
}

fn parse_asset_id(asset_id: Option<&str>, asset_name: &str) -> Result<u64, RuntimeInstallError> {
    asset_id
        .and_then(|asset_id| asset_id.parse::<u64>().ok())
        .filter(|asset_id| *asset_id != 0)
        .ok_or_else(|| {
            RuntimeInstallError::InvalidMetadata(format!(
                "release asset `{asset_name}` has an invalid immutable GitHub asset ID"
            ))
        })
}

fn verify_release_asset(
    live_assets: &[GitHubReleaseAsset],
    expected_id: Option<&str>,
    expected_name: &str,
    expected_download: &norted_core::RuntimeDownload,
) -> Result<(), RuntimeInstallError> {
    let expected_id = parse_asset_id(expected_id, expected_name)?;
    let live = live_assets
        .iter()
        .find(|asset| asset.id == expected_id)
        .ok_or_else(|| {
            RuntimeInstallError::SourceChanged(format!(
                "release asset `{expected_name}` no longer has GitHub asset ID {expected_id}"
            ))
        })?;
    let live_digest = live
        .digest
        .as_deref()
        .and_then(|digest| norted_core::RuntimeDigest::parse_github(digest).ok());
    if live.state != "uploaded"
        || live.name != expected_name
        || live.size != expected_download.size_bytes
        || live.browser_download_url != expected_download.url
        || live_digest.as_ref() != expected_download.digest.as_ref()
    {
        return Err(RuntimeInstallError::SourceChanged(format!(
            "release asset `{expected_name}` metadata changed after catalog discovery"
        )));
    }
    Ok(())
}

fn validate_release_asset_url(
    available: &AvailableRuntime,
    asset_name: &str,
    download: &norted_core::RuntimeDownload,
) -> Result<reqwest::Url, RuntimeInstallError> {
    let package = &available.identity.package;
    let repository = package.repository.as_deref().ok_or_else(|| {
        RuntimeInstallError::InvalidMetadata("official repository identity is missing".to_owned())
    })?;
    let release_tag = package.release_tag.as_deref().ok_or_else(|| {
        RuntimeInstallError::InvalidMetadata("official release tag is missing".to_owned())
    })?;
    let expected = github_url(&[repository, "releases", "download", release_tag, asset_name])?;
    let observed = download
        .url
        .parse::<reqwest::Url>()
        .map_err(|error| RuntimeInstallError::InvalidMetadata(error.to_string()))?;
    if !is_allowed_github_host(&observed) || observed != expected {
        return Err(RuntimeInstallError::UntrustedUrl(observed.to_string()));
    }
    Ok(observed)
}

fn github_url(path: &[&str]) -> Result<reqwest::Url, RuntimeInstallError> {
    let Some((owner, repository)) = path[0].split_once('/') else {
        return Err(RuntimeInstallError::InvalidMetadata(
            "official repository must be an owner/name pair".to_owned(),
        ));
    };
    if owner.is_empty()
        || repository.is_empty()
        || repository.contains('/')
        || !path.iter().skip(1).all(|segment| !segment.is_empty())
    {
        return Err(RuntimeInstallError::InvalidMetadata(
            "official release path contains an empty or invalid component".to_owned(),
        ));
    }
    let mut url = reqwest::Url::parse("https://github.com")
        .expect("the built-in GitHub origin is a valid URL");
    url.path_segments_mut()
        .expect("the built-in GitHub origin supports path segments")
        .push(owner)
        .push(repository)
        .extend(path.iter().skip(1).copied());
    Ok(url)
}

fn extract_zip(archive_path: &Path, staging: &Path) -> Result<(), RuntimeInstallError> {
    let file = std::fs::File::open(archive_path)
        .map_err(|error| RuntimeInstallError::UnsafeArchive(error.to_string()))?;
    let mut archive = zip::ZipArchive::new(file)
        .map_err(|error| RuntimeInstallError::UnsafeArchive(error.to_string()))?;
    if archive.len() > MAX_ARCHIVE_ENTRIES {
        return Err(RuntimeInstallError::UnsafeArchive(format!(
            "archive contains more than {MAX_ARCHIVE_ENTRIES} entries"
        )));
    }
    let mut seen = HashSet::new();
    let mut extracted = 0_u64;
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|error| RuntimeInstallError::UnsafeArchive(error.to_string()))?;
        let name = entry.name().to_owned();
        let relative = checked_archive_path(&name)?;
        if !seen.insert(normalized_archive_key(&relative)) {
            return Err(RuntimeInstallError::UnsafeArchive(format!(
                "archive contains duplicate path `{name}`"
            )));
        }
        if let Some(mode) = entry.unix_mode() {
            let file_type = mode & 0o170000;
            if file_type == 0o120000 {
                return Err(RuntimeInstallError::UnsafeArchive(format!(
                    "ZIP symlink entry is not allowed: `{name}`"
                )));
            }
            if file_type != 0 && file_type != 0o100000 && file_type != 0o040000 {
                return Err(RuntimeInstallError::UnsafeArchive(format!(
                    "ZIP contains a special filesystem entry: `{name}`"
                )));
            }
        }
        let destination = staging.join(&relative);
        if entry.is_dir() {
            std::fs::create_dir_all(&destination)
                .map_err(|error| RuntimeInstallError::UnsafeArchive(error.to_string()))?;
            continue;
        }
        extracted = extracted.checked_add(entry.size()).ok_or_else(|| {
            RuntimeInstallError::UnsafeArchive("extracted size overflow".to_owned())
        })?;
        if extracted > MAX_EXTRACTED_BYTES {
            return Err(RuntimeInstallError::UnsafeArchive(format!(
                "archive expands beyond {MAX_EXTRACTED_BYTES} bytes"
            )));
        }
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| RuntimeInstallError::UnsafeArchive(error.to_string()))?;
        }
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&destination)
            .map_err(|error| RuntimeInstallError::UnsafeArchive(error.to_string()))?;
        std::io::copy(&mut entry, &mut output)
            .map_err(|error| RuntimeInstallError::UnsafeArchive(error.to_string()))?;
        output
            .sync_all()
            .map_err(|error| RuntimeInstallError::UnsafeArchive(error.to_string()))?;
        set_unix_mode(&destination, entry.unix_mode())?;
    }
    Ok(())
}

fn extract_tar_gz(archive_path: &Path, staging: &Path) -> Result<(), RuntimeInstallError> {
    let file = std::fs::File::open(archive_path)
        .map_err(|error| RuntimeInstallError::UnsafeArchive(error.to_string()))?;
    let mut archive = tar::Archive::new(GzDecoder::new(file));
    let mut seen = HashSet::new();
    let mut count = 0_usize;
    let mut extracted = 0_u64;
    let mut symlinks = Vec::new();
    let mut hardlinks = Vec::new();
    let entries = archive
        .entries()
        .map_err(|error| RuntimeInstallError::UnsafeArchive(error.to_string()))?;
    for entry in entries {
        count += 1;
        if count > MAX_ARCHIVE_ENTRIES {
            return Err(RuntimeInstallError::UnsafeArchive(format!(
                "archive contains more than {MAX_ARCHIVE_ENTRIES} entries"
            )));
        }
        let mut entry =
            entry.map_err(|error| RuntimeInstallError::UnsafeArchive(error.to_string()))?;
        let path = entry
            .path()
            .map_err(|error| RuntimeInstallError::UnsafeArchive(error.to_string()))?;
        let path_text = path.to_string_lossy();
        let relative = checked_archive_path(&path_text)?;
        if !seen.insert(normalized_archive_key(&relative)) {
            return Err(RuntimeInstallError::UnsafeArchive(format!(
                "archive contains duplicate path `{}`",
                path.display()
            )));
        }
        let entry_type = entry.header().entry_type();
        let destination = staging.join(&relative);
        if entry_type.is_dir() {
            std::fs::create_dir_all(&destination)
                .map_err(|error| RuntimeInstallError::UnsafeArchive(error.to_string()))?;
            continue;
        }
        if entry_type.is_symlink() {
            let target = entry
                .link_name()
                .map_err(|error| RuntimeInstallError::UnsafeArchive(error.to_string()))?
                .ok_or_else(|| {
                    RuntimeInstallError::UnsafeArchive(format!(
                        "symlink `{}` has no target",
                        path.display()
                    ))
                })?;
            validate_relative_link_target(&relative, &target)?;
            symlinks.push((destination, target.into_owned()));
            continue;
        }
        if entry_type.is_hard_link() {
            let target = entry
                .link_name()
                .map_err(|error| RuntimeInstallError::UnsafeArchive(error.to_string()))?
                .ok_or_else(|| {
                    RuntimeInstallError::UnsafeArchive(format!(
                        "hardlink `{}` has no target",
                        path.display()
                    ))
                })?;
            let target = checked_archive_path(&target.to_string_lossy())?;
            hardlinks.push((destination, staging.join(target)));
            continue;
        }
        if !entry_type.is_file() {
            return Err(RuntimeInstallError::UnsafeArchive(format!(
                "archive contains an unsupported entry: `{}`",
                path.display()
            )));
        }
        let size = entry
            .header()
            .size()
            .map_err(|error| RuntimeInstallError::UnsafeArchive(error.to_string()))?;
        extracted = extracted.checked_add(size).ok_or_else(|| {
            RuntimeInstallError::UnsafeArchive("extracted size overflow".to_owned())
        })?;
        if extracted > MAX_EXTRACTED_BYTES {
            return Err(RuntimeInstallError::UnsafeArchive(format!(
                "archive expands beyond {MAX_EXTRACTED_BYTES} bytes"
            )));
        }
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| RuntimeInstallError::UnsafeArchive(error.to_string()))?;
        }
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&destination)
            .map_err(|error| RuntimeInstallError::UnsafeArchive(error.to_string()))?;
        std::io::copy(&mut entry, &mut output)
            .map_err(|error| RuntimeInstallError::UnsafeArchive(error.to_string()))?;
        output
            .sync_all()
            .map_err(|error| RuntimeInstallError::UnsafeArchive(error.to_string()))?;
        let mode = entry.header().mode().ok();
        set_unix_mode(&destination, mode)?;
    }
    for (destination, target) in hardlinks {
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| RuntimeInstallError::UnsafeArchive(error.to_string()))?;
        }
        if !target.is_file() {
            return Err(RuntimeInstallError::UnsafeArchive(format!(
                "hardlink target is missing or not a regular file: {}",
                target.display()
            )));
        }
        std::fs::hard_link(&target, &destination)
            .map_err(|error| RuntimeInstallError::UnsafeArchive(error.to_string()))?;
    }
    for (destination, target) in &symlinks {
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| RuntimeInstallError::UnsafeArchive(error.to_string()))?;
        }
        create_symlink(target, destination)?;
    }
    let canonical_staging = std::fs::canonicalize(staging)
        .map_err(|error| RuntimeInstallError::UnsafeArchive(error.to_string()))?;
    for (destination, _) in symlinks {
        let resolved = std::fs::canonicalize(&destination).map_err(|error| {
            RuntimeInstallError::UnsafeArchive(format!(
                "archive symlink is broken or cyclic at {}: {error}",
                destination.display()
            ))
        })?;
        if !resolved.starts_with(&canonical_staging) {
            return Err(RuntimeInstallError::UnsafeArchive(format!(
                "archive symlink escapes staging: {}",
                destination.display()
            )));
        }
    }
    Ok(())
}

fn checked_archive_path(value: &str) -> Result<PathBuf, RuntimeInstallError> {
    if value.is_empty()
        || value.contains('\\')
        || value.contains(':')
        || value.starts_with('/')
        || value.starts_with('~')
    {
        return Err(RuntimeInstallError::UnsafeArchive(format!(
            "unsafe archive path `{value}`"
        )));
    }
    let path = PathBuf::from(value);
    if !is_safe_relative_path(&path)
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_) | Component::CurDir))
    {
        return Err(RuntimeInstallError::UnsafeArchive(format!(
            "unsafe archive path `{value}`"
        )));
    }
    Ok(path)
}

fn validate_relative_link_target(
    link_path: &Path,
    target: &Path,
) -> Result<(), RuntimeInstallError> {
    let target_text = target.to_string_lossy();
    if target_text.is_empty()
        || target_text.contains('\\')
        || target_text.contains(':')
        || target.is_absolute()
    {
        return Err(RuntimeInstallError::UnsafeArchive(format!(
            "unsafe archive link target `{target_text}`"
        )));
    }
    let mut depth = link_path.parent().map_or(0, |parent| {
        parent
            .components()
            .filter(|component| matches!(component, Component::Normal(_)))
            .count()
    });
    for component in target.components() {
        match component {
            Component::Normal(_) => depth += 1,
            Component::CurDir => {}
            Component::ParentDir if depth > 0 => depth -= 1,
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(RuntimeInstallError::UnsafeArchive(format!(
                    "archive link target escapes staging: `{target_text}`"
                )));
            }
        }
    }
    Ok(())
}

fn normalized_archive_key(path: &Path) -> String {
    let value = path.to_string_lossy().replace('\\', "/");
    if cfg!(windows) {
        value.to_ascii_lowercase()
    } else {
        value
    }
}

fn locate_entrypoint(staging: &Path, expected: &[String]) -> Result<PathBuf, String> {
    let mut matches = Vec::new();
    for entry in walkdir::WalkDir::new(staging).follow_links(false) {
        let entry = entry.map_err(|error| error.to_string())?;
        if !entry.file_type().is_file() {
            continue;
        }
        let Some(name) = entry.file_name().to_str() else {
            continue;
        };
        if expected
            .iter()
            .any(|expected| name.eq_ignore_ascii_case(expected))
        {
            matches.push(entry.path().to_path_buf());
        }
    }
    match matches.as_slice() {
        [entrypoint] => Ok(entrypoint.clone()),
        [] => Err(format!(
            "expected one of [{}], but none was present",
            expected.join(", ")
        )),
        _ => Err(format!(
            "expected one entrypoint, found {} matching files",
            matches.len()
        )),
    }
}

async fn write_manifest(
    path: &Path,
    manifest: &RuntimeManifest,
) -> Result<(), RuntimeInstallError> {
    let bytes = serde_json::to_vec_pretty(manifest)
        .map_err(|error| RuntimeInstallError::InvalidMetadata(error.to_string()))?;
    let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .await
        .map_err(|error| RuntimeInstallError::InvalidMetadata(error.to_string()))?;
    file.write_all(&bytes)
        .await
        .map_err(|error| RuntimeInstallError::InvalidMetadata(error.to_string()))?;
    file.flush()
        .await
        .map_err(|error| RuntimeInstallError::InvalidMetadata(error.to_string()))?;
    file.sync_all()
        .await
        .map_err(|error| RuntimeInstallError::InvalidMetadata(error.to_string()))
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

fn verify_package_digest(expected: &str, observed: &str) -> Result<(), RuntimeInstallError> {
    if observed == expected {
        Ok(())
    } else {
        Err(RuntimeInstallError::ChecksumMismatch {
            expected: expected.to_owned(),
            observed: observed.to_owned(),
        })
    }
}

#[cfg(unix)]
fn set_unix_mode(path: &Path, mode: Option<u32>) -> Result<(), RuntimeInstallError> {
    use std::os::unix::fs::PermissionsExt;

    if let Some(mode) = mode {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode & 0o777))
            .map_err(|error| RuntimeInstallError::UnsafeArchive(error.to_string()))?;
    }
    Ok(())
}

#[cfg(unix)]
fn create_symlink(target: &Path, destination: &Path) -> Result<(), RuntimeInstallError> {
    std::os::unix::fs::symlink(target, destination)
        .map_err(|error| RuntimeInstallError::UnsafeArchive(error.to_string()))
}

#[cfg(not(unix))]
fn create_symlink(_target: &Path, _destination: &Path) -> Result<(), RuntimeInstallError> {
    Err(RuntimeInstallError::UnsafeArchive(
        "archive symlinks are unsupported on this host".to_owned(),
    ))
}

#[cfg(not(unix))]
fn set_unix_mode(_path: &Path, _mode: Option<u32>) -> Result<(), RuntimeInstallError> {
    Ok(())
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
    use std::collections::BTreeMap;
    use std::io::Write;
    use std::path::Path;
    use std::time::Duration;

    use norted_core::{
        AppPaths, RuntimeArchiveFormat, RuntimeSourceBuildPlan, RuntimeSourceBuildPrerequisites,
        RuntimeSourceBuildRecipe, RuntimeSourceBuildSystem, RuntimeSourceSnapshot,
        effective_cmake_configuration_arguments,
    };
    use sha2::{Digest, Sha256};
    use tokio::sync::oneshot;

    use crate::store::RuntimeStore;

    use super::{
        RuntimeInstallError, SourceBuildPrerequisiteEvaluationKey, extract_archive, hex_digest,
        inspect_build_dependency_contract, require_cuda_compiler_targets, require_minimum_version,
        require_version_below, required_cuda_compiler_targets, run_owned_staging_operation,
        run_source_command, validate_relative_link_target, verify_package_digest,
        verify_source_checkout,
    };

    #[test]
    fn explicit_cuda_architectures_are_checked_against_nvcc_targets() {
        let arguments = vec![
            "-G".to_owned(),
            "Ninja".to_owned(),
            "-DCMAKE_CUDA_ARCHITECTURES=75-real;90-virtual;120a".to_owned(),
        ];
        let required = required_cuda_compiler_targets(&arguments)
            .expect("valid CUDA policy")
            .expect("explicit CUDA policy");
        assert_eq!(required.real, ["sm_75", "sm_120a"]);
        assert_eq!(required.virtual_targets, ["compute_90", "compute_120a"]);
        require_cuda_compiler_targets("real", &required.real, "sm_75\nsm_90\nsm_120a\n")
            .expect("all real targets supported");
        require_cuda_compiler_targets(
            "real",
            &required.real,
            "accepted values: 'sm_75','sm_90','sm_120a'",
        )
        .expect("nvcc help punctuation preserves exact target tokens");
        assert!(matches!(
            require_cuda_compiler_targets("real", &required.real, "sm_75\nsm_90\n"),
            Err(RuntimeInstallError::Prerequisite(message))
                if message.contains("sm_120a")
        ));
    }

    #[test]
    fn malformed_or_ambiguous_cuda_architecture_policies_fail_closed() {
        for architecture in ["", "120aa", "sm_120", "75-real;"] {
            let arguments = [format!("-DCMAKE_CUDA_ARCHITECTURES={architecture}")];
            assert!(required_cuda_compiler_targets(&arguments).is_err());
        }
        let duplicate = [
            "-DCMAKE_CUDA_ARCHITECTURES=75-real".to_owned(),
            "-DCMAKE_CUDA_ARCHITECTURES=120a-real".to_owned(),
        ];
        assert!(required_cuda_compiler_targets(&duplicate).is_err());
    }

    #[test]
    fn exclusive_cuda_toolkit_ceiling_rejects_the_next_major() {
        require_version_below("CUDA Toolkit", "12.8", "13.0").expect("CUDA 12.8 is admitted");
        require_version_below("CUDA Toolkit", "12.9.1", "13.0")
            .expect("all CUDA 12.x releases are admitted");
        assert!(require_version_below("CUDA Toolkit", "13.0", "13.0").is_err());
        assert!(require_version_below("CUDA Toolkit", "14.0", "13.0").is_err());

        require_minimum_version("CUDA Toolkit", "13.0", "13.0")
            .expect("CUDA 13.0 is admitted by the CUDA-13 recipe");
        require_minimum_version("CUDA Toolkit", "13.3.73", "13.0")
            .expect("later CUDA 13.x is admitted by the CUDA-13 recipe");
        require_version_below("CUDA Toolkit", "13.3.73", "14.0")
            .expect("CUDA 13.x remains below the next major");
        assert!(require_minimum_version("CUDA Toolkit", "12.9", "13.0").is_err());
        assert!(require_version_below("CUDA Toolkit", "14.0", "14.0").is_err());
    }

    #[test]
    fn prerequisite_evaluation_key_includes_explicit_cuda_architectures() {
        let plan = RuntimeSourceBuildPlan {
            source: RuntimeSourceSnapshot {
                repository: "owner/repository".to_owned(),
                repository_url: "https://github.com/owner/repository.git".to_owned(),
                source_branch: "main".to_owned(),
                commit_sha: "a".repeat(40),
                tree_sha: "b".repeat(40),
                commit_timestamp_unix: 1,
                source_provider: "fixture".to_owned(),
            },
            recipe: RuntimeSourceBuildRecipe {
                recipe_version: "fixture-v1".to_owned(),
                build_system: RuntimeSourceBuildSystem::Cmake,
                build_definition_sha256: None,
                cmake_configuration_arguments: vec![
                    "-DCMAKE_CUDA_ARCHITECTURES=75-real".to_owned(),
                ],
                build_target: "server".to_owned(),
                entrypoint: "build/server".into(),
                accelerator_target: "sm_75".to_owned(),
                rejected_build_environment: Vec::new(),
            },
            prerequisites: RuntimeSourceBuildPrerequisites {
                minimum_cmake_version: "3.18".to_owned(),
                minimum_cuda_version: Some("12.8".to_owned()),
                maximum_cuda_version_exclusive: Some("13.0".to_owned()),
                requires_ninja: true,
                requires_cpp20_compiler: false,
                requires_make: false,
                minimum_cpp_standard: Some(17),
                cpp_compiler: None,
                cuda_compiler: None,
                requires_pkg_config: false,
                pkg_config_modules: BTreeMap::new(),
            },
        };
        let mut different_targets = plan.clone();
        different_targets.recipe.cmake_configuration_arguments =
            vec!["-DCMAKE_CUDA_ARCHITECTURES=120a-real".to_owned()];

        assert_eq!(plan.prerequisites, different_targets.prerequisites);
        assert_ne!(
            SourceBuildPrerequisiteEvaluationKey::from_plan(&plan),
            SourceBuildPrerequisiteEvaluationKey::from_plan(&different_targets)
        );

        let mut explicit_compiler = plan.clone();
        explicit_compiler.prerequisites.cuda_compiler = Some("/usr/local/cuda/bin/nvcc".into());
        assert_ne!(
            SourceBuildPrerequisiteEvaluationKey::from_plan(&plan),
            SourceBuildPrerequisiteEvaluationKey::from_plan(&explicit_compiler)
        );
        assert_eq!(
            effective_cmake_configuration_arguments(&explicit_compiler)
                .expect("typed CUDA compiler binding"),
            Some(vec![
                "-DCMAKE_CUDA_ARCHITECTURES=75-real".to_owned(),
                "-DCMAKE_CUDA_COMPILER=/usr/local/cuda/bin/nvcc".to_owned(),
            ])
        );

        explicit_compiler
            .recipe
            .cmake_configuration_arguments
            .push("-DCMAKE_CUDA_COMPILER=/other/nvcc".to_owned());
        assert!(effective_cmake_configuration_arguments(&explicit_compiler).is_err());

        explicit_compiler.recipe.cmake_configuration_arguments.pop();
        explicit_compiler.recipe.build_system = RuntimeSourceBuildSystem::Make;
        assert_eq!(
            effective_cmake_configuration_arguments(&explicit_compiler)
                .expect("Make recipes retain their own compiler contract"),
            None
        );
    }

    #[tokio::test]
    async fn blocking_staging_writer_retains_cleanup_ownership_after_caller_cancellation() {
        let workspace = tempfile::tempdir().expect("temporary runtime store");
        let paths = AppPaths {
            config_dir: workspace.path().join("config"),
            config_file: workspace.path().join("config/config.toml"),
            data_dir: workspace.path().join("data"),
            state_dir: workspace.path().join("state"),
            cache_dir: workspace.path().join("cache"),
            log_dir: workspace.path().join("logs"),
            runtimes_dir: workspace.path().join("data/runtimes"),
            runtime_cache_dir: workspace.path().join("cache/runtime-packs"),
            runtime_selections_file: workspace.path().join("data/runtime-selections.json"),
            settings_file: workspace.path().join("data/settings.json"),
            settings_lock_file: workspace.path().join("data/.settings.lock"),
            model_profiles_file: workspace.path().join("data/model-profiles.json"),
            model_profiles_lock_file: workspace.path().join("data/.model-profiles.lock"),
        };
        let store = RuntimeStore::new(&paths);
        let staging = store.create_staging().await.expect("staging directory");
        let staging_path = staging.path().to_path_buf();
        let (started_tx, started_rx) = oneshot::channel();
        let (release_tx, release_rx) = oneshot::channel();
        let (finished_tx, finished_rx) = oneshot::channel();
        let writer = tokio::spawn(async move {
            run_owned_staging_operation(staging, move |path| {
                started_tx.send(()).expect("signal writer start");
                release_rx.blocking_recv().expect("release staging writer");
                std::fs::create_dir_all(path.join("late"))
                    .expect("write staging after caller cancellation");
                std::fs::write(path.join("late/entry"), b"complete").expect("finish staging write");
                finished_tx.send(()).expect("signal writer completion");
                Ok(())
            })
            .await
        });

        started_rx.await.expect("blocking writer started");
        writer.abort();
        let _ = writer.await;
        assert!(
            staging_path.exists(),
            "blocking writer must retain the staging guard after its caller is dropped"
        );

        release_tx.send(()).expect("release blocking writer");
        finished_rx.await.expect("blocking writer finished");
        tokio::time::timeout(Duration::from_secs(5), async {
            while staging_path.exists() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("detached blocking writer must ultimately drop and clean staging");
        assert!(!staging_path.exists());
    }

    #[test]
    fn zip_and_tar_traversal_are_rejected() {
        let workspace = tempfile::tempdir().expect("temporary directory");
        let zip_path = workspace.path().join("bad.zip");
        {
            let file = std::fs::File::create(&zip_path).expect("zip file");
            let mut zip = zip::ZipWriter::new(file);
            zip.start_file("../escape", zip::write::SimpleFileOptions::default())
                .expect("zip entry");
            zip.write_all(b"bad").expect("zip contents");
            zip.finish().expect("finish zip");
        }
        let staging = workspace.path().join("zip-staging");
        std::fs::create_dir(&staging).expect("zip staging");
        assert!(matches!(
            extract_archive(&zip_path, &staging, RuntimeArchiveFormat::Zip),
            Err(RuntimeInstallError::UnsafeArchive(_))
        ));

        let tar_path = workspace.path().join("bad.tar.gz");
        {
            let file = std::fs::File::create(&tar_path).expect("tar file");
            let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
            let mut tar = tar::Builder::new(encoder);
            let mut header = tar::Header::new_gnu();
            header.set_size(3);
            header.set_mode(0o644);
            header.set_cksum();
            // Build the malicious path directly in the header because recent
            // tar builders correctly refuse traversal paths themselves.
            let bytes = header.as_mut_bytes();
            bytes[..9].copy_from_slice(b"../escape");
            header.set_cksum();
            tar.append(&header, &b"bad"[..]).expect("tar entry");
            tar.finish().expect("finish tar");
        }
        let staging = workspace.path().join("tar-staging");
        std::fs::create_dir(&staging).expect("tar staging");
        assert!(matches!(
            extract_archive(&tar_path, &staging, RuntimeArchiveFormat::TarGz),
            Err(RuntimeInstallError::UnsafeArchive(_))
        ));
    }

    #[test]
    fn checksum_mismatch_is_rejected() {
        let expected = "a".repeat(64);
        let observed = "b".repeat(64);
        let error = verify_package_digest(&expected, &observed)
            .expect_err("mismatched package digest must fail");
        assert!(error.to_string().contains(&expected));
        assert!(error.to_string().contains(&observed));
        assert!(verify_package_digest(&expected, &expected).is_ok());
    }

    #[test]
    fn archive_link_targets_must_remain_inside_staging() {
        assert!(
            validate_relative_link_target(Path::new("bin/link"), Path::new("../lib/file")).is_ok()
        );
        assert!(
            validate_relative_link_target(Path::new("bin/link"), Path::new("../../escape"))
                .is_err()
        );
    }

    #[test]
    fn source_checkout_requires_both_exact_commit_and_tree() {
        let commit = "a".repeat(40);
        let tree = "b".repeat(40);
        assert!(verify_source_checkout(&commit, &tree, &commit, &tree).is_ok());
        assert!(matches!(
            verify_source_checkout(&commit, &tree, &"c".repeat(40), &tree),
            Err(RuntimeInstallError::SourceChanged(message)) if message.contains("revision mismatch")
        ));
        assert!(matches!(
            verify_source_checkout(&commit, &tree, &commit, &"d".repeat(40)),
            Err(RuntimeInstallError::SourceChanged(message)) if message.contains("tree mismatch")
        ));
    }

    #[test]
    fn source_dependency_audit_rejects_networked_or_changed_build_contracts() {
        let safe = tempfile::tempdir().expect("safe CMake fixture");
        std::fs::write(
            safe.path().join("CMakeLists.txt"),
            "cmake_minimum_required(VERSION 3.28)\nproject(local LANGUAGES CXX)\n",
        )
        .expect("safe CMake fixture");
        inspect_build_dependency_contract(
            safe.path(),
            RuntimeSourceBuildSystem::Cmake,
            "fixture",
            None,
            &[],
        )
        .expect("local-only CMake tree");

        let networked = tempfile::tempdir().expect("networked CMake fixture");
        std::fs::write(
            networked.path().join("CMakeLists.txt"),
            "include(FetchContent)\nFetchContent_Declare(dep URL https://example.invalid/dep.tar.gz)\n",
        )
        .expect("networked CMake fixture");
        assert!(matches!(
            inspect_build_dependency_contract(
                networked.path(),
                RuntimeSourceBuildSystem::Cmake,
                "fixture",
                None,
                &[],
            ),
            Err(RuntimeInstallError::SourceBuild(message)) if message.contains("dependency audit rejected")
        ));

        let make = tempfile::tempdir().expect("Makefile fixture");
        std::fs::write(
            make.path().join("Makefile"),
            "build/q27-server:\n\t$(NVCC) server.cu -o $@\n",
        )
        .expect("Makefile fixture");
        inspect_build_dependency_contract(
            make.path(),
            RuntimeSourceBuildSystem::Make,
            "build/q27-server",
            Some(&hex_digest(Sha256::digest(
                b"build/q27-server:\n\t$(NVCC) server.cu -o $@\n",
            ))),
            &[],
        )
        .expect("declared local-only Make target");
        assert!(matches!(
            inspect_build_dependency_contract(
                make.path(),
                RuntimeSourceBuildSystem::Make,
                "build/q27-server-w8",
                Some(&hex_digest(Sha256::digest(
                    b"build/q27-server:\n\t$(NVCC) server.cu -o $@\n",
                ))),
                &[],
            ),
            Err(RuntimeInstallError::SourceBuild(message)) if message.contains("does not declare")
        ));

        let audited_makefile = "build/q27-server:\n\t$(NVCC) server.cu -o $@\n";
        let audited_digest = hex_digest(Sha256::digest(audited_makefile.as_bytes()));
        std::fs::create_dir_all(make.path().join("tools")).expect("helper directory");
        std::fs::write(
            make.path().join("tools/fetch_dependency.py"),
            "import urllib.request\nurllib.request.urlopen('https://example.invalid/dep')\n",
        )
        .expect("network helper");
        std::fs::write(
            make.path().join("Makefile"),
            "build/q27-server:\n\tpython3 tools/fetch_dependency.py\n\t$(NVCC) server.cu -o $@\n",
        )
        .expect("indirect network Makefile");
        assert!(matches!(
            inspect_build_dependency_contract(
                make.path(),
                RuntimeSourceBuildSystem::Make,
                "build/q27-server",
                Some(&audited_digest),
                &[],
            ),
            Err(RuntimeInstallError::SourceBuild(message))
                if message.contains("provider-audited dependency/command closure")
        ));
    }

    #[tokio::test]
    async fn source_commands_are_owned_and_report_bounded_failure_output() {
        run_source_command("Rust compiler probe", "rustc", &["--version"], None, &[])
            .await
            .expect("successful direct source command");
        let error = run_source_command(
            "failing Rust compiler probe",
            "rustc",
            &["--definitely-not-a-rustc-option"],
            None,
            &[],
        )
        .await
        .expect_err("failing direct source command");
        assert!(error.to_string().contains("failing Rust compiler probe"));
        assert!(error.to_string().contains("definitely-not-a-rustc-option"));
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn dropping_source_command_kills_its_entire_process_group() {
        let workspace = tempfile::tempdir().expect("temporary process fixture");
        let direct_pid_path = workspace.path().join("direct.pid");
        let descendant_pid_path = workspace.path().join("descendant.pid");
        let working_directory = workspace.path().to_path_buf();
        let command = tokio::spawn(async move {
            run_source_command(
                "test compiler process tree",
                "sh",
                &[
                    "-c",
                    "sleep 30 & child=$!; printf '%s\\n' \"$$\" > direct.pid; printf '%s\\n' \"$child\" > descendant.pid; wait",
                ],
                Some(&working_directory),
                &[],
            )
            .await
        });

        let direct_pid = wait_for_fixture_pid(&direct_pid_path).await;
        let descendant_pid = wait_for_fixture_pid(&descendant_pid_path).await;
        assert!(linux_process_is_running(direct_pid));
        assert!(linux_process_is_running(descendant_pid));

        command.abort();
        let _ = command.await;
        for _ in 0..100 {
            if !linux_process_is_running(direct_pid) && !linux_process_is_running(descendant_pid) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(!linux_process_is_running(direct_pid));
        assert!(!linux_process_is_running(descendant_pid));
    }

    #[cfg(target_os = "linux")]
    async fn wait_for_fixture_pid(path: &Path) -> libc::pid_t {
        for _ in 0..100 {
            if let Ok(text) = tokio::fs::read_to_string(path).await
                && let Ok(pid) = text.trim().parse()
            {
                return pid;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("fixture PID was not written to {}", path.display());
    }

    #[cfg(target_os = "linux")]
    fn linux_process_is_running(pid: libc::pid_t) -> bool {
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
            return false;
        };
        stat.rsplit_once(") ")
            .and_then(|(_, fields)| fields.split_whitespace().next())
            .is_some_and(|state| state != "Z")
    }
}
