use std::collections::BTreeMap;

use async_trait::async_trait;
use norted_core::{
    ArtifactFormat, AvailableRuntime, RuntimeAcquisitionPlan, RuntimeId, RuntimeIdentity,
    RuntimePackageIdentity, RuntimeReleaseChannel, RuntimeRequirements, RuntimeSourceBuildPlan,
    RuntimeSourceBuildPrerequisites, RuntimeSourceBuildRecipe, RuntimeSourceBuildSystem,
    RuntimeSourceSnapshot,
};
use norted_engine::{
    CatalogError, GitHubCommit, GitHubRelease, GitHubReleaseClient, RuntimeCatalogProvider,
};

use crate::catalog::{
    GITHUB_REPOSITORY, nightly_number, nightly_tag_from_reference, parse_github_timestamp,
};
use crate::{ENGINE_ID, UPSTREAM_REPOSITORY};

const PACKAGE_FAMILY: &str = "llama-cpp-managed-source";
const RECIPE_VERSION: &str = "managed-portable-v1";

pub const LLAMA_CPP_SOURCE_RUNTIME_PROVIDER_ID: &str = "llama-cpp-managed-source-github";

#[derive(Debug, Clone, Copy, Default)]
pub struct LlamaCppSourceRuntimeCatalogProvider;

impl LlamaCppSourceRuntimeCatalogProvider {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl RuntimeCatalogProvider for LlamaCppSourceRuntimeCatalogProvider {
    fn id(&self) -> &'static str {
        LLAMA_CPP_SOURCE_RUNTIME_PROVIDER_ID
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
        verify_repository(github).await?;
        let release = github
            .releases(GITHUB_REPOSITORY)
            .await?
            .into_iter()
            .filter(|release| !release.draft)
            .filter_map(|release| nightly_number(&release.tag_name).map(|build| (build, release)))
            .max_by_key(|(build, _)| *build)
            .map(|(_, release)| release)
            .ok_or_else(|| provider_error("no valid upstream nightly release was found"))?;
        let commit = resolve_release_commit(github, &release).await?;
        Ok(vec![source_runtime(&release, commit)?])
    }

    async fn fetch_reference(
        &self,
        github: &GitHubReleaseClient,
        reference: &str,
    ) -> Result<Vec<AvailableRuntime>, CatalogError> {
        let Some(tag) = nightly_tag_from_reference(reference) else {
            return Ok(Vec::new());
        };
        verify_repository(github).await?;
        let Some(release) = github.release_by_tag(GITHUB_REPOSITORY, &tag).await? else {
            return Ok(Vec::new());
        };
        if release.draft || nightly_number(&release.tag_name).is_none() {
            return Ok(Vec::new());
        }
        let commit = resolve_release_commit(github, &release).await?;
        Ok(vec![source_runtime(&release, commit)?])
    }

    async fn verify_candidate(
        &self,
        github: &GitHubReleaseClient,
        candidate: &AvailableRuntime,
    ) -> Result<Option<AvailableRuntime>, CatalogError> {
        let RuntimeAcquisitionPlan::SourceBuild(plan) = &candidate.acquisition else {
            return Ok(None);
        };
        if candidate.identity.engine_id != ENGINE_ID
            || candidate.identity.package.provider_id != LLAMA_CPP_SOURCE_RUNTIME_PROVIDER_ID
            || candidate.identity.package.repository.as_deref() != Some(GITHUB_REPOSITORY)
            || candidate.identity.package.release_tag.as_deref()
                != Some(plan.source.source_branch.as_str())
            || plan.source.repository != GITHUB_REPOSITORY
            || plan.source.repository_url != format!("{UPSTREAM_REPOSITORY}.git")
            || plan.source.source_provider != LLAMA_CPP_SOURCE_RUNTIME_PROVIDER_ID
        {
            return Ok(None);
        }

        verify_repository(github).await?;
        let Some(release) = github
            .release_by_tag(GITHUB_REPOSITORY, &plan.source.source_branch)
            .await?
        else {
            return Ok(None);
        };
        if release.draft || nightly_number(&release.tag_name).is_none() {
            return Ok(None);
        }
        let commit = resolve_release_commit(github, &release).await?;
        if commit.sha != plan.source.commit_sha || commit.commit.tree.sha != plan.source.tree_sha {
            return Err(provider_error(
                "selected source commit/tree metadata differs from the live canonical repository",
            ));
        }
        let live = source_runtime(&release, commit)?;
        if live.runtime_id != candidate.runtime_id
            || live.identity != candidate.identity
            || live.display_name != candidate.display_name
            || live.supported_formats != candidate.supported_formats
            || live.source_url != candidate.source_url
            || live.published_at_unix != candidate.published_at_unix
            || live.prerelease != candidate.prerelease
            || live.acquisition != candidate.acquisition
            || live.supported_native_identities != candidate.supported_native_identities
            || live.requirements != candidate.requirements
        {
            return Err(provider_error(
                "selected source runtime contract differs from the canonical Norted build recipe",
            ));
        }
        Ok(Some(live))
    }
}

