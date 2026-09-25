//! Per-user OS login startup registration for `scala serve`.
//!
//! The native registration is the authoritative state: no desired-state flag is
//! persisted in `settings.json`, so removing or disabling the registration
//! outside Scala is reported accurately. `enable`/`disable` only create or
//! remove the registration that applies at the next login; they never start,
//! stop, or restart the currently running application.
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::settings::{SettingCategory, SettingDefinition, SettingId, SettingKind, SettingScope};

/// Settings-UI identifier for the login-startup control. It is a valid
/// Server-scoped id, but it is special-routed to the live OS registration and
/// is never written to `SettingsState`.
pub const SETTING_ID: &str = "server.start_on_login";

/// Deterministic per-user registration identity owned by Scala on platforms
/// whose native namespaces are already per-user: the systemd unit stem and the
/// launchd label. Windows Task Scheduler task names are machine-global, so its
/// identity is derived per user (see [`windows_task_identity`]).
pub const IDENTITY: &str = "dev.scala.serve";

/// Windows Task Scheduler task-name prefix. The full name appends a digest of
/// the current user's SID so distinct users on one machine get independent
/// Scala-owned tasks.
#[cfg(any(windows, test))]
pub const WINDOWS_TASK_PREFIX: &str = "dev.scala.serve";

#[cfg(any(windows, test))]
const SERVE_ARGUMENT: &str = "serve";

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum StartupState {
    Disabled,
    Enabled,
    /// The platform or session has no supported per-user startup mechanism.
    Unsupported,
}

/// The observed native registration state.
#[derive(Debug, Clone)]
pub struct StartupStatus {
    pub state: StartupState,
    /// Human-readable mechanism name for this platform.
    pub mechanism: &'static str,
    /// The registration identity (unit name, launchd label, or task name). On
    /// Windows this is the per-user task name actually calculated for the
    /// current SID.
    pub identity: String,
    /// The executable the registration launches, when readable.
    pub executable: Option<PathBuf>,
    /// The registration exists but points at a different executable than the
    /// running binary (for example after the binary was moved). Re-enabling
    /// startup rebinds the registration to the current executable.
    pub stale: bool,
    /// A registration exists but does not match the login-start contract the
    /// app owns (disabled task, wrong principal/trigger, or wrong arguments).
    /// Re-enabling re-registers the expected definition. `Enabled` is never
    /// reported while this is true.
    pub broken: bool,
}

impl StartupStatus {
    pub fn enabled(&self) -> bool {
        self.state == StartupState::Enabled
    }

