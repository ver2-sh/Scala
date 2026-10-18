//! Software preparation shared by explicit managed Python installations.
use super::*;
use crate::{ManagedSourceDependencies, SourceDependencyAsset};

fn msvc_assets() -> ManagedSourceDependencies {
    ManagedSourceDependencies {
        entrypoint: "prepare-msvc.ps1",
        assets: vec![
            SourceDependencyAsset {
                name: "prepare-msvc.ps1",
                bytes: include_bytes!("../../runtime/native/prepare-msvc.ps1"),
            },
            SourceDependencyAsset {
                name: "windows-tools.ps1",
                bytes: include_bytes!("../../runtime/native/windows-tools.ps1"),
            },
            SourceDependencyAsset {
                name: "msvc.lock.json",
                bytes: include_bytes!("../../runtime/native/msvc.lock.json"),
            },
            SourceDependencyAsset {
                name: "vs-channel.json",
                bytes: include_bytes!("../../runtime/native/vs-channel.json"),
            },
        ],
    }
}
impl RuntimeInstaller {
    pub(super) async fn prepare_python_msvc(
        &self,
        available: &AvailableRuntime,
        staging: &Path,
    ) -> Result<(), RuntimeInstallError> {
        let assets = msvc_assets();
        assets
            .validate()
            .map_err(RuntimeInstallError::InvalidMetadata)?;
        let digest = assets.sha256();
        let native = self.cache_root.join("native");
        std::fs::create_dir_all(&native).map_err(|e| RuntimeInstallError::Task(e.to_string()))?;
        reject_cache_aliases(&self.cache_root, &native)?;
        let lock = native.join("acquisition.lock");
        if lock.exists() {
            managed_source::reject_managed_cache_file(&native, &lock)?;
        }
        let lease = Arc::new(
            OpenOptions::new()
                .create(true)
                .truncate(false)
                .read(true)
                .write(true)
                .open(&lock)
                .map_err(|e| RuntimeInstallError::Task(e.to_string()))?,
        );
        fs2::FileExt::try_lock_exclusive(lease.as_ref()).map_err(|_| {
            RuntimeInstallError::Prerequisite("native dependency acquisition is active".into())
        })?;
        let root = native.join(&digest);
        std::fs::create_dir_all(&root).map_err(|e| RuntimeInstallError::Task(e.to_string()))?;
        reject_cache_aliases(&native, &root)?;
        let owner = root.join(".scala-cache-owner");
        if owner.exists() {
            managed_source::reject_managed_cache_file(&native, &owner)?;
            if std::fs::read_to_string(&owner).ok().as_deref() != Some(&digest) {
                return Err(RuntimeInstallError::InvalidMetadata(
                    "compiler cache ownership changed".into(),
                ));
            }
        } else {
            if std::fs::read_dir(&root)
                .map_err(|e| RuntimeInstallError::Task(e.to_string()))?
                .next()
                .is_some()
            {
                return Err(RuntimeInstallError::InvalidMetadata(
                    "unowned compiler cache".into(),
                ));
            }
            std::fs::write(&owner, &digest)
                .map_err(|e| RuntimeInstallError::Task(e.to_string()))?;
        }
        let work = tempfile::Builder::new()
            .prefix("provision-")
            .tempdir_in(&root)
            .map_err(|e| RuntimeInstallError::Task(e.to_string()))?;
        for asset in &assets.assets {
            std::fs::write(work.path().join(asset.name), asset.bytes)
                .map_err(|e| RuntimeInstallError::Task(e.to_string()))?;
        }
        let mut environment = managed_source::base_environment();
        let system =
            PathBuf::from(std::env::var_os("SystemRoot").ok_or_else(|| {
                RuntimeInstallError::Prerequisite("SystemRoot unavailable".into())
            })?);
        environment.insert(
            "PATH".into(),
            format!("{};{}", system.join("System32").display(), system.display()),
        );
        environment.insert(
            "ComSpec".into(),
            system.join("System32/cmd.exe").display().to_string(),
        );
        environment.insert("PATHEXT".into(), ".COM;.EXE;.BAT;.CMD".into());
        let powershell = system.join("System32/WindowsPowerShell/v1.0/powershell.exe");
        let result = work.path().join("result.json");
        let arguments = [
            "-NoProfile".to_owned(),
            "-NonInteractive".into(),
            "-ExecutionPolicy".into(),
            "Bypass".into(),
            "-File".into(),
            work.path().join(assets.entrypoint).display().to_string(),
            "-Root".into(),
            root.display().to_string(),
            "-ResultPath".into(),
            result.display().to_string(),
            "-BundleSha256".into(),
            digest.clone(),
        ];
        let this = self.clone();
        let candidate = available.clone();
        self.emit(
            available,
            RuntimeOperationPhase::Installing,
            None,
            None,
            "Preparing or reusing Microsoft C++ tools; Windows approval may be required",
        );
        let output = crate::supervisor::capture_install_command(
            &powershell,
            &arguments.iter().map(String::as_str).collect::<Vec<_>>(),
            &environment,
            Arc::new(move |line| {
                this.emit(
                    &candidate,
                    RuntimeOperationPhase::Installing,
                    None,
                    None,
                    line.trim(),
                )
            }),
            Some(lease.as_ref()),
        )
        .await?;
        if !output.success {
            return Err(RuntimeInstallError::Prerequisite(format!(
                "Microsoft compiler provisioning failed: {}",
                compact_identity(&format!("{}\n{}", output.stdout, output.stderr))
            )));
        }
        managed_source::reject_managed_cache_file(&root, &result)?;
        if std::fs::metadata(&result)
            .map_err(|e| RuntimeInstallError::Task(e.to_string()))?
            .len()
            > 65536
        {
            return Err(RuntimeInstallError::InvalidMetadata(
                "compiler receipt too large".into(),
            ));
        }
        let bytes = std::fs::read(&result).map_err(|e| RuntimeInstallError::Task(e.to_string()))?;
        let report: serde_json::Value = serde_json::from_slice(&bytes)
            .map_err(|e| RuntimeInstallError::InvalidMetadata(e.to_string()))?;
        if report["schema_version"] != 1
            || report["bundle_sha256"].as_str() != Some(&digest)
            || report["compiler"]
                .as_str()
                .is_none_or(|p| !Path::new(p).is_absolute() || !Path::new(p).is_file())
        {
            return Err(RuntimeInstallError::InvalidMetadata(
                "Microsoft compiler receipt failed verification".into(),
            ));
        }
        let evidence = staging.join("source-dependencies");
        std::fs::create_dir(&evidence).map_err(|e| RuntimeInstallError::Task(e.to_string()))?;
        std::fs::write(evidence.join("msvc.json"), bytes)
            .map_err(|e| RuntimeInstallError::Task(e.to_string()))?;
        for asset in &assets.assets {
            std::fs::write(evidence.join(asset.name), asset.bytes)
                .map_err(|e| RuntimeInstallError::Task(e.to_string()))?;
        }
        Ok(())
    }
}
