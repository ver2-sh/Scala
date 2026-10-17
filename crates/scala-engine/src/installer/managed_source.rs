//! Explicit-install-only dependency provisioning for native source recipes.
use super::*;
use crate::ManagedSourceDependencies;
use serde::Deserialize;
#[derive(Deserialize)]
struct DependencyReport {
    schema_version: u32,
    bundle_sha256: String,
    environment: BTreeMap<String, String>,
    tools: BTreeMap<String, PathBuf>,
    cuda_root: PathBuf,
    dependency_prefix: PathBuf,
    runtime_files: Vec<PathBuf>,
    toolchain: RuntimeSourceBuildToolchain,
}
pub(super) struct Prepared {
    report: DependencyReport,
    environment: BTreeMap<String, String>,
    assets: ManagedSourceDependencies,
    _work: tempfile::TempDir,
    lease: Arc<std::fs::File>,
    progress: Arc<dyn Fn(&str) + Send + Sync>,
}
impl RuntimeInstaller {
    pub(super) fn managed_source_recipe(
        &self,
        plan: &RuntimeSourceBuildPlan,
    ) -> Option<ManagedSourceDependencies> {
        let authority = self.authorities.get(&plan.source.source_provider)?;
        self.registry
            .get(&authority.engine_id)?
            .managed_source_dependencies(plan)
    }
    pub(super) async fn prepare_managed_source(
        &self,
        available: &AvailableRuntime,
        plan: &RuntimeSourceBuildPlan,
    ) -> Result<Option<Prepared>, RuntimeInstallError> {
        let Some(assets) = self.managed_source_recipe(plan) else {
            return Ok(None);
        };
        assets
            .validate()
            .map_err(RuntimeInstallError::InvalidMetadata)?;
        if !cfg!(target_os = "windows") || std::env::consts::ARCH != "x86_64" {
            return Err(RuntimeInstallError::Prerequisite(
                "this native dependency recipe requires Windows x86_64".into(),
            ));
        }
        let digest = assets.sha256();
        let root = self.cache_root.join("native").join(&digest);
        std::fs::create_dir_all(&root).map_err(|e| RuntimeInstallError::Task(e.to_string()))?;
        reject_cache_aliases(&self.cache_root, &root)?;
        let owner = root.join(".scala-cache-owner");
        if owner.exists() {
            reject_managed_cache_file(&root, &owner)?;
            if std::fs::read_to_string(&owner).ok().as_deref() != Some(&digest) {
                return Err(RuntimeInstallError::InvalidMetadata(
                    "native dependency cache ownership changed".into(),
                ));
            }
        } else if std::fs::read_dir(&root)
            .map_err(|e| RuntimeInstallError::Task(e.to_string()))?
            .next()
            .is_some()
        {
            return Err(RuntimeInstallError::InvalidMetadata(
                "refusing an unowned native dependency cache".into(),
            ));
        } else {
            std::fs::write(&owner, &digest)
                .map_err(|e| RuntimeInstallError::Task(e.to_string()))?;
        }
        let lock = root.join("acquisition.lock");
        if lock.exists() {
            reject_managed_cache_file(&root, &lock)?;
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
            RuntimeInstallError::Prerequisite(
                "another runtime install is preparing this native dependency environment".into(),
            )
        })?;
        let work = tempfile::Builder::new()
            .prefix("provision-")
            .tempdir_in(&root)
            .map_err(|e| RuntimeInstallError::Task(e.to_string()))?;
        for asset in &assets.assets {
            let path = work.path().join(asset.name);
            std::fs::create_dir_all(path.parent().expect("recipe resource parent"))
                .map_err(|e| RuntimeInstallError::Task(e.to_string()))?;
            std::fs::write(path, asset.bytes)
                .map_err(|e| RuntimeInstallError::Task(e.to_string()))?;
        }
        let result = work.path().join("result.json");
        let system = std::env::var_os("SystemRoot")
            .map(PathBuf::from)
            .ok_or_else(|| {
                RuntimeInstallError::Prerequisite("Windows SystemRoot is unavailable".into())
            })?;
        let powershell = system.join("System32/WindowsPowerShell/v1.0/powershell.exe");
        let mut environment = base_environment();
        // PowerShell must recognize native .exe tools from process startup, even
        // when the inherited host environment has been stripped intentionally.
        environment.insert("PATHEXT".into(), ".COM;.EXE;.BAT;.CMD".into());
        environment.insert(
            "PATH".into(),
            format!("{};{}", system.join("System32").display(), system.display()),
        );
        environment.insert(
            "ComSpec".into(),
            system.join("System32/cmd.exe").display().to_string(),
        );
        let this = self.clone();
        let candidate = available.clone();
        let progress: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(move |detail| {
            this.emit(
                &candidate,
                RuntimeOperationPhase::Installing,
                None,
                None,
                detail.trim(),
            );
        });
        self.emit(available, RuntimeOperationPhase::Installing, None, None,"Preparing all native build dependencies; Windows may request permission for Microsoft build tools");
        let arguments = vec![
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
        let output = crate::supervisor::capture_install_command(
            &powershell,
            &arguments.iter().map(String::as_str).collect::<Vec<_>>(),
            &environment,
            Arc::clone(&progress),
            Some(lease.as_ref()),
        )
        .await?;
        if !output.success {
            return Err(RuntimeInstallError::Prerequisite(format!(
                "native dependency setup failed ({:?}): {}",
                output.code,
                compact_identity(if output.stderr.trim().is_empty() {
                    &output.stdout
                } else {
                    &output.stderr
                })
            )));
        }
        if std::fs::metadata(&result)
            .map_err(|e| RuntimeInstallError::Task(e.to_string()))?
            .len()
            > 1024 * 1024
        {
            return Err(RuntimeInstallError::InvalidMetadata(
                "native dependency receipt is too large".into(),
            ));
        }
        let report: DependencyReport = serde_json::from_slice(
            &std::fs::read(&result).map_err(|e| RuntimeInstallError::Task(e.to_string()))?,
        )
        .map_err(|e| {
            RuntimeInstallError::InvalidMetadata(format!("invalid native dependency receipt: {e}"))
        })?;
        validate_report(&report, &digest, &root)?;
        environment.extend(report.environment.clone());
        Ok(Some(Prepared {
            report,
            environment,
            assets,
            _work: work,
            lease,
            progress,
        }))
    }
}
fn reject_managed_cache_file(root: &Path, file: &Path) -> Result<(), RuntimeInstallError> {
    let parent = file.parent().ok_or_else(|| {
        RuntimeInstallError::InvalidMetadata("managed cache file has no parent".into())
    })?;
    reject_cache_aliases(root, parent)?;
    let metadata = std::fs::symlink_metadata(file)
        .map_err(|e| RuntimeInstallError::InvalidMetadata(e.to_string()))?;
    #[cfg(windows)]
    let alias = {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    };
    #[cfg(not(windows))]
    let alias = metadata.file_type().is_symlink();
    if alias || !metadata.is_file() {
        return Err(RuntimeInstallError::InvalidMetadata(
            "managed cache control file is an alias or non-file".into(),
        ));
    }
    Ok(())
}
fn base_environment() -> BTreeMap<String, String> {
    [
        "SystemRoot",
        "WINDIR",
        "SystemDrive",
        "TEMP",
        "TMP",
        "USERPROFILE",
        "LOCALAPPDATA",
        "APPDATA",
        "ProgramData",
        "ProgramFiles",
        "ProgramFiles(x86)",
        "ProgramW6432",
        "PROCESSOR_ARCHITECTURE",
        "NUMBER_OF_PROCESSORS",
        "OS",
        "COMPUTERNAME",
    ]
    .into_iter()
    .filter_map(|name| {
        std::env::var(name)
            .ok()
            .map(|value| (name.to_owned(), value))
    })
    .collect()
}
fn admitted_environment(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "path"
            | "include"
            | "lib"
            | "libpath"
            | "vsinstalldir"
            | "vcinstalldir"
            | "vctoolsinstalldir"
            | "vctoolsredistdir"
            | "vctoolsversion"
            | "windowssdkdir"
            | "windowssdkversion"
            | "windowssdklibversion"
            | "universalcrtsdkdir"
            | "ucrtversion"
            | "vscmd_arg_tgt_arch"
            | "vscmd_arg_host_arch"
            | "cuda_path"
            | "vcpkg_root"
            | "vcpkg_target_triplet"
            | "vcpkg_default_triplet"
            | "vcpkg_disable_metrics"
            | "git_config_nosystem"
            | "git_config_global"
            | "git_terminal_prompt"
    )
}
fn validate_report(
    r: &DependencyReport,
    digest: &str,
    root: &Path,
) -> Result<(), RuntimeInstallError> {
    let fail = |s: &str| RuntimeInstallError::InvalidMetadata(s.into());
    if r.schema_version != 1
        || r.bundle_sha256 != digest
        || r.runtime_files.is_empty()
        || r.runtime_files.len() > 256
    {
        return Err(fail(
            "native dependency receipt identity or runtime library inventory is invalid",
        ));
    }
    if r.environment
        .iter()
        .any(|(k, v)| !admitted_environment(k) || v.contains('\0') || v.len() > 65536)
    {
        return Err(fail(
            "native dependency receipt contains unadmitted environment bindings",
        ));
    }
    let root = std::fs::canonicalize(root).map_err(|e| fail(&e.to_string()))?;
    for name in ["git", "cmake", "ninja", "nvcc", "cl"] {
        let path = r
            .tools
            .get(name)
            .ok_or_else(|| fail("missing selected native build tool"))?;
        if !path.is_absolute() || !path.is_file() {
            return Err(fail("selected native build tool is missing"));
        }
        if name != "cl" && !std::fs::canonicalize(path).is_ok_and(|p| p.starts_with(&root)) {
            return Err(fail(
                "selected managed tool escapes the private dependency environment",
            ));
        }
    }
    for path in [&r.cuda_root, &r.dependency_prefix] {
        if !std::fs::canonicalize(path).is_ok_and(|p| p.starts_with(&root)) {
            return Err(fail(
                "native dependency prefix escapes the private environment",
            ));
        }
    }
    let msvc = r
        .toolchain
        .system_dependencies
        .get("msvc_installation")
        .and_then(|p| std::fs::canonicalize(p).ok())
        .ok_or_else(|| fail("missing Microsoft compiler provenance"))?;
    if !std::fs::canonicalize(&r.tools["cl"]).is_ok_and(|p| p.starts_with(&msvc)) {
        return Err(fail(
            "selected compiler conflicts with its Microsoft installation",
        ));
    }
    for path in &r.runtime_files {
        if path
            .extension()
            .is_none_or(|e| !e.eq_ignore_ascii_case("dll"))
            || !std::fs::canonicalize(path)
                .is_ok_and(|p| p.is_file() && (p.starts_with(&root) || p.starts_with(&msvc)))
        {
            return Err(fail(
                "runtime dependency is absent or outside its acquired environment",
            ));
        }
    }
    Ok(())
}
impl Prepared {
    pub(super) fn checked(
        &self,
        plan: &RuntimeSourceBuildPlan,
    ) -> Result<CheckedSourceBuildPrerequisites, RuntimeInstallError> {
        let mut bound = plan.clone();
        bound.prerequisites.cuda_compiler = Some(self.report.tools["nvcc"].clone());
        let mut arguments = effective_cmake_configuration_arguments(&bound)
            .map_err(|e| RuntimeInstallError::InvalidMetadata(e.to_string()))?
            .ok_or_else(|| {
                RuntimeInstallError::InvalidMetadata(
                    "managed native dependencies require CMake".into(),
                )
            })?;
        for (name, path) in [
            ("CMAKE_C_COMPILER", &self.report.tools["cl"]),
            ("CMAKE_CXX_COMPILER", &self.report.tools["cl"]),
            ("CMAKE_MAKE_PROGRAM", &self.report.tools["ninja"]),
            ("CUDAToolkit_ROOT", &self.report.cuda_root),
            ("CMAKE_PREFIX_PATH", &self.report.dependency_prefix),
        ] {
            if arguments
                .iter()
                .any(|arg| arg.starts_with(&format!("-D{name}=")))
            {
                return Err(RuntimeInstallError::InvalidMetadata(format!(
                    "competing generated native binding {name}"
                )));
            }
            arguments.push(format!(
                "-D{name}={}",
                path.to_string_lossy().replace('\\', "/")
            ));
        }
        let mut toolchain = self.report.toolchain.clone();
        toolchain.system_dependencies.insert(
            "native_dependency_recipe_sha256".into(),
            self.assets.sha256(),
        );
        Ok(CheckedSourceBuildPrerequisites {
            toolchain,
            effective_cmake_configuration_arguments: Some(arguments),
        })
    }
    fn command(
        &self,
        program: &str,
        arguments: &[&str],
        cwd: Option<&Path>,
    ) -> Result<(PathBuf, Vec<String>), RuntimeInstallError> {
        let executable = self
            .report
            .tools
            .get(program)
            .ok_or_else(|| {
                RuntimeInstallError::SourceBuild(format!(
                    "tool {program} is outside the selected managed source recipe"
                ))
            })?
            .clone();
        let mut args = Vec::new();
        if let Some(cwd) = cwd {
            if program != "git" {
                return Err(RuntimeInstallError::SourceBuild(
                    "unsupported managed source working directory".into(),
                ));
            }
            args.extend(["-C".into(), cwd.display().to_string()]);
        }
        args.extend(arguments.iter().map(|a| a.to_string()));
        if program == "cmake" && arguments.first() == Some(&"--build") {
            args.extend(["--parallel".into(), "2".into()]);
        }
        Ok((executable, args))
    }
    pub(super) async fn stage_runtime_files(
        &self,
        root: &Path,
        entrypoint: &Path,
        toolchain: &mut RuntimeSourceBuildToolchain,
    ) -> Result<(), RuntimeInstallError> {
        let destination = entrypoint.parent().ok_or_else(|| {
            RuntimeInstallError::Entrypoint("runtime entrypoint has no parent".into())
        })?;
        for path in &self.report.runtime_files {
            let name = path.file_name().expect("verified file");
            let target = destination.join(name);
            let digest = hash_file(path)
                .await
                .map_err(|e| RuntimeInstallError::Task(e.to_string()))?;
            if target.exists() {
                if hash_file(&target)
                    .await
                    .map_err(|e| RuntimeInstallError::Task(e.to_string()))?
                    != digest
                {
                    return Err(RuntimeInstallError::InvalidMetadata(format!(
                        "conflicting runtime DLL {}",
                        name.to_string_lossy()
                    )));
                }
            } else {
                tokio::fs::copy(path, &target)
                    .await
                    .map_err(|e| RuntimeInstallError::Task(e.to_string()))?;
            }
            toolchain.system_dependencies.insert(
                format!("runtime_dll.{}.sha256", name.to_string_lossy()),
                digest,
            );
        }
        let evidence = root.join("source-dependencies");
        tokio::fs::create_dir(&evidence)
            .await
            .map_err(|e| RuntimeInstallError::Task(e.to_string()))?;
        for asset in &self.assets.assets {
            tokio::fs::write(evidence.join(asset.name), asset.bytes)
                .await
                .map_err(|e| RuntimeInstallError::Task(e.to_string()))?;
        }
        toolchain
            .system_dependencies
            .insert("build_parallel_jobs".into(), "2".into());
        Ok(())
    }
}
pub(super) async fn run(
    prepared: Option<&Prepared>,
    label: &str,
    program: &str,
    arguments: &[&str],
    cwd: Option<&Path>,
    rejected: &[&str],
) -> Result<(), RuntimeInstallError> {
    let Some(prepared) = prepared else {
        return run_source_command(label, program, arguments, cwd, rejected).await;
    };
    let (executable, args) = prepared.command(program, arguments, cwd)?;
    let result = crate::supervisor::capture_install_command(
        &executable,
        &args.iter().map(String::as_str).collect::<Vec<_>>(),
        &prepared.environment,
        Arc::clone(&prepared.progress),
        Some(prepared.lease.as_ref()),
    )
    .await?;
    if !result.success {
        return Err(RuntimeInstallError::SourceBuild(format!(
            "{label} failed: {}",
            compact_identity(&format!("{}\n{}", result.stdout, result.stderr))
        )));
    }
    Ok(())
}
pub(super) async fn text(
    prepared: Option<&Prepared>,
    program: &str,
    args: &[&str],
    cwd: Option<&Path>,
) -> Result<String, RuntimeInstallError> {
    let Some(prepared) = prepared else {
        return command_text(program, args, cwd).await;
    };
    let (executable, args) = prepared.command(program, args, cwd)?;
    let result = crate::capture_owned_command(
        &executable,
        &args.iter().map(String::as_str).collect::<Vec<_>>(),
        &prepared.environment,
        &[],
        SOURCE_PROBE_TIMEOUT,
    )
    .await?;
    if !result.success {
        return Err(RuntimeInstallError::SourceBuild(format!(
            "managed source probe failed: {}",
            compact_identity(&result.stderr)
        )));
    }
    Ok(result.stdout.trim().into())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_environment_does_not_import_tokens_or_competing_compilers() {
        for name in ["PATH", "INCLUDE", "LIB", "CUDA_PATH", "VCPKG_ROOT"] {
            assert!(admitted_environment(name));
        }
        for name in [
            "GITHUB_TOKEN",
            "AWS_SECRET_ACCESS_KEY",
            "CUDACXX",
            "CXXFLAGS",
            "CMAKE_TOOLCHAIN_FILE",
            "CUDA_VISIBLE_DEVICES",
        ] {
            assert!(!admitted_environment(name));
        }
    }
    #[test]
    fn native_dependency_receipt_cannot_fake_completion() {
        let root = tempfile::tempdir().unwrap();
        let r = DependencyReport {
            schema_version: 1,
            bundle_sha256: "a".repeat(64),
            environment: BTreeMap::new(),
            tools: BTreeMap::new(),
            cuda_root: root.path().into(),
            dependency_prefix: root.path().into(),
            runtime_files: Vec::new(),
            toolchain: RuntimeSourceBuildToolchain {
                cmake_version: String::new(),
                ninja_version: String::new(),
                make_version: String::new(),
                cpp_compiler: String::new(),
                nvcc_version: String::new(),
                pkg_config_version: String::new(),
                system_dependencies: BTreeMap::new(),
            },
        };
        assert!(validate_report(&r, &"a".repeat(64), root.path()).is_err());
        assert!(validate_report(&r, &"b".repeat(64), root.path()).is_err());
    }

    fn prepared_fixture() -> Prepared {
        let work = tempfile::tempdir().unwrap();
        let root = work.path();
        let msvc = root.join("msvc");
        std::fs::create_dir(&msvc).unwrap();
        let mut tools = BTreeMap::new();
        for name in ["git", "cmake", "ninja", "nvcc", "cl"] {
            let path = if name == "cl" {
                msvc.join("cl.exe")
            } else {
                root.join(format!("{name}.exe"))
            };
            std::fs::write(&path, b"synthetic tool; never executed").unwrap();
            tools.insert(name.to_owned(), path);
        }
        let dll = root.join("dependency.dll");
        std::fs::write(&dll, b"runtime dependency fixture").unwrap();
        let assets = ManagedSourceDependencies {
            entrypoint: "prepare.ps1",
            assets: vec![crate::SourceDependencyAsset {
                name: "prepare.ps1",
                bytes: b"# fixture",
            }],
        };
        let digest = assets.sha256();
        let report = DependencyReport {
            schema_version: 1,
            bundle_sha256: digest.clone(),
            environment: BTreeMap::new(),
            tools,
            cuda_root: root.to_owned(),
            dependency_prefix: root.to_owned(),
            runtime_files: vec![dll],
            toolchain: RuntimeSourceBuildToolchain {
                cmake_version: "fixture".into(),
                ninja_version: "fixture".into(),
                make_version: "not required".into(),
                cpp_compiler: "fixture".into(),
                nvcc_version: "fixture".into(),
                pkg_config_version: "not required".into(),
                system_dependencies: BTreeMap::from([(
                    "msvc_installation".into(),
                    msvc.display().to_string(),
                )]),
            },
        };
        validate_report(&report, &digest, root).unwrap();
        let lease = Arc::new(
            OpenOptions::new()
                .create_new(true)
                .write(true)
                .read(true)
                .open(root.join("lock"))
                .unwrap(),
        );
        Prepared {
            report,
            environment: BTreeMap::new(),
            assets,
            _work: work,
            lease,
            progress: Arc::new(|_| {}),
        }
    }
    #[test]
    fn prepared_environment_binds_exact_tools_without_mutating_the_recipe() {
        let prepared = prepared_fixture();
        let plan: RuntimeSourceBuildPlan = serde_json::from_value(serde_json::json!({
            "source":{"repository":"example/runtime","repository_url":"https://github.com/example/runtime.git","source_branch":"main","commit_sha":"a".repeat(40),"tree_sha":"b".repeat(40),"commit_timestamp_unix":1,"source_provider":"test"},
            "recipe":{"recipe_version":"r2","build_system":"cmake","cmake_configuration_arguments":["-G","Ninja"],"build_target":"server","entrypoint":"build/server.exe","accelerator_target":"sm_120a"},
            "prerequisites":{"minimum_cmake_version":"3.28","requires_ninja":true,"requires_cpp20_compiler":true,"cpp_compiler":"cl.exe","requires_pkg_config":false,"pkg_config_modules":{}}
        })).unwrap();
        let checked = prepared.checked(&plan).unwrap();
        let args = checked.effective_cmake_configuration_arguments.unwrap();
        for prefix in [
            "-DCMAKE_CUDA_COMPILER=",
            "-DCMAKE_C_COMPILER=",
            "-DCMAKE_CXX_COMPILER=",
            "-DCMAKE_MAKE_PROGRAM=",
            "-DCMAKE_PREFIX_PATH=",
        ] {
            assert_eq!(
                args.iter().filter(|arg| arg.starts_with(prefix)).count(),
                1,
                "{prefix}"
            );
        }
        assert_eq!(plan.recipe.cmake_configuration_arguments, ["-G", "Ninja"]);
        assert!(plan.prerequisites.cuda_compiler.is_none());
        let (program, args) = prepared
            .command("git", &["rev-parse", "HEAD"], Some(prepared._work.path()))
            .unwrap();
        assert_eq!(program, prepared.report.tools["git"]);
        assert_eq!(args[0], "-C");
        assert!(prepared.command("unselected-tool", &[], None).is_err());
    }
    #[tokio::test]
    async fn installed_dlls_survive_cache_removal_and_conflicts_fail_before_activation() {
        let prepared = prepared_fixture();
        let installed = tempfile::tempdir().unwrap();
        let exe = installed.path().join("server.exe");
        std::fs::write(&exe, b"fixture").unwrap();
        let mut toolchain = prepared.report.toolchain.clone();
        prepared
            .stage_runtime_files(installed.path(), &exe, &mut toolchain)
            .await
            .unwrap();
        assert!(
            toolchain
                .system_dependencies
                .contains_key("runtime_dll.dependency.dll.sha256")
        );
        std::fs::write(
            installed.path().join("dependency.dll"),
            b"conflicting bytes",
        )
        .unwrap();
        assert!(
            prepared
                .stage_runtime_files(installed.path(), &exe, &mut toolchain)
                .await
                .is_err()
        );
        std::fs::write(
            installed.path().join("dependency.dll"),
            b"runtime dependency fixture",
        )
        .unwrap();
        std::fs::remove_file(&prepared.report.runtime_files[0]).unwrap();
        assert_eq!(
            std::fs::read(installed.path().join("dependency.dll")).unwrap(),
            b"runtime dependency fixture"
        );
    }
}
