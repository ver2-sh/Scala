use std::collections::BTreeMap;

use async_trait::async_trait;
use scala_core::{
    ArtifactFormat, AvailableRuntime, RuntimeAcquisitionPlan, RuntimeArchiveFormat, RuntimeDigest,
    RuntimeDownload, RuntimeIdentity, RuntimePackageAssetIdentity, RuntimePackageIdentity,
    RuntimeReleaseChannel, RuntimeRequirements,
};
use scala_engine::{
    CatalogError, GitHubRelease, GitHubReleaseAsset, GitHubReleaseClient, RuntimeCatalogProvider,
};
use sha2::{Digest, Sha256};

use crate::ENGINE_ID;

pub(crate) const GITHUB_REPOSITORY: &str = "ggml-org/llama.cpp";
const PACKAGE_FAMILY: &str = "llama-cpp-official-release";
const NIGHTLY_POINTER_ASSET: &str = "nightly-tag.txt";
const NIGHTLY_POINTER_MAXIMUM_BYTES: usize = 64;

pub const LLAMA_CPP_RUNTIME_PROVIDER_ID: &str = "llama-cpp-official-github";

#[derive(Debug, Clone, Copy, Default)]
pub struct LlamaCppRuntimeCatalogProvider;

impl LlamaCppRuntimeCatalogProvider {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl RuntimeCatalogProvider for LlamaCppRuntimeCatalogProvider {
    fn id(&self) -> &'static str {
        LLAMA_CPP_RUNTIME_PROVIDER_ID
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
        let mut runtimes = Vec::new();
        for release in &releases {
            if release.draft || nightly_number(&release.tag_name).is_none() {
                continue;
            }
            runtimes.extend(runtimes_for_release(release)?);
        }

        let stable_nightly = verified_stable_nightly(github, &releases).await;
        assign_channels(&mut runtimes, stable_nightly.as_deref());
        Ok(runtimes)
    }

    async fn fetch_reference(
        &self,
        github: &GitHubReleaseClient,
        reference: &str,
    ) -> Result<Vec<AvailableRuntime>, CatalogError> {
        let Some(tag) = nightly_tag_from_reference(reference) else {
            return Ok(Vec::new());
        };
        let Some(release) = github.release_by_tag(GITHUB_REPOSITORY, &tag).await? else {
            return Ok(Vec::new());
        };
        if release.draft || nightly_number(&release.tag_name).is_none() {
            return Ok(Vec::new());
        }
        let mut runtimes = runtimes_for_release(&release)?;
        for runtime in &mut runtimes {
            runtime.channels = release
                .prerelease
                .then_some(RuntimeReleaseChannel::Prerelease)
                .into_iter()
                .collect();
        }
        Ok(runtimes)
    }
}

pub(crate) fn nightly_tag_from_reference(reference: &str) -> Option<String> {
    reference
        .split(|character: char| !character.is_ascii_alphanumeric())
        .find(|part| nightly_number(part).is_some())
        .map(str::to_owned)
}

#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd)]
struct RuntimeLine {
    platform: String,
    architecture: String,
    accelerator: String,
    variant: String,
}

impl RuntimeLine {
    fn from_runtime(runtime: &AvailableRuntime) -> Self {
        Self {
            platform: runtime.identity.platform.clone(),
            architecture: runtime.identity.architecture.clone(),
            accelerator: runtime.identity.accelerator.clone(),
            variant: runtime.identity.variant.clone(),
        }
    }
}

#[derive(Debug, Clone)]
struct AssetClassification {
    platform: &'static str,
    architecture: &'static str,
    accelerator: &'static str,
    variant: String,
    display_backend: String,
    archive_format: RuntimeArchiveFormat,
    cuda_companion_name: Option<String>,
}

