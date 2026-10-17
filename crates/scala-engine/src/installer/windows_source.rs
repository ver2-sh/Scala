//! Native MSVC source prerequisites. No WSL, dependency downloads or environment mutation.
use super::*;

pub(super) fn is_msvc_plan(plan: &RuntimeSourceBuildPlan) -> bool {
    plan.recipe.build_system == RuntimeSourceBuildSystem::Cmake
        && plan.prerequisites.cpp_compiler.as_deref() == Some("cl.exe")
        && plan
            .recipe
            .entrypoint
            .extension()
            .is_some_and(|extension| extension == "exe")
}

pub(super) fn dependencies(
    plan: &RuntimeSourceBuildPlan,
) -> Result<BTreeMap<String, String>, RuntimeInstallError> {
    let fail = |message: String| RuntimeInstallError::Prerequisite(message);
    let compiler =
        plan.prerequisites.cuda_compiler.as_deref().ok_or_else(|| {
            fail("native MSVC source build needs an explicit CUDA compiler".into())
        })?;
    if !compiler.is_absolute() || !compiler.is_file() {
        return Err(fail(format!(
            "install the selected CUDA toolkit at {}; no alternate toolkit or CPU fallback is selected",
            compiler.display()
        )));
    }
    for name in ["INCLUDE", "LIB"] {
        if std::env::var_os(name).is_none_or(|v| v.is_empty()) {
            return Err(fail(format!(
                "MSVC developer environment is missing {name}; run Scala from the x64 Native Tools prompt (MSVC 14.44 or newer)"
            )));
        }
    }
    let argument = |prefix: &str| {
        plan.recipe
            .cmake_configuration_arguments
            .iter()
            .find_map(|a| a.strip_prefix(prefix))
    };
    let Some(triplet) = argument("-DVCPKG_TARGET_TRIPLET=") else {
        return Ok(BTreeMap::new());
    };
    if triplet != "x64-windows" {
        return Err(fail(
            "native source build only admits the x64-windows dependency triplet".into(),
        ));
    }
    let prefix = argument("-DCMAKE_PREFIX_PATH=").ok_or_else(|| {
        fail("native vcpkg recipe is missing its explicit CMAKE_PREFIX_PATH".into())
    })?;
    let configured = Path::new(prefix);
    let root = std::env::var_os("VCPKG_ROOT")
        .ok_or_else(|| fail(format!("set VCPKG_ROOT to the prebuilt dependency tree for {prefix}; Scala does not install system dependencies")))?;
    if std::env::var("VCPKG_TARGET_TRIPLET").as_deref() != Ok(triplet) {
        return Err(fail(format!(
            "set VCPKG_TARGET_TRIPLET={triplet} to match the selected recipe"
        )));
    }
    let observed = PathBuf::from(root).join("installed").join(triplet);
    let observed = std::fs::canonicalize(&observed).map_err(|e| {
        fail(format!(
            "prebuilt vcpkg dependency tree is unavailable: {e}"
        ))
    })?;
    if std::fs::canonicalize(configured).ok().as_ref() != Some(&observed) {
        return Err(fail(format!(
            "VCPKG_ROOT does not resolve to the selected {prefix}; refusing a different dependency tree"
        )));
    }
    for relative in [
        "include/libavformat/avformat.h",
        "include/curl/curl.h",
        "lib/avformat.lib",
        "lib/avcodec.lib",
        "lib/avutil.lib",
        "lib/swscale.lib",
        "share/curl/CURLConfig.cmake",
    ] {
        if !observed.join(relative).is_file() {
            return Err(fail(format!(
                "prebuilt FFmpeg/curl dependency is missing: {prefix}/{relative}"
            )));
        }
    }
    let has_dll = std::fs::read_dir(observed.join("bin")).is_ok_and(|entries| {
        entries.flatten().any(|e| {
            e.path()
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("dll"))
        })
    });
    if !has_dll {
        return Err(fail(
            "prebuilt vcpkg runtime DLLs are missing; no self-contained runtime can be staged"
                .into(),
        ));
    }
    let status_path = observed.parent().unwrap().join("vcpkg/status");
    let status = std::fs::read(&status_path).map_err(|e| {
        fail(format!(
            "cannot record vcpkg installed dependency provenance: {e}"
        ))
    })?;
    Ok(BTreeMap::from([
        ("vcpkg_triplet".into(), triplet.into()),
        (
            "vcpkg_prefix".into(),
            observed.to_string_lossy().into_owned(),
        ),
        (
            "vcpkg_status_sha256".into(),
            hex_digest(Sha256::digest(status)),
        ),
    ]))
}

