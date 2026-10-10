//! Windows runtimes use their real isolated interpreter as the entrypoint.
//! The adjacent descriptor supplies structured, immutable wrapper arguments.
use super::*;
use std::ffi::OsString;

pub(super) fn runtime_environment() -> Result<BTreeMap<String, String>, EngineError> {
    let mut environment = BTreeMap::new();
    if cfg!(windows) {
        // Winsock (including Python's _overlapped module) requires this OS
        // location when the parent environment is cleared. Do not inherit
        // PYTHONPATH, PATH, or model/shim configuration from the caller.
        let root = std::env::var("SystemRoot")
            .map_err(|_| invalid("Windows SystemRoot is unavailable"))?;
        if !Path::new(&root).is_absolute() || !Path::new(&root).is_dir() {
            return Err(invalid(
                "Windows SystemRoot must name an existing absolute directory",
            ));
        }
        environment.insert("SystemRoot".into(), root);
    }
    Ok(environment)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WindowsLauncher {
    protocol: String,
    interpreter: PathBuf,
    runner: PathBuf,
}

pub(super) async fn windows_launcher(path: &Path) -> Result<PathBuf, EngineError> {
    Ok(read_windows_launcher(path).await?.runner)
}

async fn read_windows_launcher(path: &Path) -> Result<WindowsLauncher, EngineError> {
    use tokio::io::AsyncReadExt;
    let descriptor = path.with_file_name("scala-native-decision.json");
    let mut bytes = Vec::new();
    tokio::fs::File::open(&descriptor)
        .await
        .map_err(|e| invalid(e.to_string()))?
        .take(65537)
        .read_to_end(&mut bytes)
        .await
        .map_err(|e| invalid(e.to_string()))?;
    if bytes.len() > 65536 {
        return Err(invalid("Native Windows launcher exceeds inspection bound"));
    }
    let launcher: WindowsLauncher =
        serde_json::from_slice(&bytes).map_err(|e| invalid(e.to_string()))?;
    if launcher.protocol != PROTOCOL
        || !launcher.interpreter.is_absolute()
        || !launcher.runner.is_absolute()
        || path
            .extension()
            .is_none_or(|e| !e.eq_ignore_ascii_case("exe"))
        || tokio::fs::canonicalize(&launcher.interpreter)
            .await
            .map_err(|e| invalid(e.to_string()))?
            != tokio::fs::canonicalize(path)
                .await
                .map_err(|e| invalid(e.to_string()))?
        || !tokio::fs::metadata(&launcher.runner)
            .await
            .is_ok_and(|m| m.is_file())
    {
        return Err(invalid(
            "Invalid native Windows interpreter/wrapper binding",
        ));
    }
    Ok(launcher)
}

pub(super) async fn executable(path: &Path) -> Result<PathBuf, EngineError> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::{OsStrExt, OsStringExt};
        // Execute the bound interpreter spelling, rather than propagating
        // Rust's canonical device prefix into Python's venv/site-package paths.
        // Identity and source validation still compare canonical files. Retain
        // device syntax when it is needed for an actually long executable path.
        let declared = read_windows_launcher(path).await?.interpreter;
        let wide: Vec<u16> = declared.as_os_str().encode_wide().collect();
        let unc: Vec<u16> = "\\\\?\\UNC\\".encode_utf16().collect();
        let prefix: Vec<u16> = "\\\\?\\".encode_utf16().collect();
        let ordinary = if wide.starts_with(&unc) {
            let mut value: Vec<u16> = "\\\\".encode_utf16().collect();
            value.extend_from_slice(&wide[8..]);
            value
        } else if wide.starts_with(&prefix) {
            wide[4..].to_vec()
        } else {
            wide
        };
        if ordinary.len() < 260 {
            Ok(PathBuf::from(OsString::from_wide(&ordinary)))
        } else {
            Ok(declared)
        }
    }
    #[cfg(not(windows))]
    Ok(path.to_owned())
}

pub(super) async fn launch_prefix(path: &Path) -> Result<Vec<OsString>, EngineError> {
    if cfg!(windows) {
        Ok(vec![
            "-I".into(),
            windows_launcher(path).await?.into_os_string(),
        ])
    } else {
        Ok(Vec::new())
    }
}

pub(super) async fn observation_launcher(path: &Path) -> Option<(PathBuf, PathBuf, Vec<PathBuf>)> {
    if cfg!(windows) {
        let runner = windows_launcher(path).await.ok()?;
        return Some((
            executable(path).await.ok()?,
            runner,
            vec![path.with_file_name("scala-native-decision.json")],
        ));
    }
    let launcher = tokio::fs::read_to_string(path).await.ok()?;
    let words = shlex::split(launcher.lines().nth(1)?)?;
    if words.len() != 5
        || words[0] != "exec"
        || words[2] != "-I"
        || words[4] != "$@"
        || !Path::new(&words[1]).is_absolute()
        || !Path::new(&words[3]).is_absolute()
    {
        return None;
    }
    Some((PathBuf::from(&words[1]), PathBuf::from(&words[3]), vec![]))
}