fn runtimes_for_release(release: &GitHubRelease) -> Result<Vec<AvailableRuntime>, CatalogError> {
    let mut runtimes = Vec::new();
    for asset in release
        .assets
        .iter()
        .filter(|asset| usable_release_asset(asset))
    {
        let Some(classification) = classify_asset(&release.tag_name, &asset.name) else {
            continue;
        };
        let companion = if let Some(name) = &classification.cuda_companion_name {
            let Some(companion) = release
                .assets
                .iter()
                .find(|candidate| usable_release_asset(candidate) && candidate.name == *name)
            else {
                continue;
            };
            Some(companion)
        } else {
            None
        };
        let runtime = available_runtime(release, asset, companion, classification)?;
        let verified = runtime
            .release_assets()
            .is_some_and(|(download, additional)| {
                download.digest.is_some()
                    && additional.iter().all(|download| download.digest.is_some())
            });
        if verified {
            runtimes.push(runtime);
        }
    }
    Ok(runtimes)
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

fn available_runtime(
    release: &GitHubRelease,
    asset: &GitHubReleaseAsset,
    companion: Option<&GitHubReleaseAsset>,
    classification: AssetClassification,
) -> Result<AvailableRuntime, CatalogError> {
    let additional_assets = companion
        .map(|companion| RuntimePackageAssetIdentity {
            asset_id: companion.id.to_string(),
            asset_name: companion.name.clone(),
            role: "cuda-runtime".to_owned(),
        })
        .into_iter()
        .collect();
    let identity = RuntimeIdentity {
        engine_id: ENGINE_ID.to_owned(),
        package_family: PACKAGE_FAMILY.to_owned(),
        version: release.tag_name.clone(),
        upstream_revision: commit_revision(&release.target_commitish),
        platform: classification.platform.to_owned(),
        architecture: classification.architecture.to_owned(),
        accelerator: classification.accelerator.to_owned(),
        variant: classification.variant,
        package: RuntimePackageIdentity {
            provider_id: LLAMA_CPP_RUNTIME_PROVIDER_ID.to_owned(),
            repository: Some(GITHUB_REPOSITORY.to_owned()),
            release_tag: Some(release.tag_name.clone()),
            asset_id: Some(asset.id.to_string()),
            asset_name: Some(asset.name.clone()),
            additional_assets,
        },
    };
    let runtime_id = scala_core::RuntimeId::from_identity(&identity);
    let requirements = RuntimeRequirements {
        requires_nvidia_gpu: classification.accelerator == "cuda",
        minimum_nvidia_driver: None,
        minimum_vram_bytes: None,
        minimum_vram_class_gib: None,
        minimum_vram_exclusive_class_gib: None,
        supported_cuda_compute_capabilities: Vec::new(),
        required_nvidia_device_names: Vec::new(),
        notes: Vec::new(),
        advisories: Vec::new(),
        unverified_requirements: companion
            .map(|_| {
                vec![
                    "Matching official CUDA runtime companion is included, but this host's exact GPU/toolkit compatibility has not been proven"
                        .to_owned(),
                ]
            })
            .unwrap_or_default(),
    };
    let runtime = AvailableRuntime {
        runtime_id,
        identity,
        display_name: format!(
            "llama.cpp {} ({} {})",
            classification.display_backend,
            display_platform(classification.platform),
            classification.architecture
        ),
        supported_formats: vec![ArtifactFormat::Gguf],
        source_url: release.html_url.clone(),
        published_at_unix: release
            .published_at
            .as_deref()
            .and_then(parse_github_timestamp),
        channels: Vec::new(),
        prerelease: release.prerelease,
        acquisition: RuntimeAcquisitionPlan::ReleaseAsset {
            download: runtime_download(
                asset,
                classification.archive_format,
                vec![entrypoint_name(classification.platform).to_owned()],
            ),
            additional_downloads: companion
                .map(|companion| {
                    vec![runtime_download(
                        companion,
                        RuntimeArchiveFormat::Zip,
                        Vec::new(),
                    )]
                })
                .unwrap_or_default(),
        },
        supported_native_identities: Vec::new(),
        requirements,
    };
    runtime.validate().map_err(|error| CatalogError::Provider {
        provider: LLAMA_CPP_RUNTIME_PROVIDER_ID.to_owned(),
        message: error.to_string(),
    })?;
    Ok(runtime)
}

fn runtime_download(
    asset: &GitHubReleaseAsset,
    archive_format: RuntimeArchiveFormat,
    entrypoint_names: Vec<String>,
) -> RuntimeDownload {
    RuntimeDownload {
        url: asset.browser_download_url.clone(),
        size_bytes: asset.size,
        digest: asset
            .digest
            .as_deref()
            .and_then(|digest| RuntimeDigest::parse_github(digest).ok()),
        archive_format,
        entrypoint_names,
    }
}

fn classify_asset(tag: &str, name: &str) -> Option<AssetClassification> {
    nightly_number(tag)?;
    let remainder = name.strip_prefix(&format!("llama-{tag}-bin-"))?;

    if let Some(package) = remainder.strip_suffix(".zip") {
        if let Some(architecture) = package
            .strip_prefix("win-cpu-")
            .and_then(windows_architecture)
        {
            return Some(classification(
                "windows",
                architecture,
                "cpu",
                "default",
                "CPU",
                RuntimeArchiveFormat::Zip,
            ));
        }
        if package == "win-vulkan-x64" {
            return Some(classification(
                "windows",
                "x86_64",
                "vulkan",
                "default",
                "Vulkan",
                RuntimeArchiveFormat::Zip,
            ));
        }
        if let Some(cuda) = package.strip_prefix("win-cuda-") {
            let (version, upstream_architecture) = cuda.rsplit_once('-')?;
            if !valid_dotted_version(version) {
                return None;
            }
            let architecture = windows_architecture(upstream_architecture)?;
            return Some(AssetClassification {
                platform: "windows",
                architecture,
                accelerator: "cuda",
                variant: version.to_owned(),
                display_backend: format!("CUDA {version}"),
                archive_format: RuntimeArchiveFormat::Zip,
                cuda_companion_name: Some(format!(
                    "cudart-llama-bin-win-cuda-{version}-{upstream_architecture}.zip"
                )),
            });
        }
        return None;
    }

    let package = remainder.strip_suffix(".tar.gz")?;
    if let Some(architecture) = package.strip_prefix("ubuntu-").and_then(linux_architecture) {
        return Some(classification(
            "linux",
            architecture,
            "cpu",
            "default",
            "CPU",
            RuntimeArchiveFormat::TarGz,
        ));
    }
    if let Some(architecture) = package
        .strip_prefix("ubuntu-vulkan-")
        .and_then(linux_vulkan_architecture)
    {
        return Some(classification(
            "linux",
            architecture,
            "vulkan",
            "default",
            "Vulkan",
            RuntimeArchiveFormat::TarGz,
        ));
    }
    None
}

fn classification(
    platform: &'static str,
    architecture: &'static str,
    accelerator: &'static str,
    variant: &str,
    display_backend: &str,
    archive_format: RuntimeArchiveFormat,
) -> AssetClassification {
    AssetClassification {
        platform,
        architecture,
        accelerator,
        variant: variant.to_owned(),
        display_backend: display_backend.to_owned(),
        archive_format,
        cuda_companion_name: None,
    }
}

fn windows_architecture(value: &str) -> Option<&'static str> {
    match value {
        "x64" => Some("x86_64"),
        "arm64" => Some("aarch64"),
        _ => None,
    }
}

