use crate::app::Screen;

#[derive(Debug, Clone, Copy)]
pub enum CommandAction {
    Navigate(Screen),
    LoadSelected,
    Unload,
    ShowHelp,
    Quit,
}

#[derive(Debug, Clone, Copy)]
pub struct SlashCommand {
    pub name: &'static str,
    pub description: &'static str,
    pub action: CommandAction,
}

pub const COMMANDS: &[SlashCommand] = &[
    SlashCommand {
        name: "/link",
        description: "Set up Scala Link and inspect peer connectivity",
        action: CommandAction::Navigate(Screen::Link),
    },
    SlashCommand {
        name: "/benchmarks",
        description: "Run and inspect per-profile benchmarks",
        action: CommandAction::Navigate(Screen::Benchmarks),
    },
    SlashCommand {
        name: "/load",
        description: "Load the selected Model Profile through the running server",
        action: CommandAction::LoadSelected,
    },
    SlashCommand {
        name: "/unload",
        description: "Unload the selected resident Model Profile",
        action: CommandAction::Unload,
    },
    SlashCommand {
        name: "/help",
        description: "Show commands and keyboard shortcuts",
        action: CommandAction::ShowHelp,
    },
    SlashCommand {
        name: "/models",
        description: "Open discovered model artifact inventory",
        action: CommandAction::Navigate(Screen::Models),
    },
    SlashCommand {
        name: "/model-profiles",
        description: "Open user-created Model Profiles",
        action: CommandAction::Navigate(Screen::ModelProfiles),
    },
    SlashCommand {
        name: "/runtimes",
        description: "Open runtime pack management",
        action: CommandAction::Navigate(Screen::Runtimes),
    },
    SlashCommand {
        name: "/status",
        description: "Return to the overview",
        action: CommandAction::Navigate(Screen::Overview),
    },
    SlashCommand {
        name: "/server",
        description: "Open API server status",
        action: CommandAction::Navigate(Screen::Server),
    },
    SlashCommand {
        name: "/logs",
        description: "Open application logs",
        action: CommandAction::Navigate(Screen::Logs),
    },
    SlashCommand {
        name: "/settings",
        description: "Open Server Settings and independent engine overrides",
        action: CommandAction::Navigate(Screen::Settings),
    },
    SlashCommand {
        name: "/quit",
        description: "Exit Scala",
        action: CommandAction::Quit,
    },
];

pub fn suggestions(input: &str) -> Vec<&'static SlashCommand> {
    let query = input.trim().to_ascii_lowercase();
    if !query.starts_with('/') {
        return Vec::new();
    }
    COMMANDS
        .iter()
        .filter(|command| command.name.starts_with(&query))
        .collect()
}

pub fn exact(input: &str) -> Option<&'static SlashCommand> {
    let query = input.trim();
    COMMANDS.iter().find(|command| command.name == query)
}
