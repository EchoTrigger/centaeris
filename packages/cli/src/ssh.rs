pub(crate) const HELP: &str = "cet ssh connect <host>";

pub(crate) fn run(args: &[String]) -> Result<i32, String> {
    match args {
        [help] if matches!(help.as_str(), "--help" | "help") => {
            println!("{HELP}");
            Ok(0)
        }
        [command, host] if command == "connect" => {
            centaeris_agent::openssh::command_args(host, "validate")?;
            // Inherit the terminal so OpenSSH can handle interactive prompts.
            let status = std::process::Command::new("ssh")
                .args(["--", host])
                .status()
                .map_err(|error| format!("start system OpenSSH: {error}"))?;
            Ok(status.code().unwrap_or(1))
        }
        _ => Err(HELP.into()),
    }
}
