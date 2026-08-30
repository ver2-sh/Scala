use std::collections::BTreeMap;

use async_trait::async_trait;
use norted_core::{
    ArtifactFormat, AvailableRuntime, ComputeCapability, RuntimeAcquisitionPlan, RuntimeId,
    RuntimeIdentity, RuntimePackageIdentity, RuntimeReleaseChannel, RuntimeRequirements,
    RuntimeSourceBuildPlan, RuntimeSourceBuildPrerequisites, RuntimeSourceBuildRecipe,
    RuntimeSourceBuildSystem, RuntimeSourceSnapshot,
};
use norted_engine::{CatalogError, GitHubCommit, GitHubReleaseClient, RuntimeCatalogProvider};

use crate::{ENGINE_ID, GITHUB_REPOSITORY, PROVIDER_ID, UPSTREAM_REPOSITORY};

pub const PACKAGE_FAMILY: &str = "ninfer-source";
pub const RECIPE_VERSION: &str = "ninfer-serve-v1";

#[derive(Debug, Clone, Copy, Default)]
pub struct NinferRuntimeCatalogProvider;

impl NinferRuntimeCatalogProvider {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl RuntimeCatalogProvider for NinferRuntimeCatalogProvider {
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
        let repository = github.repository(GITHUB_REPOSITORY).await?;
        if repository.full_name != GITHUB_REPOSITORY
            || repository.html_url != UPSTREAM_REPOSITORY
            || repository.default_branch.trim().is_empty()
        {
            return Err(provider_error(
                "canonical repository identity or default branch is invalid",
            ));
        }
        let commit = github
            .commit(GITHUB_REPOSITORY, &repository.default_branch)
            .await?;
        if !norted_core::is_full_git_sha(&commit.sha)
            || !norted_core::is_full_git_sha(&commit.commit.tree.sha)
        {
            return Err(provider_error(
                "default-branch HEAD did not resolve to full commit and tree SHAs",
            ));
        }
        Ok(vec![source_runtime(repository.default_branch, commit)?])
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
            || candidate.identity.package.provider_id != PROVIDER_ID
            || candidate.identity.package.repository.as_deref() != Some(GITHUB_REPOSITORY)
            || plan.source.repository != GITHUB_REPOSITORY
            || plan.source.repository_url != format!("{UPSTREAM_REPOSITORY}.git")
            || plan.source.source_provider != PROVIDER_ID
        {
            return Ok(None);
        }
        let repository = github.repository(GITHUB_REPOSITORY).await?;
        if repository.full_name != GITHUB_REPOSITORY
            || repository.html_url != UPSTREAM_REPOSITORY
            || repository.default_branch.trim().is_empty()
        {
            return Err(provider_error(
                "canonical repository identity is invalid during source-candidate verification",
            ));
        }
        let commit = github
            .commit(GITHUB_REPOSITORY, &plan.source.commit_sha)
            .await?;
        if commit.sha != plan.source.commit_sha || commit.commit.tree.sha != plan.source.tree_sha {
            return Err(provider_error(
                "selected source commit/tree metadata differs from the live canonical repository",
            ));
        }
        let live = source_runtime(plan.source.source_branch.clone(), commit)?;
        if &live != candidate {
            return Err(provider_error(
                "selected source runtime contract differs from the canonical Norted build recipe",
            ));
        }
        Ok(Some(live))
    }
}

