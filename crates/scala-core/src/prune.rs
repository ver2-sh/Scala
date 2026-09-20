//! Application-owned storage planning and exclusion. Configuration is never a root.
use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Component, Path, PathBuf};

use serde::Serialize;

use crate::{AppPaths, RuntimeDescriptor};

#[derive(Debug)]
pub struct StorageLease(#[allow(dead_code)] File);

impl Drop for StorageLease {
    fn drop(&mut self) {
        // Release ownership even if an unrelated concurrent fork briefly holds
        // a duplicate descriptor before exec closes it.
        let _ = fs2::FileExt::unlock(&self.0);
    }
}

impl StorageLease {
    /// Sessions hold a shared lease for their entire lifetime. Prune takes an
    /// exclusive lease, including during planning. Never unlink this lock inode.
    pub fn acquire(paths: &AppPaths, exclusive: bool, dry_run: bool) -> io::Result<Option<Self>> {
        let path = paths.data_dir.join(".storage.lock");
        validate_ancestors(&path)?;
        if dry_run && !path.try_exists()? {
            return Ok(None);
        }
        if !dry_run {
            fs::create_dir_all(&paths.data_dir)?;
        }
        if let Ok(metadata) = fs::symlink_metadata(&path)
            && is_link(&metadata)
        {
            return Err(unsafe_path(&path));
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(!dry_run)
            .truncate(false)
            .open(&path)?;
        let result = if exclusive {
            fs2::FileExt::try_lock_exclusive(&file)
        } else {
            fs2::FileExt::try_lock_shared(&file)
        };
        result.map_err(|error| io::Error::new(error.kind(),
            "application storage is in use; stop Scala/control and other storage operations before pruning"))?;
        Ok(Some(Self(file)))
    }
}

/// Also honor older runtime/model per-operation leases. Keep their inodes in
/// place even during reset so waiters cannot acquire a different lock file.
pub fn lock_existing_operations(paths: &AppPaths) -> io::Result<Vec<File>> {
    let mut locks = Vec::new();
    for root in [
        paths.runtimes_dir.join(".locks"),
        paths.cache_dir.join("model-downloads/locks"),
    ] {
        validate_ancestors(&root.join("lock"))?;
        if !root.try_exists()? {
            continue;
        }
        for entry in walkdir::WalkDir::new(&root).follow_links(false) {
            let entry = entry.map_err(io::Error::other)?;
            if is_link(&fs::symlink_metadata(entry.path())?) {
                return Err(unsafe_path(entry.path()));
            }
            if entry.file_type().is_file() {
                let file = OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(entry.path())?;
                fs2::FileExt::try_lock_exclusive(&file).map_err(|_| {
                    io::Error::other("a runtime/model operation is active; retry after it finishes")
                })?;
                locks.push(file);
            }
        }
    }
    let startup = paths.state_dir.join("runtime/server-start.lock");
    validate_ancestors(&startup)?;
    if startup.try_exists()? {
        if is_link(&fs::symlink_metadata(&startup)?) {
            return Err(unsafe_path(&startup));
        }
        let file = OpenOptions::new().read(true).write(true).open(startup)?;
        fs2::FileExt::try_lock_exclusive(&file)
            .map_err(|_| io::Error::other("a Server startup is active"))?;
        locks.push(file);
    }
    Ok(locks)
}

#[derive(Debug, Clone, Serialize)]
pub struct PruneEntry {
    pub path: PathBuf,
    pub category: String,
    pub expected_bytes: u64,
    pub files: u64,
    #[serde(skip)]
    root: PathBuf,
}

#[derive(Debug, Serialize)]
pub struct PrunePlan {
    pub mode: &'static str,
    pub dry_run: bool,
    pub categories: Vec<String>,
    pub entries: Vec<PruneEntry>,
    pub protected: Vec<String>,
    pub expected_bytes: u64,
    pub removed_paths: u64,
    pub removed_files: u64,
    /// Free-space delta is noisy in the presence of unrelated filesystem activity.
    pub filesystem_reclaimed_bytes: Option<u64>,
    pub filesystem_reclaimed_by_root: std::collections::BTreeMap<PathBuf, u64>,
}

impl PrunePlan {
    pub fn new(all: bool, dry_run: bool) -> Self {
        Self {
            mode: if all { "all" } else { "normal" },
            dry_run,
            categories: vec![],
            entries: vec![],
            protected: vec![],
            expected_bytes: 0,
            removed_paths: 0,
            removed_files: 0,
            filesystem_reclaimed_bytes: None,
            filesystem_reclaimed_by_root: Default::default(),
        }
    }

    pub fn add(&mut self, root: &Path, path: &Path, category: &str) -> io::Result<()> {
        validate_target(root, path)?;
        if !self.categories.iter().any(|value| value == category) {
            self.categories.push(category.to_owned());
        }
        let Some((expected_bytes, files)) = inventory(path)? else {
            return Ok(());
        };
        if self
            .entries
            .iter()
            .any(|entry| path.starts_with(&entry.path))
        {
            return Ok(());
        }
        self.entries.retain(|entry| !entry.path.starts_with(path));
        self.entries.push(PruneEntry {
            path: path.to_owned(),
            root: root.to_owned(),
            category: category.to_owned(),
            expected_bytes,
            files,
        });
        self.expected_bytes = self.entries.iter().map(|entry| entry.expected_bytes).sum();
        Ok(())
    }

    pub fn children(&mut self, root: &Path, category: &str) -> io::Result<()> {
        validate_ancestors(&root.join("child"))?;
        if !self.categories.iter().any(|value| value == category) {
            self.categories.push(category.to_owned());
        }
        let children = match fs::read_dir(root) {
            Ok(children) => children,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
        };
        for child in children {
            self.add(root, &child?.path(), category)?;
        }
        Ok(())
    }

    pub fn operational_storage(&mut self, paths: &AppPaths) -> io::Result<()> {
        for name in ["servers", "failures"] {
            self.children(
                &paths.state_dir.join("runtime").join(name),
                "stale runtime state",
            )?;
        }
        if self.mode == "all" {
            self.children(&paths.log_dir, "logs")?;
            // Owning libraries handle these two cache namespaces and preserve
            // their stable operation-lock files. Other cache children are disposable.
            if paths.cache_dir.try_exists()? {
                for child in fs::read_dir(&paths.cache_dir)? {
                    let child = child?;
                    if child.file_name() != "model-downloads"
                        && child.file_name() != "runtime-packs"
                        && child.file_name() != "app-update.lock"
                    {
                        self.add(&paths.cache_dir, &child.path(), "application caches")?;
                    }
                }
            }
        } else {
            self.categories.push("obsolete logs".into());
            if paths.log_dir.try_exists()? {
                let mut logs = fs::read_dir(&paths.log_dir)?.collect::<io::Result<Vec<_>>>()?;
                logs.retain(|entry| {
                    entry
                        .file_name()
                        .to_string_lossy()
                        .starts_with("scala.log.")
                });
                logs.sort_by_key(|entry| entry.file_name());
                logs.pop();
                for log in logs {
                    self.add(&paths.log_dir, &log.path(), "obsolete logs")?;
                }
            }
        }
        Ok(())
    }

    /// Validate the whole plan before the first mutation. The caller retains
    /// its exclusive StorageLease from planning through this operation.
    pub fn execute(&mut self) -> io::Result<()> {
        for entry in &self.entries {
            validate_target(&entry.root, &entry.path)?;
        }
        if self.dry_run {
            return Ok(());
        }
        let mut volumes = std::collections::BTreeMap::new();
        for entry in &self.entries {
            // Keep one sample per managed root; roots may share a volume, so
            // report the largest delta rather than double-counting it.
            if let Ok(free) = fs2::available_space(&entry.root) {
                volumes.insert(entry.root.clone(), free);
            }
        }
        for entry in &self.entries {
            validate_target(&entry.root, &entry.path)?;
            let Some((_, files)) = inventory(&entry.path)? else {
                continue;
            };
            remove_tree(&entry.path)?;
            self.removed_paths += 1;
            self.removed_files += files;
        }
        self.filesystem_reclaimed_by_root = volumes
            .into_iter()
            .filter_map(|(root, before)| {
                fs2::available_space(&root)
                    .ok()
                    .map(|after| (root, after.saturating_sub(before)))
            })
            .collect();
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let mut devices = std::collections::BTreeMap::new();
            for (root, delta) in &self.filesystem_reclaimed_by_root {
                if let Ok(metadata) = fs::metadata(root) {
                    devices.entry(metadata.dev()).or_insert(*delta);
                }
            }
            self.filesystem_reclaimed_bytes = (!devices.is_empty()).then(|| devices.values().sum());
        }
        // On other platforms report per-root measurements without guessing
        // whether multiple roots share a volume (which would double-count).
        Ok(())
    }
}

pub fn validate_target(root: &Path, path: &Path) -> io::Result<()> {
    if path == root || !path.starts_with(root) {
        return Err(unsafe_path(path));
    }
    validate_ancestors(path)?;
    if root.exists() {
        let canonical = root.canonicalize()?;
        if let Some(parent) = path.parent()
            && parent.exists()
            && !parent.canonicalize()?.starts_with(canonical)
        {
            return Err(unsafe_path(path));
        }
    }
    Ok(())
}

pub fn validate_ancestors(path: &Path) -> io::Result<()> {
    if !path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, Component::ParentDir))
    {
        return Err(unsafe_path(path));
    }
    for parent in path.ancestors().skip(1) {
        match fs::symlink_metadata(parent) {
            Ok(metadata) if is_link(&metadata) => return Err(unsafe_path(parent)),
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn unsafe_path(path: &Path) -> io::Error {
    io::Error::other(format!(
        "refusing unsafe or linked storage path: {}",
        path.display()
    ))
}

fn is_link(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return true;
        }
    }
    metadata.file_type().is_symlink()
}

