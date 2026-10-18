//! Process-scoped short aliases for verified Windows runtime paths.
//! No file copying, storage migration, driver changes or capability decisions.
use std::{
    fs, io,
    os::windows::{ffi::OsStrExt, fs::OpenOptionsExt, io::AsRawHandle},
    path::{Path, PathBuf},
};
use windows_sys::Win32::{
    Foundation::GENERIC_WRITE,
    Storage::FileSystem::{
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE,
        FILE_SHARE_READ, FILE_SHARE_WRITE,
    },
    System::IO::DeviceIoControl,
};

pub(super) struct RuntimeLaunchAlias {
    executable: PathBuf,
    link: PathBuf,
    target_parent: PathBuf,
    _root: tempfile::TempDir,
}
impl RuntimeLaunchAlias {
    pub(super) fn prepare(executable: &Path) -> io::Result<Option<Self>> {
        if executable.as_os_str().encode_wide().count() < 248 {
            return Ok(None);
        }
        let target = fs::canonicalize(executable)?;
        if !fs::metadata(&target)?.is_file() {
            return Err(io::Error::other("runtime executable is not a file"));
        }
        let target_parent = target
            .parent()
            .ok_or_else(|| io::Error::other("runtime has no parent"))?
            .to_owned();
        let root = tempfile::Builder::new().prefix("scala-run-").tempdir()?;
        let link = root.path().join("runtime");
        let short_executable = link.join(
            target
                .file_name()
                .ok_or_else(|| io::Error::other("runtime has no filename"))?,
        );
        if short_executable.as_os_str().encode_wide().count() >= 248 {
            return Err(io::Error::other(
                "Windows temporary directory is too long for a runtime launch alias",
            ));
        }
        fs::create_dir(&link)?;
        create_junction(&link, &target_parent)?;
        let alias = Self {
            executable: short_executable,
            link,
            target_parent,
            _root: root,
        };
        if fs::canonicalize(&alias.executable)? != target {
            return Err(io::Error::other(
                "Windows launch alias does not resolve to the verified executable",
            ));
        }
        Ok(Some(alias))
    }
    pub(super) fn executable(&self) -> &Path {
        &self.executable
    }
    pub(super) fn working_directory(&self, directory: &Path) -> Option<&Path> {
        fs::canonicalize(directory)
            .ok()
            .filter(|p| p == &self.target_parent)
            .map(|_| self.link.as_path())
    }
}
impl Drop for RuntimeLaunchAlias {
    fn drop(&mut self) {
        // Unlink the directory junction itself; never recurse into the runtime.
        let _ = fs::remove_dir(&self.link);
    }
}

fn create_junction(link: &Path, target: &Path) -> io::Result<()> {
    let wide = target.as_os_str().encode_wide().collect::<Vec<_>>();
    let extended = "\\\\?\\".encode_utf16().collect::<Vec<_>>();
    let rest = wide.strip_prefix(extended.as_slice()).ok_or_else(|| {
        io::Error::other("junction target is not an absolute canonical Windows path")
    })?;
    let mut substitute = "\\??\\".encode_utf16().collect::<Vec<_>>();
    substitute.extend_from_slice(rest);
    let sub_bytes = substitute.len() * 2;
    let print_bytes = wide.len() * 2;
    let data_bytes = 8 + sub_bytes + 2 + print_bytes + 2;
    if data_bytes + 8 > 16 * 1024 {
        return Err(io::Error::other(
            "runtime junction target exceeds Windows reparse limit",
        ));
    }
    let mut buffer = Vec::with_capacity(data_bytes + 8);
    buffer.extend_from_slice(&0xa0000003u32.to_le_bytes()); // IO_REPARSE_TAG_MOUNT_POINT
    buffer.extend_from_slice(&(data_bytes as u16).to_le_bytes());
    buffer.extend_from_slice(&0u16.to_le_bytes());
    for field in [
        0,
        sub_bytes as u16,
        (sub_bytes + 2) as u16,
        print_bytes as u16,
    ] {
        buffer.extend_from_slice(&field.to_le_bytes());
    }
    for unit in substitute.into_iter().chain([0]).chain(wide).chain([0]) {
        buffer.extend_from_slice(&unit.to_le_bytes());
    }
    let handle = fs::OpenOptions::new()
        .access_mode(GENERIC_WRITE)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(link)?;
    let mut returned = 0;
    // The directory was exclusively created beneath our fresh tempfile root.
    let ok = unsafe {
        DeviceIoControl(
            handle.as_raw_handle() as _,
            0x000900a4,
            buffer.as_ptr() as _,
            buffer.len() as u32,
            std::ptr::null_mut(),
            0,
            &mut returned,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn long_runtime_alias_resolves_exact_bytes_and_cleans_only_itself() {
        let root = tempfile::tempdir().unwrap();
        let base = fs::canonicalize(root.path()).unwrap();
        let mut path = base.clone();
        for i in 0..6 {
            path = path.join(format!("runtime-component-{i}-{}", "a".repeat(24)));
        }
        fs::create_dir_all(&path).unwrap();
        let exe = path.join("native-test.exe");
        fs::write(&exe, b"original verified bytes").unwrap();
        let original = fs::canonicalize(&exe).unwrap();
        let alias = RuntimeLaunchAlias::prepare(&exe).unwrap().unwrap();
        let short = alias.executable().to_owned();
        let temporary_root = alias._root.path().to_owned();
        assert_eq!(fs::canonicalize(&short).unwrap(), original);
        assert_eq!(fs::read(&short).unwrap(), b"original verified bytes");
        assert!(short.as_os_str().encode_wide().count() < 248);
        assert_eq!(alias.working_directory(&path), Some(alias.link.as_path()));
        assert!(alias.working_directory(&base).is_none());
        drop(alias);
        assert!(!temporary_root.exists());
        assert_eq!(fs::read(&exe).unwrap(), b"original verified bytes");
    }
    #[test]
    fn ordinary_runtime_paths_do_not_create_aliases() {
        assert!(
            RuntimeLaunchAlias::prepare(Path::new("C:\\runtime\\engine.exe"))
                .unwrap()
                .is_none()
        );
    }
}