fn source_runtime(
    source_branch: String,
    commit: GitHubCommit,
) -> Result<AvailableRuntime, CatalogError> {
    let timestamp = parse_github_timestamp(&commit.commit.committer.date)
        .ok_or_else(|| provider_error("source commit has an invalid timestamp"))?;
    let date = commit
        .commit
        .committer
        .date
        .get(..10)
        .unwrap_or("unknown")
        .replace('-', "");
    let identity = RuntimeIdentity {
        engine_id: ENGINE_ID.to_owned(),
        package_family: PACKAGE_FAMILY.to_owned(),
        version: format!("git-{date}-{}", &commit.sha[..8]),
        upstream_revision: Some(commit.sha.clone()),
        platform: "linux".to_owned(),
        architecture: "x86_64".to_owned(),
        accelerator: "cuda".to_owned(),
        variant: format!("{RECIPE_VERSION}-sm120a"),
        package: RuntimePackageIdentity {
            provider_id: PROVIDER_ID.to_owned(),
            repository: Some(GITHUB_REPOSITORY.to_owned()),
            release_tag: None,
            asset_id: None,
            asset_name: None,
            additional_assets: Vec::new(),
        },
    };
    let runtime = AvailableRuntime {
        runtime_id: RuntimeId::from_identity(&identity),
        identity,
        display_name: format!("NInfer source snapshot {}", &commit.sha[..8]),
        supported_formats: vec![ArtifactFormat::Ninfer],
        source_url: commit.html_url,
        published_at_unix: Some(timestamp),
        channels: vec![RuntimeReleaseChannel::Latest],
        prerelease: false,
        acquisition: RuntimeAcquisitionPlan::SourceBuild(Box::new(RuntimeSourceBuildPlan {
            source: RuntimeSourceSnapshot {
                repository: GITHUB_REPOSITORY.to_owned(),
                repository_url: format!("{UPSTREAM_REPOSITORY}.git"),
                source_branch,
                commit_sha: commit.sha,
                tree_sha: commit.commit.tree.sha,
                commit_timestamp_unix: timestamp,
                source_provider: PROVIDER_ID.to_owned(),
            },
            recipe: RuntimeSourceBuildRecipe {
                recipe_version: RECIPE_VERSION.to_owned(),
                build_system: RuntimeSourceBuildSystem::Cmake,
                cmake_configuration_arguments: vec![
                    "-G".to_owned(),
                    "Ninja".to_owned(),
                    "-DCMAKE_BUILD_TYPE=Release".to_owned(),
                    "-DNINFER_BUILD_APPS=ON".to_owned(),
                    "-DBUILD_TESTING=OFF".to_owned(),
                    "-DNINFER_BUILD_BENCHMARKS=OFF".to_owned(),
                    "-DCMAKE_CUDA_ARCHITECTURES=120a".to_owned(),
                ],
                build_target: "ninfer-serve".to_owned(),
                entrypoint: "build/apps/ninfer-serve".into(),
                accelerator_target: "sm_120a".to_owned(),
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
                minimum_cmake_version: "3.28".to_owned(),
                minimum_cuda_version: "13.1".to_owned(),
                requires_ninja: true,
                requires_cpp20_compiler: true,
                requires_make: false,
                minimum_cpp_standard: Some(20),
                cpp_compiler: None,
                cuda_compiler: None,
                requires_pkg_config: true,
                pkg_config_modules: BTreeMap::from([
                    ("libavformat".to_owned(), "60".to_owned()),
                    ("libavcodec".to_owned(), "60".to_owned()),
                    ("libavutil".to_owned(), "58".to_owned()),
                    ("libswscale".to_owned(), "7".to_owned()),
                    ("libcurl".to_owned(), "7.85".to_owned()),
                ]),
            },
        })),
        supported_native_identities: Vec::new(),
        requirements: RuntimeRequirements {
            requires_nvidia_gpu: true,
            minimum_nvidia_driver: None,
            minimum_vram_bytes: None,
            minimum_vram_class_gib: None,
            minimum_vram_exclusive_class_gib: None,
            supported_cuda_compute_capabilities: vec![ComputeCapability::new(12, 0)],
            required_nvidia_device_names: vec!["NVIDIA GeForce RTX 5090".to_owned()],
            notes: Vec::new(),
            advisories: vec![
                "Official upstream currently supports Linux x86_64 and NVIDIA GeForce RTX 5090 only"
                    .to_owned(),
                "This is a Norted-managed build from an exact official source snapshot, not an upstream binary release"
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
        provider: PROVIDER_ID.to_owned(),
        message: message.to_owned(),
    }
}

fn parse_github_timestamp(value: &str) -> Option<i64> {
    if value.len() != 20
        || value.as_bytes().get(4) != Some(&b'-')
        || value.as_bytes().get(7) != Some(&b'-')
        || value.as_bytes().get(10) != Some(&b'T')
        || value.as_bytes().get(13) != Some(&b':')
        || value.as_bytes().get(16) != Some(&b':')
        || value.as_bytes().get(19) != Some(&b'Z')
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

#[cfg(test)]
mod tests {
    use norted_core::{RuntimeAcquisitionPlan, RuntimeReleaseChannel};

    use super::{parse_github_timestamp, source_runtime};

    #[test]
    fn source_commit_timestamp_is_strict() {
        assert!(parse_github_timestamp("2026-08-29T04:13:34Z").is_some());
        assert!(parse_github_timestamp("2026-02-29T04:13:34Z").is_none());
    }

    #[test]
    fn canonical_source_candidate_has_only_latest_and_the_fixed_recipe() {
        let commit = serde_json::from_value(serde_json::json!({
            "sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "html_url": "https://github.com/Neroued/ninfer/commit/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "commit": {
                "committer": {"date": "2026-08-28T20:13:34Z"},
                "tree": {"sha": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"}
            }
        }))
        .expect("GitHub commit fixture");
        let runtime = source_runtime("master".to_owned(), commit).expect("source runtime");
        assert_eq!(runtime.channels, [RuntimeReleaseChannel::Latest]);
        assert!(!runtime.channels.contains(&RuntimeReleaseChannel::Stable));
        let RuntimeAcquisitionPlan::SourceBuild(plan) = runtime.acquisition else {
            panic!("expected source build");
        };
        assert_eq!(plan.source.source_branch, "master");
        assert_eq!(
            plan.recipe.cmake_configuration_arguments,
            [
                "-G",
                "Ninja",
                "-DCMAKE_BUILD_TYPE=Release",
                "-DNINFER_BUILD_APPS=ON",
                "-DBUILD_TESTING=OFF",
                "-DNINFER_BUILD_BENCHMARKS=OFF",
                "-DCMAKE_CUDA_ARCHITECTURES=120a",
            ]
        );
        assert_eq!(plan.recipe.build_target, "ninfer-serve");
        assert_eq!(
            plan.recipe.entrypoint.to_string_lossy(),
            "build/apps/ninfer-serve"
        );
    }
}
