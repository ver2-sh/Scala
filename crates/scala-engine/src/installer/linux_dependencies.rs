//! Explicit-install-only Linux acquisition. No catalog/discovery entry point
//! acquires a lease, creates directories, downloads, or starts a process.
mod bindings;
mod cleanup;
mod python;
#[cfg(test)]
mod tests;
use super::*;
use crate::linux_dependencies::{
    LinuxDependencyPackage, LinuxDependencyRecipe, LinuxDependencyReference,
};
pub(crate) use bindings::provenance_matches;
pub(super) use cleanup::clean_locked;
use serde::{Deserialize, Serialize};
use std::fs::{self, File, Metadata};
use std::io::{Read, Write};

const IMPLEMENTATION: &str = "scala-linux-dependencies-v1";
const CACHE_OWNER: &str = "scala-linux-micromamba-cache-v1";
const DURABLE_OWNER: &str = "scala-linux-durable-dependencies-v1";
const BOOTSTRAP_URL: &str = "https://github.com/mamba-org/micromamba-releases/releases/download/2.9.0-0/micromamba-linux-64";
const BOOTSTRAP_SHA256: &str = "366cd9cd8be14df1ab8ed50352a82111082a36686b2d389fdb79a92c3fafb3e3";
const BOOTSTRAP_SIZE: u64 = 18_292_808;
const MAX_INVENTORY_BYTES: u64 = 128 * 1024 * 1024;
const MAX_INVENTORY_ENTRIES: usize = 500_000;
const MAX_INVENTORY_PAYLOAD: u64 = 96 * 1024 * 1024 * 1024;
type Progress = Arc<dyn Fn(&str) + Send + Sync>;

fn invalid(detail: impl Into<String>) -> RuntimeInstallError {
    RuntimeInstallError::InvalidMetadata(detail.into())
}
fn io(error: impl std::fmt::Display) -> RuntimeInstallError {
    RuntimeInstallError::Task(error.to_string())
}
fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn environment_digest(recipe: &LinuxDependencyRecipe) -> String {
    digest(
        format!(
            "{IMPLEMENTATION}\n{BOOTSTRAP_SHA256}\n{}\n",
            recipe.sha256()
        )
        .as_bytes(),
    )
}
fn present(path: &Path) -> Result<bool, RuntimeInstallError> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(io(error)),
    }
}
fn alias(metadata: &Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}

