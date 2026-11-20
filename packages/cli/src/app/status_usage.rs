use super::{show_message, App};
use centaeris_cli_runtime::rpc_client::TerminalTokenUsage;
use std::sync::mpsc::{Receiver, TryRecvError};

#[derive(Default)]
pub(super) struct StatusUsage {
    context: Option<u64>,
    output: Option<(String, u64)>,
    pending: Option<PendingUsage>,
    read_context: bool,
    read_output: bool,
}

struct PendingUsage {
    response: Receiver<Result<TerminalTokenUsage, String>>,
    run_id: Option<String>,
    include_context: bool,
}

impl StatusUsage {
    pub(super) fn request_output(&mut self, running: bool) {
        self.read_output = running;
    }

    pub(super) fn record_context(&mut self, context: Option<u64>) {
        self.context = context;
        self.read_context = false;
        if let Some(pending) = self.pending.as_mut() {
            pending.include_context = false;
        }
    }
}

pub(super) fn restore(app: &mut App) {
    let usage = &mut app.session.status_usage;
    usage.context = None;
    usage.output = None;
    usage.pending = None;
    usage.read_context = app.session.active_session.is_some();
    usage.read_output = app.session.active_agent_run_id.is_some();
}

pub(super) fn poll(app: &mut App) -> bool {
    let usage = &mut app.session.status_usage;
    let current_run = app.session.active_agent_run_id.as_deref();
    let mut changed = false;
    if let Some(pending) = usage.pending.take() {
        let result = match pending.response.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => {
                usage.pending = Some(pending);
                return false;
            }
            Err(TryRecvError::Disconnected) => Err("Token usage read was interrupted".into()),
        };
        let current_output = pending.run_id.as_deref() == current_run && current_run.is_some();
        match result {
            Ok(snapshot) => {
                if pending.include_context {
                    changed |= usage.context != snapshot.context_tokens;
                    usage.context = snapshot.context_tokens;
                }
                if current_output {
                    let output = pending.run_id.zip(snapshot.output_tokens);
                    changed |= usage.output != output;
                    usage.output = output;
                }
            }
            Err(error) if pending.include_context || current_output => {
                show_message(app, error);
                return true;
            }
            Err(_) => {}
        }
    }
    let include_context = std::mem::take(&mut usage.read_context);
    let run_id = std::mem::take(&mut usage.read_output)
        .then_some(current_run)
        .flatten();
    usage.pending = (|| {
        let runtime = app.runtime.as_ref()?;
        let session = app.session.active_session.as_ref()?;
        if !include_context && run_id.is_none() {
            return None;
        }
        Some(PendingUsage {
            response: runtime.terminal_token_usage(&session.id, run_id, include_context)?,
            run_id: run_id.map(str::to_owned),
            include_context,
        })
    })();
    changed
}

pub(super) fn context_label(app: &App) -> Option<String> {
    let usage = &app.session.status_usage;
    let window = short_tokens(app.model_context_window?);
    Some(match usage.context {
        Some(context) => format!("{} / {window}", short_tokens(context)),
        None => format!("– / {window}"),
    })
}

pub(super) fn output_label(app: &App) -> Option<String> {
    let (run_id, tokens) = app.session.status_usage.output.as_ref()?;
    (app.session.active_agent_run_id.as_deref() == Some(run_id.as_str()))
        .then(|| format!("↑{}", short_tokens(*tokens)))
}

fn short_tokens(tokens: u64) -> String {
    let (scale, suffix) = match tokens {
        0..=999 => return tokens.to_string(),
        1_000..=999_999 => (1_000, "k"),
        _ => (1_000_000, "m"),
    };
    let whole = tokens / scale;
    let fractional = tokens % scale / (scale / 10);
    if fractional == 0 {
        format!("{whole}{suffix}")
    } else {
        format!("{whole}.{fractional}{suffix}")
    }
}
