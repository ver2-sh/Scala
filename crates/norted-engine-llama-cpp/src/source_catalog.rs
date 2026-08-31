use std::collections::BTreeMap;

use async_trait::async_trait;
use futures_util::try_join;
use norted_core::{
    ArtifactFormat, AvailableRuntime, ComputeCapability, RuntimeAcquisitionPlan, RuntimeId,
    RuntimeIdentity, RuntimePackageIdentity, RuntimeReleaseChannel, RuntimeRequirements,
    RuntimeSourceBuildPlan, RuntimeSourceBuildPrerequisites, RuntimeSourceBuildRecipe,
    RuntimeSourceBuildSystem, RuntimeSourceSnapshot,
};
use norted_engine::{
    CatalogError, GitHubCommit, GitHubRelease, GitHubReleaseClient, RuntimeCatalogProvider,
};

use crate::catalog::{
    GITHUB_REPOSITORY, nightly_number, nightly_tag_from_reference, parse_github_timestamp,
};
use crate::{ENGINE_ID, UPSTREAM_REPOSITORY};

const PACKAGE_FAMILY: &str = "llama-cpp-managed-source";
const RECIPE_VERSION: &str = "managed-portable-v2";
const CUDA_ARCHITECTURES: &str = "75-real;80-real;86-real;89-real;90-real;120a-real";
const CUDA_TOOLKIT_FLOOR: &str = "12.8";
const ACCELERATOR_TARGET: &str = "sm_75+sm_80+sm_86+sm_89+sm_90+sm_120a";
const SOURCE_CONTRACT_FILE_LIMIT: usize = 512 * 1024;

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
        let mut releases = github
            .releases(GITHUB_REPOSITORY)
            .await?
            .into_iter()
            .filter(|release| !release.draft)
            .filter_map(|release| nightly_number(&release.tag_name).map(|build| (build, release)))
            .collect::<Vec<_>>();
        releases.sort_by_key(|(build, _)| std::cmp::Reverse(*build));
        for (_, release) in releases {
            if let Some(commit) = admit_source_revision(github, &release).await? {
                return Ok(vec![source_runtime(&release, commit)?]);
            }
        }
        Err(provider_error(
            "no upstream nightly release satisfies the managed llama.cpp source contract",
        ))
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
        let Some(commit) = admit_source_revision(github, &release).await? else {
            return Ok(Vec::new());
        };
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
        let Some(commit) = admit_source_revision(github, &release).await? else {
            return Ok(None);
        };
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

struct SourceContractFiles {
    root: String,
    ggml: String,
    ggml_src: String,
    cuda: String,
    common: String,
    tools: String,
    ui: String,
    ui_assets: String,
    server: String,
}

async fn admit_source_revision(
    github: &GitHubReleaseClient,
    release: &GitHubRelease,
) -> Result<Option<GitHubCommit>, CatalogError> {
    let commit = resolve_release_commit(github, release).await?;
    let raw_root = format!(
        "https://raw.githubusercontent.com/{GITHUB_REPOSITORY}/{}",
        commit.sha
    );
    let (root, ggml, ggml_src, cuda, common, tools, ui, ui_assets, server) = try_join!(
        fetch_source_contract_file(github, &raw_root, "CMakeLists.txt"),
        fetch_source_contract_file(github, &raw_root, "ggml/CMakeLists.txt"),
        fetch_source_contract_file(github, &raw_root, "ggml/src/CMakeLists.txt"),
        fetch_source_contract_file(github, &raw_root, "ggml/src/ggml-cuda/CMakeLists.txt"),
        fetch_source_contract_file(github, &raw_root, "common/CMakeLists.txt"),
        fetch_source_contract_file(github, &raw_root, "tools/CMakeLists.txt"),
        fetch_source_contract_file(github, &raw_root, "tools/ui/CMakeLists.txt"),
        fetch_source_contract_file(github, &raw_root, "scripts/ui-assets.cmake"),
        fetch_source_contract_file(github, &raw_root, "tools/server/CMakeLists.txt"),
    )?;
    let (
        Some(root),
        Some(ggml),
        Some(ggml_src),
        Some(cuda),
        Some(common),
        Some(tools),
        Some(ui),
        Some(ui_assets),
        Some(server),
    ) = (
        root, ggml, ggml_src, cuda, common, tools, ui, ui_assets, server,
    )
    else {
        return Ok(None);
    };
    let files = SourceContractFiles {
        root,
        ggml,
        ggml_src,
        cuda,
        common,
        tools,
        ui,
        ui_assets,
        server,
    };
    Ok(source_contract_is_admitted(&files).then_some(commit))
}

