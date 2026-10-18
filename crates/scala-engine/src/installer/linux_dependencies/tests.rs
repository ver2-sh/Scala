use super::*;

const LOCKS: &[(&str, &[u8])] = &[
    (
        "cpp",
        include_bytes!("../../../runtime/native/linux/cpp.lock.json"),
    ),
    (
        "cuda128",
        include_bytes!("../../../runtime/native/linux/cuda128.lock.json"),
    ),
    (
        "cuda132",
        include_bytes!("../../../runtime/native/linux/cuda132.lock.json"),
    ),
    (
        "ninfer132",
        include_bytes!("../../../runtime/native/linux/ninfer132.lock.json"),
    ),
    (
        "ninfer134",
        include_bytes!("../../../runtime/native/linux/ninfer134.lock.json"),
    ),
];
fn recipe(name: &str) -> LinuxDependencyRecipe {
    LinuxDependencyRecipe::new(LOCKS.iter().find(|(n, _)| *n == name).unwrap().1).unwrap()
}
fn synthetic_recipe() -> LinuxDependencyRecipe {
    let value = serde_json::json!({"schema_version":1,"platform":"linux-64","minimum_glibc":"2.28","cuda_release":null,
        "packages":[{"name":"fixture","version":"1","build":"0","url":"https://conda.anaconda.org/conda-forge/linux-64/fixture-1-0.conda","sha256":digest(b"archive"),"size":7}]});
    LinuxDependencyRecipe::new(Box::leak(
        serde_json::to_vec(&value).unwrap().into_boxed_slice(),
    ))
    .unwrap()
}
fn plan() -> RuntimeSourceBuildPlan {
    serde_json::from_value(serde_json::json!({
        "source":{"repository":"example/runtime","repository_url":"https://github.com/example/runtime.git","source_branch":"main","commit_sha":"a".repeat(40),"tree_sha":"b".repeat(40),"commit_timestamp_unix":1,"source_provider":"test"},
        "recipe":{"recipe_version":"r1","build_system":"cmake","cmake_configuration_arguments":["-G","Ninja","-DCMAKE_BUILD_RPATH_USE_ORIGIN=ON","-DCMAKE_CUDA_ARCHITECTURES=120a-real"],"build_target":"server","entrypoint":"build/server","accelerator_target":"sm_120a"},
        "prerequisites":{"minimum_cmake_version":"3.28","minimum_cuda_version":"13.1","requires_ninja":true,"requires_cpp20_compiler":true,"cpp_compiler":"c++","cuda_compiler":"/usr/local/cuda/bin/nvcc","requires_pkg_config":false,"pkg_config_modules":{}}
    })).unwrap()
}
#[test]
fn all_admitted_linux_locks_are_complete_and_share_compatible_components() {
    let mut environments = std::collections::BTreeSet::new();
    for (name, _) in LOCKS {
        let r = recipe(name);
        assert!(environments.insert(environment_digest(&r)));
        for required in [
            "git",
            "cmake",
            "ninja",
            "make",
            "pkg-config",
            "gcc_linux-64",
            "gxx_linux-64",
            "sysroot_linux-64",
        ] {
            assert!(
                r.metadata().package(required).is_some(),
                "{name}: {required}"
            );
        }
        assert_eq!(
            r.metadata().package("sysroot_linux-64").unwrap().version,
            "2.28"
        );
        assert!(!r.explicit_lock().contains("latest"));
        assert!(r.explicit_lock().starts_with("@EXPLICIT\n"));
        for p in &r.metadata().packages {
            let saved: LinuxDependencyPackage =
                serde_json::from_slice(&serde_json::to_vec(p).unwrap()).unwrap();
            assert_eq!(&saved, p);
            assert_eq!(saved.filename(), p.filename());
        }
    }
    let a = recipe("cuda132");
    let b = recipe("ninfer132");
    for name in [
        "cmake",
        "ninja",
        "gcc_linux-64",
        "gxx_linux-64",
        "cuda-nvcc",
    ] {
        assert_eq!(
            a.metadata().package(name),
            b.metadata().package(name),
            "{name} should share one package payload"
        );
    }
    assert_ne!(
        recipe("cuda128").metadata().package("cuda-nvcc"),
        recipe("cuda132").metadata().package("cuda-nvcc")
    );
}
#[test]
fn dependency_lock_refuses_unpinned_unsafe_or_conflated_packages() {
    let original: serde_json::Value =
        serde_json::from_slice(synthetic_recipe().lock_bytes()).unwrap();
    for (key, value) in [
        (
            "url",
            "http://conda.anaconda.org/conda-forge/linux-64/fixture-1-0.conda",
        ),
        ("url", "https://example.com/fixture.conda"),
        ("name", "../escape"),
        ("sha256", "latest"),
        ("build", "../x"),
    ] {
        let mut bad = original.clone();
        bad["packages"][0][key] = value.into();
        assert!(
            crate::linux_dependencies::validate_lock(&serde_json::to_vec(&bad).unwrap()).is_err()
        );
    }
    let mut duplicate = original.clone();
    duplicate["packages"]
        .as_array_mut()
        .unwrap()
        .push(original["packages"][0].clone());
    assert!(
        crate::linux_dependencies::validate_lock(&serde_json::to_vec(&duplicate).unwrap()).is_err()
    );
    let mut bad_cuda = original;
    bad_cuda["cuda_release"] = "13.4".into();
    assert!(
        crate::linux_dependencies::validate_lock(&serde_json::to_vec(&bad_cuda).unwrap()).is_err()
    );
}
#[test]
fn private_cmake_binding_preserves_upstream_contract_and_origin_rpaths() {
    let p = plan();
    let before = p.clone();
    let root = tempfile::tempdir().unwrap();
    let prefix = root.path();
    // Windows tests construct Unix deployment paths: this pure reconstruction
    // does not inspect or execute a host compiler.
    let prefix = if cfg!(windows) {
        Path::new("/scala/runtime-dependencies/linux/fixture/prefix")
    } else {
        prefix
    };
    let args = bindings::effective_arguments(&p, prefix, true)
        .unwrap()
        .unwrap();
    assert_eq!(p, before);
    assert_eq!(&args[..4], &p.recipe.cmake_configuration_arguments);
    assert!(
        args.iter()
            .any(|a| a == "-DCMAKE_BUILD_RPATH_USE_ORIGIN=ON")
    );
    assert!(args.iter().any(|a| a
        == &format!(
            "-DCMAKE_CUDA_COMPILER={}",
            prefix.join("bin/nvcc").display()
        )));
    assert!(!args.iter().any(|a| a.contains("/usr/local/cuda")));
    assert!(!args.iter().any(|a| a.contains("BUILD_WITH_INSTALL_RPATH")));
    let mut competing = p;
    competing
        .recipe
        .cmake_configuration_arguments
        .push("-DCMAKE_CXX_COMPILER=/untrusted/c++".into());
    assert!(bindings::effective_arguments(&competing, prefix, true).is_err());
}
#[test]
fn private_make_and_jit_environment_do_not_import_ambient_settings() {
    let prefix = Path::new("/scala/runtime-dependencies/linux/fixture/prefix");
    let args = make_arguments(prefix, true, &["--no-builtin-rules", "server"]).unwrap();
    assert_eq!(&args[..2], &["--no-builtin-rules", "server"]);
    assert!(args.iter().any(|a| a.starts_with("NVCC=")));
    assert!(!args.iter().any(|a| a.starts_with("LDFLAGS=")));
    assert!(make_arguments(prefix, true, &["NVCC=/wrong/nvcc"]).is_err());
    let env = build_environment(prefix, true, Path::new("/scala/scratch")).unwrap();
    for name in [
        "GITHUB_TOKEN",
        "PYTHONPATH",
        "CUDA_VISIBLE_DEVICES",
        "CXXFLAGS",
        "LD_PRELOAD",
    ] {
        assert!(!env.contains_key(name));
    }
    assert!(env["PATH"].starts_with("/scala/"));
    assert!(!env.values().any(|v| v.contains("/usr/local/cuda")));
    let args = mamba_arguments(Path::new("/cache"), prefix, Path::new("/lock.txt"));
    for flag in [
        "--no-rc",
        "--no-env",
        "--no-allow-softlinks",
        "--extra-safety-checks",
        "--skip-run-link-scripts",
    ] {
        assert!(args.iter().any(|a| a == flag));
    }
}
#[tokio::test]
async fn verified_shared_package_survives_archive_eviction_and_rejects_corruption() {
    let root = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(root.path()).unwrap();
    owned(&root, CACHE_OWNER).unwrap();
    let r = synthetic_recipe();
    let p = &r.metadata().packages[0];
    let extracted = package_directory(&root, p).join(p.archive_stem());
    directories(&extracted, true).unwrap();
    fs::write(extracted.join("tool.txt"), b"retained").unwrap();
    let archive = package_directory(&root, p).join(p.filename());
    fs::write(&archive, b"archive").unwrap();
    check_package_cache(&root, &r, true).await.unwrap();
    check_package_cache(&root, &r, false).await.unwrap();
    cleanup::trim_archives(&root, 0).unwrap();
    assert!(!archive.exists());
    check_package_cache(&root, &r, false).await.unwrap();
    fs::write(extracted.join("tool.txt"), b"tampered").unwrap();
    assert!(check_package_cache(&root, &r, false).await.is_err());
}
#[tokio::test]
async fn cache_cleanup_preserves_referenced_durable_hardlinks_and_collects_unused_envs() {
    let workspace = tempfile::tempdir().unwrap();
    let canonical = fs::canonicalize(workspace.path()).unwrap();
    let paths = crate::test_support::paths(&canonical);
    let installer = RuntimeInstaller::new(
        Arc::new(RuntimeStore::new(&paths)),
        EngineRegistry::default(),
        paths.runtime_cache_dir.clone(),
        std::iter::empty(),
    )
    .unwrap();
    let native = installer.cache_root.join("native");
    let lease = native_lease(&native).unwrap();
    let cache = native.join("linux");
    owned(&cache, CACHE_OWNER).unwrap();
    let payload = cache.join("payload");
    fs::write(&payload, b"shared library").unwrap();
    let durable = paths.data_dir.join("runtime-dependencies/linux");
    owned(&durable, DURABLE_OWNER).unwrap();
    for key in ["a".repeat(64), "b".repeat(64)] {
        let env = durable.join(&key);
        owned(&env, &format!("{IMPLEMENTATION}:{key}")).unwrap();
        directories(&env.join("prefix"), true).unwrap();
        fs::hard_link(&payload, env.join("prefix/library.so")).unwrap();
    }
    let reference = LinuxDependencyReference {
        schema_version: 1,
        recipe_sha256: "c".repeat(64),
        environment_sha256: "a".repeat(64),
        bootstrap_sha256: BOOTSTRAP_SHA256.into(),
        implementation_version: IMPLEMENTATION.into(),
        inventory_sha256: "d".repeat(64),
        prefix: durable.join("a".repeat(64)).join("prefix"),
    };
    let runtime = paths.runtimes_dir.join("engine/platform/version/id");
    directories(&runtime, true).unwrap();
    fs::write(
        runtime.join("linux-dependencies.json"),
        serde_json::to_vec(&reference).unwrap(),
    )
    .unwrap();
    fs::write(runtime.join(RUNTIME_MANIFEST_FILE), b"synthetic manifest").unwrap();
    assert!(
        installer.clean_native_cache().await.is_err(),
        "active acquisition protects all dependencies"
    );
    drop(lease);
    installer.clean_native_cache().await.unwrap();
    assert!(!cache.exists());
    assert!(!durable.join("b".repeat(64)).exists());
    assert_eq!(
        fs::read(reference.prefix.join("library.so")).unwrap(),
        b"shared library"
    );
    fs::remove_dir_all(&runtime).unwrap();
    installer.clean_native_cache().await.unwrap();
    assert!(!reference.prefix.exists());
}
#[test]
fn ownership_and_failed_preparation_never_adopt_foreign_files() {
    let temp = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(temp.path()).unwrap();
    let foreign = root.join("foreign");
    fs::create_dir(&foreign).unwrap();
    fs::write(foreign.join("keep"), b"unowned").unwrap();
    assert!(owned(&foreign, CACHE_OWNER).is_err());
    assert!(foreign.join("keep").exists());
    let base = root
        .to_string_lossy()
        .trim_start_matches(r"\\?\")
        .to_owned();
    let invalid_path = PathBuf::from(format!(
        "{base}{}..{}scala-escape-rejected",
        std::path::MAIN_SEPARATOR,
        std::path::MAIN_SEPARATOR
    ));
    assert!(directories(&invalid_path, true).is_err());
    let owned_root = root.join("pending");
    owned(&owned_root, "synthetic owned pending").unwrap();
    let pending = PendingEnvironment {
        root: owned_root.clone(),
        armed: true,
    };
    fs::write(owned_root.join("partial"), b"incomplete").unwrap();
    drop(pending);
    assert!(!owned_root.exists());
}

/// Explicit integration smoke test: native acquisition and a tiny C++ fixture only.
/// No GPU runtime, model, inference, host package manager, or global environment changes.
#[cfg(target_os = "linux")]
#[tokio::test]
#[ignore = "downloads pinned development dependencies into temporary Scala roots"]
async fn pinned_cpp_install_build_reuse_and_clean_cache() {
    use std::os::unix::fs::MetadataExt;
    let workspace = tempfile::tempdir().unwrap();
    let paths = crate::test_support::paths(workspace.path());
    paths.ensure_required().unwrap();
    let installer = RuntimeInstaller::new(
        Arc::new(RuntimeStore::new(&paths)),
        EngineRegistry::default(),
        paths.runtime_cache_dir.clone(),
        std::iter::empty(),
    )
    .unwrap();
    let release: crate::GitHubRelease =
        serde_json::from_value(crate::test_support::release(b"fixture")).unwrap();
    let mut available = crate::test_support::runtime(&release);
    available.identity.platform = "linux".into();
    available.identity.architecture = "x86_64".into();
    let first_recipe = recipe("cpp");
    let first = installer
        .prepare_linux_dependencies(&available, &first_recipe)
        .await
        .unwrap();
    let project = workspace.path().join("tiny-cpp");
    fs::create_dir(&project).unwrap();
    fs::write(project.join("CMakeLists.txt"),
        "cmake_minimum_required(VERSION 3.28)\nproject(scala_dependency_smoke LANGUAGES CXX)\nadd_executable(smoke main.cpp)\ntarget_compile_features(smoke PRIVATE cxx_std_20)\n").unwrap();
    fs::write(
        project.join("main.cpp"),
        "#include <span>\nint main(){int a[1]{};std::span<int> s{a};return int(s.size())-1;}\n",
    )
    .unwrap();
    let build = workspace.path().join("tiny-build");
    let args = [
        "-S".to_owned(),
        project.display().to_string(),
        "-B".into(),
        build.display().to_string(),
        "-G".into(),
        "Ninja".into(),
        format!(
            "-DCMAKE_CXX_COMPILER={}",
            first.prefix().join("scala-bin/c++").display()
        ),
        format!("-DCMAKE_MAKE_PROGRAM={}", first.tools["ninja"].display()),
    ];
    first
        .run(
            "cmake",
            &args.iter().map(String::as_str).collect::<Vec<_>>(),
            None,
        )
        .await
        .unwrap();
    first
        .run(
            "cmake",
            &["--build", build.to_str().unwrap(), "--target", "smoke"],
            None,
        )
        .await
        .unwrap();
    assert!(build.join("smoke").is_file());
    let keep = paths.runtimes_dir.join("fixture/linux/1/first");
    fs::create_dir_all(&keep).unwrap();
    first.write_python_binding(&keep).await.unwrap();
    let prefix = first.prefix().to_owned();
    let cmake = first.tools["cmake"].clone();
    let original_inode = fs::metadata(&cmake).unwrap().ino();
    drop(first);
    let reused = installer
        .prepare_linux_dependencies(&available, &first_recipe)
        .await
        .unwrap();
    assert_eq!(reused.prefix(), prefix);
    assert_eq!(
        fs::metadata(&reused.tools["cmake"]).unwrap().ino(),
        original_inode
    );
    drop(reused);
    // Distinct immutable environment identity, identical pinned package inputs.
    let mut second_bytes = first_recipe.lock_bytes().to_vec();
    second_bytes.push(b'\n');
    let second_recipe =
        LinuxDependencyRecipe::new(Box::leak(second_bytes.into_boxed_slice())).unwrap();
    let second = installer
        .prepare_linux_dependencies(&available, &second_recipe)
        .await
        .unwrap();
    assert_ne!(second.prefix(), prefix);
    assert_eq!(
        fs::metadata(&second.tools["cmake"]).unwrap().ino(),
        original_inode
    );
    let unreferenced = second.prefix().parent().unwrap().to_owned();
    drop(second);
    installer.clean_native_cache().await.unwrap();
    assert!(cmake.is_file());
    assert!(!unreferenced.exists());
    assert!(!paths.runtime_cache_dir.join("native/linux").exists());
    assert!(keep.join("build-toolchain.json").is_file());
}
