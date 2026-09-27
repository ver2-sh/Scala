use async_trait::async_trait;
use scala_core::{
    ArtifactFormat, AvailableRuntime, ComputeCapability, RuntimeAcquisitionPlan,
    RuntimeArchiveFormat, RuntimeDigest, RuntimeDownload, RuntimeId, RuntimeIdentity,
    RuntimeManifest, RuntimePackageIdentity, RuntimeReleaseChannel, RuntimeRequirements,
};
use scala_engine::{
    CatalogError, GitHubRelease, GitHubReleaseAsset, GitHubReleaseClient, RuntimeCatalogProvider,
};

use crate::ENGINE_ID;

// The portable Windows NInfer package line published by natpate/ninfer-windows.
// This provider is deliberately separate from the canonical Neroued/ninfer
// source provider: it retains its own repository provenance and never relabels
// the port as canonical upstream. Canonical upstream publishes Linux source
// only; this is a community port whose releases are the only managed native
// Windows route.
pub const PROVIDER_ID: &str = "ninfer-windows-portable-github";
pub const GITHUB_REPOSITORY: &str = "natpate/ninfer-windows";
pub const UPSTREAM_REPOSITORY: &str = "https://github.com/natpate/ninfer-windows";
pub const PACKAGE_FAMILY: &str = "ninfer-windows-portable";
pub const VARIANT: &str = "portable-v2-sm120a";

// v0.7.1 is the last reviewed v2-era portable Windows release. Upstream v0.8.0
// moved to NInfer v3 artifacts and does not load v2 containers; Scala currently
// admits only v2 artifacts, so later releases are excluded until a reviewed v3
// contract exists.
pub(crate) const REVIEWED_RELEASE_VERSION: &str = "0.7.1";
pub(crate) const REVIEWED_RELEASE_TAG: &str = "v0.7.1";

// The exact reviewed package: GitHub release 384326322 at tag v0.7.1, which
// resolves to source commit aa64ee5353b3c7... and tree fd35700dbbf797... -- the
// snapshot the WINDOWS_REVIEWED_SOURCE_BLOBS evidence in lib.rs was recorded
// against. The release publishes exactly one managed asset: 549408305
// `ninfer-windows-0.7.1-win64-cuda131.zip`, 386877422 bytes, SHA-256 d9bb7b4c...
// Catalog admission and capability credit are bound to every one of these
// immutable facts; a replaced asset, a moved tag, or a lookalike v0.7.x release
// is a different package and stays unavailable rather than inheriting review.
const REVIEWED_RELEASE_ID: u64 = 384326322;
const REVIEWED_TAG_COMMIT: &str = "aa64ee5353b3c79a0a4301a0d60f1716fb0451be";
const REVIEWED_TAG_TREE: &str = "fd35700dbbf797b6e46114efd46bbc569e151844";
const REVIEWED_ASSET_ID: u64 = 549408305;
const REVIEWED_ASSET_ID_STR: &str = "549408305";
const REVIEWED_ASSET_NAME: &str = "ninfer-windows-0.7.1-win64-cuda131.zip";
const REVIEWED_ASSET_SIZE: u64 = 386877422;
pub(crate) const REVIEWED_ARCHIVE_SHA256: &str =
    "d9bb7b4c4f5ded7de9f42ba25635a64d9d4f3412310701ff37f66743e8e940b5";

const ENTRYPOINT_NAME: &str = "ninfer-serve.exe";

#[derive(Debug, Clone, Copy, Default)]
pub struct NinferWindowsRuntimeCatalogProvider;

impl NinferWindowsRuntimeCatalogProvider {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl RuntimeCatalogProvider for NinferWindowsRuntimeCatalogProvider {
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
        if repository.full_name != GITHUB_REPOSITORY || repository.html_url != UPSTREAM_REPOSITORY {
            return Err(provider_error(
                "portable Windows NInfer repository identity is invalid",
            ));
        }
        let releases = github.releases(GITHUB_REPOSITORY).await?;
        let mut qualified = Vec::new();
        for release in &releases {
            if release.draft {
                continue;
            }
            if let Some(runtime) = reviewed_package_for_release(github, release).await? {
                qualified.push((release, runtime));
            }
        }
        assign_channels(&mut qualified);
        Ok(qualified.into_iter().map(|(_, runtime)| runtime).collect())
    }

