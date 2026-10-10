//! Bounded operational investigation with the production owned-process guard.
//! Usage: owned_native_command TIMEOUT_SECONDS EXECUTABLE ARG...
use scala_engine::capture_owned_command;
use std::{collections::BTreeMap, path::Path, time::Duration};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    if arguments.len() < 2 {
        return Err("usage: owned_native_command TIMEOUT_SECONDS EXECUTABLE ARG...".into());
    }
    let timeout = Duration::from_secs(arguments[0].parse()?);
    let executable = Path::new(&arguments[1]);
    if !executable.is_absolute() || timeout.is_zero() || timeout > Duration::from_secs(600) {
        return Err("absolute executable and a timeout of 1–600 seconds required".into());
    }
    let result = capture_owned_command(
        executable,
        &arguments[2..]
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        &BTreeMap::new(),
        &[],
        timeout,
    )
    .await?;
    println!("{}", result.stdout);
    eprintln!("{}", result.stderr);
    if !result.success {
        return Err(format!("owned command failed: {:?}", result.code).into());
    }
    Ok(())
}