async fn verify_repository(github: &GitHubReleaseClient) -> Result<(), CatalogError> {
    let repository = github.repository(GITHUB_REPOSITORY).await?;
    if repository.full_name != GITHUB_REPOSITORY
        || repository.html_url != UPSTREAM_REPOSITORY
        || repository.default_branch.trim().is_empty()
    {
        return Err(provider_error("canonical repository identity is invalid"));
    }
    Ok(())
}

async fn resolve_release_commit(
    github: &GitHubReleaseClient,
    release: &GitHubRelease,
) -> Result<GitHubCommit, CatalogError> {
    let commit = github.commit(GITHUB_REPOSITORY, &release.tag_name).await?;
    if !norted_core::is_full_git_sha(&commit.sha)
        || !norted_core::is_full_git_sha(&commit.commit.tree.sha)
        || (norted_core::is_full_git_sha(&release.target_commitish)
            && !release.target_commitish.eq_ignore_ascii_case(&commit.sha))
    {
        return Err(provider_error(
            "release tag did not resolve to the expected full commit and tree SHAs",
        ));
    }
    Ok(commit)
}

fn source_runtime(
    release: &GitHubRelease,
    commit: GitHubCommit,
) -> Result<AvailableRuntime, CatalogError> {
    let build = nightly_number(&release.tag_name)
        .ok_or_else(|| provider_error("source release is not a valid nightly build"))?;
    let timestamp = parse_github_timestamp(&commit.commit.committer.date)
        .ok_or_else(|| provider_error("source commit has an invalid timestamp"))?;
    let commit_sha = commit.sha.clone();
    let identity = RuntimeIdentity {
        engine_id: ENGINE_ID.to_owned(),
        package_family: PACKAGE_FAMILY.to_owned(),
        version: release.tag_name.clone(),
        upstream_revision: Some(commit.sha.clone()),
        platform: "linux".to_owned(),
        architecture: "x86_64".to_owned(),
        accelerator: "cuda".to_owned(),
        variant: RECIPE_VERSION.to_owned(),
        package: RuntimePackageIdentity {
            provider_id: LLAMA_CPP_SOURCE_RUNTIME_PROVIDER_ID.to_owned(),
            repository: Some(GITHUB_REPOSITORY.to_owned()),
            release_tag: Some(release.tag_name.clone()),
            asset_id: None,
            asset_name: None,
            additional_assets: Vec::new(),
        },
    };
    let runtime = AvailableRuntime {
        runtime_id: RuntimeId::from_identity(&identity),
        identity,
        display_name: "llama.cpp CUDA managed source (Linux x86_64)".to_owned(),
        supported_formats: vec![ArtifactFormat::Gguf],
        source_url: commit.html_url,
        published_at_unix: Some(timestamp),
        channels: std::iter::once(RuntimeReleaseChannel::Latest)
            .chain(release.prerelease.then_some(RuntimeReleaseChannel::Prerelease))
            .collect(),
        prerelease: release.prerelease,
        acquisition: RuntimeAcquisitionPlan::SourceBuild(Box::new(RuntimeSourceBuildPlan {
            source: RuntimeSourceSnapshot {
                repository: GITHUB_REPOSITORY.to_owned(),
                repository_url: format!("{UPSTREAM_REPOSITORY}.git"),
                source_branch: release.tag_name.clone(),
                commit_sha: commit.sha,
                tree_sha: commit.commit.tree.sha,
                commit_timestamp_unix: timestamp,
                source_provider: LLAMA_CPP_SOURCE_RUNTIME_PROVIDER_ID.to_owned(),
            },
            recipe: RuntimeSourceBuildRecipe {
                recipe_version: RECIPE_VERSION.to_owned(),
                build_system: RuntimeSourceBuildSystem::Cmake,
                build_definition_sha256: None,
                cmake_configuration_arguments: vec![
                    "-G".to_owned(),
                    "Ninja".to_owned(),
                    "-DCMAKE_BUILD_TYPE=Release".to_owned(),
                    format!("-DLLAMA_BUILD_NUMBER={build}"),
                    format!("-DLLAMA_BUILD_COMMIT={commit_sha}"),
                    "-DGGML_CUDA=ON".to_owned(),
                    "-DGGML_NATIVE=OFF".to_owned(),
                    "-DLLAMA_BUILD_SERVER=ON".to_owned(),
                    "-DLLAMA_BUILD_TOOLS=ON".to_owned(),
                    "-DLLAMA_BUILD_TESTS=OFF".to_owned(),
                    "-DLLAMA_BUILD_EXAMPLES=OFF".to_owned(),
                    "-DLLAMA_BUILD_APP=OFF".to_owned(),
                    "-DLLAMA_BUILD_UI=OFF".to_owned(),
                    "-DLLAMA_USE_PREBUILT_UI=OFF".to_owned(),
                    "-DLLAMA_LLGUIDANCE=OFF".to_owned(),
                    "-DLLAMA_OPENSSL=OFF".to_owned(),
                    "-DGGML_CUDA_CUB_3DOT2=OFF".to_owned(),
                    "-DGGML_CPU_KLEIDIAI=OFF".to_owned(),
                    "-DFETCHCONTENT_FULLY_DISCONNECTED=ON".to_owned(),
                    "-DFETCHCONTENT_UPDATES_DISCONNECTED=ON".to_owned(),
                ],
                build_target: "llama-server".to_owned(),
                entrypoint: "build/bin/llama-server".into(),
                accelerator_target: "portable-cuda".to_owned(),
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
                    "NVCC_PREPEND_FLAGS".to_owned(),
                    "NVCC_APPEND_FLAGS".to_owned(),
                    "CMAKE_ARGS".to_owned(),
                    "CMAKE_BUILD_PARALLEL_LEVEL".to_owned(),
                    "CMAKE_GENERATOR".to_owned(),
                    "CMAKE_GENERATOR_INSTANCE".to_owned(),
                    "CMAKE_GENERATOR_PLATFORM".to_owned(),
                    "CMAKE_GENERATOR_TOOLSET".to_owned(),
                    "CMAKE_TOOLCHAIN_FILE".to_owned(),
                ],
            },
            prerequisites: RuntimeSourceBuildPrerequisites {
                minimum_cmake_version: "3.18".to_owned(),
                minimum_cuda_version: None,
                requires_ninja: true,
                requires_cpp20_compiler: false,
                requires_make: false,
                minimum_cpp_standard: Some(17),
                cpp_compiler: None,
                cuda_compiler: None,
                requires_pkg_config: false,
                pkg_config_modules: BTreeMap::new(),
            },
        })),
        supported_native_identities: Vec::new(),
        requirements: RuntimeRequirements {
            requires_nvidia_gpu: true,
            minimum_nvidia_driver: None,
            minimum_vram_bytes: None,
            minimum_vram_class_gib: None,
            minimum_vram_exclusive_class_gib: None,
            supported_cuda_compute_capabilities: Vec::new(),
            required_nvidia_device_names: Vec::new(),
            notes: Vec::new(),
            advisories: vec![
                "Norted builds this runtime from the exact official llama.cpp source revision; it is not an upstream CUDA binary"
                    .to_owned(),
                "The portable build covers CUDA GPU architectures supported by the installed CUDA Toolkit"
                    .to_owned(),
            ],
            unverified_requirements: Vec::new(),
        },
    };
    runtime
        .validate()
        .map_err(|error| provider_error(&error.to_string()))?;
    Ok(runtime)
}

fn provider_error(message: &str) -> CatalogError {
    CatalogError::Provider {
        provider: LLAMA_CPP_SOURCE_RUNTIME_PROVIDER_ID.to_owned(),
        message: message.to_owned(),
    }
}