    async fn fetch_reference(
        &self,
        github: &GitHubReleaseClient,
        reference: &str,
    ) -> Result<Vec<AvailableRuntime>, CatalogError> {
        let Some(tag) = windows_tag_from_reference(reference) else {
            return Ok(Vec::new());
        };
        let Some(release) = github.release_by_tag(GITHUB_REPOSITORY, &tag).await? else {
            return Ok(Vec::new());
        };
        if release.draft {
            return Ok(Vec::new());
        }
        let mut runtimes = reviewed_package_for_release(github, &release)
            .await?
            .into_iter()
            .collect::<Vec<_>>();
        for runtime in &mut runtimes {
            runtime.channels = release
                .prerelease
                .then_some(RuntimeReleaseChannel::Prerelease)
                .into_iter()
                .collect();
        }
        Ok(runtimes)
    }

    async fn verify_candidate(
        &self,
        github: &GitHubReleaseClient,
        candidate: &AvailableRuntime,
    ) -> Result<Option<AvailableRuntime>, CatalogError> {
        if !is_managed_windows_identity(&candidate.identity)
            || !matches!(
                candidate.acquisition,
                RuntimeAcquisitionPlan::ReleaseAsset { .. }
            )
        {
            return Ok(None);
        }
        let Some(tag) = candidate.identity.package.release_tag.as_deref() else {
            return Ok(None);
        };
        let Some(release) = github.release_by_tag(GITHUB_REPOSITORY, tag).await? else {
            return Ok(None);
        };
        let live = reviewed_package_for_release(github, &release)
            .await?
            .filter(|runtime| runtime.runtime_id == candidate.runtime_id);
        Ok(live)
    }
}

/// Turns a live release record into a managed candidate, or hides it when it
/// is not the exact reviewed package. The reviewed tag must also still resolve
/// to the recorded source commit and tree: a moved tag fails closed rather
/// than attributing the recorded source evidence to different bytes.
async fn reviewed_package_for_release(
    github: &GitHubReleaseClient,
    release: &GitHubRelease,
) -> Result<Option<AvailableRuntime>, CatalogError> {
    if release.tag_name != REVIEWED_RELEASE_TAG {
        return Ok(None);
    }
    let commit = github
        .commit(GITHUB_REPOSITORY, REVIEWED_RELEASE_TAG)
        .await?;
    if !commit.sha.eq_ignore_ascii_case(REVIEWED_TAG_COMMIT)
        || !commit
            .commit
            .tree
            .sha
            .eq_ignore_ascii_case(REVIEWED_TAG_TREE)
    {
        return Ok(None);
    }
    runtime_for_release(release)
}

/// The exact immutable identity this provider publishes. Shared by catalog,
/// capability, and update-line logic so the portable Windows contract cannot
/// drift between sites.
pub(crate) fn is_managed_windows_identity(identity: &RuntimeIdentity) -> bool {
    identity.engine_id == ENGINE_ID
        && identity.package_family == PACKAGE_FAMILY
        && identity.platform == "windows"
        && identity.architecture == "x86_64"
        && identity.accelerator == "cuda"
        && identity.variant == VARIANT
        && identity.package.provider_id == PROVIDER_ID
        && identity.package.repository.as_deref() == Some(GITHUB_REPOSITORY)
}

/// Identity-level facts of the exact reviewed package. This pins the release
/// tag and the immutable asset identity (id + name); it does not by itself
/// prove the package bytes, which callers must additionally bind through the
/// acquisition digest (candidates) or the persisted archive digest (installed
/// runtimes) via `is_reviewed_release_candidate` /
/// `is_reviewed_installed_release`.
pub(crate) fn is_reviewed_release_identity(identity: &RuntimeIdentity) -> bool {
    is_managed_windows_identity(identity)
        && identity.version == REVIEWED_RELEASE_VERSION
        && identity.package.release_tag.as_deref() == Some(REVIEWED_RELEASE_TAG)
        && identity.package.asset_id.as_deref() == Some(REVIEWED_ASSET_ID_STR)
        && identity.package.asset_name.as_deref() == Some(REVIEWED_ASSET_NAME)
        && identity.package.additional_assets.is_empty()
}