fn linux_architecture(value: &str) -> Option<&'static str> {
    match value {
        "x64" => Some("x86_64"),
        "arm64" => Some("aarch64"),
        "s390x" => Some("s390x"),
        _ => None,
    }
}

fn linux_vulkan_architecture(value: &str) -> Option<&'static str> {
    match value {
        "x64" => Some("x86_64"),
        "arm64" => Some("aarch64"),
        _ => None,
    }
}

fn valid_dotted_version(value: &str) -> bool {
    let mut components = value.split('.');
    let Some(first) = components.next() else {
        return false;
    };
    !first.is_empty()
        && first.bytes().all(|byte| byte.is_ascii_digit())
        && components.all(|component| {
            !component.is_empty() && component.bytes().all(|byte| byte.is_ascii_digit())
        })
}

fn assign_channels(runtimes: &mut [AvailableRuntime], stable_nightly: Option<&str>) {
    let mut latest_by_line = BTreeMap::<RuntimeLine, u64>::new();
    for runtime in runtimes.iter() {
        let Some(build) = nightly_number(&runtime.identity.version) else {
            continue;
        };
        latest_by_line
            .entry(RuntimeLine::from_runtime(runtime))
            .and_modify(|latest| *latest = (*latest).max(build))
            .or_insert(build);
    }

    for runtime in runtimes {
        let build = nightly_number(&runtime.identity.version);
        let is_latest = build.is_some_and(|build| {
            latest_by_line.get(&RuntimeLine::from_runtime(runtime)) == Some(&build)
        });
        if runtime.identity.package.release_tag.as_deref() == stable_nightly {
            runtime.channels.push(RuntimeReleaseChannel::Stable);
        }
        if is_latest {
            runtime.channels.push(RuntimeReleaseChannel::Latest);
        }
        if runtime.prerelease {
            runtime.channels.push(RuntimeReleaseChannel::Prerelease);
        }
    }
}

