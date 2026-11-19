use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RunState {
    Unknown,
    Running,
    Waiting,
    WaitingInput,
    Completed,
    Failed,
    Cancelled,
    Interrupted,
}

impl RunState {
    pub(super) fn from_source(source: &str, reason_type: Option<&str>) -> Self {
        match source {
            "agent_run_completed" => Self::Completed,
            "agent_run_failed" => Self::Failed,
            "agent_run_interrupted" => Self::interrupted(reason_type),
            // A historical start is not evidence that a run is still active.
            _ => Self::Unknown,
        }
    }

    pub(super) fn interrupted(reason_type: Option<&str>) -> Self {
        if reason_type == Some("cancelled") {
            Self::Cancelled
        } else {
            Self::Interrupted
        }
    }

    pub(super) fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::Cancelled | Self::Interrupted
        )
    }
}

pub(super) fn record_activity(state: &mut SessionState, event: &Value, run_id: Option<&str>) {
    let Some(run_id) = run_id else {
        return;
    };
    let activity = if event["type"] == "RuntimeWaitChanged" {
        match event.pointer("/payload/status").and_then(Value::as_str) {
            Some("waiting") => Some(RunState::Waiting),
            Some("resumed" | "abandoned") => Some(RunState::Running),
            _ => None,
        }
    } else if event["type"] == "QuestionRequired" {
        Some(RunState::WaitingInput)
    } else {
        event
            .get("processState")
            .and_then(Value::as_str)
            .map(|state| match state {
                "waiting_user" => RunState::WaitingInput,
                "waiting" | "provider_waiting" => RunState::Waiting,
                _ => RunState::Running,
            })
    };
    if let Some(activity) = activity {
        state.run_activity.insert(run_id.into(), activity);
    }
}

fn states(app: &App) -> HashMap<&str, (RunState, &str)> {
    let mut states = HashMap::new();
    for run_id in app
        .session
        .active_agent_run_ids
        .iter()
        .chain(app.session.active_agent_run_id.iter())
    {
        let state = app
            .session
            .run_activity
            .get(run_id)
            .copied()
            .unwrap_or(RunState::Running);
        states.insert(run_id.as_str(), (state, ""));
    }
    for line in &app.session.transcript {
        if let TranscriptLine::RunBoundary {
            run_id,
            state,
            reason,
        } = line
        {
            if *state != RunState::Unknown {
                states.insert(run_id.as_str(), (*state, reason.as_str()));
            }
        }
    }
    states
}

pub(super) fn process_visible(app: &App, run_id: &str) -> bool {
    states(app)
        .get(run_id)
        .is_none_or(|(state, _)| *state != RunState::Completed)
}

/// Preserve stage summaries, images and final answers while hiding completed execution detail.
pub(super) fn visible_transcript(app: &App) -> Vec<TranscriptLine> {
    let states = states(app);
    let mut visible = Vec::new();
    for item in &app.session.transcript {
        match item {
            TranscriptLine::RunItem {
                run_id,
                final_answer,
                line,
            } => {
                if matches!(line.content(), TranscriptLine::Reasoning { .. }) {
                    continue;
                }
                let completed = states
                    .get(run_id.as_str())
                    .is_some_and(|(state, _)| *state == RunState::Completed);
                let execution = match line.content() {
                    TranscriptLine::Tool(tool) => !tool.is_bash && tool.images.is_empty(),
                    TranscriptLine::Subagent(_) | TranscriptLine::LiveAssistant { .. } => true,
                    _ => false,
                };
                if !completed || *final_answer || !execution {
                    visible.push(line.content().clone());
                }
            }
            TranscriptLine::RunBoundary {
                run_id,
                state,
                reason,
            } => match state {
                RunState::Failed | RunState::Interrupted => {
                    visible.push(TranscriptLine::RunFailure {
                        key: format!("run:{run_id}"),
                        reason: reason.clone(),
                    })
                }
                _ => {}
            },
            TranscriptLine::Reasoning { .. } => {}
            _ => {
                visible.push(item.clone());
            }
        }
    }
    let active = active_agent_run_ids(&app.session);
    let primary = app
        .session
        .active_agent_run_id
        .as_deref()
        .or_else(|| active.first().map(String::as_str));
    for run_id in active.iter().filter(|id| Some(id.as_str()) != primary) {
        let label = match states.get(run_id.as_str()).map(|(state, _)| *state) {
            Some(RunState::Waiting) => "Waiting",
            Some(RunState::WaitingInput) => "Waiting for input",
            Some(RunState::Running) => "Working…",
            _ => continue,
        };
        visible.push(TranscriptLine::RunActivity {
            key: format!("status:{run_id}"),
            label,
        });
    }
    visible
}
