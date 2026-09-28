//! Launch policy for Scala-owned noninteractive child processes.

/// Keeps a spawned runtime/probe/helper child from sharing Scala's Windows
/// console.
///
/// A Windows console-subsystem child inherits the parent's console unless a
/// creation flag says otherwise, so redirecting stdio alone does not isolate
/// it. Shared attachment lets child code mutate console-global state of the
/// process that owns the TUI: llama.cpp's `common_init` calls
/// `SetConsoleOutputCP(CP_UTF8)` and `SetConsoleCP(CP_UTF8)` at startup, which
/// changed the host conhost code page and visibly corrupted font
/// rasterization. `CREATE_NO_WINDOW` starts the child with no console at all
/// and never opens a window, so its console calls cannot reach the parent's
/// console while piped stdout/stderr and Scala-owned termination behave
/// exactly as before. This is intentionally not `CREATE_NEW_CONSOLE` (a new
/// user-visible window) or `DETACHED_PROCESS`.
///
/// For `tokio::process::Command`, apply this through `command.as_std_mut()`.
/// Other platforms are unaffected.
pub fn isolate_child_from_console(command: &mut std::process::Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    let _ = command;
}
