use super::*;

pub(super) enum Action {
    List,
    Ssh {
        destination: String,
        command: String,
    },
    Start(String),
    Read(String),
    Stop(String),
}
pub(super) fn parse(input: &str) -> Result<Action, String> {
    let input = input.trim();
    if input.is_empty() || input == "list" {
        return Ok(Action::List);
    }
    if let Some(script) = input
        .strip_prefix("start ")
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return Ok(Action::Start(script.into()));
    }
    match input.split_whitespace().collect::<Vec<_>>().as_slice() {
        ["output", id] => Ok(Action::Read((*id).into())),
        ["stop", id] => Ok(Action::Stop((*id).into())),
        _ => Err("Usage: /process [list | start <bash command> | output <processSessionId> | stop <processSessionId>]".into()),
    }
}
#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Snapshot {
    process_session_id: String,
    session_id: String,
    program: String,
    args: Vec<String>,
    cwd: String,
    source: Source,
    state: State,
    exit_code: Option<i32>,
    termination_reason: Option<Reason>,
    cleanup_complete: bool,
    output_complete: bool,
    error: Option<String>,
}
#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
enum Source {
    HostCommand,
    AgentTool,
}
#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
enum State {
    Running,
    Stopping,
    Exited,
}
#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
enum Reason {
    Stopped,
    TimedOut,
}
impl Snapshot {
    fn describe(&self) -> String {
        format!(
            "{}\n{} {}\n{:?} · exit {:?} · {:?}\n{} · {:?} · cleanup {} · output complete {}{}",
            self.process_session_id,
            self.program,
            self.args.join(" "),
            self.state,
            self.exit_code,
            self.termination_reason,
            self.cwd,
            self.source,
            self.cleanup_complete,
            self.output_complete,
            self.error
                .as_ref()
                .map(|e| format!("\n{e}"))
                .unwrap_or_default()
        )
    }
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ProcessList {
    service_instance_id: String,
    processes: Vec<Snapshot>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Page {
    process: Snapshot,
    chunks: Vec<Chunk>,
    next_cursor: String,
    earliest_cursor: String,
    gap: bool,
    has_more: bool,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Chunk {
    cursor: String,
    stream: Stream,
    data_base64: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
enum Stream {
    Stdout,
    Stderr,
}
enum Kind {
    List,
    Start {
        script: String,
        operation_id: String,
    },
    Ssh {
        destination: String,
        command: String,
        operation_id: String,
    },
    Started,
    Read,
    Stop,
}
struct Pending {
    response: RuntimeResponse,
    kind: Kind,
}
pub(super) struct Panel {
    session_id: String,
    output_id: Option<String>,
    content: String,
    title: String,
    cursor: String,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    pending: Option<Pending>,
    scroll: u16,
    max_scroll: u16,
    complete: bool,
}
fn request(app: &mut App, method: &str, value: Value) -> Result<RuntimeResponse, String> {
    app.runtime
        .as_mut()
        .ok_or("Runtime disconnected")?
        .request_async(method, json!({"request":value}))
}
pub(super) fn open(app: &mut App, input: &str) -> Result<(), String> {
    open_action(app, parse(input)?)
}
pub(super) fn open_ssh(app: &mut App, input: &str) -> Result<(), String> {
    let (destination, command) = input
        .trim()
        .split_once(char::is_whitespace)
        .ok_or("Usage: /ssh <host> <remote command>")?;
    centaeris_runtime::openssh::command_args(destination, command.trim())?;
    open_action(
        app,
        Action::Ssh {
            destination: destination.into(),
            command: command.trim().into(),
        },
    )
}
fn open_action(app: &mut App, action: Action) -> Result<(), String> {
    ensure_runtime(app)?;
    let session = if matches!(action, Action::Start(_) | Action::Ssh { .. }) {
        ensure_active_session(app, "Background task")?
    } else {
        app.active_session.clone().ok_or("Select a session first")?
    };
    let mut panel = Panel {
        session_id: session.id,
        output_id: None,
        content: String::new(),
        title: "Background tasks".into(),
        cursor: "0".into(),
        stdout: Vec::new(),
        stderr: Vec::new(),
        pending: None,
        scroll: 0,
        max_scroll: 0,
        complete: false,
    };
    let (method, payload, kind) = match action {
        Action::List => (
            "process_session_list",
            json!({"sessionId":panel.session_id}),
            Kind::List,
        ),
        Action::Start(script) => (
            "process_session_list",
            json!({"sessionId":panel.session_id}),
            Kind::Start {
                script,
                operation_id: new_runtime_operation_id()?,
            },
        ),
        Action::Ssh {
            destination,
            command,
        } => (
            "process_session_list",
            json!({"sessionId":panel.session_id}),
            Kind::Ssh {
                destination,
                command,
                operation_id: new_runtime_operation_id()?,
            },
        ),
        Action::Read(id) => {
            panel.output_id = Some(id.clone());
            (
                "process_session_read",
                json!({"sessionId":panel.session_id,"processSessionId":id,"cursor":"0","waitMs":0}),
                Kind::Read,
            )
        }
        Action::Stop(id) => (
            "process_session_stop",
            json!({"sessionId":panel.session_id,"processSessionId":id}),
            Kind::Stop,
        ),
    };
    panel.pending = Some(Pending {
        response: request(app, method, payload)?,
        kind,
    });
    app.process_panel = Some(panel);
    clear_composer(app);
    Ok(())
}
pub(super) fn show_response(app: &mut App, title: &str, value: Value) {
    app.process_panel = Some(Panel {
        session_id: String::new(),
        output_id: None,
        content: serde_json::to_string_pretty(&value).unwrap_or_else(|e| e.to_string()),
        title: title.into(),
        cursor: "0".into(),
        stdout: vec![],
        stderr: vec![],
        pending: None,
        scroll: 0,
        max_scroll: 0,
        complete: true,
    });
    clear_composer(app);
}
fn parse_value<T: serde::de::DeserializeOwned>(value: Value) -> Result<T, String> {
    serde_json::from_value(value).map_err(|e| format!("Invalid process response: {e}"))
}
pub(super) fn poll(app: &mut App) -> bool {
    let Some(mut panel) = app.process_panel.take() else {
        return false;
    };
    let Some(pending) = panel.pending.take() else {
        app.process_panel = Some(panel);
        return false;
    };
    match pending.response.try_recv() {
        Ok(None) => {
            panel.pending = Some(pending);
            app.process_panel = Some(panel);
            false
        }
        result => {
            let result = result
                .map_err(|e| e.to_string())
                .and_then(|v| v.ok_or("Missing process response".into()))
                .and_then(|v| apply(app, &mut panel, pending.kind, v));
            if let Err(error) = result {
                panel.content.push_str(&format!("\n{error}"));
            }
            app.process_panel = Some(panel);
            true
        }
    }
}
fn apply(app: &mut App, panel: &mut Panel, kind: Kind, value: Value) -> Result<(), String> {
    match kind {
        Kind::List | Kind::Start { .. } | Kind::Ssh { .. } => {
            let list: ProcessList = parse_value(value)?;
            if list
                .processes
                .iter()
                .any(|p| p.session_id != panel.session_id)
            {
                return Err("Process owner mismatch".into());
            }
            if let Kind::Start {
                script,
                operation_id,
            } = kind
            {
                panel.content = format!("Starting background command · operationId {operation_id}");
                panel.pending = Some(Pending {
                    kind: Kind::Started,
                    response: request(
                        app,
                        "process_session_start",
                        json!({
                            "sessionId":panel.session_id,"serviceInstanceId":list.service_instance_id,"operationId":operation_id,
                            "program":"bash","args":["-c",script],"timeoutMs":0
                        }),
                    )?,
                });
            } else if let Kind::Ssh {
                destination,
                command,
                operation_id,
            } = kind
            {
                panel.content = format!("Starting OpenSSH · operationId {operation_id}");
                panel.pending = Some(Pending {
                    kind: Kind::Started,
                    response: request(
                        app,
                        "ssh_start",
                        json!({
                            "sessionId":panel.session_id,"serviceInstanceId":list.service_instance_id,"operationId":operation_id,
                            "destination":destination,"command":command,"timeoutMs":0
                        }),
                    )?,
                });
            } else {
                panel.content = if list.processes.is_empty() {
                    "No background tasks in this session".into()
                } else {
                    list.processes
                        .iter()
                        .map(Snapshot::describe)
                        .collect::<Vec<_>>()
                        .join("\n\n")
                };
            }
        }
        Kind::Started | Kind::Stop => {
            let snapshot: Snapshot = parse_value(value)?;
            if snapshot.session_id != panel.session_id {
                return Err("Process owner mismatch".into());
            }
            panel.content = snapshot.describe();
        }
        Kind::Read => {
            let page: Page = parse_value(value)?;
            if page.process.session_id != panel.session_id
                || Some(&page.process.process_session_id) != panel.output_id.as_ref()
            {
                return Err("Process owner mismatch".into());
            }
            cursor(&page.next_cursor)?;
            cursor(&page.earliest_cursor)?;
            if page.gap {
                panel.stdout.clear();
                panel.stderr.clear();
                panel
                    .content
                    .push_str("\n[Earlier output is no longer retained]\n");
            }
            for chunk in page.chunks {
                cursor(&chunk.cursor)?;
                let bytes = general_purpose::STANDARD
                    .decode(chunk.data_base64)
                    .map_err(|e| e.to_string())?;
                let pending = match chunk.stream {
                    Stream::Stdout => &mut panel.stdout,
                    Stream::Stderr => &mut panel.stderr,
                };
                panel.content.push_str(&decode(pending, &bytes, false));
            }
            panel.complete = page.process.output_complete && !page.has_more;
            if panel.complete {
                panel
                    .content
                    .push_str(&decode(&mut panel.stdout, &[], true));
                panel
                    .content
                    .push_str(&decode(&mut panel.stderr, &[], true));
            }
            panel.cursor = page.next_cursor;
            panel.title = format!(
                "{} · {:?} · cursor {}{}{}",
                page.process.process_session_id,
                page.process.state,
                panel.cursor,
                if page.has_more {
                    " · more output"
                } else if panel.complete {
                    " · output complete"
                } else {
                    ""
                },
                page.process
                    .error
                    .map(|e| format!(" · {e}"))
                    .unwrap_or_default()
            );
            if panel.content.len() > 256 * 1024 {
                let mut start = panel.content.len() - 256 * 1024;
                while !panel.content.is_char_boundary(start) {
                    start += 1;
                }
                panel.content = format!("[Showing latest 256 KiB]\n{}", &panel.content[start..]);
                panel.scroll = 0;
            }
        }
    }
    Ok(())
}
fn cursor(value: &str) -> Result<(), String> {
    let number = value.parse::<u64>().map_err(|_| "Invalid process cursor")?;
    if number.to_string() != value {
        return Err("Invalid process cursor".into());
    }
    Ok(())
}
fn decode(pending: &mut Vec<u8>, bytes: &[u8], complete: bool) -> String {
    pending.extend_from_slice(bytes);
    let mut result = String::new();
    loop {
        match std::str::from_utf8(pending) {
            Ok(text) => {
                result.push_str(text);
                pending.clear();
                break;
            }
            Err(error) => {
                let valid = error.valid_up_to();
                result.push_str(std::str::from_utf8(&pending[..valid]).expect("validated prefix"));
                pending.drain(..valid);
                match error.error_len() {
                    Some(length) => {
                        result.push('\u{fffd}');
                        pending.drain(..length);
                    }
                    None => {
                        if complete {
                            result.push('\u{fffd}');
                            pending.clear();
                        }
                        break;
                    }
                }
            }
        }
    }
    result
}
// The raw text is bounded and retained across pages, so split ANSI sequences are
// stripped on every render without ever writing escape codes to the terminal.
fn plain(text: &str) -> String {
    let mut result = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            match chars.next() {
                Some('[') => {
                    for c in chars.by_ref() {
                        if ('@'..='~').contains(&c) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    while let Some(c) = chars.next() {
                        if c == '\x07' || (c == '\x1b' && chars.peek() == Some(&'\\')) {
                            if c == '\x1b' {
                                chars.next();
                            }
                            break;
                        }
                    }
                }
                _ => {}
            }
        } else if !c.is_control() || c == '\n' || c == '\t' {
            result.push(c);
        }
    }
    result
}
pub(super) fn key(app: &mut App, key: KeyEvent) {
    if key.code == KeyCode::Esc
        || (key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL))
    {
        app.process_panel = None;
        return;
    }
    let Some(mut panel) = app.process_panel.take() else {
        return;
    };
    match key.code {
        KeyCode::Up => panel.scroll = panel.scroll.saturating_sub(1),
        KeyCode::Down => panel.scroll = panel.scroll.saturating_add(1).min(panel.max_scroll),
        KeyCode::PageUp => panel.scroll = panel.scroll.saturating_sub(10),
        KeyCode::PageDown => panel.scroll = panel.scroll.saturating_add(10).min(panel.max_scroll),
        KeyCode::Home => panel.scroll = 0,
        KeyCode::End => panel.scroll = panel.max_scroll,
        KeyCode::Char('r' | 'n') if panel.pending.is_none() && !panel.session_id.is_empty() => {
            let refresh = key.code == KeyCode::Char('r');
            let result = if let Some(id) = panel.output_id.as_ref() {
                if refresh {
                    panel.cursor = "0".into();
                    panel.content.clear();
                    panel.stdout.clear();
                    panel.stderr.clear();
                    panel.scroll = 0;
                }
                request(app,"process_session_read",json!({"sessionId":panel.session_id,"processSessionId":id,"cursor":panel.cursor,"waitMs":0})).map(|response|Pending{response,kind:Kind::Read})
            } else {
                request(
                    app,
                    "process_session_list",
                    json!({"sessionId":panel.session_id}),
                )
                .map(|response| Pending {
                    response,
                    kind: Kind::List,
                })
            };
            match result {
                Ok(pending) => panel.pending = Some(pending),
                Err(e) => panel.content.push_str(&format!("\n{e}")),
            }
        }
        _ => {}
    }
    app.process_panel = Some(panel);
}
pub(super) fn render(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    let Some(panel) = app.process_panel.as_mut() else {
        return;
    };
    let header = Rect { height: 2, ..area };
    let body = Rect {
        y: area.y + 2,
        height: area.height.saturating_sub(2),
        ..area
    };
    frame.render_widget(
        Paragraph::new(format!(
            "{}{}\nEsc close · ↑↓/PgUp/PgDn scroll{}",
            plain(&panel.title),
            if panel.pending.is_some() {
                " · loading"
            } else {
                ""
            },
            if panel.session_id.is_empty() {
                ""
            } else {
                " · r refresh · n next output"
            }
        )),
        header,
    );
    let lines = plain(&panel.content)
        .lines()
        .map(|s| Line::raw(s.to_owned()))
        .collect::<Vec<_>>();
    panel.max_scroll = paragraph_line_count(&lines, body.width).saturating_sub(body.height);
    panel.scroll = panel.scroll.min(panel.max_scroll);
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((panel.scroll, 0)),
        body,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rust_contract_samples_and_stream_decoding() {
        let samples: Value = serde_json::from_str(include_str!(
            "../../../runtime/generated/process-session-samples.json"
        ))
        .unwrap();
        let page: Page = parse_value(samples["output"].clone()).unwrap();
        let agent: Snapshot = parse_value(samples["agentSnapshot"].clone()).unwrap();
        assert!(matches!(agent.source, Source::AgentTool));
        assert_eq!(page.process.process_session_id, "process-sample");
        assert!(page.process.output_complete);
        let list: ProcessList = parse_value(samples["list"].clone()).unwrap();
        assert_eq!(list.service_instance_id, "service-sample");
        let mut pending = vec![];
        assert_eq!(decode(&mut pending, &[0xe4, 0xb8], false), "");
        assert_eq!(decode(&mut pending, &[0xad], true), "中");
        assert_eq!(plain("\x1b[31mred\x1b[0m\x1b]52;c;secret\x07!"), "red!");
        assert!(cursor("01").is_err());
    }
}