/// Check every component before creating its immediate child. In particular,
/// create_dir_all must not first follow an unexamined parent alias.
fn directories(path: &Path, create: bool) -> Result<(), RuntimeInstallError> {
    if !path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, Component::ParentDir | Component::CurDir))
    {
        return Err(invalid(
            "dependency directory must be a normalized absolute path",
        ));
    }
    let mut current = PathBuf::new();
    for part in path.components() {
        current.push(part.as_os_str());
        // A Windows drive/UNC prefix is not queryable until its root separator.
        if matches!(part, Component::Prefix(_)) {
            continue;
        }
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.is_dir() && !alias(&metadata) => {}
            Ok(_) => {
                return Err(invalid(format!(
                    "dependency directory is an alias or non-directory: {}",
                    current.display()
                )));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && create => {
                fs::create_dir(&current).map_err(io)?;
                let metadata = fs::symlink_metadata(&current).map_err(io)?;
                if !metadata.is_dir() || alias(&metadata) {
                    return Err(invalid("dependency directory changed while being created"));
                }
            }
            Err(error) => return Err(io(error)),
        }
    }
    Ok(())
}
fn regular(path: &Path) -> Result<Metadata, RuntimeInstallError> {
    directories(
        path.parent().ok_or_else(|| invalid("file has no parent"))?,
        false,
    )?;
    let metadata = fs::symlink_metadata(path).map_err(io)?;
    if !metadata.is_file() || alias(&metadata) {
        return Err(invalid(format!(
            "expected an unaliased regular file: {}",
            path.display()
        )));
    }
    Ok(metadata)
}
fn read_bounded(path: &Path, limit: u64) -> Result<Vec<u8>, RuntimeInstallError> {
    if regular(path)?.len() > limit {
        return Err(invalid("dependency evidence exceeds its size limit"));
    }
    let mut bytes = Vec::new();
    File::open(path)
        .map_err(io)?
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(io)?;
    if bytes.len() as u64 > limit {
        return Err(invalid("dependency evidence grew beyond its size limit"));
    }
    Ok(bytes)
}
fn publish(path: &Path, bytes: &[u8]) -> Result<(), RuntimeInstallError> {
    directories(
        path.parent()
            .ok_or_else(|| invalid("missing evidence parent"))?,
        false,
    )?;
    if present(path)? {
        return Err(invalid(
            "refusing to overwrite immutable dependency evidence",
        ));
    }
    let mut partial = tempfile::Builder::new()
        .prefix(".scala-partial-")
        .tempfile_in(path.parent().expect("checked parent"))
        .map_err(io)?;
    partial.write_all(bytes).map_err(io)?;
    partial.as_file().sync_all().map_err(io)?;
    partial.persist_noclobber(path).map_err(io)?;
    sync_directory(path.parent().expect("checked parent"))
}
fn sync_directory(path: &Path) -> Result<(), RuntimeInstallError> {
    #[cfg(unix)]
    File::open(path).map_err(io)?.sync_all().map_err(io)?;
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}
fn owned(path: &Path, identity: &str) -> Result<(), RuntimeInstallError> {
    directories(path, true)?;
    let marker = path.join(".scala-owner");
    if present(&marker)? {
        if read_bounded(&marker, 1024)? != identity.as_bytes() {
            return Err(invalid("dependency ownership marker mismatch"));
        }
    } else {
        if fs::read_dir(path)
            .map_err(io)?
            .next()
            .transpose()
            .map_err(io)?
            .is_some()
        {
            return Err(invalid(
                "refusing to adopt a nonempty unowned dependency directory",
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(io)?;
        }
        publish(&marker, identity.as_bytes())?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let metadata = fs::metadata(path).map_err(io)?;
        if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o022 != 0 {
            return Err(invalid(
                "owned dependency directory has unsafe ownership or write permissions",
            ));
        }
    }
    Ok(())
}
fn native_lease(native: &Path) -> Result<Arc<File>, RuntimeInstallError> {
    directories(native, true)?;
    let path = native.join("acquisition.lock");
    if present(&path)? {
        regular(&path)?;
    }
    let mut options = OpenOptions::new();
    options.create(true).truncate(false).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW).mode(0o600);
    }
    let lease = Arc::new(options.open(&path).map_err(io)?);
    regular(&path)?;
    fs2::FileExt::try_lock_exclusive(lease.as_ref()).map_err(|_| {
        RuntimeInstallError::Prerequisite(
            "another native install or cache-clean operation holds the acquisition lease".into(),
        )
    })?;
    Ok(lease)
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct InventoryEntry {
    kind: String,
    size: u64,
    sha256: String,
    target: Option<String>,
    mode: u32,
}
type Inventory = BTreeMap<String, InventoryEntry>;

fn mode(metadata: &Metadata) -> u32 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o7777
    }
    #[cfg(not(unix))]
    {
        u32::from(metadata.permissions().readonly())
    }
}
fn internal_link(root: &Path, path: &Path, complete: bool) -> Result<String, RuntimeInstallError> {
    let target = fs::read_link(path).map_err(io)?;
    if target.is_absolute()
        || target
            .components()
            .any(|part| matches!(part, Component::Prefix(_) | Component::RootDir))
    {
        return Err(invalid("absolute dependency symlink is forbidden"));
    }
    let mut relative = path
        .parent()
        .ok_or_else(|| invalid("symlink has no parent"))?
        .strip_prefix(root)
        .map_err(io)?
        .to_owned();
    for part in target.components() {
        match part {
            Component::Normal(name) => relative.push(name),
            Component::CurDir => {}
            Component::ParentDir if relative.pop() => {}
            _ => return Err(invalid("dependency symlink escapes its payload")),
        }
    }
    // A cached package may link to a member supplied by another locked package.
    // Each link is lexically contained; the assembled environment additionally
    // requires every complete chain to resolve inside the durable prefix.
    if complete {
        let resolved = fs::canonicalize(path).map_err(io)?;
        let canonical_root = fs::canonicalize(root).map_err(io)?;
        if !resolved.starts_with(canonical_root) {
            return Err(invalid("dependency symlink resolves outside its payload"));
        }
    }
    target
        .to_str()
        .map(str::to_owned)
        .ok_or_else(|| invalid("non-UTF-8 dependency symlink"))
}
async fn inventory(root: &Path) -> Result<Inventory, RuntimeInstallError> {
    inventory_with_links(root, true).await
}
async fn inventory_with_links(
    root: &Path,
    complete: bool,
) -> Result<Inventory, RuntimeInstallError> {
    directories(root, false)?;
    let mut result = BTreeMap::new();
    let mut total = 0_u64;
    for entry in walkdir::WalkDir::new(root)
        .follow_links(false)
        .min_depth(1)
        .max_depth(64)
    {
        let entry = entry.map_err(io)?;
        if result.len() >= MAX_INVENTORY_ENTRIES
            || entry.depth() == 64 && entry.file_type().is_dir()
        {
            return Err(invalid("dependency inventory entry/depth limit exceeded"));
        }
        let path = entry.path();
        let name = path
            .strip_prefix(root)
            .map_err(io)?
            .to_str()
            .ok_or_else(|| invalid("non-UTF-8 dependency payload path"))?
            .replace('\\', "/");
        if !is_safe_relative_path(Path::new(&name)) {
            return Err(invalid("unsafe dependency payload path"));
        }
        let metadata = fs::symlink_metadata(path).map_err(io)?;
        let (kind, size, sha256, target) = if metadata.file_type().is_symlink() {
            (
                "symlink",
                0,
                String::new(),
                Some(internal_link(root, path, complete)?),
            )
        } else if alias(&metadata) {
            return Err(invalid("unsupported dependency reparse point"));
        } else if metadata.is_file() {
            total = total
                .checked_add(metadata.len())
                .ok_or_else(|| invalid("payload size overflow"))?;
            if total > MAX_INVENTORY_PAYLOAD {
                return Err(invalid("dependency payload size limit exceeded"));
            }
            (
                "file",
                metadata.len(),
                hash_file(path).await.map_err(io)?,
                None,
            )
        } else if metadata.is_dir() {
            ("directory", 0, String::new(), None)
        } else {
            return Err(invalid("dependency payload contains a special file"));
        };
        if mode(&metadata) & 0o6000 != 0 && cfg!(unix) {
            return Err(invalid("setuid/setgid dependency payload is forbidden"));
        }
        result.insert(
            name,
            InventoryEntry {
                kind: kind.into(),
                size,
                sha256,
                target,
                mode: mode(&metadata),
            },
        );
    }
    Ok(result)
}
async fn verify_inventory(root: &Path, bytes: &[u8]) -> Result<(), RuntimeInstallError> {
    let expected: Inventory = serde_json::from_slice(bytes).map_err(io)?;
    if expected.is_empty() || inventory(root).await? != expected {
        return Err(invalid(
            "immutable dependency payload changed; automatic in-place repair is refused",
        ));
    }
    Ok(())
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PackageInventory {
    package: LinuxDependencyPackage,
    payload: Inventory,
}
/// Micromamba 2.9 keys its package store by canonical source URL, not a flat filename.
fn package_directory(root: &Path, package: &LinuxDependencyPackage) -> PathBuf {
    let subdir = package
        .url
        .split('/')
        .nth(4)
        .expect("validated conda-forge URL");
    root.join("pkgs/https/conda.anaconda.org/conda-forge")
        .join(subdir)
}
async fn check_package_cache(
    root: &Path,
    recipe: &LinuxDependencyRecipe,
    record: bool,
) -> Result<(), RuntimeInstallError> {
    let pkgs = root.join("pkgs");
    if !present(&pkgs)? {
        return Ok(());
    }
    directories(&pkgs, false)?;
    let evidence = root.join("package-inventories");
    directories(&evidence, true)?;
    for package in &recipe.metadata().packages {
        let package_root = package_directory(root, package);
        let archive = package_root.join(package.filename());
        if present(&archive)?
            && (regular(&archive)?.len() != package.size
                || !hash_file(&archive)
                    .await
                    .map_err(io)?
                    .eq_ignore_ascii_case(&package.sha256))
        {
            return Err(invalid(
                "cached dependency archive differs from its pinned lock",
            ));
        }
        let extracted = package_root.join(package.archive_stem());
        let receipt = evidence.join(format!("{}.json", package.archive_stem()));
        if !present(&extracted)? {
            if record {
                return Err(invalid(
                    "micromamba did not retain the expected URL-keyed package payload",
                ));
            }
            continue;
        }
        directories(&extracted, false)?;
        if present(&receipt)? {
            let bytes = read_bounded(&receipt, MAX_INVENTORY_BYTES)?;
            let saved: PackageInventory = serde_json::from_slice(&bytes).map_err(io)?;
            if saved.package != *package
                || saved.payload != inventory_with_links(&extracted, false).await?
            {
                return Err(invalid(
                    "cached extracted package is corrupt or has a conflicting identity",
                ));
            }
        } else if record {
            let saved = PackageInventory {
                package: package.clone(),
                payload: inventory_with_links(&extracted, false).await?,
            };
            publish(&receipt, &serde_json::to_vec(&saved).map_err(io)?)?;
        } else {
            // A cancelled transaction may have completed extraction before its
            // receipt. Never execute/adopt it: unlink only this owned cache entry
            // so micromamba reconstructs it from the verified archive.
            fs::remove_dir_all(&extracted).map_err(io)?;
        }
    }
    Ok(())
}

async fn bootstrap(root: &Path) -> Result<PathBuf, RuntimeInstallError> {
    let executable = root.join("micromamba-2.9.0-0-linux-64");
    if !present(&executable)? {
        let policy = reqwest::redirect::Policy::custom(|attempt| {
            let url = attempt.url();
            if attempt.previous().len() < 5
                && url.scheme() == "https"
                && matches!(
                    url.host_str(),
                    Some(
                        "github.com"
                            | "release-assets.githubusercontent.com"
                            | "objects.githubusercontent.com"
                    )
                )
                && url.username().is_empty()
                && url.password().is_none()
                && url.port().is_none()
            {
                attempt.follow()
            } else {
                attempt.stop()
            }
        });
        let client = reqwest::Client::builder()
            .https_only(true)
            .no_proxy()
            .connect_timeout(Duration::from_secs(30))
            .timeout(Duration::from_secs(300))
            .redirect(policy)
            .build()
            .map_err(|_| {
                RuntimeInstallError::Download("cannot create bootstrap HTTPS client".into())
            })?;
        let response = client
            .get(BOOTSTRAP_URL)
            .header(reqwest::header::ACCEPT_ENCODING, "identity")
            .send()
            .await
            .map_err(|_| {
                RuntimeInstallError::Download("pinned micromamba HTTPS download failed".into())
            })?;
        if response.status() != reqwest::StatusCode::OK
            || response
                .content_length()
                .is_some_and(|size| size != BOOTSTRAP_SIZE)
        {
            return Err(RuntimeInstallError::Download(
                "pinned micromamba response has an unexpected status or size".into(),
            ));
        }
        let partial = tempfile::Builder::new()
            .prefix(".scala-bootstrap-partial-")
            .tempfile_in(root)
            .map_err(io)?;
        let mut output = tokio::fs::File::from_std(partial.as_file().try_clone().map_err(io)?);
        let mut stream = response.bytes_stream();
        let mut hash = Sha256::new();
        let mut size = 0_u64;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| {
                RuntimeInstallError::Download("pinned bootstrap transfer was interrupted".into())
            })?;
            size = size
                .checked_add(chunk.len() as u64)
                .ok_or_else(|| invalid("bootstrap size overflow"))?;
            if size > BOOTSTRAP_SIZE {
                return Err(invalid("bootstrap exceeds its pinned size"));
            }
            hash.update(&chunk);
            output.write_all(&chunk).await.map_err(io)?;
        }
        output.sync_all().await.map_err(io)?;
        drop(output);
        if size != BOOTSTRAP_SIZE || format!("{:x}", hash.finalize()) != BOOTSTRAP_SHA256 {
            return Err(invalid("pinned micromamba size or SHA-256 mismatch"));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            partial
                .as_file()
                .set_permissions(fs::Permissions::from_mode(0o755))
                .map_err(io)?;
        }
        partial.as_file().sync_all().map_err(io)?;
        partial.persist_noclobber(&executable).map_err(io)?;
        sync_directory(root)?;
    }
    if regular(&executable)?.len() != BOOTSTRAP_SIZE
        || hash_file(&executable).await.map_err(io)? != BOOTSTRAP_SHA256
    {
        return Err(invalid(
            "cached micromamba failed pinned bootstrap verification",
        ));
    }
    executable_mode(&executable)?;
    Ok(executable)
}
fn mamba_arguments(root: &Path, prefix: &Path, explicit: &Path) -> Vec<String> {
    vec![
        "--no-rc".into(),
        "--no-env".into(),
        "create".into(),
        "--yes".into(),
        "--platform".into(),
        "linux-64".into(),
        "--root-prefix".into(),
        root.display().to_string(),
        "--prefix".into(),
        prefix.display().to_string(),
        "--file".into(),
        explicit.display().to_string(),
        "--no-allow-softlinks".into(),
        "--safety-checks".into(),
        "enabled".into(),
        "--extra-safety-checks".into(),
        "--skip-run-link-scripts".into(),
    ]
}
fn isolated_environment(work: &Path) -> BTreeMap<String, String> {
    BTreeMap::from([
        ("PATH".into(), "/usr/bin:/bin".into()),
        ("HOME".into(), work.display().to_string()),
        ("TMPDIR".into(), work.display().to_string()),
        ("LANG".into(), "C".into()),
        ("LC_ALL".into(), "C".into()),
        ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
        ("GIT_CONFIG_GLOBAL".into(), "/dev/null".into()),
        ("GIT_TERMINAL_PROMPT".into(), "0".into()),
    ])
}
fn executable_mode(path: &Path) -> Result<(), RuntimeInstallError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if fs::metadata(path).map_err(io)?.permissions().mode() & 0o111 == 0 {
            return Err(invalid(format!(
                "dependency tool is not executable: {}",
                path.display()
            )));
        }
    }
    let _ = path;
    Ok(())
}
fn selected_file(prefix: &Path, relative: &str) -> Result<PathBuf, RuntimeInstallError> {
    let path = prefix.join(relative);
    let resolved = fs::canonicalize(&path).map_err(|_| {
        RuntimeInstallError::Prerequisite(format!(
            "pinned dependency closure lacks required layout: {relative}"
        ))
    })?;
    if !resolved.starts_with(fs::canonicalize(prefix).map_err(io)?) || !resolved.is_file() {
        return Err(invalid(
            "selected tool/header resolves outside the durable prefix",
        ));
    }
    Ok(path)
}
fn text_path(path: &Path) -> Result<String, RuntimeInstallError> {
    let value = path
        .to_str()
        .ok_or_else(|| invalid("dependency path is not UTF-8"))?;
    // These delimiters have incompatible meanings in PATH, CMake lists, Make
    // expansion, and GCC's -Wl transport. Reject instead of misquoting them.
    if value
        .chars()
        .any(|character| character.is_control() || ":;,$".contains(character))
    {
        return Err(invalid(
            "dependency path contains an unsupported build-list delimiter",
        ));
    }
    Ok(value.into())
}
fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}
fn library_paths(prefix: &Path, cuda: bool) -> Vec<PathBuf> {
    let mut paths = vec![prefix.join("lib")];
    if cuda {
        paths.push(prefix.join("targets/x86_64-linux/lib"));
    }
    paths
}
fn rpath_flags(prefix: &Path, cuda: bool) -> Result<Vec<String>, RuntimeInstallError> {
    library_paths(prefix, cuda)
        .iter()
        .map(|path| Ok(format!("-Wl,-rpath,{}", text_path(path)?)))
        .collect()
}
fn wrapper(executable: &Path, flags: &[String]) -> Result<String, RuntimeInstallError> {
    Ok(format!(
        "#!/bin/sh\nexec {} {} \"$@\"\n",
        quote(&text_path(executable)?),
        flags
            .iter()
            .map(|flag| quote(flag))
            .collect::<Vec<_>>()
            .join(" ")
    ))
}
fn write_executable(path: &Path, text: &str) -> Result<(), RuntimeInstallError> {
    publish(path, text.as_bytes())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).map_err(io)?;
    }
    Ok(())
}
fn install_bindings(prefix: &Path, cuda: bool) -> Result<(), RuntimeInstallError> {
    let wrappers = prefix.join("scala-bin");
    if present(&wrappers)? || present(&prefix.join("scala-cuda"))? {
        return Err(invalid(
            "vendor payload collides with Scala's private binding namespace",
        ));
    }
    directories(&wrappers, true)?;
    let flags = rpath_flags(prefix, cuda)?;
    for (name, vendor) in [
        ("cc", "bin/x86_64-conda-linux-gnu-gcc"),
        ("gcc", "bin/x86_64-conda-linux-gnu-gcc"),
        ("c++", "bin/x86_64-conda-linux-gnu-g++"),
        ("g++", "bin/x86_64-conda-linux-gnu-g++"),
    ] {
        let compiler = selected_file(prefix, vendor)?;
        write_executable(&wrappers.join(name), &wrapper(&compiler, &flags)?)?;
    }
    if cuda {
        selected_file(prefix, "targets/x86_64-linux/include/cuda_runtime.h")?;
        selected_file(prefix, "targets/x86_64-linux/include/cuda.h")?;
        directories(&prefix.join("targets/x86_64-linux/lib"), false)?;
        let nvcc = selected_file(prefix, "bin/nvcc")?;
        let mut flags = Vec::new();
        for path in library_paths(prefix, true) {
            flags.extend([
                "-Xlinker".into(),
                "-rpath".into(),
                "-Xlinker".into(),
                text_path(&path)?,
            ]);
        }
        write_executable(&wrappers.join("nvcc"), &wrapper(&nvcc, &flags)?)?;
        // PyTorch's JIT expects CUDA_HOME/{bin,include,lib64}; do not overwrite
        // vendor files to manufacture that layout. All links stay in PREFIX.
        let facade = prefix.join("scala-cuda");
        directories(&facade.join("bin"), true)?;
        write_executable(&facade.join("bin/nvcc"), &wrapper(&nvcc, &flags)?)?;
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("../targets/x86_64-linux/include", facade.join("include"))
                .map_err(io)?;
            std::os::unix::fs::symlink("../targets/x86_64-linux/lib", facade.join("lib64"))
                .map_err(io)?;
        }
        #[cfg(not(unix))]
        return Err(RuntimeInstallError::Prerequisite(
            "Linux CUDA bindings require Unix symlinks".into(),
        ));
    }
    Ok(())
}
fn tools(prefix: &Path, cuda: bool) -> Result<BTreeMap<String, PathBuf>, RuntimeInstallError> {
    let mut tools = BTreeMap::new();
    for (name, relative) in [
        ("git", "bin/git"),
        ("cmake", "bin/cmake"),
        ("ninja", "bin/ninja"),
        ("make", "bin/make"),
        ("pkg-config", "bin/pkg-config"),
        ("cc", "bin/x86_64-conda-linux-gnu-gcc"),
        ("c++", "bin/x86_64-conda-linux-gnu-g++"),
    ] {
        let path = selected_file(prefix, relative)?;
        executable_mode(&path)?;
        tools.insert(name.into(), path);
    }
    if cuda {
        let path = selected_file(prefix, "bin/nvcc")?;
        executable_mode(&path)?;
        tools.insert("nvcc".into(), path);
        selected_file(prefix, "targets/x86_64-linux/include/cuda_runtime.h")?;
        directories(&prefix.join("targets/x86_64-linux/lib"), false)?;
    }
    directories(&prefix.join("lib"), false)?;
    Ok(tools)
}
fn build_environment(
    prefix: &Path,
    cuda: bool,
    work: &Path,
) -> Result<BTreeMap<String, String>, RuntimeInstallError> {
    let mut environment = isolated_environment(work);
    environment.insert(
        "PATH".into(),
        format!(
            "{}:{}:/usr/bin:/bin",
            text_path(&prefix.join("scala-bin"))?,
            text_path(&prefix.join("bin"))?
        ),
    );
    for (key, relative) in [("CC", "scala-bin/cc"), ("CXX", "scala-bin/c++")] {
        environment.insert(key.into(), text_path(&prefix.join(relative))?);
    }
    environment.insert("CMAKE_PREFIX_PATH".into(), text_path(prefix)?);
    environment.insert(
        "PKG_CONFIG_LIBDIR".into(),
        format!(
            "{}:{}",
            text_path(&prefix.join("lib/pkgconfig"))?,
            text_path(&prefix.join("share/pkgconfig"))?
        ),
    );
    if cuda {
        for key in ["CUDA_HOME", "CUDA_PATH"] {
            environment.insert(key.into(), text_path(&prefix.join("scala-cuda"))?);
        }
        environment.insert("CUDACXX".into(), text_path(&prefix.join("bin/nvcc"))?);
        for key in ["NVCC_CCBIN", "CUDAHOSTCXX"] {
            environment.insert(
                key.into(),
                text_path(&prefix.join("bin/x86_64-conda-linux-gnu-g++"))?,
            );
        }
    }
    Ok(environment)
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Ready {
    reference: LinuxDependencyReference,
    toolchain: RuntimeSourceBuildToolchain,
}

pub(super) struct LinuxPrepared {
    pub(super) tools: BTreeMap<String, PathBuf>,
    pub(super) environment: BTreeMap<String, String>,
    pub(super) toolchain: RuntimeSourceBuildToolchain,
    pub(super) reference: LinuxDependencyReference,
    recipe: LinuxDependencyRecipe,
    // Work cleanup precedes lease release.
    _work: tempfile::TempDir,
    pub(super) lease: Arc<File>,
    progress: Progress,
}
impl LinuxPrepared {
    pub(super) fn prefix(&self) -> &Path {
        &self.reference.prefix
    }

    pub(super) fn checked(
        &self,
        plan: &RuntimeSourceBuildPlan,
    ) -> Result<CheckedSourceBuildPrerequisites, RuntimeInstallError> {
        check_contract(plan, &self.recipe)?;
        Ok(CheckedSourceBuildPrerequisites {
            toolchain: self.toolchain.clone(),
            effective_cmake_configuration_arguments: bindings::effective_arguments(
                plan,
                self.prefix(),
                self.tools.contains_key("nvcc"),
            )?,
        })
    }

    pub(super) fn make_arguments(
        &self,
        original: &[&str],
    ) -> Result<Vec<String>, RuntimeInstallError> {
        make_arguments(self.prefix(), self.tools.contains_key("nvcc"), original)
    }

    /// Only adapter-selected executable names are accepted; no PATH lookup.
    pub(super) fn command(
        &self,
        program: &str,
        original: &[&str],
        cwd: Option<&Path>,
    ) -> Result<(PathBuf, Vec<String>), RuntimeInstallError> {
        let executable = self
            .tools
            .get(program)
            .ok_or_else(|| invalid("tool is outside the selected Linux dependency recipe"))?
            .clone();
        let mut arguments = Vec::new();
        if let Some(cwd) = cwd {
            if !cwd.is_absolute() {
                return Err(invalid("source command working directory must be absolute"));
            }
            match program {
                "git" | "make" => arguments.extend(["-C".into(), cwd.display().to_string()]),
                _ => {
                    return Err(invalid(
                        "use explicit CMake -S/-B/--build paths instead of an implicit cwd",
                    ));
                }
            }
        }
        if program == "make" {
            arguments.extend(self.make_arguments(original)?);
        } else {
            arguments.extend(original.iter().map(|argument| (*argument).to_owned()));
        }
        Ok((executable, arguments))
    }

    pub(super) async fn text(
        &self,
        program: &str,
        arguments: &[&str],
        cwd: Option<&Path>,
    ) -> Result<String, RuntimeInstallError> {
        let (executable, arguments) = self.command(program, arguments, cwd)?;
        let refs = arguments.iter().map(String::as_str).collect::<Vec<_>>();
        // capture_owned_command inherits the ambient environment. Use the
        // install capture here deliberately: it env_clear()s and passes the
        // native acquisition descriptor to the owned subprocess tree.
        let output = crate::supervisor::capture_install_command(
            &executable,
            &refs,
            &self.environment,
            Arc::clone(&self.progress),
            Some(self.lease.as_ref()),
        )
        .await?;
        if !output.success {
            return Err(RuntimeInstallError::SourceBuild(format!(
                "selected Linux tool {program} exited {:?}: {}",
                output.code,
                compact_identity(&format!("{}\n{}", output.stdout, output.stderr))
            )));
        }
        Ok(output.stdout.trim().to_owned())
    }
    pub(super) async fn run(
        &self,
        program: &str,
        arguments: &[&str],
        cwd: Option<&Path>,
    ) -> Result<(), RuntimeInstallError> {
        self.text(program, arguments, cwd).await.map(|_| ())
    }

    /// The destination is the managed runtime's operational child-environment
    /// map, never an inference Settings/profile map. No PATH, HOME, loader path,
    /// flags, Python path, credentials, or acquisition cache paths are persisted.
    pub(super) fn add_jit_environment(
        &self,
        destination: &mut BTreeMap<String, String>,
    ) -> Result<LinuxDependencyReference, RuntimeInstallError> {
        let mut additions = BTreeMap::new();
        for key in [
            "CC",
            "CXX",
            "CUDACXX",
            "NVCC_CCBIN",
            "CUDAHOSTCXX",
            "CUDA_HOME",
            "CUDA_PATH",
            "CMAKE_PREFIX_PATH",
            "PKG_CONFIG_LIBDIR",
        ] {
            if let Some(value) = self.environment.get(key) {
                if destination
                    .get(key)
                    .is_some_and(|existing| existing != value)
                {
                    return Err(invalid(format!(
                        "managed Python JIT environment already binds {key}"
                    )));
                }
                additions.insert(key.to_owned(), value.clone());
            }
        }
        destination.extend(additions);
        Ok(self.reference.clone())
    }
}
fn make_arguments(
    prefix: &Path,
    cuda: bool,
    original: &[&str],
) -> Result<Vec<String>, RuntimeInstallError> {
    let mut arguments = original
        .iter()
        .map(|argument| (*argument).to_owned())
        .collect::<Vec<_>>();
    for key in ["CC", "CXX", "NVCC", "CUDA_PATH"] {
        if original
            .iter()
            .any(|argument| argument.starts_with(&format!("{key}=")))
        {
            return Err(invalid(format!("competing Make compiler binding: {key}")));
        }
    }
    arguments.push(format!(
        "CC={}",
        quote(&text_path(&prefix.join("scala-bin/cc"))?)
    ));
    arguments.push(format!(
        "CXX={}",
        quote(&text_path(&prefix.join("scala-bin/c++"))?)
    ));
    if cuda {
        arguments.push(format!(
            "NVCC={}",
            quote(&text_path(&prefix.join("scala-bin/nvcc"))?)
        ));
        arguments.push(format!(
            "CUDA_PATH={}",
            text_path(&prefix.join("scala-cuda"))?
        ));
    }
    // The private CC/CXX and NVCC wrappers each inject native linker rpaths.
    // Do not pass GCC-only -Wl options to an upstream NVCC link command.
    Ok(arguments)
}
fn version(value: &str) -> Result<Vec<u64>, RuntimeInstallError> {
    value
        .split('.')
        .map(|part| {
            part.parse()
                .map_err(|_| invalid("non-numeric prerequisite version"))
        })
        .collect()
}
pub(super) fn check_contract(
    plan: &RuntimeSourceBuildPlan,
    recipe: &LinuxDependencyRecipe,
) -> Result<(), RuntimeInstallError> {
    let locked = |name: &str| {
        recipe
            .metadata()
            .package(name)
            .map(|package| package.version.as_str())
            .ok_or_else(|| invalid(format!("selected dependency lock lacks {name}")))
    };
    if plan.recipe.build_system == RuntimeSourceBuildSystem::Cmake
        && version(locked("cmake")?)? < version(&plan.prerequisites.minimum_cmake_version)?
    {
        return Err(RuntimeInstallError::Prerequisite(
            "pinned CMake is below the source contract".into(),
        ));
    }
    match recipe.metadata().cuda_release.as_deref() {
        Some(cuda) => {
            let cuda = version(cuda)?;
            if let Some(minimum) = &plan.prerequisites.minimum_cuda_version {
                if cuda < version(minimum)? {
                    return Err(RuntimeInstallError::Prerequisite(
                        "selected CUDA lock is below the source contract".into(),
                    ));
                }
            }
            if let Some(maximum) = &plan.prerequisites.maximum_cuda_version_exclusive {
                if cuda >= version(maximum)? {
                    return Err(RuntimeInstallError::Prerequisite(
                        "selected CUDA lock exceeds the source contract".into(),
                    ));
                }
            }
        }
        None if plan.prerequisites.minimum_cuda_version.is_some()
            || plan.prerequisites.maximum_cuda_version_exclusive.is_some()
            || plan.prerequisites.cuda_compiler.is_some() =>
        {
            return Err(invalid(
                "CPU dependency lock cannot satisfy a CUDA source contract",
            ));
        }
        None => {}
    }
    Ok(())
}
pub(super) fn host() -> Result<(), RuntimeInstallError> {
    if !cfg!(all(
        target_os = "linux",
        target_arch = "x86_64",
        target_env = "gnu"
    )) {
        return Err(RuntimeInstallError::Prerequisite(
            "managed Linux dependencies require native glibc Linux x86_64".into(),
        ));
    }
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        let value = unsafe { std::ffi::CStr::from_ptr(libc::gnu_get_libc_version()) }
            .to_str()
            .map_err(io)?;
        if version(value)? < vec![2, 28] {
            return Err(RuntimeInstallError::Prerequisite(
                "managed Linux dependencies require glibc 2.28 or newer".into(),
            ));
        }
    }
    Ok(())
}
async fn verify_conda_records(
    prefix: &Path,
    recipe: &LinuxDependencyRecipe,
) -> Result<(), RuntimeInstallError> {
    let metadata = prefix.join("conda-meta");
    directories(&metadata, false)?;
    let mut observed = BTreeMap::new();
    for entry in fs::read_dir(metadata).map_err(io)? {
        let entry = entry.map_err(io)?;
        if entry
            .path()
            .extension()
            .and_then(|extension| extension.to_str())
            != Some("json")
        {
            continue;
        }
        let record: serde_json::Value =
            serde_json::from_slice(&read_bounded(&entry.path(), 16 * 1024 * 1024)?).map_err(io)?;
        let name = record["name"]
            .as_str()
            .ok_or_else(|| invalid("conda record lacks a package name"))?
            .to_owned();
        if observed.insert(name, record).is_some() {
            return Err(invalid("duplicate installed conda package"));
        }
    }
    if observed.len() != recipe.metadata().packages.len() {
        return Err(invalid(
            "installed conda closure differs from the explicit lock",
        ));
    }
    for package in &recipe.metadata().packages {
        let record = observed
            .get(&package.name)
            .ok_or_else(|| invalid("locked package is missing from conda-meta"))?;
        for (key, expected) in [
            ("version", &package.version),
            ("build", &package.build),
            ("url", &package.url),
        ] {
            if record[key].as_str() != Some(expected.as_str()) {
                return Err(invalid(format!(
                    "installed package does not bind locked {key}"
                )));
            }
        }
        if !record["sha256"]
            .as_str()
            .is_some_and(|hash| hash.eq_ignore_ascii_case(&package.sha256))
        {
            return Err(invalid(
                "installed package does not bind its locked SHA-256",
            ));
        }
        if record
            .get("size")
            .is_some_and(|size| size.as_u64() != Some(package.size))
        {
            return Err(invalid(
                "installed package size disagrees with lock evidence",
            ));
        }
    }
    Ok(())
}

