#[derive(Clone, Copy, Debug)]
pub(super) struct SlashCommand {
    pub(super) name: &'static str,
    pub(super) description: &'static str,
}

pub(super) const SLASH_COMMANDS: &[SlashCommand] = &[
    SlashCommand {
        name: "/new",
        description: "Start a new session",
    },
    SlashCommand {
        name: "/resume",
        description: "Resume a workspace session",
    },
    SlashCommand {
        name: "/model",
        description: "Configure or switch the model",
    },
    SlashCommand {
        name: "/effort",
        description: "Show or set reasoning effort",
    },
    SlashCommand {
        name: "/state",
        description: "Show current runtime state",
    },
    SlashCommand {
        name: "/stop",
        description: "Stop the running task",
    },
    SlashCommand {
        name: "/plugins",
        description: "List or manage plugins",
    },
    SlashCommand {
        name: "/mcp",
        description: "View and configure MCP servers",
    },
    SlashCommand {
        name: "/clear",
        description: "Clear the terminal",
    },
    SlashCommand {
        name: "/help",
        description: "Show available commands",
    },
    SlashCommand {
        name: "/exit",
        description: "Exit the TUI",
    },
];

pub(super) fn slash_command_name(input: &str) -> Option<&str> {
    if !input.starts_with('/') {
        return None;
    }
    let first_line = input.lines().next().unwrap_or_default();
    let command = first_line.split_whitespace().next().unwrap_or(first_line);
    Some(command)
}

pub(super) fn command_exists(name: &str) -> bool {
    SLASH_COMMANDS.iter().any(|command| command.name == name)
}

pub(super) fn selected_matching_command(
    input: &str,
    selected_command: usize,
) -> Option<SlashCommand> {
    let commands = matching_commands(input);
    if commands.is_empty() {
        return None;
    }
    Some(commands[selected_command.min(commands.len() - 1)])
}

pub(super) fn command_completion_suffix(input: &str, selected_command: usize) -> Option<String> {
    let prefix = slash_command_name(input)?;
    let rest = input.get(prefix.len()..)?;
    if !rest.is_empty() {
        return None;
    }
    let command = selected_matching_command(input, selected_command)?;
    let suffix = command.name.strip_prefix(prefix)?;
    if suffix.is_empty() {
        return None;
    }
    Some(format!("{suffix} "))
}

pub(super) fn matching_commands(input: &str) -> Vec<SlashCommand> {
    let query = input
        .split_whitespace()
        .next()
        .unwrap_or(input)
        .trim_start_matches('/');

    if query.is_empty() {
        return SLASH_COMMANDS.to_vec();
    }

    SLASH_COMMANDS
        .iter()
        .copied()
        .filter(|command| command.name.trim_start_matches('/').starts_with(query))
        .collect()
}
