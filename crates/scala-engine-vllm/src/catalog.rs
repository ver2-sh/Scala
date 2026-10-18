use async_trait::async_trait;
use scala_core::AvailableRuntime;
use scala_engine::python_runtime::{ManagedPythonAsset, common_python_assets, python_candidates};
use scala_engine::{CatalogError, GitHubReleaseClient, RuntimeCatalogProvider};

pub(crate) const PROVIDER: &str = "vllm-reviewed-python";
pub(crate) fn assets() -> Vec<ManagedPythonAsset> {
    let mut assets = common_python_assets();
    assets.extend([
        ManagedPythonAsset {
            name: "crates/scala-engine/runtime/native/linux/cpp.lock.json",
            bytes: include_bytes!("../../scala-engine/runtime/native/linux/cpp.lock.json"),
        },
        ManagedPythonAsset {
            name: "crates/scala-engine/runtime/native/msvc.lock.json",
            bytes: include_bytes!("../../scala-engine/runtime/native/msvc.lock.json"),
        },
        ManagedPythonAsset {
            name: "crates/scala-engine/runtime/native/windows-tools.ps1",
            bytes: include_bytes!("../../scala-engine/runtime/native/windows-tools.ps1"),
        },
        ManagedPythonAsset {
            name: "crates/scala-engine/runtime/native/prepare-msvc.ps1",
            bytes: include_bytes!("../../scala-engine/runtime/native/prepare-msvc.ps1"),
        },
        ManagedPythonAsset {
            name: "crates/scala-engine/runtime/native/vs-channel.json",
            bytes: include_bytes!("../../scala-engine/runtime/native/vs-channel.json"),
        },
    ]);
    assets.extend([
        ManagedPythonAsset {
            name: "crates/scala-engine-vllm/runtime/prepare.py",
            bytes: include_bytes!("../runtime/prepare.py"),
        },
        ManagedPythonAsset {
            name: "crates/scala-engine-vllm/runtime/server.py",
            bytes: include_bytes!("../runtime/server.py"),
        },
        ManagedPythonAsset {
            name: "crates/scala-engine-vllm/runtime/windows_vllm.py",
            bytes: include_bytes!("../runtime/windows_vllm.py"),
        },
        ManagedPythonAsset {
            name: "crates/scala-engine-vllm/runtime/windows/provision_h2o.py",
            bytes: include_bytes!("../runtime/windows/provision_h2o.py"),
        },
        ManagedPythonAsset {
            name: "crates/scala-engine-vllm/runtime/windows/h2o-requirements.lock",
            bytes: include_bytes!("../runtime/windows/h2o-requirements.lock"),
        },
        ManagedPythonAsset {
            name: "crates/scala-engine-vllm/runtime/windows/h2o-packaging.lock",
            bytes: include_bytes!("../runtime/windows/h2o-packaging.lock"),
        },
        ManagedPythonAsset {
            name: "crates/scala-engine-vllm/runtime/linux/h2o-requirements.lock",
            bytes: include_bytes!("../runtime/linux/h2o-requirements.lock"),
        },
        ManagedPythonAsset {
            name: "crates/scala-engine-vllm/runtime/fixtures/h2o/h2o_lightning_shim.py",
            bytes: include_bytes!("../runtime/fixtures/h2o/h2o_lightning_shim.py"),
        },
        ManagedPythonAsset {
            name: "crates/scala-engine-vllm/runtime/fixtures/h2o/config.json",
            bytes: include_bytes!("../runtime/fixtures/h2o/config.json"),
        },
        ManagedPythonAsset {
            name: "crates/scala-engine-vllm/runtime/fixtures/h2o/serve_config.json",
            bytes: include_bytes!("../runtime/fixtures/h2o/serve_config.json"),
        },
        ManagedPythonAsset {
            name: "crates/scala-engine-vllm/runtime/fixtures/h2o/LICENSE",
            bytes: include_bytes!("../runtime/fixtures/h2o/LICENSE"),
        },
        ManagedPythonAsset {
            name: "crates/scala-engine-vllm/runtime/fixtures/h2o/VLLM-LICENSE",
            bytes: include_bytes!("../runtime/fixtures/h2o/VLLM-LICENSE"),
        },
        ManagedPythonAsset {
            name: "crates/scala-engine-vllm/runtime/fixtures/h2o/source-review.json",
            bytes: include_bytes!("../runtime/fixtures/h2o/source-review.json"),
        },
    ]);
    assets
}

#[derive(Default)]
pub struct VllmRuntimeCatalogProvider;
impl VllmRuntimeCatalogProvider {
    pub fn new() -> Self {
        Self
    }
    pub fn candidates(&self) -> Vec<AvailableRuntime> {
        python_candidates("vllm", PROVIDER, "vllm-project/vllm", &assets())
    }
}
#[async_trait]
impl RuntimeCatalogProvider for VllmRuntimeCatalogProvider {
    fn id(&self) -> &'static str {
        PROVIDER
    }
    fn engine_id(&self) -> &'static str {
        "vllm"
    }
    fn repository(&self) -> &'static str {
        "vllm-project/vllm"
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
        ArtifactFormat, HostCapabilities, RuntimeAcquisitionPlan, RuntimeCompatibility,
    };
    #[test]
    fn reviewed_catalog_has_only_exact_platform_runner_and_dependency_contracts() {
        let candidates = VllmRuntimeCatalogProvider::new().candidates();
        assert_eq!(candidates.len(), 4);
        for candidate in &candidates {
            candidate.validate().unwrap();
            scala_engine::RuntimeProviderAuthority {
                provider_id: PROVIDER.into(),
                engine_id: "vllm".into(),
                repository: "vllm-project/vllm".into(),
            }
            .validate(candidate)
            .unwrap();
            let RuntimeAcquisitionPlan::ManagedPython(plan) = &candidate.acquisition else {
                panic!("Python environments are not release assets")
            };
            assert_eq!(candidate.identity.architecture, "x86_64");
            assert_eq!(candidate.identity.package.release_tag, None);
            assert_eq!(
                candidate.supported_formats,
                vec![if plan.backend == "vllm-openai" {
                    ArtifactFormat::HuggingFace
                } else {
                    ArtifactFormat::DecisionBundle
                }]
            );
            assert_eq!(plan.dependencies["transformers"], "5.19.0");
            if candidate.identity.platform == "linux" {
                assert_eq!(plan.dependencies["vllm"], "0.30.0");
                assert_eq!(plan.dependencies["torch"], "2.13.0");
                assert_eq!(plan.dependencies["torchvision"], "0.28.0");
                assert_eq!(plan.minimum_glibc.as_deref(), Some("2.31"));
            } else {
                assert_eq!(plan.dependencies["vllm"], "0.29.0+cu132");
                assert_eq!(plan.dependencies["torch"], "2.11.0+cu130");
                assert_eq!(plan.dependencies["triton-windows"], "3.6.0.post26");
                assert_eq!(plan.dependencies["flashinfer-python"], "0.6.11.post3");
            }
            let mut host = HostCapabilities::current_without_accelerator_probe();
            host.platform = "macos".into();
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
        assert!(
            assets()
                .iter()
                .any(|a| a.name.ends_with("h2o_lightning_shim.py"))
        );
        assert!(assets().iter().any(|a| a.name.ends_with("LICENSE")));
    }
}
