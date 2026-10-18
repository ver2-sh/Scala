//! Native Windows build environment owned by this NInfer-all recipe, not the host.
use scala_engine::{ManagedSourceDependencies, SourceDependencyAsset};
pub(super) fn assets() -> ManagedSourceDependencies {
    ManagedSourceDependencies {
        entrypoint: "prepare-windows.ps1",
        assets: vec![
            SourceDependencyAsset {
                name: "windows-tools.ps1",
                bytes: include_bytes!("../../scala-engine/runtime/native/windows-tools.ps1"),
            },
            SourceDependencyAsset {
                name: "prepare-windows.ps1",
                bytes: include_bytes!("source_dependencies/prepare-windows.ps1"),
            },
            SourceDependencyAsset {
                name: "windows.lock.json",
                bytes: include_bytes!("source_dependencies/windows.lock.json"),
            },
            SourceDependencyAsset {
                name: "vcpkg.json",
                bytes: include_bytes!("source_dependencies/vcpkg.json"),
            },
            SourceDependencyAsset {
                name: "vs-channel.json",
                bytes: include_bytes!("../../scala-engine/runtime/native/vs-channel.json"),
            },
        ],
    }
}
pub(super) fn recipe_version() -> String {
    format!("ninfer-all-serve-v3-r2-{}", &assets().sha256()[..16])
}
// Previously published installer recipe; source/serving evidence remains unchanged.
pub(super) const PRE_SHARED_RECIPE: &str = "ninfer-all-serve-v3-r2-39442d439048aa87";

pub(super) fn for_plan(
    plan: &scala_core::RuntimeSourceBuildPlan,
) -> Option<ManagedSourceDependencies> {
    (plan.source.repository == super::all_catalog::REPOSITORY
        && plan.source.source_provider == super::all_catalog::PROVIDER_ID
        && plan.source.commit_sha == super::all_catalog::REVISION
        && plan.source.tree_sha == super::all_catalog::TREE
        && plan.recipe.recipe_version == recipe_version()
        && plan.recipe.build_target == "ninfer-serve"
        && plan.recipe.accelerator_target == "sm_120a"
        && plan.recipe.entrypoint == std::path::Path::new("build/apps/ninfer-serve.exe"))
    .then(assets)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_dependency_lock_is_complete_pinned_and_does_not_install_a_driver() {
        let bundle = assets();
        bundle.validate().unwrap();
        let lock: serde_json::Value =
            serde_json::from_slice(include_bytes!("source_dependencies/windows.lock.json"))
                .unwrap();
        let packages = lock["assets"].as_array().unwrap();
        for required in [
            "git",
            "cmake",
            "ninja",
            "vcpkg-tool",
            "cuda_nvcc",
            "cuda_cudart",
            "cuda_crt",
            "cccl",
            "libnvvm",
            "cuda_nvrtc",
            "cuda_nvml_dev",
            "libcublas",
            "vs-setup",
            "vs-manifest",
        ] {
            assert!(packages.iter().any(|p| p["id"] == required), "{required}");
        }
        for package in packages {
            assert_eq!(package["sha256"].as_str().unwrap().len(), 64);
            assert!(package["size"].as_u64().unwrap() > 0);
            assert!(package["url"].as_str().unwrap().starts_with("https://"));
            assert!(!package["id"].as_str().unwrap().contains("driver"));
        }
        use sha2::{Digest, Sha256};
        assert_eq!(
            format!(
                "{:x}",
                Sha256::digest(include_bytes!(
                    "../../scala-engine/runtime/native/vs-channel.json"
                ))
            ),
            lock["vs_channel_sha256"]
        );
        let manifest: serde_json::Value =
            serde_json::from_slice(include_bytes!("source_dependencies/vcpkg.json")).unwrap();
        assert_eq!(manifest["builtin-baseline"], lock["vcpkg_commit"]);
        assert_eq!(
            recipe_version(),
            format!("ninfer-all-serve-v3-r2-{}", &bundle.sha256()[..16])
        );
    }
}
