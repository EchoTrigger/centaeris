use std::path::PathBuf;

pub(crate) const HELP: &str = "cet [--mode tui] [--workspace <directory>]\ncet --mode print|json [--session <id> | --cwd <directory>] <prompt>\ncet --mode rpc\ncet --help | --version\n\nPrint waits for the durable Run outcome and writes its final assistant text.\nJSON writes acceptance, canonical session/update notifications and the durable outcome.\nRPC forwards the existing Agent JSON-RPC protocol over standard input/output.\nUse -- before prompt text that begins with a dash.";

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Invocation {
    Interactive { workspace: Option<PathBuf> },
    Prompt(PromptOptions),
    Rpc,
    Help,
    Version,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OutputMode {
    Print,
    Json,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct PromptOptions {
    pub(crate) mode: OutputMode,
    pub(crate) session_id: Option<String>,
    pub(crate) cwd: Option<PathBuf>,
    pub(crate) message: String,
}

pub(crate) fn parse(arguments: &[String]) -> Result<Invocation, String> {
    let mut mode = "tui";
    let mut workspace = None;
    let mut session_id = None;
    let mut cwd = None;
    let mut prompt = Vec::new();
    let mut args = arguments.iter();
    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--help" | "-h" => return Ok(Invocation::Help),
            "--version" | "-V" => return Ok(Invocation::Version),
            "--mode" => mode = option_value(&mut args, "--mode")?,
            "--workspace" => {
                workspace = Some(PathBuf::from(option_value(&mut args, "--workspace")?))
            }
            "--session" => session_id = Some(option_value(&mut args, "--session")?.to_owned()),
            "--cwd" => cwd = Some(PathBuf::from(option_value(&mut args, "--cwd")?)),
            "--" => {
                prompt.extend(args.cloned());
                break;
            }
            value if value.starts_with('-') => return Err(format!("unknown CLI option: {value}")),
            _ => prompt.push(argument.clone()),
        }
    }
    if matches!(mode, "tui" | "rpc") {
        if session_id.is_some() || cwd.is_some() || !prompt.is_empty() {
            return Err(format!("{mode} mode does not accept prompt options"));
        }
        if mode == "tui" {
            return Ok(Invocation::Interactive { workspace });
        }
        if workspace.is_some() {
            return Err("--workspace is only supported in tui mode".into());
        }
        return Ok(Invocation::Rpc);
    }
    let mode = match mode {
        "print" => OutputMode::Print,
        "json" => OutputMode::Json,
        value => return Err(format!("unsupported CLI mode: {value}")),
    };
    if workspace.is_some() {
        return Err(
            "--workspace is only supported in tui mode; use --cwd for print or json".into(),
        );
    }
    if session_id.is_some() && cwd.is_some() {
        return Err("--session uses the existing Session's working directory; use --cwd when creating a Session".into());
    }
    let message = prompt.join(" ");
    if message.trim().is_empty() {
        return Err("print and json modes require a prompt".into());
    }
    Ok(Invocation::Prompt(PromptOptions {
        mode,
        session_id,
        cwd,
        message,
    }))
}

fn option_value<'a>(
    args: &mut std::slice::Iter<'a, String>,
    option: &str,
) -> Result<&'a str, String> {
    args.next()
        .map(String::as_str)
        .filter(|value| !value.trim().is_empty() && !value.starts_with("--"))
        .ok_or_else(|| format!("{option} requires a value"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arguments(args: &[&str]) -> Vec<String> {
        args.iter().map(|value| value.to_string()).collect()
    }

    #[test]
    fn modes_accept_their_workspace_session_and_directory_options() {
        assert_eq!(
            parse(&[]).unwrap(),
            Invocation::Interactive { workspace: None }
        );
        assert_eq!(
            parse(&arguments(&["--mode", "rpc"])).unwrap(),
            Invocation::Rpc
        );
        for args in [vec!["--mode", "tui"], vec![]] {
            assert_eq!(
                parse(&arguments(&args)).unwrap(),
                Invocation::Interactive { workspace: None }
            );
        }
        for args in [
            vec!["--workspace", "/work"],
            vec!["--mode", "tui", "--workspace", "/work"],
        ] {
            assert_eq!(
                parse(&arguments(&args)).unwrap(),
                Invocation::Interactive {
                    workspace: Some(PathBuf::from("/work"))
                }
            );
        }
        assert_eq!(
            parse(&arguments(&[
                "--mode",
                "print",
                "--session",
                "session-1",
                "hello",
                "world"
            ]))
            .unwrap(),
            Invocation::Prompt(PromptOptions {
                mode: OutputMode::Print,
                session_id: Some("session-1".into()),
                cwd: None,
                message: "hello world".into()
            })
        );
        assert_eq!(
            parse(&arguments(&[
                "--mode",
                "json",
                "--cwd",
                "/work",
                "--",
                "--literal"
            ]))
            .unwrap(),
            Invocation::Prompt(PromptOptions {
                mode: OutputMode::Json,
                session_id: None,
                cwd: Some(PathBuf::from("/work")),
                message: "--literal".into()
            })
        );
    }

    #[test]
    fn help_and_version_use_local_invocations() {
        for flag in ["--help", "-h"] {
            assert_eq!(parse(&arguments(&[flag])).unwrap(), Invocation::Help);
        }
        for flag in ["--version", "-V"] {
            assert_eq!(parse(&arguments(&[flag])).unwrap(), Invocation::Version);
        }
    }

    #[test]
    fn invalid_mode_options_and_missing_prompts_fail_loudly() {
        for args in [
            vec!["--mode"],
            vec!["--mode", "unknown"],
            vec!["--mode", "json"],
            vec!["--unknown"],
            vec!["--workspace"],
            vec!["--workspace", "--mode", "tui"],
            vec!["--mode", "rpc", "hello"],
            vec!["--mode", "rpc", "--workspace", "/work"],
            vec!["--mode", "tui", "--cwd", "/work"],
            vec!["--mode", "tui", "--session", "one"],
            vec!["--mode", "print", "--workspace", "/work", "hello"],
            vec![
                "--mode",
                "print",
                "--session",
                "one",
                "--cwd",
                "/work",
                "hello",
            ],
        ] {
            assert!(parse(&arguments(&args)).is_err(), "{args:?}");
        }
    }
}
