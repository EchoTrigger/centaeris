//! OpenSSH owns authentication, configuration and remote-shell semantics.
pub fn command_args(destination: &str, command: &str) -> Result<Vec<String>, String> {
    if destination.is_empty()
        || destination.starts_with('-')
        || destination
            .chars()
            .any(|c| c.is_whitespace() || c.is_control())
    {
        return Err("destination must be one OpenSSH host alias or user@host".into());
    }
    if command.trim().is_empty() || command.contains('\0') {
        return Err("a nonempty remote shell command is required".into());
    }
    Ok([
        "-T",
        "-o",
        "BatchMode=yes",
        "-o",
        "ConnectTimeout=15",
        "--",
        destination,
        command,
    ]
    .into_iter()
    .map(str::to_owned)
    .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn remote_command_is_one_literal_argument_with_noninteractive_auth() {
        let args = command_args("user@dev", "printf '$HOME'; git status").unwrap();
        assert_eq!(
            args,
            [
                "-T",
                "-o",
                "BatchMode=yes",
                "-o",
                "ConnectTimeout=15",
                "--",
                "user@dev",
                "printf '$HOME'; git status"
            ]
        );
    }
    #[test]
    fn destination_cannot_inject_options_or_control_characters() {
        for destination in ["", "-oProxyCommand=evil", "dev\nother", "dev host"] {
            assert!(command_args(destination, "pwd").is_err());
        }
        assert!(command_args("dev", "").is_err());
    }
}