/// Full reviewed-package binding for a catalog candidate: the exact identity
/// plus acquisition metadata carrying the reviewed archive size and digest.
pub(crate) fn is_reviewed_release_candidate(runtime: &AvailableRuntime) -> bool {
    is_reviewed_release_identity(&runtime.identity)
        && matches!(
            &runtime.acquisition,
            RuntimeAcquisitionPlan::ReleaseAsset {
                download,
                additional_downloads,
            } if additional_downloads.is_empty()
                && download.archive_format == RuntimeArchiveFormat::Zip
                && download.size_bytes == REVIEWED_ASSET_SIZE
                && download.digest.as_ref().is_some_and(|digest| {
                    digest.algorithm == "sha256" && digest.value == REVIEWED_ARCHIVE_SHA256
                })
        )
}

/// Full reviewed-package binding for an installed runtime: the exact identity
/// plus the archive digest verified and persisted at install time. A package
/// whose recorded bytes differ from the reviewed archive receives no reviewed
/// capability credit, even under the same provider, version and tag.
pub(crate) fn is_reviewed_installed_release(manifest: &RuntimeManifest) -> bool {
    is_reviewed_release_identity(&manifest.identity)
        && manifest.downloaded_archive_sha256.as_deref() == Some(REVIEWED_ARCHIVE_SHA256)
        && manifest.additional_downloaded_archive_sha256.is_empty()
}

fn runtime_for_release(release: &GitHubRelease) -> Result<Option<AvailableRuntime>, CatalogError> {
    if release.draft {
        return Ok(None);
    }
    let Some(version) = release_version(&release.tag_name) else {
        return Ok(None);
    };
    let matching = release
        .assets
        .iter()
        .filter(|asset| usable_release_asset(asset) && portable_asset_name(&version, asset))
        .collect::<Vec<_>>();
    // A release whose assets cannot be bound to exactly one portable package is
    // not guessed at; additional or renamed assets keep the release hidden.
    let [asset] = matching.as_slice() else {
        return Ok(None);
    };
    // Only the exact reviewed package is exposed: the release object, the tag
    // (checked by the caller against the recorded commit/tree), and the single
    // asset's id, name, size and advertised digest must all match the reviewed
    // facts. A replaced or re-uploaded asset under the same tag stays hidden
    // rather than inheriting the reviewed release's identity.
    if release.id != REVIEWED_RELEASE_ID
        || asset.id != REVIEWED_ASSET_ID
        || asset.name != REVIEWED_ASSET_NAME
        || asset.size != REVIEWED_ASSET_SIZE
        || asset
            .digest
            .as_deref()
            .and_then(|digest| RuntimeDigest::parse_github(digest).ok())
            .is_none_or(|digest| digest.value != REVIEWED_ARCHIVE_SHA256)
    {
        return Ok(None);
    }
    let identity = RuntimeIdentity {
        engine_id: ENGINE_ID.to_owned(),
        package_family: PACKAGE_FAMILY.to_owned(),
        version: version.clone(),
        upstream_revision: commit_revision(&release.target_commitish),
        platform: "windows".to_owned(),
        architecture: "x86_64".to_owned(),
        accelerator: "cuda".to_owned(),
        variant: VARIANT.to_owned(),
        package: RuntimePackageIdentity {
            provider_id: PROVIDER_ID.to_owned(),
            repository: Some(GITHUB_REPOSITORY.to_owned()),
            release_tag: Some(release.tag_name.clone()),
            asset_id: Some(asset.id.to_string()),
            asset_name: Some(asset.name.clone()),
            additional_assets: Vec::new(),
        },
    };
    let runtime = AvailableRuntime {
        runtime_id: RuntimeId::from_identity(&identity),
        identity,
        display_name: format!("NInfer {version} portable Windows x86_64 CUDA"),
        supported_formats: vec![ArtifactFormat::Ninfer],
        source_url: release.html_url.clone(),
        published_at_unix: release
            .published_at
            .as_deref()
            .and_then(super::catalog::parse_github_timestamp),
        channels: Vec::new(),
        prerelease: release.prerelease,
        acquisition: RuntimeAcquisitionPlan::ReleaseAsset {
            download: RuntimeDownload {
                url: asset.browser_download_url.clone(),
                size_bytes: asset.size,
                digest: asset
                    .digest
                    .as_deref()
                    .and_then(|digest| RuntimeDigest::parse_github(digest).ok()),
                archive_format: RuntimeArchiveFormat::Zip,
                entrypoint_names: vec![ENTRYPOINT_NAME.to_owned()],
            },
            additional_downloads: Vec::new(),
        },
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
                "Portable Windows x86_64 package published by natpate/ninfer-windows; it is not an official Neroued/ninfer release"
                    .to_owned(),
                "Self-contained package: the retained archive layout keeps the runtime DLLs required beside ninfer-serve.exe; no WSL or CUDA toolkit installation is needed"
                    .to_owned(),
            ],
            unverified_requirements: vec![
                "Upstream requires a CUDA 13.1-capable NVIDIA driver; this host's driver support for that runtime has not been proven"
                    .to_owned(),
            ],
        },
    };
    runtime.validate().map_err(|error| CatalogError::Provider {
        provider: PROVIDER_ID.to_owned(),
        message: format!(
            "portable Windows release `{}` produced invalid runtime metadata: {error}",
            release.tag_name
        ),
    })?;
    Ok(Some(runtime))
}

