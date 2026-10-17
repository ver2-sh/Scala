//! NInfer-all acquisition is independent of official NInfer and of serving qualification.
//! Reviewed native build/container snapshot; schema-28 inference is not inherited from v3.
use std::collections::BTreeMap;
use std::path::Path;

use async_trait::async_trait;
use scala_core::{
    ArtifactFormat, AvailableRuntime, ComputeCapability, InstalledRuntime,
    RuntimeAcquisitionMethod, RuntimeAcquisitionPlan, RuntimeId, RuntimeIdentity,
    RuntimePackageIdentity, RuntimeReleaseChannel, RuntimeRequirements, RuntimeSourceBuildPlan,
    RuntimeSourceBuildPrerequisites, RuntimeSourceBuildRecipe, RuntimeSourceBuildSystem,
    RuntimeSourceSnapshot,
};
use scala_engine::{CatalogError, GitHubCommit, GitHubReleaseClient, RuntimeCatalogProvider};
use sha2::{Digest, Sha256};

pub const PROVIDER_ID: &str = "ninfer-all-source-github";
pub const REPOSITORY: &str = "iamwavecut/ninfer-all";
const URL: &str = "https://github.com/iamwavecut/ninfer-all";
const FAMILY: &str = "ninfer-all-source";
const RECIPE: &str = "ninfer-all-serve-v3-r1";
fn recipe_for(platform: &str) -> String {
    if platform == "windows" {
        super::source_dependencies::recipe_version()
    } else {
        RECIPE.into()
    }
}
pub(super) const REVISION: &str = "08fd7019925f43b4b42539400afd461b70f7144a";
pub(super) const TREE: &str = "bd17d1f3ccd45f81a25369e779cdf4cfa56de919";
const CMAKE_SHA256: &str = "0eecf524c1f20e55f62b2bc32efab2864c0b439dca4f3aa3743c339f2068182d";
const QUALIFICATION: &str = "NInfer-all schema-28 Chat/streaming and native YaRN/rotated-KV controls are independently qualified by static source review and synthetic tests; GPU memory fit, Windows device execution and v2-to-v3 conversion are not implied";

#[derive(Debug, Clone, Copy, Default)]
pub struct NinferAllRuntimeCatalogProvider;
impl NinferAllRuntimeCatalogProvider {
    pub fn new() -> Self {
        Self
    }
}

fn error(message: impl Into<String>) -> CatalogError {
    CatalogError::Provider {
        provider: PROVIDER_ID.into(),
        message: message.into(),
    }
}

#[async_trait]
impl RuntimeCatalogProvider for NinferAllRuntimeCatalogProvider {
    fn id(&self) -> &'static str {
        PROVIDER_ID
    }
    fn engine_id(&self) -> &'static str {
        super::ENGINE_ID
    }
    fn repository(&self) -> &'static str {
        REPOSITORY
    }

    async fn fetch(
        &self,
        _github: &GitHubReleaseClient,
    ) -> Result<Vec<AvailableRuntime>, CatalogError> {
        // Exact reviewed source and tree; Git checkout verifies both without REST quota.
        let commit = pinned_commit()?;
        Ok(vec![
            source_runtime("windows", commit.clone())?,
            source_runtime("linux", commit)?,
        ])
    }
    async fn verify_candidate(
        &self,
        _github: &GitHubReleaseClient,
        candidate: &AvailableRuntime,
    ) -> Result<Option<AvailableRuntime>, CatalogError> {
        if candidate.identity.package.provider_id != PROVIDER_ID {
            return Ok(None);
        }
        let expected = source_runtime(&candidate.identity.platform, pinned_commit()?)?;
        if &expected != candidate {
            return Err(error(
                "NInfer-all candidate differs from the reviewed source/build recipe",
            ));
        }
        Ok(Some(expected))
    }
}

