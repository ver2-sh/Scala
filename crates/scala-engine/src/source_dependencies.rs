//! Adapter-owned dependency recipes for managed native source builds.
//! Discovery is read-only. These assets execute only after explicit runtime installation.
use sha2::{Digest, Sha256};

#[derive(Clone, Copy)]
pub struct SourceDependencyAsset {
    pub name: &'static str,
    pub bytes: &'static [u8],
}

#[derive(Clone)]
pub struct ManagedSourceDependencies {
    pub entrypoint: &'static str,
    pub assets: Vec<SourceDependencyAsset>,
}

impl ManagedSourceDependencies {
    /// Covers scripts, package locks and manifests. Changed recipes get a new runtime identity.
    pub fn sha256(&self) -> String {
        let mut hash = Sha256::new();
        hash.update(b"scala-managed-source-dependencies-v1\0");
        hash.update(self.entrypoint.as_bytes());
        for asset in &self.assets {
            hash.update((asset.name.len() as u64).to_le_bytes());
            hash.update(asset.name.as_bytes());
            hash.update((asset.bytes.len() as u64).to_le_bytes());
            hash.update(asset.bytes);
        }
        format!("{:x}", hash.finalize())
    }

    /// Reuse the same toolchain when only a runtime revision or provisioning script
    /// changes. Pinned dependency locks, manifests and configuration still isolate
    /// incompatible environments. Every installation retains the full recipe hash.
    pub fn cache_sha256(&self) -> String {
        let mut hash = Sha256::new();
        hash.update(b"scala-native-dependency-environment-v1\0");
        for asset in self.assets.iter().filter(|a| !a.name.ends_with(".ps1")) {
            hash.update((asset.name.len() as u64).to_le_bytes());
            hash.update(asset.name.as_bytes());
            hash.update((asset.bytes.len() as u64).to_le_bytes());
            hash.update(asset.bytes);
        }
        format!("{:x}", hash.finalize())
    }

    pub fn validate(&self) -> Result<(), String> {
        let mut names = std::collections::BTreeSet::new();
        if self.assets.is_empty() || self.assets.len() > 16 {
            return Err("native dependency recipe has an invalid asset inventory".into());
        }
        for asset in &self.assets {
            if !scala_core::is_safe_relative_path(std::path::Path::new(asset.name))
                || !names.insert(asset.name)
                || asset.bytes.is_empty()
                || asset.bytes.len() > 2 * 1024 * 1024
            {
                return Err(
                    "native dependency recipe has unsafe, duplicate or oversized assets".into(),
                );
            }
        }
        if !names.contains(self.entrypoint) || !self.entrypoint.ends_with(".ps1") {
            return Err("native dependency recipe has no admitted PowerShell entrypoint".into());
        }
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn recipe() -> ManagedSourceDependencies {
        ManagedSourceDependencies {
            entrypoint: "prepare.ps1",
            assets: vec![
                SourceDependencyAsset {
                    name: "prepare.ps1",
                    bytes: b"# fixture",
                },
                SourceDependencyAsset {
                    name: "lock.json",
                    bytes: b"{}",
                },
            ],
        }
    }
    #[test]
    fn dependency_identity_covers_every_asset_and_is_deterministic() {
        let original = recipe();
        assert!(original.validate().is_ok());
        assert_eq!(original.sha256(), recipe().sha256());
        for index in 0..original.assets.len() {
            let mut changed = original.clone();
            changed.assets[index].bytes = b"changed";
            assert_ne!(original.sha256(), changed.sha256());
        }
    }
    #[test]
    fn cache_reuses_pinned_dependencies_across_script_and_runtime_revisions() {
        let original = recipe();
        let mut changed = original.clone();
        changed.assets[0].bytes = b"# revised provisioner";
        assert_ne!(original.sha256(), changed.sha256());
        assert_eq!(original.cache_sha256(), changed.cache_sha256());
        changed.assets[1].bytes = b"{\"toolchain\":\"different\"}";
        assert_ne!(original.cache_sha256(), changed.cache_sha256());
    }
    #[test]
    fn dependency_resources_reject_escape_duplicates_and_missing_script() {
        let mut bad = recipe();
        bad.assets[1].name = "../escape";
        assert!(bad.validate().is_err());
        bad = recipe();
        bad.assets[1].name = "prepare.ps1";
        assert!(bad.validate().is_err());
        bad = recipe();
        bad.entrypoint = "absent.ps1";
        assert!(bad.validate().is_err());
    }
}