pub(super) async fn probe_compiler(
    program: &str,
    standard: u16,
) -> Result<String, RuntimeInstallError> {
    let temporary =
        tempfile::tempdir().map_err(|e| RuntimeInstallError::Prerequisite(e.to_string()))?;
    let path = temporary.path().join("scala-msvc-probe.cpp");
    // /Zs checks headers and syntax only. No binary is executed and no GPU is used.
    std::fs::write(&path, b"#include <span>\n#if _MSC_VER < 1944\n#error MSVC 14.44 or newer required\n#endif\n#define S1(x) #x\n#define S(x) S1(x)\n#pragma message(\"Scala MSVC \" S(_MSC_FULL_VER))\nint main(){int x[1]{}; std::span<int> s{x}; return int(s.size())-1;}\n")
        .map_err(|e| RuntimeInstallError::Prerequisite(e.to_string()))?;
    command_text(
        program,
        &[
            "/nologo",
            &format!("/std:c++{standard}"),
            "/Zc:__cplusplus",
            "/Zs",
            path.to_str().ok_or_else(|| {
                RuntimeInstallError::Prerequisite("non-UTF8 compiler probe path".into())
            })?,
        ],
        Some(temporary.path()),
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn source_manifest_entrypoint_uses_portable_separators() {
        let path = source_install_entrypoint(Path::new("build/apps/ninfer-serve.exe"));
        assert_eq!(path.to_str(), Some("source/build/apps/ninfer-serve.exe"));
        assert!(is_safe_relative_path(&path));
    }

    #[test]
    fn native_windows_source_is_explicit_not_a_linux_fallback() {
        let mut plan: RuntimeSourceBuildPlan = serde_json::from_value(serde_json::json!({
            "source":{"repository":"example/runtime","repository_url":"https://github.com/example/runtime.git",
                "source_branch":"main","commit_sha":"a".repeat(40),"tree_sha":"b".repeat(40),"commit_timestamp_unix":1,"source_provider":"test"},
            "recipe":{"recipe_version":"r1","build_system":"cmake","cmake_configuration_arguments":[],
                "build_target":"server","entrypoint":"build/server.exe","accelerator_target":"sm_120a"},
            "prerequisites":{"minimum_cmake_version":"3.28","requires_ninja":true,"requires_cpp20_compiler":true,
                "cpp_compiler":"cl.exe","requires_pkg_config":false,"pkg_config_modules":{}}
        })).unwrap();
        assert!(is_msvc_plan(&plan));
        plan.prerequisites.cpp_compiler = None;
        assert!(!is_msvc_plan(&plan));
        plan.prerequisites.cpp_compiler = Some("cl.exe".into());
        plan.recipe.entrypoint = "build/server".into();
        assert!(!is_msvc_plan(&plan));
    }
    #[test]
    fn cmake_digest_cannot_be_bypassed_by_disconnected_fetchcontent() {
        let dir = tempfile::tempdir().unwrap();
        let bytes = b"cmake_minimum_required(VERSION 3.28)\nproject(test)\n";
        std::fs::write(dir.path().join("CMakeLists.txt"), bytes).unwrap();
        let arguments = vec![
            "-DFETCHCONTENT_FULLY_DISCONNECTED=ON".into(),
            "-DFETCHCONTENT_UPDATES_DISCONNECTED=ON".into(),
        ];
        assert!(
            inspect_build_dependency_contract(
                dir.path(),
                RuntimeSourceBuildSystem::Cmake,
                "test",
                Some(&hex_digest(Sha256::digest(bytes))),
                &arguments
            )
            .is_ok()
        );
        assert!(
            inspect_build_dependency_contract(
                dir.path(),
                RuntimeSourceBuildSystem::Cmake,
                "test",
                Some(&"0".repeat(64)),
                &arguments
            )
            .is_err()
        );
    }
}