fn pinned_commit() -> Result<GitHubCommit, CatalogError> {
    serde_json::from_value(serde_json::json!({
        "sha": REVISION, "html_url": format!("{URL}/commit/{REVISION}"),
        "commit": {"tree": {"sha": TREE}, "committer": {"date": "2026-10-16T17:09:10Z"}}
    }))
    .map_err(|e| error(format!("invalid pinned NInfer-all source identity: {e}")))
}
fn source_runtime(platform: &str, commit: GitHubCommit) -> Result<AvailableRuntime, CatalogError> {
    if !matches!(platform, "windows" | "linux")
        || commit.sha != REVISION
        || commit.commit.tree.sha != TREE
        || commit.html_url != format!("{URL}/commit/{REVISION}")
    {
        return Err(error(
            "NInfer-all source commit, tree, URL or platform is not admitted",
        ));
    }
    let timestamp = super::catalog::parse_github_timestamp(&commit.commit.committer.date)
        .ok_or_else(|| error("NInfer-all commit timestamp is invalid"))?;
    let windows = platform == "windows";
    let identity = RuntimeIdentity {
        engine_id: super::ENGINE_ID.into(),
        package_family: FAMILY.into(),
        version: "git-20261016-08fd7019".into(),
        upstream_revision: Some(REVISION.into()),
        platform: platform.into(),
        architecture: "x86_64".into(),
        accelerator: "cuda".into(),
        variant: format!("{}-sm120a", recipe_for(platform)),
        package: RuntimePackageIdentity {
            provider_id: PROVIDER_ID.into(),
            repository: Some(REPOSITORY.into()),
            release_tag: None,
            asset_id: None,
            asset_name: None,
            additional_assets: Vec::new(),
        },
    };
    let mut arguments: Vec<String> = [
        "-G",
        "Ninja",
        "-DCMAKE_BUILD_TYPE=Release",
        "-DNINFER_BUILD_APPS=ON",
        "-DBUILD_TESTING=OFF",
        "-DNINFER_BUILD_BENCHMARKS=OFF",
        "-DCMAKE_CUDA_ARCHITECTURES=120a",
        "-DNINFER_DIRECTSTORAGE=OFF",
        "-DFETCHCONTENT_FULLY_DISCONNECTED=ON",
        "-DFETCHCONTENT_UPDATES_DISCONNECTED=ON",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    if windows {
        arguments.extend(
            ["-DVCPKG_TARGET_TRIPLET=x64-windows"]
                .into_iter()
                .map(str::to_owned),
        );
    }
    let runtime = AvailableRuntime {
        runtime_id: RuntimeId::from_identity(&identity), identity,
        display_name: format!("NInfer-all native {platform} source 08fd7019 (v3, CUDA 13.4)"),
        supported_formats: vec![ArtifactFormat::Ninfer], source_url: commit.html_url,
        published_at_unix: Some(timestamp), channels: vec![RuntimeReleaseChannel::Latest], prerelease: false,
        acquisition: RuntimeAcquisitionPlan::SourceBuild(Box::new(RuntimeSourceBuildPlan {
            source: RuntimeSourceSnapshot { repository: REPOSITORY.into(), repository_url: format!("{URL}.git"),
                source_branch: "master".into(), commit_sha: REVISION.into(), tree_sha: TREE.into(),
                commit_timestamp_unix: timestamp, source_provider: PROVIDER_ID.into() },
            recipe: RuntimeSourceBuildRecipe { recipe_version: recipe_for(platform), build_system: RuntimeSourceBuildSystem::Cmake,
                build_definition_sha256: Some(CMAKE_SHA256.into()), cmake_configuration_arguments: arguments,
                build_target: "ninfer-serve".into(),
                entrypoint: if windows { "build/apps/ninfer-serve.exe" } else { "build/apps/ninfer-serve" }.into(),
                accelerator_target: "sm_120a".into(),
                rejected_build_environment: ["CC", "CXX", "CUDACXX", "CUDAHOSTCXX", "CFLAGS", "CPPFLAGS", "CXXFLAGS",
                    "CUDAFLAGS", "LDFLAGS", "NVCC_PREPEND_FLAGS", "NVCC_APPEND_FLAGS", "CMAKE_ARGS",
                    "CMAKE_BUILD_PARALLEL_LEVEL", "CMAKE_GENERATOR", "CMAKE_GENERATOR_INSTANCE", "CMAKE_GENERATOR_PLATFORM",
                    "CMAKE_GENERATOR_TOOLSET", "CMAKE_TOOLCHAIN_FILE"].into_iter().map(str::to_owned).collect(),
            },
            prerequisites: RuntimeSourceBuildPrerequisites {
                minimum_cmake_version: "3.28".into(), minimum_cuda_version: Some("13.4".into()),
                maximum_cuda_version_exclusive: Some("13.5".into()), requires_ninja: true,
                requires_cpp20_compiler: true, requires_make: false, minimum_cpp_standard: Some(20),
                cpp_compiler: windows.then(|| "cl.exe".into()),
                cuda_compiler: (!windows).then(|| "/usr/local/cuda-13.4/bin/nvcc".into()),
                requires_pkg_config: !windows,
                pkg_config_modules: if windows { BTreeMap::new() } else { BTreeMap::from([
                    ("libavformat".into(), "60".into()), ("libavcodec".into(), "60".into()),
                    ("libavutil".into(), "58".into()), ("libswscale".into(), "7".into()), ("libcurl".into(), "7.85".into())]) },
            },
        })),
        supported_native_identities: super::v3::native_identities(),
        requirements: RuntimeRequirements { requires_nvidia_gpu: true,
            supported_cuda_compute_capabilities: vec![ComputeCapability::new(12, 0)],
            advisories: vec![
                QUALIFICATION.into(),
                "Community NInfer-all fork, not an official Neroued/ninfer release; installs side by side without changing selections".into(),
                "No upstream release assets exist at admission; Scala fetches and builds this exact source snapshot, not a moving branch".into(),
                if windows { "Install provisions private pinned Git/CMake/Ninja/CUDA and vcpkg FFmpeg/curl dependencies, and installs or reuses MSVC/SDK. Windows may request approval. No WSL, driver update, model conversion or global CUDA/PATH change" } else { "Requires CUDA 13.4, CMake/Ninja, a C++20 compiler and pkg-config FFmpeg/curl development packages" }.into(),
            ], unverified_requirements: Vec::new(), ..Default::default() },
    };
    runtime.validate().map_err(|e| error(e.to_string()))?;
    Ok(runtime)
}

pub(super) fn reviewed_source(identity: &RuntimeIdentity, source: &RuntimeSourceSnapshot) -> bool {
    identity.engine_id == super::ENGINE_ID
        && identity.package_family == FAMILY
        && identity.package.provider_id == PROVIDER_ID
        && identity.package.repository.as_deref() == Some(REPOSITORY)
        && identity.upstream_revision.as_deref() == Some(REVISION)
        && matches!(identity.platform.as_str(), "windows" | "linux")
        && identity.architecture == "x86_64"
        && identity.accelerator == "cuda"
        && identity.variant == format!("{}-sm120a", recipe_for(&identity.platform))
        && source.repository == REPOSITORY
        && source.repository_url == format!("{URL}.git")
        && source.source_provider == PROVIDER_ID
        && source.commit_sha == REVISION
        && source.tree_sha == TREE
}

pub(super) fn available(runtime: &AvailableRuntime) -> bool {
    matches!(&runtime.acquisition, RuntimeAcquisitionPlan::SourceBuild(p)
        if reviewed_source(&runtime.identity, &p.source) && p.recipe.recipe_version == recipe_for(&runtime.identity.platform)
        && p.recipe.build_definition_sha256.as_deref() == Some(CMAKE_SHA256)
        && p.recipe.build_target == "ninfer-serve" && p.recipe.accelerator_target == "sm_120a")
        && runtime.supported_native_identities == super::v3::native_identities()
}

pub(super) fn installed(runtime: &InstalledRuntime) -> bool {
    let m = &runtime.manifest;
    m.acquisition_method == RuntimeAcquisitionMethod::SourceBuild
        && m.supported_native_identities == super::v3::native_identities()
        && m.source_build.as_ref().is_some_and(|p| {
            reviewed_source(&m.identity, &p.source)
                && p.recipe_version == recipe_for(&m.identity.platform)
                && p.build_definition_sha256.as_deref() == Some(CMAKE_SHA256)
                && p.build_platform == m.identity.platform
                && p.build_architecture == "x86_64"
                && p.build_target == "ninfer-serve"
                && p.accelerator_target == "sm_120a"
                && p.entrypoint
                    == Path::new(if p.build_platform == "windows" {
                        "source/build/apps/ninfer-serve.exe"
                    } else {
                        "source/build/apps/ninfer-serve"
                    })
        })
        && source_registry_matches(&runtime.installation_root.join("source"))
}

pub(super) fn source_registry_matches(root: &Path) -> bool {
    container_owners::FILES.iter().all(|(path, digest)| {
        std::fs::read(root.join(path))
            .is_ok_and(|bytes| format!("{:x}", Sha256::digest(bytes)) == *digest)
    })
}

mod container_owners;

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> GitHubCommit {
        serde_json::from_value(serde_json::json!({"sha": REVISION,
            "html_url": format!("{URL}/commit/{REVISION}"),
            "commit": {"tree": {"sha": TREE}, "committer": {"date": "2026-10-16T17:09:10Z"}}}))
        .unwrap()
    }
    #[tokio::test]
    async fn pinned_source_is_discoverable_and_verifiable_without_live_github_api() {
        let github = GitHubReleaseClient::new().unwrap();
        let provider = NinferAllRuntimeCatalogProvider::new();
        let entries = provider.fetch(&github).await.unwrap();
        assert_eq!(entries.len(), 2);
        for entry in &entries {
            assert!(available(entry));
            assert_eq!(
                provider.verify_candidate(&github, entry).await.unwrap(),
                Some(entry.clone())
            );
        }
        let mut changed = entries[0].clone();
        changed.display_name.push_str(" replaced");
        assert!(provider.verify_candidate(&github, &changed).await.is_err());
    }
    #[test]
    fn native_windows_and_linux_candidates_are_distinct_source_builds() {
        let windows = source_runtime("windows", fixture()).unwrap();
        let linux = source_runtime("linux", fixture()).unwrap();
        assert_ne!(windows.runtime_id, linux.runtime_id);
        for r in [&windows, &linux] {
            assert!(available(r));
            assert!(r.requirements.unverified_requirements.is_empty());
            assert!(r.requirements.advisories.iter().any(|a| a == QUALIFICATION));
            assert!(super::super::v3::available_capabilities(r).is_none());
            assert!(super::super::ninfer_runtime_capabilities_for_available(r).startup_proof);
            let RuntimeAcquisitionPlan::SourceBuild(p) = &r.acquisition else {
                panic!("source");
            };
            assert_eq!(p.source.commit_sha, REVISION);
            assert!(
                p.recipe
                    .cmake_configuration_arguments
                    .contains(&"-DNINFER_DIRECTSTORAGE=OFF".into())
            );
        }
    }
    #[test]
    fn source_candidates_reject_unreviewed_identity() {
        let mut bad = fixture();
        bad.commit.tree.sha = "a".repeat(40);
        assert!(source_runtime("windows", bad).is_err());
        assert!(source_runtime("macos", fixture()).is_err());
        let mut r = source_runtime("windows", fixture()).unwrap();
        r.identity.package.repository = Some(super::super::GITHUB_REPOSITORY.into());
        assert!(!available(&r));
    }
    #[test]
    fn v2_is_not_advertised_and_no_fork_inference_domains_are_inherited() {
        let r = source_runtime("windows", fixture()).unwrap();
        assert!(r.supported_native_identities.iter().all(|i| matches!(i,
            scala_core::ArtifactNativeIdentity::Ninfer(i) if i.container_version == 3)));
        let c = super::super::ninfer_runtime_capabilities_for_available(&r);
        assert!(c.tool_calling && c.speculation_controls && c.request_sampler_semantics);
        assert_eq!(c.request_log_schema, Some(28));
    }

    #[test]
    fn windows_install_acquires_software_instead_of_requiring_manual_host_tools() {
        let windows = source_runtime("windows", fixture()).unwrap();
        let RuntimeAcquisitionPlan::SourceBuild(plan) = &windows.acquisition else {
            panic!("source build");
        };
        let dependencies =
            super::super::source_dependencies::for_plan(plan).expect("Windows dependency plan");
        dependencies.validate().unwrap();
        assert!(plan.prerequisites.cuda_compiler.is_none());
        assert!(
            plan.recipe
                .cmake_configuration_arguments
                .iter()
                .all(|argument| !argument.contains("C:/vcpkg")
                    && !argument.contains("CMAKE_CXX_COMPILER")
                    && !argument.contains("CMAKE_PREFIX_PATH"))
        );
        assert!(
            windows
                .identity
                .variant
                .contains(&dependencies.sha256()[..16])
        );
        let mut changed = plan.clone();
        changed.source.tree_sha = "a".repeat(40);
        assert!(super::super::source_dependencies::for_plan(&changed).is_none());
        let linux = source_runtime("linux", fixture()).unwrap();
        let RuntimeAcquisitionPlan::SourceBuild(plan) = &linux.acquisition else {
            panic!("source build");
        };
        assert!(super::super::source_dependencies::for_plan(plan).is_none());
        assert_eq!(plan.recipe.recipe_version, RECIPE);
    }
}