    pub fn label(&self) -> &'static str {
        match self.state {
            StartupState::Enabled => "Enabled",
            StartupState::Disabled => "Disabled",
            StartupState::Unsupported => "Unsupported",
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum StartupError {
    #[error("could not determine platform user directories")]
    PathsUnavailable,
    #[error("login startup I/O failed at {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("current executable path is unavailable or not valid UTF-8")]
    InvalidExecutable,
    #[error("`{program}` failed: {message}")]
    Command {
        program: &'static str,
        message: String,
    },
    #[error("per-user login startup is not supported on this platform/session: {reason}")]
    Unsupported { reason: String },
}

fn io_error(path: &Path, source: std::io::Error) -> StartupError {
    StartupError::Io {
        path: path.to_owned(),
        source,
    }
}

/// The Settings row definition. The value is never persisted; the TUI reads
/// the live OS registration via [`status`] and applies changes via
/// [`enable`]/[`disable`].
pub fn setting_definition() -> SettingDefinition {
    SettingDefinition {
        id: SettingId::new(SETTING_ID).expect("startup setting ID is valid"),
        label: "Start automatically on login".to_owned(),
        description: "Register `scala serve` with the OS so the headless server starts at your next login for this user. This is an immediate operating-system registration, not a stored preference: the OS registration is authoritative, and changing it never starts or stops the currently running server.".to_owned(),
        kind: SettingKind::Toggle,
        scope: SettingScope::Server,
        category: SettingCategory::Application,
        supported: true,
        unsupported_reason: None,
        unit: None,
        default_preview: None,
    }
}

/// The executable the registration binds: this exact binary.
fn current_executable() -> Result<PathBuf, StartupError> {
    std::env::current_exe().map_err(|_| StartupError::InvalidExecutable)
}

fn invoke(program: &'static str, args: &[&str]) -> Result<String, StartupError> {
    #[cfg(all(test, unix))]
    if program == "systemctl" {
        return test_systemctl(args);
    }
    let output = Command::new(program)
        .args(args)
        .output()
        .map_err(|source| StartupError::Command {
            program,
            message: format!("could not run: {source}"),
        })?;
    if !output.status.success() {
        return Err(StartupError::Command {
            program,
            message: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Whether `program` is resolvable on PATH. Pure filesystem check: probing the
/// user manager itself could block when no user bus exists.
#[cfg(target_os = "linux")]
fn command_exists(program: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|paths| std::env::split_paths(&paths).any(|dir| dir.join(program).is_file()))
}

#[cfg(target_os = "linux")]
fn units_dir() -> Result<PathBuf, StartupError> {
    // Unit tests must never write into the real user unit directory.
    #[cfg(test)]
    return test_units_dir().ok_or(StartupError::PathsUnavailable);
    #[cfg(not(test))]
    Ok(directories::BaseDirs::new()
        .ok_or(StartupError::PathsUnavailable)?
        .config_dir()
        .join("systemd/user"))
}

const UNIT_NAME: &str = "dev.scala.serve.service";

#[cfg(any(target_os = "linux", test))]
fn unit_file(units: &Path) -> PathBuf {
    units.join(UNIT_NAME)
}
#[cfg(any(target_os = "linux", test))]
fn wants_link(units: &Path) -> PathBuf {
    units.join("default.target.wants").join(UNIT_NAME)
}

// systemd specifier/environment expansion applies even inside quotes.
#[cfg(any(target_os = "linux", test))]
fn systemd_quote(path: &Path) -> Result<String, StartupError> {
    let s = path.to_str().ok_or(StartupError::InvalidExecutable)?;
    if s.chars().any(char::is_control) {
        return Err(StartupError::InvalidExecutable);
    }
    Ok(format!(
        "\"{}\"",
        s.replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('%', "%%")
            .replace('$', "$$")
    ))
}

#[cfg(any(target_os = "linux", test))]
fn unit_text(executable: &Path) -> Result<String, StartupError> {
    Ok(format!(
        "[Unit]\nDescription=Scala server (current user)\n[Service]\nExecStart={} serve\nRestart=on-failure\nRestartSec=5\nUMask=0077\n[Install]\nWantedBy=default.target\n",
        systemd_quote(executable)?
    ))
}

/// First ExecStart program token, reversing `systemd_quote` when possible.
#[cfg(target_os = "linux")]
fn unit_executable(text: &str) -> Option<PathBuf> {
    let line = text.lines().find(|line| line.starts_with("ExecStart="))?;
    let value = line.strip_prefix("ExecStart=")?;
    let mut chars = value.chars();
    if chars.next() != Some('"') {
        return Some(PathBuf::from(value.split_whitespace().next()?));
    }
    let mut decoded = String::new();
    let mut escaped = false;
    for c in chars {
        if escaped {
            match c {
                '%' => decoded.push('%'),
                '$' => decoded.push('$'),
                _ => {
                    decoded.push('\\');
                    decoded.push(c);
                }
            }
            escaped = false;
        } else if c == '\\' {
            escaped = true;
        } else if c == '"' {
            break;
        } else {
            decoded.push(c);
        }
    }
    Some(PathBuf::from(decoded))
}

fn remove_if_exists(path: &Path) -> Result<(), StartupError> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(io_error(path, e)),
    }
}

#[cfg(target_os = "linux")]
fn linux_status() -> Result<StartupStatus, StartupError> {
    let units = units_dir()?;
    let file = unit_file(&units);
    let registered = file.exists() && wants_link(&units).symlink_metadata().is_ok();
    if !registered {
        let state = if command_exists("systemctl") || file.exists() {
            StartupState::Disabled
        } else {
            StartupState::Unsupported
        };
        return Ok(StartupStatus {
            state,
            mechanism: "systemd --user service",
            identity: IDENTITY.to_owned(),
            executable: None,
            stale: false,
            broken: false,
        });
    }
    let text = std::fs::read_to_string(&file).map_err(|e| io_error(&file, e))?;
    let executable = unit_executable(&text);
    let stale = match (&executable, current_executable().ok()) {
        (Some(registered), Some(current)) => registered != &current,
        _ => false,
    };
    Ok(StartupStatus {
        state: StartupState::Enabled,
        mechanism: "systemd --user service",
        identity: IDENTITY.to_owned(),
        executable,
        stale,
        broken: false,
    })
}

#[cfg(target_os = "linux")]
fn linux_enable() -> Result<(), StartupError> {
    let units = units_dir()?;
    let file = unit_file(&units);
    std::fs::create_dir_all(&units).map_err(|e| io_error(&units, e))?;
    std::fs::write(&file, unit_text(&current_executable()?)?.as_bytes())
        .map_err(|e| io_error(&file, e))?;
    invoke("systemctl", &["--user", "daemon-reload"])?;
    // `enable` without `--now` registers next-login startup only; it does not
    // launch a second `scala serve` while the TUI owns the serving stack.
    invoke("systemctl", &["--user", "enable", UNIT_NAME])?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn linux_disable() -> Result<(), StartupError> {
    let units = units_dir()?;
    let file = unit_file(&units);
    let wants = wants_link(&units);
    // `disable` never stops a running unit; removing the definition files is
    // what prevents the next login from starting the server. It is tolerated
    // when the unit is already unknown to the manager.
    if file.exists() {
        let _ = invoke("systemctl", &["--user", "disable", UNIT_NAME]);
    }
    remove_if_exists(&wants)?;
    remove_if_exists(&file)?;
    if file.exists() || wants.symlink_metadata().is_ok() {
        return Err(StartupError::Command {
            program: "systemctl",
            message: "startup definition could not be fully removed".to_owned(),
        });
    }
    let _ = invoke("systemctl", &["--user", "daemon-reload"]);
    Ok(())
}

#[cfg(any(target_os = "macos", test))]
fn plist_text(executable: &Path) -> Result<String, StartupError> {
    let exe = executable.to_str().ok_or(StartupError::InvalidExecutable)?;
    if exe.chars().any(char::is_control) {
        return Err(StartupError::InvalidExecutable);
    }
    let xml = |s: &str| {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
            .replace('\'', "&apos;")
    };
    Ok(format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?><!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\"><plist version=\"1.0\"><dict><key>Label</key><string>{IDENTITY}</string><key>ProgramArguments</key><array><string>{}</string><string>serve</string></array><key>RunAtLoad</key><true/><key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict><key>Umask</key><integer>63</integer></dict></plist>",
        xml(exe)
    ))
}

#[cfg(target_os = "macos")]
fn plist_file() -> Result<PathBuf, StartupError> {
    Ok(directories::BaseDirs::new()
        .ok_or(StartupError::PathsUnavailable)?
        .home_dir()
        .join("Library/LaunchAgents")
        .join(format!("{IDENTITY}.plist")))
}

#[cfg(target_os = "macos")]
fn plist_executable(text: &str) -> Option<PathBuf> {
    let start = text.find("<key>ProgramArguments</key><array><string>")?;
    let rest = &text[start + "<key>ProgramArguments</key><array><string>".len()..];
    let end = rest.find("</string>")?;
    let encoded = &rest[..end];
    Some(PathBuf::from(
        encoded
            .replace("&quot;", "\"")
            .replace("&apos;", "'")
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&amp;", "&"),
    ))
}

#[cfg(target_os = "macos")]
fn macos_status() -> Result<StartupStatus, StartupError> {
    let file = plist_file()?;
    if !file.exists() {
        return Ok(StartupStatus {
            state: StartupState::Disabled,
            mechanism: "per-user LaunchAgent (launchd)",
            identity: IDENTITY.to_owned(),
            executable: None,
            stale: false,
            broken: false,
        });
    }
    let text = std::fs::read_to_string(&file).map_err(|e| io_error(&file, e))?;
    let executable = plist_executable(&text);
    let stale = match (&executable, current_executable().ok()) {
        (Some(registered), Some(current)) => registered != &current,
        _ => false,
    };
    Ok(StartupStatus {
        state: StartupState::Enabled,
        mechanism: "per-user LaunchAgent (launchd)",
        identity: IDENTITY.to_owned(),
        executable,
        stale,
        broken: false,
    })
}

#[cfg(target_os = "macos")]
fn macos_enable() -> Result<(), StartupError> {
    // A LaunchAgent plist under ~/Library/LaunchAgents is loaded by launchd at
    // the next login; writing it starts nothing now.
    let file = plist_file()?;
    let parent = file.parent().ok_or(StartupError::PathsUnavailable)?;
    std::fs::create_dir_all(parent).map_err(|e| io_error(parent, e))?;
    std::fs::write(&file, plist_text(&current_executable()?)?.as_bytes())
        .map_err(|e| io_error(&file, e))
}

#[cfg(target_os = "macos")]
fn macos_disable() -> Result<(), StartupError> {
    remove_if_exists(&plist_file()?)
}

#[cfg(any(windows, test))]
fn windows_ps(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

/// Deterministic, bounded, Scala-owned Task Scheduler task name for the current
/// Windows user. Task Scheduler names are machine-global, so the current user's
/// SID is folded into a stable digest: distinct users get independent tasks and
/// the same user always calculates the same name. No credentials are involved.
#[cfg(any(windows, test))]
pub fn windows_task_identity(sid: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = format!("{:x}", Sha256::digest(sid.as_bytes()));
    format!("{WINDOWS_TASK_PREFIX}.{}", &digest[..16])
}

/// Emits the current Windows identity as structured JSON: the user SID (for the
/// task name) and the user name (for principal/trigger matching).
#[cfg(any(windows, test))]
fn windows_identity_script() -> String {
    "$i=[System.Security.Principal.WindowsIdentity]::GetCurrent(); [pscustomobject]@{ sid=$i.User.Value; name=$i.Name } | ConvertTo-Json -Compress".to_owned()
}

#[cfg(any(windows, test))]
#[derive(Debug, serde::Deserialize)]
struct WindowsIdentityView {
    #[serde(default)]
    sid: String,
    #[serde(default)]
    name: String,
}

/// One Task Scheduler trigger as reported by the query script.
#[cfg(any(windows, test))]
#[derive(Debug, serde::Deserialize)]
struct WindowsTriggerView {
    #[serde(default, rename = "type")]
    trigger_type: String,
    #[serde(default)]
    user: Option<String>,
    #[serde(default)]
    enabled: Option<bool>,
}

/// One Task Scheduler action as reported by the query script.
#[cfg(any(windows, test))]
#[derive(Debug, serde::Deserialize)]
struct WindowsActionView {
    #[serde(default)]
    execute: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
}

/// The Task Scheduler task as reported by the query script. Built from
/// structured PowerShell objects (`Get-ScheduledTask` properties), never from
/// localized human-readable `schtasks` text. `action_count`/`trigger_count`
/// are explicit integers so cardinality is verifiable without depending on
/// PowerShell scalar/array JSON shape quirks.
#[cfg(any(windows, test))]
#[derive(Debug, serde::Deserialize)]
struct WindowsTaskView {
    #[serde(default)]
    present: bool,
    #[serde(default)]
    enabled: bool,
    #[serde(default)]
    user: Option<String>,
    #[serde(default)]
    logon_type: Option<String>,
    #[serde(default)]
    run_level: Option<String>,
    #[serde(default)]
    action_count: usize,
    #[serde(default)]
    actions: Vec<WindowsActionView>,
    #[serde(default)]
    trigger_count: usize,
    #[serde(default)]
    triggers: Vec<WindowsTriggerView>,
}

#[cfg(any(windows, test))]
fn windows_parse<T: serde::de::DeserializeOwned>(output: &str) -> Result<T, StartupError> {
    serde_json::from_str(output.trim()).map_err(|error| StartupError::Command {
        program: "powershell.exe",
        message: format!("unexpected Task Scheduler query output: {error}"),
    })
}

// Registers (or replaces) the per-user AtLogOn task without starting it.
// Current user, interactive logon, limited run level: no elevation required.
#[cfg(any(windows, test))]
fn windows_register_script(task: &str, executable: &str) -> String {
    let task = windows_ps(task);
    format!(
        "$a=New-ScheduledTaskAction -Execute {} -Argument {}; $u=[System.Security.Principal.WindowsIdentity]::GetCurrent().Name; $p=New-ScheduledTaskPrincipal -UserId $u -LogonType Interactive -RunLevel Limited; $t=New-ScheduledTaskTrigger -AtLogOn -User $u; $s=New-ScheduledTaskSettingsSet -ExecutionTimeLimit ([TimeSpan]::Zero) -RestartCount 3 -RestartInterval (New-TimeSpan -Minutes 1) -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries; Register-ScheduledTask -TaskName {task} -Action $a -Principal $p -Trigger $t -Settings $s -Force | Out-Null",
        windows_ps(executable),
        windows_ps(SERVE_ARGUMENT),
    )
}

// Unregisters the task only if present; never stops task instances.
#[cfg(any(windows, test))]
fn windows_unregister_script(task: &str) -> String {
    let task = windows_ps(task);
    format!(
        "if (Get-ScheduledTask -TaskName {task} -ErrorAction SilentlyContinue) {{ Unregister-ScheduledTask -TaskName {task} -Confirm:$false }}"
    )
}

// Emits the registered task as structured JSON, or `{"present":false}`. Reports
// the task enabled flag, principal semantics, the complete action and trigger
// lists, and explicit element counts so health is judged against the full
// owned login-start definition rather than mere task existence.
#[cfg(any(windows, test))]
fn windows_query_script(task: &str) -> String {
    let task = windows_ps(task);
    format!(
        "$n={task}; $t=Get-ScheduledTask -TaskName $n -ErrorAction SilentlyContinue; if ($null -eq $t) {{ [pscustomobject]@{{ present=$false }} | ConvertTo-Json -Compress }} else {{ $a=@(@($t.Actions) | ForEach-Object {{ [pscustomobject]@{{ execute=[string]$_.Execute; arguments=[string]$_.Arguments }} }}); $tr=@(@($t.Triggers) | ForEach-Object {{ [pscustomobject]@{{ type=[string]$_.CimClass.CimClassName; user=$_.UserId; enabled=$_.Enabled }} }}); [pscustomobject]@{{ present=$true; enabled=[bool]$t.Settings.Enabled; user=[string]$t.Principal.UserId; logon_type=[string]$t.Principal.LogonType; run_level=[string]$t.Principal.RunLevel; action_count=$a.Count; actions=$a; trigger_count=$tr.Count; triggers=$tr }} | ConvertTo-Json -Compress -Depth 6 }}"
    )
}

/// Task Scheduler may expose the same account either as `DOMAIN\name` or as
/// its SID string; both representations identify the current user, so
/// principal and trigger users match on either form.
#[cfg(any(windows, test))]
fn windows_same_account(reported: Option<&str>, name: &str, sid: &str) -> bool {
    reported.is_some_and(|user| {
        !user.is_empty() && (user.eq_ignore_ascii_case(name) || user.eq_ignore_ascii_case(sid))
    })
}

/// Judges a queried task against the exact login-start contract Scala owns:
/// present, enabled, current-user interactive/limited principal, exactly one
/// action binding the exact executable with the exact `serve` argument, and
/// exactly one enabled AtLogOn trigger for that user. Additional actions or
/// triggers are mutations the contract does not allow: any mismatch reports
/// `Disabled` with `broken` set; `Enabled` is only reported when Windows will
/// actually run the expected definition and nothing else.
#[cfg(any(windows, test))]
fn windows_evaluate(
    view: &WindowsTaskView,
    current_user: &str,
    current_sid: &str,
    expected_executable: Option<&Path>,
    identity: String,
) -> StartupStatus {
    let mechanism = "per-user Task Scheduler logon task";
    if !view.present {
        return StartupStatus {
            state: StartupState::Disabled,
            mechanism,
            identity,
            executable: None,
            stale: false,
            broken: false,
        };
    }
    let executable = view
        .actions
        .first()
        .and_then(|action| action.execute.as_deref())
        .map(PathBuf::from);
    let stale = match (&executable, expected_executable) {
        (Some(registered), Some(current)) => registered != current,
        _ => false,
    };
    // The owned contract is exact: one action and one trigger. The explicit
    // counts and the complete arrays must agree, so extra entries cannot hide
    // behind a correct first element.
    let single_action = view.action_count == 1 && view.actions.len() == 1;
    let single_trigger = view.trigger_count == 1 && view.triggers.len() == 1;
    let executable_matches = single_action
        && match (&executable, expected_executable) {
            (_, None) => true,
            (Some(registered), Some(current)) => registered == current,
            (None, Some(_)) => false,
        };
    let arguments_match = single_action
        && view
            .actions
            .first()
            .and_then(|action| action.arguments.as_deref())
            == Some(SERVE_ARGUMENT);
    let user_matches = windows_same_account(view.user.as_deref(), current_user, current_sid);
    let logon_matches = view
        .logon_type
        .as_deref()
        .is_some_and(|logon| logon.eq_ignore_ascii_case("Interactive"));
    let run_level_matches = view
        .run_level
        .as_deref()
        .is_some_and(|level| level.eq_ignore_ascii_case("Limited"));
    let trigger_matches = single_trigger
        && view.triggers.iter().all(|trigger| {
            trigger
                .trigger_type
                .to_ascii_lowercase()
                .contains("logontrigger")
                && windows_same_account(trigger.user.as_deref(), current_user, current_sid)
                && trigger.enabled.unwrap_or(true)
        });
    let healthy = view.enabled
        && executable_matches
        && user_matches
        && logon_matches
        && run_level_matches
        && trigger_matches
        && arguments_match;
    StartupStatus {
        state: if healthy {
            StartupState::Enabled
        } else {
            StartupState::Disabled
        },
        mechanism,
        identity,
        executable,
        stale,
        broken: !healthy,
    }
}

#[cfg(windows)]
fn windows_run(script: &str) -> Result<String, StartupError> {
    invoke(
        "powershell.exe",
        &[
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            &format!("$ErrorActionPreference='Stop'; {script}"),
        ],
    )
}

#[cfg(windows)]
fn windows_identity() -> Result<(String, String), StartupError> {
    let output = windows_run(&windows_identity_script())?;
    let view: WindowsIdentityView = windows_parse(&output)?;
    Ok((view.sid, view.name))
}

#[cfg(windows)]
fn windows_status() -> Result<StartupStatus, StartupError> {
    let (sid, user) = windows_identity()?;
    let identity = windows_task_identity(&sid);
    let output = windows_run(&windows_query_script(&identity))?;
    let view: WindowsTaskView = windows_parse(&output)?;
    Ok(windows_evaluate(
        &view,
        &user,
        &sid,
        current_executable().ok().as_deref(),
        identity,
    ))
}

#[cfg(windows)]
fn windows_enable() -> Result<(), StartupError> {
    let (sid, _) = windows_identity()?;
    let identity = windows_task_identity(&sid);
    let executable = current_executable()?;
    let path = executable.to_str().ok_or(StartupError::InvalidExecutable)?;
    if path.contains('"') || path.chars().any(char::is_control) {
        return Err(StartupError::InvalidExecutable);
    }
    windows_run(&windows_register_script(&identity, path))?;
    Ok(())
}

#[cfg(windows)]
fn windows_disable() -> Result<(), StartupError> {
    let (sid, _) = windows_identity()?;
    let identity = windows_task_identity(&sid);
    windows_run(&windows_unregister_script(&identity))?;
    Ok(())
}

/// Observed native registration state. Never reads or writes `settings.json`.
pub fn status() -> Result<StartupStatus, StartupError> {
    #[cfg(target_os = "linux")]
    {
        linux_status()
    }
    #[cfg(target_os = "macos")]
    {
        macos_status()
    }
    #[cfg(windows)]
    {
        windows_status()
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        Ok(StartupStatus {
            state: StartupState::Unsupported,
            mechanism: "unsupported platform",
            identity: IDENTITY.to_owned(),
            executable: None,
            stale: false,
            broken: false,
        })
    }
}

/// Register (or re-register/rebind) `scala serve` to start at the next login.
/// Does not launch or affect the currently running server.
pub fn enable() -> Result<(), StartupError> {
    #[cfg(target_os = "linux")]
    {
        linux_enable()
    }
    #[cfg(target_os = "macos")]
    {
        macos_enable()
    }
    #[cfg(windows)]
    {
        windows_enable()
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        Err(StartupError::Unsupported {
            reason: "no per-user login startup mechanism on this platform".to_owned(),
        })
    }
}

/// Remove the login-startup registration only. Does not stop the currently
/// running server.
pub fn disable() -> Result<(), StartupError> {
    #[cfg(target_os = "linux")]
    {
        linux_disable()
    }
    #[cfg(target_os = "macos")]
    {
        macos_disable()
    }
    #[cfg(windows)]
    {
        windows_disable()
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        Err(StartupError::Unsupported {
            reason: "no per-user login startup mechanism on this platform".to_owned(),
        })
    }
}

// Synthetic user-unit manager for tests so enable/disable round-trips can be
// validated without a real `systemd --user` session.
#[cfg(all(test, unix))]
fn test_units_dir() -> Option<PathBuf> {
    TEST_UNITS_DIR
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}
#[cfg(all(test, unix))]
static TEST_UNITS_DIR: std::sync::Mutex<Option<PathBuf>> = std::sync::Mutex::new(None);

#[cfg(all(test, unix))]
fn test_systemctl(args: &[&str]) -> Result<String, StartupError> {
    let dir = test_units_dir().ok_or(StartupError::PathsUnavailable)?;
    let unit = args.last().copied().unwrap_or("");
    let link = dir.join("default.target.wants").join(unit);
    match args.get(1).copied().unwrap_or("") {
        "daemon-reload" => Ok(String::new()),
        "enable" => {
            if !dir.join(unit).is_file() {
                return Err(StartupError::Command {
                    program: "systemctl",
                    message: format!("unit file {unit} does not exist"),
                });
            }
            std::fs::create_dir_all(link.parent().unwrap()).map_err(|e| io_error(&link, e))?;
            let _ = std::fs::remove_file(&link);
            std::os::unix::fs::symlink(dir.join(unit), &link).map_err(|e| io_error(&link, e))?;
            Ok(String::new())
        }
        "disable" => {
            let _ = std::fs::remove_file(&link);
            Ok(String::new())
        }
        verb => Err(StartupError::Command {
            program: "systemctl",
            message: format!("unexpected verb {verb}"),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(tag: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("scala-startup-test-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_enable_status_disable_round_trip() {
        let root = temp_root("linux");
        let units = root.join("systemd user");
        std::fs::create_dir_all(&units).unwrap();
        *TEST_UNITS_DIR.lock().unwrap() = Some(units.clone());

        assert_eq!(status().unwrap().state, StartupState::Disabled);
        enable().unwrap();
        let observed = status().unwrap();
        assert_eq!(observed.state, StartupState::Enabled);
        assert_eq!(observed.mechanism, "systemd --user service");
        assert_eq!(observed.identity, IDENTITY);
        assert_eq!(observed.executable, current_executable().ok());
        assert!(!observed.stale);
        let text = std::fs::read_to_string(unit_file(&units)).unwrap();
        assert!(text.contains(" serve\n"), "{text}");
        assert!(text.contains("ExecStart="), "{text}");

        // A registration for a moved executable reports stale, not enabled+ok.
        let moved = root.join("moved scala.exe");
        std::fs::write(unit_file(&units), unit_text(&moved).unwrap()).unwrap();
        let observed = status().unwrap();
        assert_eq!(observed.state, StartupState::Enabled);
        assert!(observed.stale);

        disable().unwrap();
        assert_eq!(status().unwrap().state, StartupState::Disabled);
        assert!(!unit_file(&units).exists());
        *TEST_UNITS_DIR.lock().unwrap() = None;
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn unit_file_names_are_deterministic() {
        assert_eq!(UNIT_NAME, "dev.scala.serve.service");
        let units = Path::new("/tmp/example units");
        assert_eq!(unit_file(units), units.join("dev.scala.serve.service"));
        assert_eq!(
            wants_link(units),
            units.join("default.target.wants/dev.scala.serve.service")
        );
    }

    #[test]
    fn systemd_unit_quoting_handles_spaces_and_escapes() {
        let quoted = systemd_quote(Path::new("/opt/Scala App/scala")).unwrap();
        assert_eq!(quoted, "\"/opt/Scala App/scala\"");
        assert!(
            unit_text(Path::new("/opt/Scala App/scala"))
                .unwrap()
                .contains("ExecStart=\"/opt/Scala App/scala\" serve\n")
        );
    }

    #[test]
    fn startup_setting_definition_satisfies_its_scope() {
        let definition = setting_definition();
        assert_eq!(definition.id.as_str(), "server.start_on_login");
        assert_eq!(definition.id.namespace(), Some("server"));
        definition.validate_scope().expect("valid Server scope");
    }

    #[test]
    fn windows_scripts_are_registration_only_and_safely_quoted() {
        let task = windows_task_identity("S-1-5-21-1-2-3-1001");
        let script = windows_register_script(&task, r"C:\My Apps\scala.exe");
        assert!(!script.contains("Start-ScheduledTask"));
        assert!(!script.contains("Stop-ScheduledTask"));
        assert!(script.contains("Register-ScheduledTask"));
        assert!(script.contains("-AtLogOn -User $u"));
        assert!(script.contains("-LogonType Interactive -RunLevel Limited"));
        assert!(script.contains("ExecutionTimeLimit ([TimeSpan]::Zero)"));
        assert!(script.contains("RestartCount 3"));
        assert!(script.contains(&format!("-TaskName '{task}'")));
        assert!(script.contains(r"-Execute 'C:\My Apps\scala.exe'"));
        assert!(script.contains("-Argument 'serve'"));
        // Trailing backslashes are literal inside PowerShell single quotes.
        let script = windows_register_script(&task, r"C:\Scala\");
        assert!(script.contains(r"-Execute 'C:\Scala\'"));
        // Single quotes are doubled; a double quote is rejected at enable time.
        assert_eq!(windows_ps("it's"), "'it''s'");

        let unregister = windows_unregister_script(&task);
        assert!(!unregister.contains("Stop-ScheduledTask"));
        assert!(unregister.contains("Unregister-ScheduledTask"));
        assert!(unregister.contains("SilentlyContinue"));
        assert!(unregister.contains(&task));

        let query = windows_query_script(&task);
        assert!(query.contains(&task));
        // Structured object properties, not localized human-readable text.
        assert!(query.contains("present=$true"));
        assert!(query.contains("Settings.Enabled"));
        assert!(query.contains("CimClassName"));
        assert!(query.contains("LogonType"));
        assert!(query.contains("ConvertTo-Json"));
        // Complete arrays plus explicit counts: cardinality is deterministic.
        assert!(query.contains("action_count=$a.Count"));
        assert!(query.contains("actions=$a"));
        assert!(query.contains("trigger_count=$tr.Count"));
        assert!(query.contains("triggers=$tr"));

        let identity = windows_identity_script();
        assert!(identity.contains("WindowsIdentity]::GetCurrent()"));
        assert!(identity.contains("ConvertTo-Json"));
    }

    #[test]
    fn windows_task_identity_is_per_user_and_deterministic() {
        let alice = windows_task_identity("S-1-5-21-111-222-333-1001");
        let bob = windows_task_identity("S-1-5-21-111-222-333-1002");
        assert!(alice.starts_with("dev.scala.serve."), "{alice}");
        assert_eq!(alice, windows_task_identity("S-1-5-21-111-222-333-1001"));
        assert_ne!(alice, bob);
        // Bounded: prefix plus a 16-hex-digit digest suffix.
        assert_eq!(alice.len(), "dev.scala.serve.".len() + 16);
        assert!(
            alice["dev.scala.serve.".len()..]
                .chars()
                .all(|c| c.is_ascii_hexdigit())
        );

        // The identity query is parsed structurally and feeds the derivation.
        let who: WindowsIdentityView =
            windows_parse(r#"{"sid":"S-1-5-21-111-222-333-1001","name":"DESKTOP\\alice"}"#)
                .unwrap();
        assert_eq!(who.name, r"DESKTOP\alice");
        assert_eq!(windows_task_identity(&who.sid), alice);
    }

    const TEST_SID: &str = "S-1-5-21-111-222-333-1001";

    fn healthy_windows_task(execute: &str) -> WindowsTaskView {
        let json = serde_json::json!({
            "present": true,
            "enabled": true,
            "user": r"DESKTOP\alice",
            "logon_type": "Interactive",
            "run_level": "Limited",
            "action_count": 1,
            "actions": [
                {"execute": execute, "arguments": "serve"}
            ],
            "trigger_count": 1,
            "triggers": [
                {"type": "MSFT_TaskLogonTrigger", "user": r"DESKTOP\alice", "enabled": true}
            ],
        });
        serde_json::from_str(&json.to_string()).unwrap()
    }

    fn windows_health(view: &WindowsTaskView) -> StartupStatus {
        windows_evaluate(
            view,
            r"DESKTOP\alice",
            TEST_SID,
            Some(Path::new(r"C:\Scala\scala.exe")),
            "dev.scala.serve.deadbeefdeadbeef".to_owned(),
        )
    }

    #[test]
    fn windows_status_requires_full_login_contract() {
        // Absent task: truthfully Disabled, not broken.
        let absent: WindowsTaskView = serde_json::from_str(r#"{"present":false}"#).unwrap();
        let status = windows_health(&absent);
        assert_eq!(status.state, StartupState::Disabled);
        assert!(!status.broken);
        assert!(!status.stale);

        // Registered, enabled, exact definition: the only Enabled outcome.
        let status = windows_health(&healthy_windows_task(r"C:\Scala\scala.exe"));
        assert_eq!(status.state, StartupState::Enabled);
        assert!(!status.broken);
        assert!(!status.stale);
        assert_eq!(
            status.executable,
            Some(PathBuf::from(r"C:\Scala\scala.exe"))
        );

        // Registered but Disabled: never Enabled.
        let mut disabled = healthy_windows_task(r"C:\Scala\scala.exe");
        disabled.enabled = false;
        let status = windows_health(&disabled);
        assert_ne!(status.state, StartupState::Enabled);
        assert!(status.broken);

        // Wrong executable: stale/broken, never Enabled.
        let status = windows_health(&healthy_windows_task(r"C:\Other\scala.exe"));
        assert_ne!(status.state, StartupState::Enabled);
        assert!(status.stale && status.broken);

        // Wrong arguments: broken, never Enabled.
        let mut wrong_args = healthy_windows_task(r"C:\Scala\scala.exe");
        wrong_args.actions[0].arguments = Some("serve --port 9000".to_owned());
        let status = windows_health(&wrong_args);
        assert_ne!(status.state, StartupState::Enabled);
        assert!(status.broken);

        // An extra action alongside the correct one is a mutation: broken,
        // never Enabled, even though the first action still matches.
        let mut extra_action = healthy_windows_task(r"C:\Scala\scala.exe");
        extra_action.action_count = 2;
        extra_action.actions.push(WindowsActionView {
            execute: Some(r"C:\Scala\scala.exe".to_owned()),
            arguments: Some("serve".to_owned()),
        });
        let status = windows_health(&extra_action);
        assert_ne!(status.state, StartupState::Enabled);
        assert!(status.broken);

        // Zero actions: broken, never Enabled.
        let mut no_action = healthy_windows_task(r"C:\Scala\scala.exe");
        no_action.action_count = 0;
        no_action.actions.clear();
        let status = windows_health(&no_action);
        assert_ne!(status.state, StartupState::Enabled);
        assert!(status.broken);

        // Missing/foreign AtLogOn trigger: broken, never Enabled.
        let mut no_trigger = healthy_windows_task(r"C:\Scala\scala.exe");
        no_trigger.trigger_count = 0;
        no_trigger.triggers.clear();
        let status = windows_health(&no_trigger);
        assert_ne!(status.state, StartupState::Enabled);
        assert!(status.broken);

        let mut boot_trigger = healthy_windows_task(r"C:\Scala\scala.exe");
        boot_trigger.triggers[0].trigger_type = "MSFT_TaskBootTrigger".to_owned();
        let status = windows_health(&boot_trigger);
        assert_ne!(status.state, StartupState::Enabled);
        assert!(status.broken);

        // An extra Boot trigger alongside the correct AtLogOn trigger:
        // broken, never Enabled.
        let mut extra_boot = healthy_windows_task(r"C:\Scala\scala.exe");
        extra_boot.trigger_count = 2;
        extra_boot.triggers.push(WindowsTriggerView {
            trigger_type: "MSFT_TaskBootTrigger".to_owned(),
            user: None,
            enabled: Some(true),
        });
        let status = windows_health(&extra_boot);
        assert_ne!(status.state, StartupState::Enabled);
        assert!(status.broken);

        // A second AtLogOn trigger for the same user is still an extra
        // trigger: broken, never Enabled.
        let mut extra_logon = healthy_windows_task(r"C:\Scala\scala.exe");
        extra_logon.trigger_count = 2;
        extra_logon.triggers.push(WindowsTriggerView {
            trigger_type: "MSFT_TaskLogonTrigger".to_owned(),
            user: Some(r"DESKTOP\alice".to_owned()),
            enabled: Some(true),
        });
        let status = windows_health(&extra_logon);
        assert_ne!(status.state, StartupState::Enabled);
        assert!(status.broken);

        // Wrong trigger user: broken, never Enabled.
        let mut wrong_trigger_user = healthy_windows_task(r"C:\Scala\scala.exe");
        wrong_trigger_user.triggers[0].user = Some(r"DESKTOP\bob".to_owned());
        let status = windows_health(&wrong_trigger_user);
        assert_ne!(status.state, StartupState::Enabled);
        assert!(status.broken);

        // Wrong principal/user: broken, never Enabled.
        let mut wrong_user = healthy_windows_task(r"C:\Scala\scala.exe");
        wrong_user.user = Some(r"DESKTOP\bob".to_owned());
        let status = windows_health(&wrong_user);
        assert_ne!(status.state, StartupState::Enabled);
        assert!(status.broken);

        // Wrong principal semantics: broken, never Enabled.
        let mut elevated = healthy_windows_task(r"C:\Scala\scala.exe");
        elevated.run_level = Some("Highest".to_owned());
        let status = windows_health(&elevated);
        assert_ne!(status.state, StartupState::Enabled);
        assert!(status.broken);

        // The same account reported by SID instead of name still matches the
        // principal and trigger user checks.
        let mut sid_task = healthy_windows_task(r"C:\Scala\scala.exe");
        sid_task.user = Some(TEST_SID.to_owned());
        sid_task.triggers[0].user = Some(TEST_SID.to_owned());
        let status = windows_health(&sid_task);
        assert_eq!(status.state, StartupState::Enabled);
        assert!(!status.broken);
    }

    #[test]
    fn macos_plist_binds_exact_executable_with_serve_argument() {
        let text = plist_text(Path::new("/Applications/Scala App/scala")).unwrap();
        assert!(text.contains("<key>Label</key><string>dev.scala.serve</string>"));
        assert!(text.contains(
            "<key>ProgramArguments</key><array><string>/Applications/Scala App/scala</string><string>serve</string></array>"
        ));
        assert!(text.contains("<key>RunAtLoad</key><true/>"));
        assert!(text.contains("SuccessfulExit"));
    }
}
