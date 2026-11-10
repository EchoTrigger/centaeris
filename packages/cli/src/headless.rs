use centaeris_desktop_services::rpc_client::{RuntimeClient, RuntimeEvent, SessionCommand};
use serde_json::{json, Value};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub(crate) const HELP: &str = "centa [--mode tui]\ncenta --mode print|json [--session <id> | --cwd <directory>] <prompt>\ncenta --mode rpc\n\nPrint waits for the durable Run outcome and writes its final assistant text.\nJSON writes acceptance, canonical session/update notifications and the durable outcome.\nRPC forwards the existing Agent JSON-RPC protocol over standard input/output.\nUse -- before prompt text that begins with a dash.";

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Invocation {
    Interactive,
    Prompt(PromptOptions),
    Rpc,
    Help,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OutputMode {
    Print,
    Json,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct PromptOptions {
    mode: OutputMode,
    session_id: Option<String>,
    cwd: Option<PathBuf>,
    message: String,
}

pub(crate) fn parse(arguments: &[String]) -> Result<Invocation, String> {
    let mut mode = "tui";
    let mut session_id = None;
    let mut cwd = None;
    let mut prompt = Vec::new();
    let mut args = arguments.iter();
    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--help" | "-h" => return Ok(Invocation::Help),
            "--mode" => mode = option_value(&mut args, "--mode")?,
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
        return Ok(if mode == "tui" {
            Invocation::Interactive
        } else {
            Invocation::Rpc
        });
    }
    let mode = match mode {
        "print" => OutputMode::Print,
        "json" => OutputMode::Json,
        value => return Err(format!("unsupported CLI mode: {value}")),
    };
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

pub(crate) fn run(options: PromptOptions) -> Result<i32, String> {
    let stdout = io::stdout();
    let mut output = stdout.lock();
    let result = run_prompt(&options, &mut output);
    match result {
        Ok(outcome) => {
            write_outcome(options.mode, &outcome, &mut output)?;
            if outcome.status != "succeeded" {
                eprintln!("{}", outcome.error.as_deref().unwrap_or(&outcome.status));
            }
            Ok(outcome.exit_code())
        }
        Err(error) => {
            if options.mode == OutputMode::Json {
                write_json(&mut output, &json!({"type": "error", "message": error}))?;
            }
            Err(error)
        }
    }
}

fn run_prompt(options: &PromptOptions, output: &mut impl Write) -> Result<RunOutcome, String> {
    let mut client = RuntimeClient::start()?;
    let viewer_id = format!("cli-{}", std::process::id());
    let descriptor = client
        .request(
            "initialize",
            json!({"request": {"clientKind": "tui", "viewerId": viewer_id}}),
        )
        .map_err(|error| error.to_string())?;
    client.validate_initialize_descriptor(&descriptor)?;
    let operation = operation_id()?;
    let session_id = match &options.session_id {
        Some(session_id) => {
            client
                .session_request(SessionCommand::Load, json!({"sessionId": session_id}))
                .map_err(|error| error.to_string())?;
            session_id.clone()
        }
        None => {
            let cwd = std::path::absolute(options.cwd.as_deref().unwrap_or(Path::new(".")))
                .map_err(|error| format!("resolve working directory failed: {error}"))?;
            let session = client
                .session_request(
                    SessionCommand::Create,
                    json!({"operationId": format!("cli-create-{operation}"), "cwd": cwd}),
                )
                .map_err(|error| error.to_string())?;
            required_string(&session, "id")?.to_owned()
        }
    };
    client
        .session_request(
            SessionCommand::Subscribe,
            json!({"sessionId": session_id, "viewerId": viewer_id}),
        )
        .map_err(|error| error.to_string())?;
    let accepted = client
        .session_request(
            SessionCommand::Send,
            json!({
                "operationId": format!("cli-prompt-{operation}"),
                "sessionId": session_id,
                "message": options.message,
            }),
        )
        .map_err(|error| error.to_string())?;
    let run_id = required_string(&accepted, "agentRunId")?.to_owned();
    if required_string(&accepted, "sessionId")? != session_id {
        return Err("session/prompt returned a different Session".into());
    }
    if options.mode == OutputMode::Json {
        write_json(output, &json!({"type": "accepted", "result": accepted}))?;
    }
    let signals = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .map_err(|error| format!("start CLI signal listener failed: {error}"))?;
    let mut interrupt = signals.spawn(tokio::signal::ctrl_c());
    let mut cancellation_requested = false;
    loop {
        if !cancellation_requested && interrupt.is_finished() {
            signals
                .block_on(&mut interrupt)
                .map_err(|error| format!("CLI signal listener failed: {error}"))?
                .map_err(|error| format!("listen for Ctrl-C failed: {error}"))?;
            client
                .session_request(
                    SessionCommand::Cancel,
                    json!({
                        "sessionId": session_id,
                        "agentRunId": run_id,
                        "reason": "user_cancelled",
                    }),
                )
                .map_err(|error| error.to_string())?;
            cancellation_requested = true;
        }
        let Some(event) = client.recv_event_timeout(Duration::from_millis(100))? else {
            continue;
        };
        match event {
            RuntimeEvent::SessionUpdate(update) => {
                if update.get("sessionId").and_then(Value::as_str) != Some(session_id.as_str())
                    || update.get("agentRunId").and_then(Value::as_str) != Some(run_id.as_str())
                {
                    continue;
                }
                if options.mode == OutputMode::Json {
                    write_json(
                        output,
                        &json!({"jsonrpc": "2.0", "method": "session/update", "params": update}),
                    )?;
                }
                if is_durable_terminal(&update, &session_id, &run_id) {
                    break;
                }
            }
            RuntimeEvent::RuntimeConfigChanged => {}
            RuntimeEvent::Error(error) => return Err(error),
        }
    }
    let runs = client
        .session_request(
            SessionCommand::ListRuns,
            json!({"sessionId": session_id, "includeTerminal": true}),
        )
        .map_err(|error| error.to_string())?;
    let session = client
        .session_request(SessionCommand::Load, json!({"sessionId": session_id}))
        .map_err(|error| error.to_string())?;
    RunOutcome::from_projections(&runs, &session, &run_id)
}

fn operation_id() -> Result<String, String> {
    let mut bytes = [0; 16];
    getrandom::fill(&mut bytes).map_err(|error| error.to_string())?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn required_string<'a>(value: &'a Value, field: &str) -> Result<&'a str, String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("Agent response missing {field}"))
}

fn is_durable_terminal(update: &Value, session_id: &str, run_id: &str) -> bool {
    update.get("sessionId").and_then(Value::as_str) == Some(session_id)
        && update.get("agentRunId").and_then(Value::as_str) == Some(run_id)
        && update.pointer("/payload/type").and_then(Value::as_str) == Some("session_event")
        && matches!(
            update
                .pointer("/payload/event/type")
                .and_then(Value::as_str),
            Some("AgentRunCompleted" | "AgentRunFailed" | "AgentRunInterrupted")
        )
}

#[derive(Debug)]
struct RunOutcome {
    run: Value,
    status: String,
    output_text: String,
    error: Option<String>,
}

impl RunOutcome {
    fn from_projections(runs: &Value, session: &Value, run_id: &str) -> Result<Self, String> {
        let run = runs
            .get("agentRuns")
            .and_then(Value::as_array)
            .ok_or("Agent Run list response missing agentRuns")?
            .iter()
            .find(|run| run.get("agentRunId").and_then(Value::as_str) == Some(run_id))
            .ok_or("completed Agent Run is missing from its durable projection")?;
        let status = required_string(run, "status")?;
        if !matches!(status, "succeeded" | "failed" | "cancelled" | "stopped") {
            return Err(format!("expected a durable terminal Run, got {status}"));
        }
        let messages = session
            .get("messages")
            .and_then(Value::as_array)
            .ok_or("Session response missing messages")?;
        let message = messages.iter().rev().find(|message| {
            message.get("agentRunId").and_then(Value::as_str) == Some(run_id)
                && message.get("role").and_then(Value::as_str) == Some("assistant")
        });
        let output_text = match message {
            Some(message) => message
                .get("content")
                .and_then(Value::as_str)
                .ok_or("assistant message is missing content")?
                .to_owned(),
            None => String::new(),
        };
        Ok(Self {
            run: run.clone(),
            status: status.to_owned(),
            output_text,
            error: run.get("error").and_then(Value::as_str).map(str::to_owned),
        })
    }

    fn exit_code(&self) -> i32 {
        match self.status.as_str() {
            "succeeded" => 0,
            "cancelled" => 130,
            _ => 1,
        }
    }
}

fn write_outcome(
    mode: OutputMode,
    outcome: &RunOutcome,
    output: &mut impl Write,
) -> Result<(), String> {
    match mode {
        OutputMode::Print if outcome.status == "succeeded" => {
            writeln!(output, "{}", outcome.output_text).map_err(|error| error.to_string())?;
            output.flush().map_err(|error| error.to_string())
        }
        OutputMode::Print => Ok(()),
        OutputMode::Json => write_json(
            output,
            &json!({
                "type": "completed", "run": outcome.run, "outputText": outcome.output_text,
            }),
        ),
    }
}

fn write_json(output: &mut impl Write, value: &Value) -> Result<(), String> {
    serde_json::to_writer(&mut *output, value).map_err(|error| error.to_string())?;
    writeln!(output).map_err(|error| error.to_string())?;
    output.flush().map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arguments(args: &[&str]) -> Vec<String> {
        args.iter().map(|value| value.to_string()).collect()
    }

    #[test]
    fn modes_preserve_default_tui_and_accept_explicit_session_or_directory() {
        assert_eq!(parse(&[]).unwrap(), Invocation::Interactive);
        assert_eq!(
            parse(&arguments(&["--mode", "rpc"])).unwrap(),
            Invocation::Rpc
        );
        assert_eq!(
            parse(&arguments(&["--mode", "tui"])).unwrap(),
            Invocation::Interactive
        );
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
    fn invalid_mode_options_and_missing_prompts_fail_loudly() {
        for args in [
            vec!["--mode"],
            vec!["--mode", "unknown"],
            vec!["--mode", "json"],
            vec!["--unknown"],
            vec!["--mode", "rpc", "hello"],
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

    #[test]
    fn completion_requires_the_target_runs_committed_terminal_event() {
        let event = json!({"sessionId": "session", "agentRunId": "run", "payload": {
            "type": "session_event", "event": {"type": "AgentRunCompleted"},
        }});
        assert!(is_durable_terminal(&event, "session", "run"));
        assert!(!is_durable_terminal(&event, "other", "run"));
        assert!(!is_durable_terminal(&event, "session", "other"));
        let mut ephemeral = event.clone();
        ephemeral["payload"]["type"] = json!("runtime_event");
        assert!(!is_durable_terminal(&ephemeral, "session", "run"));
        let mut final_text = event;
        final_text["payload"]["event"]["type"] = json!("Final");
        assert!(!is_durable_terminal(&final_text, "session", "run"));
    }

    #[test]
    fn output_uses_complete_durable_assistant_text_and_preserves_run_outcomes() {
        let text = "line one\n完整输出 ".repeat(100);
        let mut runs =
            json!({"agentRuns": [{"agentRunId": "run", "status": "succeeded", "error": null}]});
        let session = json!({"messages": [
            {"agentRunId": "older", "role": "assistant", "content": "old"},
            {"agentRunId": "run", "role": "assistant", "content": text},
        ]});
        let outcome = RunOutcome::from_projections(&runs, &session, "run").unwrap();
        let mut output = Vec::new();
        write_outcome(OutputMode::Print, &outcome, &mut output).unwrap();
        assert_eq!(String::from_utf8(output).unwrap(), format!("{text}\n"));
        assert_eq!(outcome.exit_code(), 0);
        for (status, exit_code) in [("cancelled", 130), ("stopped", 1), ("failed", 1)] {
            runs["agentRuns"][0]["status"] = json!(status);
            let outcome = RunOutcome::from_projections(&runs, &session, "run").unwrap();
            assert_eq!(outcome.exit_code(), exit_code);
            let mut output = Vec::new();
            write_outcome(OutputMode::Json, &outcome, &mut output).unwrap();
            let value: Value = serde_json::from_slice(&output).unwrap();
            assert_eq!(value["type"], "completed");
            assert_eq!(value["run"]["status"], status);
        }
        runs["agentRuns"][0]["status"] = json!("running");
        assert!(RunOutcome::from_projections(&runs, &session, "run").is_err());
    }
}