impl RuntimeInstaller {
    pub(super) async fn prepare_linux_dependencies(
        &self,
        available: &AvailableRuntime,
        recipe: &LinuxDependencyRecipe,
    ) -> Result<LinuxPrepared, RuntimeInstallError> {
        host()?;
        if available.identity.platform != "linux" || available.identity.architecture != "x86_64" {
            return Err(invalid(
                "Linux dependency recipe was attached to a different runtime platform",
            ));
        }
        let native = self.cache_root.join("native");
        let lease = native_lease(&native)?;
        let cache = native.join("linux");
        owned(&cache, CACHE_OWNER)?;
        let durable = self
            .store
            .root()
            .parent()
            .ok_or_else(|| invalid("runtime store has no data parent"))?
            .join("runtime-dependencies/linux");
        owned(&durable, DURABLE_OWNER)?;
        let environment_sha256 = environment_digest(recipe);
        let environment_root = durable.join(&environment_sha256);
        owned(
            &environment_root,
            &format!("{IMPLEMENTATION}:{environment_sha256}"),
        )?;
        let prefix = environment_root.join("prefix");
        text_path(&prefix)?;
        let ready_path = environment_root.join("ready.json");
        let inventory_path = environment_root.join("inventory.json");
        let new_environment = !present(&ready_path)?;
        if new_environment && fs::read_dir(&environment_root).map_err(io)?.count() > 1 {
            // No ready receipt means this owned prefix was never published to a runtime.
            // The global acquisition lease excludes concurrent preparations.
            fs::remove_dir_all(&environment_root).map_err(io)?;
            owned(
                &environment_root,
                &format!("{IMPLEMENTATION}:{environment_sha256}"),
            )?;
        }
        let work_parent = cache.join("work");
        directories(&work_parent, true)?;
        let work = tempfile::Builder::new()
            .prefix("install-")
            .tempdir_in(&work_parent)
            .map_err(io)?;
        let this = self.clone();
        let candidate = available.clone();
        let progress: Progress = Arc::new(move |detail| {
            this.emit(
                &candidate,
                RuntimeOperationPhase::Installing,
                None,
                None,
                detail.trim(),
            );
        });
        self.emit(
            available,
            RuntimeOperationPhase::Installing,
            None,
            None,
            "Preparing or verifying the adapter-selected pinned Linux dependency environment",
        );
        let mut prepared;
        let mut pending = PendingEnvironment {
            root: environment_root.clone(),
            armed: new_environment,
        };
        if !new_environment {
            let ready: Ready =
                serde_json::from_slice(&read_bounded(&ready_path, 1024 * 1024)?).map_err(io)?;
            let reference = &ready.reference;
            if reference.schema_version != 1
                || reference.recipe_sha256 != recipe.sha256()
                || reference.environment_sha256 != environment_sha256
                || reference.bootstrap_sha256 != BOOTSTRAP_SHA256
                || reference.implementation_version != IMPLEMENTATION
                || reference.prefix != prefix
                || read_bounded(&environment_root.join("lock.json"), 4 * 1024 * 1024)?
                    != recipe.lock_bytes()
                || read_bounded(&environment_root.join("explicit.txt"), 4 * 1024 * 1024)?
                    != recipe.explicit_lock().as_bytes()
            {
                return Err(invalid(
                    "durable dependency identity or retained lock evidence changed",
                ));
            }
            let bytes = read_bounded(&inventory_path, MAX_INVENTORY_BYTES)?;
            if digest(&bytes) != reference.inventory_sha256 {
                return Err(invalid("durable dependency inventory digest changed"));
            }
            verify_inventory(&prefix, &bytes).await?;
            verify_conda_records(&prefix, recipe).await?;
            prepared = LinuxPrepared {
                tools: tools(&prefix, recipe.metadata().cuda_release.is_some())?,
                environment: build_environment(
                    &prefix,
                    recipe.metadata().cuda_release.is_some(),
                    work.path(),
                )?,
                toolchain: ready.toolchain,
                reference: ready.reference,
                recipe: recipe.clone(),
                _work: work,
                lease,
                progress,
            };
        } else {
            // A failed or interrupted final-prefix installation is never reused
            // or repaired in place. An unreferenced-environment GC can remove it
            // under the shared lease; this backend never deletes durable roots.
            if present(&prefix)? || present(&environment_root.join("lock.json"))? {
                return Err(invalid(
                    "incomplete durable dependency environment; collect it only after proving it has no runtime references",
                ));
            }
            publish(&environment_root.join("lock.json"), recipe.lock_bytes())?;
            publish(
                &environment_root.join("explicit.txt"),
                recipe.explicit_lock().as_bytes(),
            )?;
            publish(
                &environment_root.join("bootstrap.json"),
                &serde_json::to_vec(&serde_json::json!({
                    "version": "2.9.0-0", "url": BOOTSTRAP_URL, "size": BOOTSTRAP_SIZE,
                    "sha256": BOOTSTRAP_SHA256, "implementation_version": IMPLEMENTATION,
                }))
                .map_err(io)?,
            )?;
            check_package_cache(&cache, recipe, false).await?;
            let mamba = bootstrap(&cache).await?;
            let arguments =
                mamba_arguments(&cache, &prefix, &environment_root.join("explicit.txt"));
            let environment = isolated_environment(work.path());
            let result = crate::supervisor::capture_install_command(
                &mamba,
                &arguments.iter().map(String::as_str).collect::<Vec<_>>(),
                &environment,
                Arc::clone(&progress),
                Some(lease.as_ref()),
            )
            .await?;
            if !result.success {
                return Err(RuntimeInstallError::Prerequisite(format!(
                    "pinned Linux dependency acquisition exited {:?}; no ready receipt was published",
                    result.code
                )));
            }
            verify_conda_records(&prefix, recipe).await?;
            check_package_cache(&cache, recipe, true).await?;
            let cuda = recipe.metadata().cuda_release.is_some();
            install_bindings(&prefix, cuda)?;
            let selected = tools(&prefix, cuda)?;
            let environment = build_environment(&prefix, cuda, work.path())?;
            prepared = LinuxPrepared {
                tools: selected,
                environment,
                toolchain: RuntimeSourceBuildToolchain {
                    cmake_version: String::new(),
                    ninja_version: String::new(),
                    make_version: String::new(),
                    cpp_compiler: String::new(),
                    nvcc_version: String::new(),
                    pkg_config_version: String::new(),
                    system_dependencies: BTreeMap::new(),
                },
                reference: LinuxDependencyReference {
                    schema_version: 1,
                    recipe_sha256: recipe.sha256().into(),
                    environment_sha256,
                    bootstrap_sha256: BOOTSTRAP_SHA256.into(),
                    implementation_version: IMPLEMENTATION.into(),
                    inventory_sha256: String::new(),
                    prefix,
                },
                recipe: recipe.clone(),
                _work: work,
                lease,
                progress,
            };
            prepared.toolchain.cmake_version =
                compact_identity(&prepared.text("cmake", &["--version"], None).await?);
            prepared.toolchain.ninja_version =
                compact_identity(&prepared.text("ninja", &["--version"], None).await?);
            prepared.toolchain.make_version =
                compact_identity(&prepared.text("make", &["--version"], None).await?);
            prepared.toolchain.cpp_compiler =
                compact_identity(&prepared.text("c++", &["--version"], None).await?);
            prepared.toolchain.pkg_config_version =
                compact_identity(&prepared.text("pkg-config", &["--version"], None).await?);
            if cuda {
                prepared.toolchain.nvcc_version =
                    compact_identity(&prepared.text("nvcc", &["--version"], None).await?);
            }
            for (key, value) in [
                (
                    "linux_dependency_recipe_sha256",
                    prepared.reference.recipe_sha256.clone(),
                ),
                (
                    "linux_dependency_environment_sha256",
                    prepared.reference.environment_sha256.clone(),
                ),
                ("linux_dependency_prefix", text_path(prepared.prefix())?),
                ("linux_dependency_bootstrap_sha256", BOOTSTRAP_SHA256.into()),
                ("linux_dependency_implementation", IMPLEMENTATION.into()),
            ] {
                prepared
                    .toolchain
                    .system_dependencies
                    .insert(key.into(), value);
            }
            let bytes = serde_json::to_vec(&inventory(prepared.prefix()).await?).map_err(io)?;
            if bytes.len() as u64 > MAX_INVENTORY_BYTES {
                return Err(invalid("serialized dependency inventory is too large"));
            }
            prepared.reference.inventory_sha256 = digest(&bytes);
            publish(&inventory_path, &bytes)?;
            let ready = Ready {
                reference: prepared.reference.clone(),
                toolchain: prepared.toolchain.clone(),
            };
            // Publication is the last fallible preparation step. No TEMP rename:
            // micromamba relocated packages directly to their permanent prefix.
            publish(&ready_path, &serde_json::to_vec(&ready).map_err(io)?)?;
        }
        cleanup::trim_archives(&cache, 2 * 1024 * 1024 * 1024)?;
        pending.armed = false;
        Ok(prepared)
    }
}