async fn fetch_source_contract_file(
    github: &GitHubReleaseClient,
    raw_root: &str,
    path: &str,
) -> Result<Option<String>, CatalogError> {
    match github
        .fetch_small_text(&format!("{raw_root}/{path}"), SOURCE_CONTRACT_FILE_LIMIT)
        .await
    {
        Ok(contents) => Ok(Some(contents)),
        Err(CatalogError::Http { status: 404, .. }) => Ok(None),
        Err(error) => Err(error),
    }
}

fn source_contract_is_admitted(files: &SourceContractFiles) -> bool {
    let root = cmake_tokens(&files.root);
    let ggml = cmake_tokens(&files.ggml);
    let ggml_src = cmake_tokens(&files.ggml_src);
    let cuda = cmake_tokens(&files.cuda);
    let common = cmake_tokens(&files.common);
    let tools = cmake_tokens(&files.tools);
    let ui = cmake_tokens(&files.ui);
    let ui_assets = cmake_tokens(&files.ui_assets);
    let server = cmake_tokens(&files.server);

    [
        &["project", "llama.cpp", "c", "cxx"][..],
        &[
            "set",
            "cmake_runtime_output_directory",
            "cmake_binary_dir",
            "bin",
        ],
        &[
            "set",
            "cmake_library_output_directory",
            "cmake_binary_dir",
            "bin",
        ],
        &["option", "build_shared_libs"],
        &["option", "llama_build_common"],
        &["option", "llama_build_tests"],
        &["option", "llama_build_tools"],
        &["option", "llama_build_examples"],
        &["option", "llama_build_server"],
        &["option", "llama_build_app"],
        &["option", "llama_build_ui"],
        &["option", "llama_use_prebuilt_ui"],
        &["option", "llama_openssl"],
        &["option", "llama_subprocess"],
        &["option", "llama_llguidance"],
        &["add_subdirectory", "ggml"],
        &["if", "llama_build_common", "and", "llama_build_tools"],
        &["add_subdirectory", "tools"],
    ]
    .iter()
    .all(|sequence| has_token_sequence(&root, sequence))
        && [
            &["set", "cmake_cxx_standard", "17"][..],
            &["option", "build_shared_libs"],
            &["option", "ggml_native"],
            &["option", "ggml_cuda"],
            &["option", "ggml_cuda_nccl"],
        ]
        .iter()
        .all(|sequence| has_token_sequence(&ggml, sequence))
        && has_token_sequence(&ggml_src, &["ggml_add_backend", "cuda"])
        && [
            &["cmake_minimum_required", "version", "3.18"][..],
            &["find_package", "cudatoolkit"],
            &["if", "cudatoolkit_found"],
            &["if", "not", "defined", "cmake_cuda_architectures"],
            &["list", "append", "cmake_cuda_architectures", "120a-real"],
            &["enable_language", "cuda"],
            &["if", "ggml_cuda_cub_3dot2"],
            &["fetchcontent_makeavailable", "cccl"],
            &["ggml_add_backend_library", "ggml-cuda"],
            &["if", "ggml_cuda_nccl"],
            &["find_package", "nccl"],
        ]
        .iter()
        .all(|sequence| has_token_sequence(&cuda, sequence))
        && [
            &["target_compile_features", "target", "public", "cxx_std_17"][..],
            &["if", "llama_subprocess"],
            &["if", "llama_llguidance"],
            &["externalproject_add", "llguidance_ext"],
        ]
        .iter()
        .all(|sequence| has_token_sequence(&common, sequence))
        && [
            &["if", "llama_build_server"][..],
            &["add_subdirectory", "ui"],
            &["add_subdirectory", "server"],
        ]
        .iter()
        .all(|sequence| has_token_sequence(&tools, sequence))
        && [
            &["dhf_enabled", "llama_use_prebuilt_ui"][..],
            &["dbuild_ui", "llama_build_ui"],
        ]
        .iter()
        .all(|sequence| has_token_sequence(&ui, sequence))
        && has_token_sequence(&ui_assets, &["if", "build_ui"])
        && has_token_sequence(
            &ui_assets,
            &["if", "not", "provisioned", "and", "hf_enabled"],
        )
        && [
            &["set", "target", "llama-server"][..],
            &["add_executable", "target", "main.cpp"],
            &[
                "target_link_libraries",
                "target",
                "private",
                "llama-server-impl",
            ],
            &["target_compile_features", "target", "private", "cxx_std_17"],
        ]
        .iter()
        .all(|sequence| has_token_sequence(&server, sequence))
}

