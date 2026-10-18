//! One reconstruction for build execution and installed provenance matching.
use super::*;
pub(super) fn effective_arguments(
    plan: &RuntimeSourceBuildPlan,
    prefix: &Path,
    cuda: bool,
) -> Result<Option<Vec<String>>, RuntimeInstallError> {
    let mut bound = plan.clone();
    // Preserve the adapter's original recipe; bind separately observed private tools.
    bound.prerequisites.cuda_compiler = cuda.then(|| prefix.join("bin/nvcc"));
    let mut arguments = effective_cmake_configuration_arguments(&bound).map_err(io)?;
    if let Some(arguments) = &mut arguments {
        let mut bindings = vec![
            (
                "CMAKE_C_COMPILER",
                text_path(&prefix.join("bin/x86_64-conda-linux-gnu-gcc"))?,
            ),
            (
                "CMAKE_CXX_COMPILER",
                text_path(&prefix.join("bin/x86_64-conda-linux-gnu-g++"))?,
            ),
            ("CMAKE_MAKE_PROGRAM", text_path(&prefix.join("bin/ninja"))?),
            ("CMAKE_PREFIX_PATH", text_path(prefix)?),
            // CMake retains its upstream $ORIGIN entries for runtime-owned libraries.
            (
                "CMAKE_BUILD_RPATH",
                library_paths(prefix, cuda)
                    .iter()
                    .map(|p| text_path(p))
                    .collect::<Result<Vec<_>, _>>()?
                    .join(";"),
            ),
        ];
        if cuda {
            bindings.extend([
                (
                    "CMAKE_CUDA_HOST_COMPILER",
                    text_path(&prefix.join("bin/x86_64-conda-linux-gnu-g++"))?,
                ),
                ("CUDAToolkit_ROOT", text_path(prefix)?),
            ]);
        }
        for (name, value) in bindings {
            if plan.recipe.cmake_configuration_arguments.iter().any(|a| {
                a.starts_with(&format!("-D{name}=")) || a.starts_with(&format!("-D{name}:"))
            }) {
                return Err(invalid(format!("competing source binding: {name}")));
            }
            arguments.push(format!("-D{name}={value}"));
        }
    }
    Ok(arguments)
}
pub(crate) fn provenance_matches(
    installed: &InstalledRuntime,
    plan: &RuntimeSourceBuildPlan,
    recipe: &LinuxDependencyRecipe,
) -> bool {
    let verify = || -> Result<bool, RuntimeInstallError> {
        let Some(p) = installed.manifest.source_build.as_ref() else {
            return Ok(false);
        };
        check_contract(plan, recipe)?;
        let reference: LinuxDependencyReference = serde_json::from_slice(&read_bounded(
            &installed.installation_root.join("linux-dependencies.json"),
            65536,
        )?)
        .map_err(io)?;
        let environment = environment_digest(recipe);
        let prefix = &reference.prefix;
        if reference.schema_version != 1
            || reference.recipe_sha256 != recipe.sha256()
            || reference.environment_sha256 != environment
            || reference.bootstrap_sha256 != BOOTSTRAP_SHA256
            || reference.implementation_version != IMPLEMENTATION
            || prefix.file_name().is_none_or(|n| n != "prefix")
            || prefix
                .parent()
                .and_then(Path::file_name)
                .is_none_or(|n| n != environment.as_str())
            || p.toolchain
                .system_dependencies
                .get("linux_dependency_prefix")
                .map(String::as_str)
                != prefix.to_str()
            || p.toolchain
                .system_dependencies
                .get("linux_dependency_inventory_sha256")
                != Some(&reference.inventory_sha256)
        {
            return Ok(false);
        }
        let ready: Ready = serde_json::from_slice(&read_bounded(
            &prefix.parent().expect("checked prefix").join("ready.json"),
            1024 * 1024,
        )?)
        .map_err(io)?;
        Ok(ready.reference == reference
            && p.source == plan.source
            && p.recipe_version == plan.recipe.recipe_version
            && p.build_system == plan.recipe.build_system
            && p.cmake_configuration_arguments == plan.recipe.cmake_configuration_arguments
            && p.build_target == plan.recipe.build_target
            && p.accelerator_target == plan.recipe.accelerator_target
            && p.effective_cmake_configuration_arguments
                == effective_arguments(plan, prefix, recipe.metadata().cuda_release.is_some())?)
    };
    verify().unwrap_or(false)
}
