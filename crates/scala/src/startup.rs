//! `scala startup`: scriptable control of the per-user OS login registration.
//! The OS registration is authoritative; nothing is stored in settings.json.
use color_eyre::Result;
use scala_core::startup::{self, StartupState, StartupStatus};
use serde_json::json;

use crate::cli::StartupCommand;

pub fn run(command: &StartupCommand, json_output: bool) -> Result<()> {
    match command {
        StartupCommand::Status => report("status", startup::status()?, json_output),
        StartupCommand::Enable => {
            startup::enable()?;
            report("enable", startup::status()?, json_output)
        }
        StartupCommand::Disable => {
            startup::disable()?;
            report("disable", startup::status()?, json_output)
        }
    }
}

fn report(operation: &str, status: StartupStatus, json_output: bool) -> Result<()> {
    if json_output {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "operation": operation,
                "startup": {
                    "state": match status.state {
                        StartupState::Enabled => "enabled",
                        StartupState::Disabled => "disabled",
                        StartupState::Unsupported => "unsupported",
                    },
                    "enabled": status.enabled(),
                    "mechanism": status.mechanism,
                    "identity": status.identity,
                    "registered_executable": status.executable,
                    "stale_executable": status.stale,
                    "broken_registration": status.broken,
                }
            }))?
        );
        return Ok(());
    }
    println!("Start automatically on login: {}", status.label());
    println!("  Mechanism:  {}", status.mechanism);
    println!("  Identity:   {}", status.identity);
    if let Some(executable) = &status.executable {
        println!("  Executable: {}", executable.display());
    }
    if status.stale {
        println!(
            "  Warning:    the registration targets a different executable; `scala startup enable` rebinds it."
        );
    }
    if status.broken {
        println!(
            "  Warning:    the registration is registered but not healthy (disabled or altered); `scala startup enable` re-registers the expected definition."
        );
    }
    match operation {
        "enable" => println!(
            "`scala serve` will launch at your next login; the running server is unaffected."
        ),
        "disable" => {
            println!("Future logins will not launch Scala; the running server is unaffected.")
        }
        _ => {}
    }
    Ok(())
}