async fn verified_stable_nightly(
    github: &GitHubReleaseClient,
    releases: &[GitHubRelease],
) -> Option<String> {
    let stable = releases
        .iter()
        .filter(|release| !release.draft && !release.prerelease)
        .filter_map(|release| semantic_version(&release.tag_name).map(|version| (version, release)))
        .max_by_key(|(version, _)| *version)
        .map(|(_, release)| release)?;
    let pointer = stable.assets.iter().find(|asset| {
        asset.state == "uploaded"
            && asset.name == NIGHTLY_POINTER_ASSET
            && asset.size <= NIGHTLY_POINTER_MAXIMUM_BYTES as u64
    })?;
    let expected = pointer
        .digest
        .as_deref()
        .and_then(|digest| RuntimeDigest::parse_github(digest).ok())?;
    let text = github
        .fetch_small_text(&pointer.browser_download_url, NIGHTLY_POINTER_MAXIMUM_BYTES)
        .await
        .ok()?;
    if super::hex_digest(Sha256::digest(text.as_bytes())) != expected.value {
        return None;
    }
    let nightly_tag = text.trim();
    nightly_number(nightly_tag)?;
    let nightly = releases
        .iter()
        .find(|release| !release.draft && release.tag_name == nightly_tag && release.prerelease)?;
    if nightly.target_commitish != stable.target_commitish {
        return None;
    }
    Some(nightly_tag.to_owned())
}

pub(crate) fn nightly_number(tag: &str) -> Option<u64> {
    let number = tag.strip_prefix('b')?;
    if number.is_empty() || !number.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    number.parse().ok()
}

fn semantic_version(tag: &str) -> Option<(u64, u64, u64)> {
    let mut components = tag.strip_prefix('v')?.split('.');
    let version = (
        components.next()?.parse().ok()?,
        components.next()?.parse().ok()?,
        components.next()?.parse().ok()?,
    );
    components.next().is_none().then_some(version)
}

fn commit_revision(value: &str) -> Option<String> {
    (value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .then(|| value.to_ascii_lowercase())
}

fn entrypoint_name(platform: &str) -> &'static str {
    if platform == "windows" {
        "llama-server.exe"
    } else {
        "llama-server"
    }
}

fn display_platform(platform: &str) -> &str {
    if platform == "windows" {
        "Windows"
    } else {
        "Linux"
    }
}

pub(crate) fn parse_github_timestamp(value: &str) -> Option<i64> {
    let bytes = value.as_bytes();
    if bytes.len() != 20
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || bytes[10] != b'T'
        || bytes[13] != b':'
        || bytes[16] != b':'
        || bytes[19] != b'Z'
    {
        return None;
    }
    let year = decimal(bytes, 0, 4)? as i64;
    let month = decimal(bytes, 5, 7)? as i64;
    let day = decimal(bytes, 8, 10)? as i64;
    let hour = decimal(bytes, 11, 13)? as i64;
    let minute = decimal(bytes, 14, 16)? as i64;
    let second = decimal(bytes, 17, 19)? as i64;
    if !(1..=12).contains(&month)
        || day == 0
        || day > days_in_month(year, month)
        || hour > 23
        || minute > 59
        || second > 60
    {
        return None;
    }
    days_from_civil(year, month, day)
        .checked_mul(86_400)?
        .checked_add(hour * 3_600 + minute * 60 + second)
}