/// Only the exact reviewed release is admitted. Semantic-version resemblance
/// is not review evidence: other `v0.7.x` tags -- including hypothetical newer
/// ones -- stay hidden until a package is actually reviewed; `v0.8.0` and
/// later moved to NInfer v3 artifacts that Scala does not load.
fn release_version(tag_name: &str) -> Option<String> {
    (tag_name == REVIEWED_RELEASE_TAG).then(|| REVIEWED_RELEASE_VERSION.to_owned())
}

fn release_version_parts(tag_name: &str) -> Option<(u64, u64, u64)> {
    version_parts(tag_name.strip_prefix('v')?)
}

fn version_parts(version: &str) -> Option<(u64, u64, u64)> {
    let mut components = version.split('.');
    let parts = (
        components.next()?.parse().ok()?,
        components.next()?.parse().ok()?,
        components.next()?.parse().ok()?,
    );
    components.next().is_none().then_some(parts)
}

fn portable_asset_name(version: &str, asset: &GitHubReleaseAsset) -> bool {
    let Some(rest) = asset
        .name
        .strip_prefix(&format!("ninfer-windows-{version}-win64-"))
    else {
        return false;
    };
    rest.strip_suffix(".zip").is_some_and(|toolkit| {
        toolkit.strip_prefix("cuda").is_some_and(|digits| {
            !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit())
        })
    })
}

fn usable_release_asset(asset: &GitHubReleaseAsset) -> bool {
    asset.state == "uploaded"
        && asset.size > 0
        && asset
            .digest
            .as_deref()
            .and_then(|digest| RuntimeDigest::parse_github(digest).ok())
            .is_some()
}

fn windows_tag_from_reference(reference: &str) -> Option<String> {
    let candidate = reference.trim();
    let version = candidate.strip_prefix('v').unwrap_or(candidate);
    let parts = version.split('.').collect::<Vec<_>>();
    if parts.len() == 3
        && parts
            .iter()
            .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
    {
        return Some(format!("v{version}"));
    }
    let parts = reference.split('-').collect::<Vec<_>>();
    let start = parts.iter().position(|part| *part == ENGINE_ID)? + 1;
    let version = parts.get(start..start + 3)?;
    version
        .iter()
        .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| format!("v{}.{}.{}", version[0], version[1], version[2]))
}

