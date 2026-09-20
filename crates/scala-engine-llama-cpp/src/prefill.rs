//! Adapter-owned proof of the reviewed native prompt timing semantics.
//!
//! b10665 resolves to REVIEWED_COMMIT (tree REVIEWED_TREE). This pins all
//! semantic owners, including tools/server/server-common.{h,cpp} (counters and
//! duration), server-context.cpp (processed/cache accounting and timing updates),
//! and server-task.cpp (final OAI usage/timing placement). A new commit requires
//! review; field names, nightly tags and self-reported versions are not proof.
use scala_core::{InstalledRuntime, RuntimeAcquisitionMethod};

const REVIEWED_COMMIT: &str = "ca3d5a3e10d53f7ea672cb9b6178faca3e2807bc";
const REVIEWED_TREE: &str = "ba3e0b166abfd6f481e887e6b8d339bc220ceff9";
pub(super) const CONTRACT: &str = "llama.cpp/b10665-prompt-timing/1";

// GitHub release 378191926 targets REVIEWED_COMMIT. Pin original asset IDs AND
// archive digests, so replacement assets or a moved release/tag cannot inherit
// this review. Source: https://api.github.com/repos/ggml-org/llama.cpp/releases/tags/b10665
const REVIEWED_ASSETS: &[(&str, &str, &str)] = &[
    (
        "533037755",
        "llama-b10665-bin-ubuntu-arm64.tar.gz",
        "36983c882d7a88cbc02c190a3980cf397e526d588dd66c684b8cd53385a242a6",
    ),
    (
        "533037932",
        "llama-b10665-bin-ubuntu-s390x.tar.gz",
        "2a91c3b4159ab8d05f1c1bc878c19840c736b4cd372bbadf4b04237832773cc0",
    ),
    (
        "533038092",
        "llama-b10665-bin-ubuntu-vulkan-arm64.tar.gz",
        "746df9199ddfcc11f135f2750d1b38ce73564557642c38bef735fd2f08a9b8f6",
    ),
    (
        "533038114",
        "llama-b10665-bin-ubuntu-vulkan-x64.tar.gz",
        "92f8d63384132e6a70b3b106996a5dce06121bbf770eef68500b1cfb7ff22bcc",
    ),
    (
        "533038151",
        "llama-b10665-bin-ubuntu-x64.tar.gz",
        "7d065b7fe283eac932929bbc92b6e39b58551132a6291d7ab10ea9116997cb4e",
    ),
    (
        "533038198",
        "llama-b10665-bin-win-cpu-arm64.zip",
        "fa296ac9312b894e8ca1c620623a0620907202ae023b957959997b64abf7ec02",
    ),
    (
        "533038257",
        "llama-b10665-bin-win-cpu-x64.zip",
        "4b039869c48c2f5842ccc0c005cb36437bac33476be2d661f85e2814a7681af0",
    ),
    (
        "533038301",
        "llama-b10665-bin-win-cuda-12.4-x64.zip",
        "d9b05b81a3f60d30f6625e5561139af505a7ac1fd933c82ee9067ebbada0887a",
    ),
    (
        "533038438",
        "llama-b10665-bin-win-cuda-13.3-x64.zip",
        "5573a0f58c8cd315c001a68f0ca2c60866352130ba10945a2a403f61916d727e",
    ),
    (
        "533038584",
        "llama-b10665-bin-win-cuda-13.4-arm64.zip",
        "1f342986be5662fe958a3cfeb6a8058ca0e8e96582772e693b9381be558a5924",
    ),
    (
        "533038988",
        "llama-b10665-bin-win-vulkan-x64.zip",
        "9bee8af29495148c04c62cd2e254cf6310686d89025f04a4884eb3d7c4031f0d",
    ),
];

pub(super) fn reviewed(runtime: &InstalledRuntime) -> bool {
    let manifest = &runtime.manifest;
    let identity = &manifest.identity;
    let package = &identity.package;
    if manifest.validate().is_err()
        || identity.engine_id != super::ENGINE_ID
        || package.repository.as_deref() != Some(super::catalog::GITHUB_REPOSITORY)
        || identity.upstream_revision.as_deref() != Some(REVIEWED_COMMIT)
    {
        return false;
    }
    match manifest.acquisition_method {
        RuntimeAcquisitionMethod::SourceBuild => {
            identity.package_family == "llama-cpp-managed-source"
                && package.provider_id == super::LLAMA_CPP_SOURCE_RUNTIME_PROVIDER_ID
                && manifest.source_build.as_ref().is_some_and(|build| {
                    build.source.repository == super::catalog::GITHUB_REPOSITORY
                        && build.source.repository_url
                            == format!("{}.git", super::UPSTREAM_REPOSITORY)
                        && build.source.source_provider
                            == super::LLAMA_CPP_SOURCE_RUNTIME_PROVIDER_ID
                        && build.source.commit_sha == REVIEWED_COMMIT
                        && build.source.tree_sha == REVIEWED_TREE
                        && build.entrypoint_sha256 == manifest.entrypoint_sha256
                })
        }
        RuntimeAcquisitionMethod::OfficialReleaseAsset
        | RuntimeAcquisitionMethod::PreseededOfficialPack => {
            identity.package_family == "llama-cpp-official-release"
                && package.provider_id == super::LLAMA_CPP_RUNTIME_PROVIDER_ID
                && package.release_tag.as_deref() == Some("b10665")
                && REVIEWED_ASSETS.iter().any(|(id, name, sha)| {
                    package.asset_id.as_deref() == Some(*id)
                        && package.asset_name.as_deref() == Some(*name)
                        && manifest.downloaded_archive_sha256.as_deref() == Some(*sha)
                })
        }
        RuntimeAcquisitionMethod::ExternalBinary => false,
    }
}