fn inventory(path: &Path) -> io::Result<Option<(u64, u64)>> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if is_link(&metadata) {
        return Ok(Some((0, 1)));
    }
    if !metadata.is_dir() {
        return Ok(Some((metadata.len(), 1)));
    }
    let mut total = (0, 0);
    for child in fs::read_dir(path)? {
        if let Some((size, files)) = inventory(&child?.path())? {
            total.0 += size;
            total.1 += files;
        }
    }
    Ok(Some(total))
}

fn remove_tree(path: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if is_link(&metadata) {
        if metadata.is_dir() {
            fs::remove_dir(path)
        } else {
            fs::remove_file(path)
        }
    } else if metadata.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
}

/// Older binaries do not hold a storage lease. Refuse if their recorded PID
/// still exists, even when health/control endpoints are unreachable.
pub fn ensure_no_server_process(paths: &AppPaths) -> io::Result<()> {
    let root = paths.state_dir.join("runtime/servers");
    validate_ancestors(&root.join("descriptor"))?;
    let entries = match fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    for entry in entries {
        let path = entry?.path();
        if is_link(&fs::symlink_metadata(&path)?) {
            return Err(unsafe_path(&path));
        }
        if path.extension().is_none_or(|value| value != "json") {
            continue;
        }
        let descriptor: RuntimeDescriptor =
            serde_json::from_slice(&fs::read(&path)?).map_err(|error| {
                io::Error::other(format!("cannot prove server is stopped: {error}"))
            })?;
        if descriptor.schema_version != 2 || process_may_exist(descriptor.process_id) {
            return Err(io::Error::other(
                "a recorded Server/control process may still be running; stop it before pruning",
            ));
        }
    }
    Ok(())
}

fn process_may_exist(pid: u32) -> bool {
    if pid == 0 {
        return true;
    }
    #[cfg(unix)]
    {
        let Ok(pid) = i32::try_from(pid) else {
            return false;
        };
        // Signal zero checks existence without sending a signal.
        unsafe {
            libc::kill(pid, 0) == 0
                || io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
        }
    }
    #[cfg(windows)]
    {
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut std::ffi::c_void;
            fn CloseHandle(handle: *mut std::ffi::c_void) -> i32;
            fn GetLastError() -> u32;
        }
        unsafe {
            let handle = OpenProcess(0x1000, 0, pid);
            if handle.is_null() {
                GetLastError() != 87
            } else {
                CloseHandle(handle);
                true
            }
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        true
    }
}