fn commit_revision(value: &str) -> Option<String> {
    (value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .then(|| value.to_ascii_lowercase())
}

fn assign_channels(qualified: &mut [(&GitHubRelease, AvailableRuntime)]) {
    let newest = |filter: &dyn Fn(&GitHubRelease) -> bool| {
        qualified
            .iter()
            .filter(|(release, _)| filter(release))
            .max_by(|left, right| {
                release_version_parts(&left.0.tag_name)
                    .cmp(&release_version_parts(&right.0.tag_name))
            })
            .map(|(release, _)| release.id)
    };
    let latest_release_id = newest(&|_| true);
    let stable_release_id = newest(&|release| !release.prerelease);
    for (release, runtime) in qualified.iter_mut() {
        if stable_release_id == Some(release.id) {
            runtime.channels.push(RuntimeReleaseChannel::Stable);
        }
        if latest_release_id == Some(release.id) {
            runtime.channels.push(RuntimeReleaseChannel::Latest);
        }
        if release.prerelease {
            runtime.channels.push(RuntimeReleaseChannel::Prerelease);
        }
    }
}

fn provider_error(message: &str) -> CatalogError {
    CatalogError::Provider {
        provider: PROVIDER_ID.to_owned(),
        message: message.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use scala_core::RuntimeReleaseChannel;

    use super::*;

    #[test]
    fn only_the_exact_reviewed_release_is_admitted() {
        assert_eq!(release_version("v0.7.1").as_deref(), Some("0.7.1"));
        // No other tag is admitted from semantic-version resemblance: older
        // portable releases predate the reviewed generation, hypothetical
        // future v0.7.x releases are unreviewed until a package is actually
        // examined, and v0.8.x/v0.9.x are NInfer v3 while Scala is v2-only.
        for tag in [
            "v0.1.0",
            "v0.6.1",
            "v0.7.0",
            "v0.7.2",
            "v0.7.12",
            "v0.8.0",
            "v0.8.1",
            "v0.9.0",
            "v1.0.0",
            "0.7.1",
            "v0.7",
            "v0.7.1.2",
            "release",
            "v0.7.1-rc1",
        ] {
            assert!(release_version(tag).is_none(), "{tag}");
        }
    }

    #[test]
    fn portable_asset_grammar_is_exact() {
        let named = |name: &str| GitHubReleaseAsset {
            id: 1,
            name: name.to_owned(),
            size: 42,
            browser_download_url: format!(
                "https://github.com/{GITHUB_REPOSITORY}/releases/download/v0.7.1/{name}"
            ),
            digest: Some(format!("sha256:{}", "a".repeat(64))),
            state: "uploaded".to_owned(),
        };
        let asset = named("ninfer-windows-0.7.1-win64-cuda131.zip");
        assert!(portable_asset_name("0.7.1", &asset));
        for name in [
            "ninfer-windows-0.7.0-win64-cuda131.zip",
            "ninfer-windows-0.7.1-win64-cuda131.tar.gz",
            "ninfer-windows-0.7.1-linux-x64-cuda131.zip",
            "ninfer-windows-0.7.1-win64.zip",
            "ninfer-windows-0.7.1-win64-cuda.zip",
            "ninfer-windows-0.7.1-win64-cuda131a.zip",
            "other-0.7.1-win64-cuda131.zip",
            "ninfer-windows-0.7.1-win64-cuda131.zip.sha256",
        ] {
            assert!(
                !portable_asset_name("0.7.1", &named(name)),
                "{name} must not match the 0.7.1 package grammar"
            );
        }
    }

    #[test]
    fn release_reference_recovers_the_exact_tag() {
        for reference in [
            "v0.7.1",
            "0.7.1",
            "  v0.7.1  ",
            "ninfer-0-7-1",
            "ninfer-0-7-1-windows-x86-64-cuda-portable-v2-sm120a-0123456789abcdef",
        ] {
            assert_eq!(
                windows_tag_from_reference(reference).as_deref(),
                Some("v0.7.1"),
                "{reference}"
            );
        }
        // A v3 tag is still a resolvable release reference; admission is
        // enforced when the release is turned into a runtime candidate.
        assert_eq!(
            windows_tag_from_reference("v0.8.0").as_deref(),
            Some("v0.8.0")
        );
        for reference in [
            "ninfer",
            "ninfer source",
            "ninfer-0-7",
            "q27-0-7-1-windows-x86-64",
            "release",
        ] {
            assert!(
                windows_tag_from_reference(reference).is_none(),
                "{reference}"
            );
        }
    }

    #[test]
    fn v0_7_1_release_produces_the_portable_windows_candidate() {
        let release = reviewed_release();
        let runtime = runtime_for_release(&release)
            .expect("release builds")
            .expect("v0.7.1 is an admitted candidate");
        let identity = &runtime.identity;
        assert_eq!(identity.engine_id, ENGINE_ID);
        assert_eq!(identity.package_family, PACKAGE_FAMILY);
        assert_eq!(identity.version, "0.7.1");
        assert_eq!(identity.platform, "windows");
        assert_eq!(identity.architecture, "x86_64");
        assert_eq!(identity.accelerator, "cuda");
        assert_eq!(identity.variant, VARIANT);
        assert_eq!(identity.package.provider_id, PROVIDER_ID);
        assert_eq!(
            identity.package.repository.as_deref(),
            Some(GITHUB_REPOSITORY)
        );
        assert_eq!(identity.package.release_tag.as_deref(), Some("v0.7.1"));
        assert_eq!(
            identity.package.asset_id.as_deref(),
            Some(REVIEWED_ASSET_ID_STR)
        );
        assert_eq!(
            identity.package.asset_name.as_deref(),
            Some(REVIEWED_ASSET_NAME)
        );
        assert!(is_managed_windows_identity(identity));
        assert!(is_reviewed_release_identity(identity));
        assert!(is_reviewed_release_candidate(&runtime));
        let RuntimeAcquisitionPlan::ReleaseAsset {
            download,
            additional_downloads,
        } = &runtime.acquisition
        else {
            panic!("expected release asset acquisition");
        };
        assert!(additional_downloads.is_empty());
        assert_eq!(download.archive_format, RuntimeArchiveFormat::Zip);
        assert_eq!(download.entrypoint_names, ["ninfer-serve.exe"]);
        assert_eq!(download.size_bytes, REVIEWED_ASSET_SIZE);
        assert_eq!(
            download.digest.as_ref().map(|digest| digest.value.as_str()),
            Some(REVIEWED_ARCHIVE_SHA256)
        );
        assert_eq!(
            download.url,
            "https://github.com/natpate/ninfer-windows/releases/download/v0.7.1/ninfer-windows-0.7.1-win64-cuda131.zip"
        );
        assert_eq!(
            runtime.requirements.supported_cuda_compute_capabilities,
            vec![ComputeCapability::new(12, 0)]
        );
        assert_eq!(
            runtime.requirements.required_nvidia_device_names,
            ["NVIDIA GeForce RTX 5090"]
        );
        assert_eq!(runtime.supported_formats, [ArtifactFormat::Ninfer]);
    }

    #[test]
    fn v3_and_other_or_ambiguous_releases_are_not_candidates() {
        // Other v0.7.x tags are unreviewed rather than assumed-compatible;
        // v0.8+ moved to NInfer v3 artifacts Scala does not load.
        for tag in [
            "v0.6.1", "v0.7.0", "v0.7.2", "v0.7.12", "v0.8.0", "v0.8.1", "v0.9.0",
        ] {
            let release = release(
                tag,
                vec![asset(&format!(
                    "ninfer-windows-{}-win64-cuda131.zip",
                    tag.strip_prefix('v').unwrap()
                ))],
            );
            assert!(runtime_for_release(&release).unwrap().is_none(), "{tag}");
        }

        let ambiguous = release(
            "v0.7.1",
            vec![
                reviewed_asset(),
                asset("ninfer-windows-0.7.1-win64-cuda132.zip"),
            ],
        );
        assert!(runtime_for_release(&ambiguous).unwrap().is_none());

        for mutate in [
            |asset: &mut GitHubReleaseAsset| asset.state = "open".to_owned(),
            |asset: &mut GitHubReleaseAsset| asset.size = 0,
            |asset: &mut GitHubReleaseAsset| asset.digest = None,
            |asset: &mut GitHubReleaseAsset| asset.digest = Some("md5:0123456789abcdef".to_owned()),
        ] {
            let mut fixture = reviewed_release();
            mutate(&mut fixture.assets[0]);
            assert!(runtime_for_release(&fixture).unwrap().is_none());
        }
    }

    #[test]
    fn a_replaced_package_under_the_same_tag_is_not_the_reviewed_package() {
        // Package replacement fails closed: with the same provider, version and
        // tag, changing any immutable release/asset fact keeps the release out
        // of the managed catalog entirely.
        for mutate in [
            |release: &mut GitHubRelease| release.id += 1,
            |release: &mut GitHubRelease| release.assets[0].id += 1,
            |release: &mut GitHubRelease| {
                release.assets[0].name = "ninfer-windows-0.7.1-win64-cuda132.zip".to_owned()
            },
            |release: &mut GitHubRelease| release.assets[0].size += 1,
            |release: &mut GitHubRelease| {
                release.assets[0].digest = Some(format!("sha256:{}", "0".repeat(64)))
            },
            |release: &mut GitHubRelease| release.assets[0].digest = None,
        ] {
            let mut fixture = reviewed_release();
            mutate(&mut fixture);
            assert!(
                runtime_for_release(&fixture).unwrap().is_none(),
                "a mutated reviewed release must not produce a candidate"
            );
        }

        // The same binding is enforced for capability credit on identities and
        // candidates that were not produced by this catalog (for example a
        // persisted manifest written before the asset was replaced).
        let runtime = runtime_for_release(&reviewed_release())
            .unwrap()
            .expect("reviewed candidate");
        for mutate in [
            |runtime: &mut AvailableRuntime| {
                runtime.identity.package.asset_id = Some("1".to_owned())
            },
            |runtime: &mut AvailableRuntime| {
                runtime.identity.package.asset_name =
                    Some("ninfer-windows-0.7.1-win64-cuda132.zip".to_owned())
            },
            |runtime: &mut AvailableRuntime| {
                runtime.identity.package.release_tag = Some("v0.7.2".to_owned())
            },
        ] {
            let mut runtime = runtime.clone();
            mutate(&mut runtime);
            assert!(!is_reviewed_release_identity(&runtime.identity));
            assert!(!is_reviewed_release_candidate(&runtime));
        }
        let mut wrong_digest = runtime.clone();
        let RuntimeAcquisitionPlan::ReleaseAsset { download, .. } = &mut wrong_digest.acquisition
        else {
            panic!("expected release asset acquisition");
        };
        download.digest = Some(RuntimeDigest::sha256("0".repeat(64)).unwrap());
        assert!(is_reviewed_release_identity(&wrong_digest.identity));
        assert!(!is_reviewed_release_candidate(&wrong_digest));
    }

    #[test]
    fn the_reviewed_release_holds_stable_and_latest() {
        let release = reviewed_release();
        let mut qualified = vec![(
            &release,
            runtime_for_release(&release)
                .unwrap()
                .expect("reviewed runtime"),
        )];
        assign_channels(&mut qualified);
        assert_eq!(
            qualified[0].1.channels,
            [RuntimeReleaseChannel::Stable, RuntimeReleaseChannel::Latest]
        );
    }

    fn reviewed_release() -> GitHubRelease {
        release_with_id(
            REVIEWED_RELEASE_ID,
            REVIEWED_RELEASE_TAG,
            vec![reviewed_asset()],
        )
    }

    fn release(tag: &str, assets: Vec<GitHubReleaseAsset>) -> GitHubRelease {
        release_with_id(1, tag, assets)
    }

    fn release_with_id(id: u64, tag: &str, assets: Vec<GitHubReleaseAsset>) -> GitHubRelease {
        GitHubRelease {
            id,
            tag_name: tag.to_owned(),
            name: None,
            html_url: format!("https://github.com/{GITHUB_REPOSITORY}/releases/tag/{tag}"),
            target_commitish: "master".to_owned(),
            draft: false,
            prerelease: false,
            published_at: Some("2026-09-07T21:38:24Z".to_owned()),
            assets,
        }
    }

    fn reviewed_asset() -> GitHubReleaseAsset {
        GitHubReleaseAsset {
            id: REVIEWED_ASSET_ID,
            name: REVIEWED_ASSET_NAME.to_owned(),
            size: REVIEWED_ASSET_SIZE,
            browser_download_url: format!(
                "https://github.com/{GITHUB_REPOSITORY}/releases/download/{REVIEWED_RELEASE_TAG}/{REVIEWED_ASSET_NAME}"
            ),
            digest: Some(format!("sha256:{REVIEWED_ARCHIVE_SHA256}")),
            state: "uploaded".to_owned(),
        }
    }

    fn asset(name: &str) -> GitHubReleaseAsset {
        GitHubReleaseAsset {
            id: 1,
            name: name.to_owned(),
            size: 42,
            browser_download_url: format!(
                "https://github.com/{GITHUB_REPOSITORY}/releases/download/v0.7.1/{name}"
            ),
            digest: Some(format!("sha256:{}", "a".repeat(64))),
            state: "uploaded".to_owned(),
        }
    }
}
