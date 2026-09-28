//! OpenSSH owns authentication, configuration and remote-shell semantics.
pub fn command_args(destination: &str, command: &str) -> Result<Vec<String>, String> {
    command_args_with_timeout(destination, command, None)
}
pub fn command_args_with_timeout(
    destination: &str,
    command: &str,
    connect_timeout_seconds: Option<u32>,
) -> Result<Vec<String>, String> {
    if connect_timeout_seconds == Some(0) {
        return Err("connect timeout must be positive".into());
    }
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
    let mut args = vec!["-T".into(), "-o".into(), "BatchMode=yes".into()];
    if let Some(seconds) = connect_timeout_seconds {
        args.extend(["-o".into(), format!("ConnectTimeout={seconds}")]);
    }
    args.extend(["--".into(), destination.into(), command.into()]);
    Ok(args)
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

#[cfg(test)]
#[test]
fn default_connection_respects_openssh_timeout_configuration() {
    let args = command_args("host", "pwd").unwrap();
    assert!(!args.iter().any(|s| s.starts_with("ConnectTimeout=")));
}

#[cfg(test)]
#[test]
fn explicit_connect_timeout_is_separate_from_command_lifetime() {
    assert!(command_args_with_timeout("host", "pwd", Some(90))
        .unwrap()
        .contains(&"ConnectTimeout=90".into()));
    assert!(command_args_with_timeout("host", "pwd", Some(0)).is_err());
}
