use std::io::{self, IsTerminal, Write};

use color_eyre::eyre::{Context, Result, ensure};
use scala_core::AppPaths;

use crate::cli::UpdateArgs;

fn needs_prompt(json: bool, interactive: bool, yes: bool) -> Result<bool> {
    if yes {
        return Ok(false);
    }
    ensure!(
        !json && interactive,
        "Replacement requires explicit approval: use --yes for JSON/noninteractive execution, or --check for discovery only"
    );
    Ok(true)
}
fn approved(input: &str) -> bool {
    input.trim().eq_ignore_ascii_case("yes")
}

pub async fn run(paths: &AppPaths, args: &UpdateArgs, json: bool) -> Result<()> {
    let candidate = scala_update::Candidate::discover(&paths.cache_dir).await?;
    let state = candidate.state.clone();
    if !json {
        println!("{}", state.message());
    }
    let mut outcome = "checked";
    if !args.check && state.available() {
        if needs_prompt(
            json,
            io::stdin().is_terminal() && io::stdout().is_terminal(),
            args.yes,
        )
        .wrap_err_with(|| {
            format!(
                "{}; replacement requires --yes in JSON/noninteractive mode",
                state.message()
            )
        })? {
            print!(
                "Stop existing Scala serving/TUI instances first. Install this application update? Type yes to confirm: "
            );
            io::stdout().flush()?;
            let mut input = String::new();
            io::stdin().read_line(&mut input)?;
            if !approved(&input) {
                println!("Update cancelled.");
                return Ok(());
            }
        }
        candidate.install(paths).await?;
        outcome = "updated";
        if !json {
            println!("Scala updated. Start Scala again using your original startup method.");
        }
    }
    if json {
        println!(
            "{}",
            serde_json::json!({
                "current": scala_update::CURRENT, "latest": state.latest,
                "available": state.available(), "checked": state.checked,
                "outcome": outcome, "error": state.error,
                "restart": if outcome == "updated" { Some("Use your original startup method") } else { None },
            })
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn confirmation_is_explicit_and_json_never_prompts() {
        for (json, interactive) in [(true, true), (true, false), (false, false)] {
            assert!(needs_prompt(json, interactive, false).is_err());
            assert!(!needs_prompt(json, interactive, true).unwrap());
        }
        assert!(needs_prompt(false, true, false).unwrap());
        assert!(approved(" YES\n"));
        for input in ["", "y", "no"] {
            assert!(!approved(input));
        }
    }
}