fn cmake_tokens(contents: &str) -> Vec<String> {
    contents
        .lines()
        .flat_map(|line| {
            line.split_once('#')
                .map_or(line, |(code, _)| code)
                .split_whitespace()
        })
        .flat_map(|word| {
            word.split(|character: char| {
                !(character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.'))
            })
        })
        .map(|token| token.trim_matches('-'))
        .filter(|token| !token.is_empty())
        .map(str::to_ascii_lowercase)
        .collect()
}

fn has_token_sequence(tokens: &[String], expected: &[&str]) -> bool {
    tokens.windows(expected.len()).any(|window| {
        window
            .iter()
            .map(String::as_str)
            .eq(expected.iter().copied())
    })
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
                    "-DBUILD_SHARED_LIBS=ON".to_owned(),
                    "-DCMAKE_BUILD_RPATH_USE_ORIGIN=ON".to_owned(),
                    "-DCMAKE_SKIP_RPATH=OFF".to_owned(),
                    "-DCMAKE_SKIP_BUILD_RPATH=OFF".to_owned(),
                    "-DCMAKE_BUILD_WITH_INSTALL_RPATH=OFF".to_owned(),
                    format!("-DLLAMA_BUILD_NUMBER={build}"),
                    format!("-DLLAMA_BUILD_COMMIT={commit_sha}"),
                    "-DGGML_CUDA=ON".to_owned(),
                    "-DGGML_NATIVE=OFF".to_owned(),
                    format!("-DCMAKE_CUDA_ARCHITECTURES={CUDA_ARCHITECTURES}"),
                    "-DGGML_CUDA_NCCL=OFF".to_owned(),
                    "-DLLAMA_BUILD_COMMON=ON".to_owned(),
                    "-DLLAMA_BUILD_SERVER=ON".to_owned(),
                    "-DLLAMA_BUILD_TOOLS=ON".to_owned(),
                    "-DLLAMA_BUILD_TESTS=OFF".to_owned(),
                    "-DLLAMA_BUILD_EXAMPLES=OFF".to_owned(),
                    "-DLLAMA_BUILD_APP=OFF".to_owned(),
                    "-DLLAMA_BUILD_UI=OFF".to_owned(),
                    "-DLLAMA_USE_PREBUILT_UI=OFF".to_owned(),
                    "-DLLAMA_LLGUIDANCE=OFF".to_owned(),
                    "-DLLAMA_OPENSSL=OFF".to_owned(),
                    "-DLLAMA_SUBPROCESS=OFF".to_owned(),
                    "-DGGML_CUDA_CUB_3DOT2=OFF".to_owned(),
                    "-DGGML_CPU_KLEIDIAI=OFF".to_owned(),
                    "-DFETCHCONTENT_FULLY_DISCONNECTED=ON".to_owned(),
                    "-DFETCHCONTENT_UPDATES_DISCONNECTED=ON".to_owned(),
                ],
                build_target: "llama-server".to_owned(),
                entrypoint: "build/bin/llama-server".into(),
                accelerator_target: ACCELERATOR_TARGET.to_owned(),
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
                minimum_cuda_version: Some(CUDA_TOOLKIT_FLOOR.to_owned()),
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
            supported_cuda_compute_capabilities: vec![
                ComputeCapability::new(7, 5),
                ComputeCapability::new(8, 0),
                ComputeCapability::new(8, 6),
                ComputeCapability::new(8, 9),
                ComputeCapability::new(9, 0),
                ComputeCapability::new(12, 0),
            ],
            required_nvidia_device_names: Vec::new(),
            notes: Vec::new(),
            advisories: vec![
                "Norted builds this runtime from the exact official llama.cpp source revision; it is not an upstream CUDA binary"
                    .to_owned(),
                "The managed build contains fixed real-code CUDA targets for compute capabilities 7.5, 8.0, 8.6, 8.9, 9.0, and 12.0"
                    .to_owned(),
                "CUDA 12.8 is the minimum build Toolkit because the fixed policy includes Blackwell sm_120a"
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

#[cfg(test)]
mod tests {
    use norted_core::{AcceleratorDevice, HostCapabilities, RuntimeCompatibility};
    use norted_engine::compatibility_for;

    use super::*;

    fn admitted_contract() -> SourceContractFiles {
        SourceContractFiles {
            root: r#"
                project("llama.cpp" C CXX)
                set(CMAKE_RUNTIME_OUTPUT_DIRECTORY ${CMAKE_BINARY_DIR}/bin)
                set(CMAKE_LIBRARY_OUTPUT_DIRECTORY ${CMAKE_BINARY_DIR}/bin)
                option(BUILD_SHARED_LIBS "shared" ON)
                option(LLAMA_BUILD_COMMON "common" ON)
                option(LLAMA_BUILD_TESTS "tests" ON)
                option(LLAMA_BUILD_TOOLS "tools" ON)
                option(LLAMA_BUILD_EXAMPLES "examples" ON)
                option(LLAMA_BUILD_SERVER "server" ON)
                option(LLAMA_BUILD_APP "app" ON)
                option(LLAMA_BUILD_UI "ui" ON)
                option(LLAMA_USE_PREBUILT_UI "prebuilt ui" ON)
                option(LLAMA_OPENSSL "openssl" ON)
                option(LLAMA_SUBPROCESS "subprocess" ON)
                option(LLAMA_LLGUIDANCE "guidance" OFF)
                add_subdirectory(ggml)
                if (LLAMA_BUILD_COMMON AND LLAMA_BUILD_TOOLS)
                    add_subdirectory(tools)
                endif()
            "#
            .to_owned(),
            ggml: r#"
                set(CMAKE_CXX_STANDARD 17)
                option(BUILD_SHARED_LIBS "shared" ON)
                option(GGML_NATIVE "native" ON)
                option(GGML_CUDA "cuda" OFF)
                option(GGML_CUDA_NCCL "nccl" ON)
            "#
            .to_owned(),
            ggml_src: "ggml_add_backend(CUDA)".to_owned(),
            cuda: r#"
                cmake_minimum_required(VERSION 3.18)
                find_package(CUDAToolkit)
                if (CUDAToolkit_FOUND)
                    if (NOT DEFINED CMAKE_CUDA_ARCHITECTURES)
                        list(APPEND CMAKE_CUDA_ARCHITECTURES 120a-real)
                    endif()
                    enable_language(CUDA)
                    if (GGML_CUDA_CUB_3DOT2)
                        FetchContent_MakeAvailable(CCCL)
                    endif()
                    ggml_add_backend_library(ggml-cuda sources)
                    if (GGML_CUDA_NCCL)
                        find_package(NCCL)
                    endif()
                endif()
            "#
            .to_owned(),
            common: r#"
                target_compile_features(${TARGET} PUBLIC cxx_std_17)
                if (LLAMA_SUBPROCESS)
                endif()
                if (LLAMA_LLGUIDANCE)
                    ExternalProject_Add(llguidance_ext)
                endif()
            "#
            .to_owned(),
            tools: r#"
                if (LLAMA_BUILD_SERVER)
                    add_subdirectory(ui)
                    add_subdirectory(server)
                endif()
            "#
            .to_owned(),
            ui: r#"
                COMMAND ${CMAKE_COMMAND}
                    "-DHF_ENABLED=${LLAMA_USE_PREBUILT_UI}"
                    "-DBUILD_UI=${LLAMA_BUILD_UI}"
            "#
            .to_owned(),
            ui_assets: r#"
                if (BUILD_UI)
                endif()
                if (NOT provisioned AND HF_ENABLED)
                endif()
            "#
            .to_owned(),
            server: r#"
                set(TARGET llama-server)
                add_executable(${TARGET} main.cpp)
                target_link_libraries(${TARGET} PRIVATE llama-server-impl)
                target_compile_features(${TARGET} PRIVATE cxx_std_17)
            "#
            .to_owned(),
        }
    }

    #[test]
    fn source_contract_accepts_the_owned_recipe_semantics() {
        assert!(source_contract_is_admitted(&admitted_contract()));
    }

    #[test]
    fn source_contract_rejects_recipe_and_target_drift() {
        let mut cases = Vec::new();

        let mut missing_cuda_option = admitted_contract();
        missing_cuda_option.ggml = missing_cuda_option.ggml.replace("GGML_CUDA", "GGML_GPU");
        cases.push(missing_cuda_option);

        let mut toolkit_selected_architectures = admitted_contract();
        toolkit_selected_architectures.cuda = toolkit_selected_architectures
            .cuda
            .replace("if (NOT DEFINED CMAKE_CUDA_ARCHITECTURES)", "if (TRUE)");
        cases.push(toolkit_selected_architectures);

        let mut no_blackwell_contract = admitted_contract();
        no_blackwell_contract.cuda = no_blackwell_contract.cuda.replace("120a-real", "90-real");
        cases.push(no_blackwell_contract);

        let mut renamed_server = admitted_contract();
        renamed_server.server = renamed_server
            .server
            .replace("llama-server", "llama-service");
        cases.push(renamed_server);

        let mut moved_output = admitted_contract();
        moved_output.root = moved_output
            .root
            .replace("${CMAKE_BINARY_DIR}/bin", "${CMAKE_BINARY_DIR}/out");
        cases.push(moved_output);

        let mut newer_cpp = admitted_contract();
        newer_cpp.server = newer_cpp.server.replace("cxx_std_17", "cxx_std_23");
        cases.push(newer_cpp);

        let mut uncontrolled_ui_fetch = admitted_contract();
        uncontrolled_ui_fetch.ui_assets = uncontrolled_ui_fetch
            .ui_assets
            .replace("if (NOT provisioned AND HF_ENABLED)", "if (TRUE)");
        cases.push(uncontrolled_ui_fetch);

        for contract in cases {
            assert!(!source_contract_is_admitted(&contract));
        }
    }

    #[test]
    fn source_runtime_exposes_the_v2_cuda_and_relocation_policy() {
        let release = GitHubRelease {
            id: 1,
            tag_name: "b12345".to_owned(),
            name: Some("b12345".to_owned()),
            html_url: "https://github.com/ggml-org/llama.cpp/releases/tag/b12345".to_owned(),
            target_commitish: "main".to_owned(),
            draft: false,
            prerelease: false,
            published_at: None,
            assets: Vec::new(),
        };
        let commit: GitHubCommit = serde_json::from_value(serde_json::json!({
            "sha": "1111111111111111111111111111111111111111",
            "html_url": "https://github.com/ggml-org/llama.cpp/commit/1111111111111111111111111111111111111111",
            "commit": {
                "committer": { "date": "2026-08-31T00:00:00Z" },
                "tree": { "sha": "2222222222222222222222222222222222222222" }
            }
        }))
        .expect("commit fixture");

        let runtime = source_runtime(&release, commit).expect("valid source runtime");
        let RuntimeAcquisitionPlan::SourceBuild(plan) = &runtime.acquisition else {
            panic!("expected source build");
        };
        assert_eq!(runtime.identity.variant, RECIPE_VERSION);
        assert_eq!(plan.recipe.recipe_version, RECIPE_VERSION);
        assert_eq!(
            plan.prerequisites.minimum_cuda_version.as_deref(),
            Some(CUDA_TOOLKIT_FLOOR)
        );
        assert_eq!(plan.recipe.accelerator_target, ACCELERATOR_TARGET);
        assert!(
            plan.recipe
                .cmake_configuration_arguments
                .contains(&"-DCMAKE_BUILD_RPATH_USE_ORIGIN=ON".to_owned())
        );
        assert!(
            plan.recipe
                .cmake_configuration_arguments
                .contains(&format!("-DCMAKE_CUDA_ARCHITECTURES={CUDA_ARCHITECTURES}"))
        );
        assert_eq!(
            runtime.requirements.supported_cuda_compute_capabilities,
            vec![
                ComputeCapability::new(7, 5),
                ComputeCapability::new(8, 0),
                ComputeCapability::new(8, 6),
                ComputeCapability::new(8, 9),
                ComputeCapability::new(9, 0),
                ComputeCapability::new(12, 0),
            ]
        );
    }

    #[test]
    fn fixed_cuda_policy_is_conservative_for_observed_devices() {
        let requirements = RuntimeRequirements {
            requires_nvidia_gpu: true,
            supported_cuda_compute_capabilities: vec![
                ComputeCapability::new(7, 5),
                ComputeCapability::new(8, 0),
                ComputeCapability::new(8, 6),
                ComputeCapability::new(8, 9),
                ComputeCapability::new(9, 0),
                ComputeCapability::new(12, 0),
            ],
            ..RuntimeRequirements::default()
        };
        let host = |compute_capability| HostCapabilities {
            platform: "linux".to_owned(),
            architecture: "x86_64".to_owned(),
            accelerators: vec![AcceleratorDevice {
                accelerator: "cuda".to_owned(),
                stable_id: Some("GPU-fixture".to_owned()),
                name: Some("NVIDIA fixture".to_owned()),
                vram_bytes: None,
                driver_version: None,
                compute_capability,
            }],
            nvidia_gpu_absence_confirmed: false,
            cuda_visible_devices: None,
            observations: Vec::new(),
        };

        assert_eq!(
            compatibility_for(
                "linux",
                "x86_64",
                "cuda",
                &requirements,
                &host(Some(ComputeCapability::new(12, 0))),
            ),
            RuntimeCompatibility::Recommended
        );
        assert!(matches!(
            compatibility_for(
                "linux",
                "x86_64",
                "cuda",
                &requirements,
                &host(Some(ComputeCapability::new(8, 7))),
            ),
            RuntimeCompatibility::Incompatible(_)
        ));
        assert!(matches!(
            compatibility_for("linux", "x86_64", "cuda", &requirements, &host(None),),
            RuntimeCompatibility::NeedsAttention(_)
        ));

        let no_nvidia = HostCapabilities {
            platform: "linux".to_owned(),
            architecture: "x86_64".to_owned(),
            accelerators: Vec::new(),
            nvidia_gpu_absence_confirmed: true,
            cuda_visible_devices: None,
            observations: Vec::new(),
        };
        assert!(matches!(
            compatibility_for("linux", "x86_64", "cuda", &requirements, &no_nvidia),
            RuntimeCompatibility::Incompatible(_)
        ));
    }
}