// An environment without a published ready receipt has never been admitted by
// any runtime. The shared acquisition lease prevents concurrent consumers.
struct PendingEnvironment {
    root: PathBuf,
    armed: bool,
}
impl Drop for PendingEnvironment {
    fn drop(&mut self) {
        if self.armed
            && !self.root.join("ready.json").exists()
            && regular(&self.root.join(".scala-owner")).is_ok()
        {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
}
impl LinuxPrepared {
    pub(super) async fn validate_plan(
        &self,
        plan: &RuntimeSourceBuildPlan,
    ) -> Result<(), RuntimeInstallError> {
        check_contract(plan, &self.recipe)?;
        let standard = plan
            .prerequisites
            .minimum_cpp_standard
            .or(plan.prerequisites.requires_cpp20_compiler.then_some(20));
        if let Some(standard) = standard {
            let source = self._work.path().join("compiler-probe.cpp");
            let body = if standard >= 20 {
                "#include <span>\nint main(){int a[1]{};std::span<int> s{a};return int(s.size())-1;}\n"
            } else {
                "#include <optional>\nint main(){std::optional<int> v{1};return *v-1;}\n"
            };
            fs::write(&source, body).map_err(io)?;
            self.text(
                "c++",
                &[
                    &format!("-std=c++{standard}"),
                    "-fsyntax-only",
                    &source.display().to_string(),
                ],
                None,
            )
            .await?;
        }
        for (module, minimum) in &plan.prerequisites.pkg_config_modules {
            self.text(
                "pkg-config",
                &[&format!("--atleast-version={minimum}"), module],
                None,
            )
            .await?;
        }
        if self.tools.contains_key("nvcc") {
            let nvcc = self.text("nvcc", &["--version"], None).await?;
            let actual = version_after(&nvcc, "release")
                .ok_or_else(|| invalid("cannot identify selected CUDA compiler"))?;
            if let Some(minimum) = &plan.prerequisites.minimum_cuda_version {
                require_minimum_version("CUDA", &actual, minimum)?;
            }
            if let Some(maximum) = &plan.prerequisites.maximum_cuda_version_exclusive {
                require_version_below("CUDA", &actual, maximum)?;
            }
            if let Some(arguments) = &self.checked(plan)?.effective_cmake_configuration_arguments {
                if let Some(targets) = required_cuda_compiler_targets(arguments)? {
                    let help = self.text("nvcc", &["--help"], None).await?;
                    require_cuda_compiler_targets("real", &targets.real, &help)?;
                    require_cuda_compiler_targets("virtual", &targets.virtual_targets, &help)?;
                }
            }
        }
        Ok(())
    }
    pub(super) async fn stage_runtime_files(
        &self,
        root: &Path,
        toolchain: &mut RuntimeSourceBuildToolchain,
    ) -> Result<(), RuntimeInstallError> {
        let evidence = root.join("source-dependencies");
        fs::create_dir(&evidence).map_err(io)?;
        publish(&evidence.join("linux.lock.json"), self.recipe.lock_bytes())?;
        publish(
            &root.join("linux-dependencies.json"),
            &serde_json::to_vec_pretty(&self.reference).map_err(io)?,
        )?;
        toolchain.system_dependencies.insert(
            "linux_dependency_inventory_sha256".into(),
            self.reference.inventory_sha256.clone(),
        );
        Ok(())
    }
}
