use async_trait::async_trait;
use scala_core::AvailableRuntime;
use scala_engine::python_runtime::{ManagedPythonAsset, common_python_assets, python_candidates};
use scala_engine::{CatalogError, GitHubReleaseClient, RuntimeCatalogProvider};

pub(crate) const PROVIDER: &str = "pytorch-reviewed-python";
pub(crate) fn assets() -> Vec<ManagedPythonAsset> {
    let mut assets = common_python_assets();
    assets.extend([
        ManagedPythonAsset {
            name: "crates/scala-engine-pytorch/runtime/prepare.py",
            bytes: include_bytes!("../runtime/prepare.py"),
        },
        ManagedPythonAsset {
            name: "crates/scala-engine-pytorch/runtime/server.py",
            bytes: include_bytes!("../runtime/server.py"),
        },
        ManagedPythonAsset {
            name: "crates/scala-engine-pytorch/runtime/windows/imajev-requirements.lock",
            bytes: include_bytes!("../runtime/windows/imajev-requirements.lock"),
        },
        ManagedPythonAsset {
            name: "crates/scala-engine-pytorch/runtime/linux/imajev-requirements.lock",
            bytes: include_bytes!("../runtime/linux/imajev-requirements.lock"),
        },
    ]);
    assets
}

#[derive(Default)]
pub struct PyTorchRuntimeCatalogProvider;
impl PyTorchRuntimeCatalogProvider {
    pub fn new() -> Self {
        Self
    }
    pub fn candidates(&self) -> Vec<AvailableRuntime> {
        python_candidates("pytorch", PROVIDER, "pytorch/pytorch", &assets())
    }
}
#[async_trait]
impl RuntimeCatalogProvider for PyTorchRuntimeCatalogProvider {
    fn id(&self) -> &'static str {
        PROVIDER
    }
    fn engine_id(&self) -> &'static str {
        "pytorch"
    }
    fn repository(&self) -> &'static str {
        "pytorch/pytorch"
    }
    async fn fetch(&self, _: &GitHubReleaseClient) -> Result<Vec<AvailableRuntime>, CatalogError> {
        Ok(self.candidates())
    }
    async fn fetch_reference(
        &self,
        _: &GitHubReleaseClient,
        reference: &str,
    ) -> Result<Vec<AvailableRuntime>, CatalogError> {
        Ok(self
            .candidates()
            .into_iter()
            .filter(|candidate| candidate.runtime_id.as_str() == reference)
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use scala_core::{
        AcceleratorDevice, ArtifactFormat, ComputeCapability, HostCapabilities,
        RuntimeAcquisitionPlan, RuntimeCompatibility,
    };
    #[test]
    fn reviewed_catalog_is_bound_to_the_original_readout_and_native_platform_tuples() {
        let candidates = PyTorchRuntimeCatalogProvider::new().candidates();
        assert_eq!(candidates.len(), 2);
        for candidate in candidates {
            candidate.validate().unwrap();
            if candidate.identity.platform == "windows" {
                assert_eq!(
                    candidate.requirements.minimum_nvidia_driver.as_deref(),
                    Some("610.62")
                );
            }
            let RuntimeAcquisitionPlan::ManagedPython(plan) = &candidate.acquisition else {
                panic!("Python environment must have its own provenance")
            };
            assert_eq!(
                candidate.supported_formats,
                vec![ArtifactFormat::DecisionBundle]
            );
            assert_eq!(plan.backend, "torch-readout");
            assert_eq!(
                plan.source_revision.as_deref(),
                Some("91729bff7a806187324e287fd0d730839dc32026")
            );
            assert!(plan.source.as_ref().unwrap().digest.is_some());
            assert_eq!(plan.dependencies["peft"], "0.21.2");
            assert_eq!(plan.dependencies["transformers"], "5.19.0");
            assert_eq!(
                plan.dependencies["torch"],
                if candidate.identity.platform == "linux" {
                    "2.14.1"
                } else {
                    "2.14.1+cu130"
                }
            );
            assert_eq!(
                plan.dependencies["torchvision"],
                if candidate.identity.platform == "linux" {
                    "0.29.1"
                } else {
                    "0.29.1+cu130"
                }
            );
            let mut host = HostCapabilities::current_without_accelerator_probe();
            host.platform = candidate.identity.platform.clone();
            host.accelerators = vec![AcceleratorDevice {
                accelerator: "cuda".into(),
                stable_id: None,
                name: Some("unsupported GPU".into()),
                vram_bytes: Some(32 << 30),
                driver_version: Some("999.0".into()),
                compute_capability: Some(ComputeCapability::new(7, 0)),
            }];
            assert!(matches!(
                scala_engine::compatibility_for(
                    &candidate.identity.platform,
                    &candidate.identity.architecture,
                    &candidate.identity.accelerator,
                    &candidate.requirements,
                    &host
                ),
                RuntimeCompatibility::Incompatible(_)
            ));
        }
    }
}