fn decimal(bytes: &[u8], start: usize, end: usize) -> Option<u32> {
    bytes
        .get(start..end)?
        .iter()
        .try_fold(0_u32, |value, byte| {
            if byte.is_ascii_digit() {
                Some(value * 10 + u32::from(*byte - b'0'))
            } else {
                None
            }
        })
}

fn days_in_month(year: i64, month: i64) -> i64 {
    match month {
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

// Gregorian calendar conversion with 1970-01-01 as day zero.
fn days_from_civil(mut year: i64, month: i64, day: i64) -> i64 {
    year -= i64::from(month <= 2);
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let adjusted_month = month + if month > 2 { -3 } else { 9 };
    let day_of_year = (153 * adjusted_month + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_nightly_reference_is_recovered_from_ids_and_queries() {
        assert_eq!(
            nightly_tag_from_reference("b10665"),
            Some("b10665".to_owned())
        );
        assert_eq!(
            nightly_tag_from_reference(
                "llama-cpp-b10665-windows-x86-64-cpu-default-436987ea6bbe1ae7"
            ),
            Some("b10665".to_owned())
        );
        assert_eq!(nightly_tag_from_reference("llama cpu"), None);
    }

    #[test]
    fn classifies_only_supported_exact_asset_names() {
        let cases = [
            (
                "llama-b10665-bin-win-cpu-x64.zip",
                "windows",
                "x86_64",
                "cpu",
            ),
            (
                "llama-b10665-bin-win-cpu-arm64.zip",
                "windows",
                "aarch64",
                "cpu",
            ),
            (
                "llama-b10665-bin-win-vulkan-x64.zip",
                "windows",
                "x86_64",
                "vulkan",
            ),
            (
                "llama-b10665-bin-ubuntu-x64.tar.gz",
                "linux",
                "x86_64",
                "cpu",
            ),
            (
                "llama-b10665-bin-ubuntu-arm64.tar.gz",
                "linux",
                "aarch64",
                "cpu",
            ),
            (
                "llama-b10665-bin-ubuntu-s390x.tar.gz",
                "linux",
                "s390x",
                "cpu",
            ),
            (
                "llama-b10665-bin-ubuntu-vulkan-x64.tar.gz",
                "linux",
                "x86_64",
                "vulkan",
            ),
        ];
        for (name, platform, architecture, accelerator) in cases {
            let parsed = classify_asset("b10665", name).expect("asset should be supported");
            assert_eq!(parsed.platform, platform);
            assert_eq!(parsed.architecture, architecture);
            assert_eq!(parsed.accelerator, accelerator);
        }
        for name in [
            "llama-b10664-bin-win-cpu-x64.zip",
            "llama-b10665-bin-win-vulkan-arm64.zip",
            "llama-b10665-bin-ubuntu-rocm-7.14-x64.tar.gz",
            "llama-b10665-ui.tar.gz",
            "llama-b10665-bin-ubuntu-x64.zip",
        ] {
            assert!(classify_asset("b10665", name).is_none(), "{name}");
        }
        assert_eq!(
            parse_github_timestamp("2026-08-28T12:34:56Z"),
            Some(1_787_920_496)
        );
    }

    #[test]
    fn cuda_runtime_requires_uploaded_same_release_companion() {
        let mut release = release_with_assets(vec![
            asset("llama-b10665-bin-win-cuda-13.3-x64.zip", "uploaded"),
            asset("cudart-llama-bin-win-cuda-13.3-x64.zip", "open"),
        ]);
        assert!(runtimes_for_release(&release).unwrap().is_empty());

        release.assets[1].state = "uploaded".to_owned();
        release.assets[1].digest = None;
        assert!(runtimes_for_release(&release).unwrap().is_empty());

        release.assets[1].digest = Some(format!("sha256:{}", "a".repeat(64)));
        let runtimes = runtimes_for_release(&release).unwrap();
        assert_eq!(runtimes.len(), 1);
        let runtime = &runtimes[0];
        let (download, additional_downloads) =
            runtime.release_assets().expect("release acquisition");
        assert_eq!(runtime.identity.accelerator, "cuda");
        assert_eq!(runtime.identity.variant, "13.3");
        assert_eq!(additional_downloads.len(), 1);
        assert_eq!(runtime.identity.package.additional_assets.len(), 1);
        assert_eq!(
            runtime.identity.package.additional_assets[0].role,
            "cuda-runtime"
        );
        assert_eq!(download.entrypoint_names, ["llama-server.exe"]);
        assert!(additional_downloads[0].entrypoint_names.is_empty());
        assert!(additional_downloads[0].digest.is_some());
    }

    #[test]
    fn invalid_matching_asset_does_not_hide_other_release_variants() {
        let valid = asset("llama-b10665-bin-win-cpu-x64.zip", "uploaded");
        let mut zero_byte = asset("llama-b10665-bin-win-vulkan-x64.zip", "uploaded");
        zero_byte.size = 0;
        let runtimes = runtimes_for_release(&release_with_assets(vec![valid, zero_byte]))
            .expect("provider skips an invalid individual asset");
        assert_eq!(runtimes.len(), 1);
        assert_eq!(runtimes[0].identity.accelerator, "cpu");
    }

    #[test]
    fn latest_channel_tracks_the_newest_actual_asset_per_variant() {
        let old = release_with_tag_and_assets(
            "b10664",
            vec![
                asset("llama-b10664-bin-win-cpu-x64.zip", "uploaded"),
                asset("llama-b10664-bin-win-vulkan-x64.zip", "uploaded"),
            ],
        );
        let current = release_with_tag_and_assets(
            "b10665",
            vec![asset("llama-b10665-bin-win-cpu-x64.zip", "uploaded")],
        );
        let mut runtimes = [
            runtimes_for_release(&old).unwrap(),
            runtimes_for_release(&current).unwrap(),
        ]
        .concat();
        assign_channels(&mut runtimes, Some("b10664"));

        let old_cpu = runtimes
            .iter()
            .find(|runtime| {
                runtime.identity.version == "b10664" && runtime.identity.accelerator == "cpu"
            })
            .unwrap();
        assert!(old_cpu.channels.contains(&RuntimeReleaseChannel::Stable));
        assert!(!old_cpu.channels.contains(&RuntimeReleaseChannel::Latest));
        let old_vulkan = runtimes
            .iter()
            .find(|runtime| {
                runtime.identity.version == "b10664" && runtime.identity.accelerator == "vulkan"
            })
            .unwrap();
        assert!(old_vulkan.channels.contains(&RuntimeReleaseChannel::Latest));
        let current_cpu = runtimes
            .iter()
            .find(|runtime| {
                runtime.identity.version == "b10665" && runtime.identity.accelerator == "cpu"
            })
            .unwrap();
        assert!(
            current_cpu
                .channels
                .contains(&RuntimeReleaseChannel::Latest)
        );
    }

    fn release_with_assets(assets: Vec<GitHubReleaseAsset>) -> GitHubRelease {
        release_with_tag_and_assets("b10665", assets)
    }

    fn release_with_tag_and_assets(tag: &str, assets: Vec<GitHubReleaseAsset>) -> GitHubRelease {
        GitHubRelease {
            id: 1,
            tag_name: tag.to_owned(),
            name: None,
            html_url: format!("https://github.com/ggml-org/llama.cpp/releases/tag/{tag}"),
            target_commitish: "0123456789abcdef0123456789abcdef01234567".to_owned(),
            draft: false,
            prerelease: true,
            published_at: Some("2026-08-28T12:34:56Z".to_owned()),
            assets,
        }
    }

    fn asset(name: &str, state: &str) -> GitHubReleaseAsset {
        GitHubReleaseAsset {
            id: 1,
            name: name.to_owned(),
            size: 42,
            browser_download_url: format!(
                "https://github.com/ggml-org/llama.cpp/releases/download/b10665/{name}"
            ),
            digest: Some(format!("sha256:{}", "a".repeat(64))),
            state: state.to_owned(),
        }
    }
}
