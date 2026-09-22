use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RunState {
    Unknown,
    Running,
    Waiting,
    WaitingInput,
    Completed,
    Failed,
    Interrupted,
}

impl RunState {
    pub(super) fn from_source(source: &str) -> Self {
        match source {
            "agent_run_completed" => Self::Completed,
            "agent_run_failed" => Self::Failed,
            "agent_run_interrupted" => Self::Interrupted,
            // A historical start is not evidence that a run is still active.
            _ => Self::Unknown,
        }
    }
}

pub(super) fn record_activity(app: &mut App, event: &Value, run_id: Option<&str>) {
    let Some(run_id) = run_id else {
        return;
    };
    let state = if event["type"] == "RuntimeWaitChanged" {
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
    if let Some(state) = state {
        app.run_activity.insert(run_id.into(), state);
    }
}

fn states(app: &App) -> HashMap<&str, (RunState, &str)> {
    let mut states = HashMap::new();
    for run_id in app
        .active_agent_run_ids
        .iter()
        .chain(app.active_agent_run_id.iter())
    {
        let state = app
            .run_activity
            .get(run_id)
            .copied()
            .unwrap_or(RunState::Running);
        states.insert(run_id.as_str(), (state, ""));
    }
    for line in &app.transcript {
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

/// Preserve source order. Completed execution details become a structural line;
/// stage summaries and final answers remain visible without disclosure controls.
pub(super) fn visible_transcript(app: &App) -> Vec<TranscriptLine> {
    let states = states(app);
    let mut visible = Vec::new();
    let mut separated = HashSet::new();
    let mut last_hidden: Option<&str> = None;
    for item in &app.transcript {
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
                let execution = matches!(
                    line.content(),
                    TranscriptLine::Tool(_)
                        | TranscriptLine::Subagent(_)
                        | TranscriptLine::LiveAssistant { .. }
                );
                if completed && !final_answer && execution {
                    if last_hidden != Some(run_id) {
                        visible.push(TranscriptLine::ProcessEnd {
                            key: format!("run:{run_id}"),
                            label: String::new(),
                        });
                        separated.insert(run_id.as_str());
                    }
                    last_hidden = Some(run_id);
                } else {
                    visible.push(line.content().clone());
                    last_hidden = None;
                }
            }
            TranscriptLine::RunBoundary {
                run_id,
                state,
                reason,
            } => match state {
                RunState::Completed if separated.insert(run_id.as_str()) => {
                    visible.push(TranscriptLine::ProcessEnd {
                        key: format!("run:{run_id}"),
                        label: String::new(),
                    })
                }
                RunState::Failed | RunState::Interrupted => {
                    visible.push(TranscriptLine::ProcessEnd {
                        key: format!("run:{run_id}"),
                        label: format!(
                            "{}{}",
                            if *state == RunState::Failed {
                                "Failed"
                            } else {
                                "Interrupted"
                            },
                            if reason.is_empty() {
                                String::new()
                            } else {
                                format!(" · {reason}")
                            }
                        ),
                    })
                }
                _ => {}
            },
            TranscriptLine::Reasoning { .. } => {}
            _ => {
                visible.push(item.clone());
                last_hidden = None;
            }
        }
    }
    for run_id in active_agent_run_ids(app) {
        let state = states
            .get(run_id.as_str())
            .map(|(state, _)| *state)
            .unwrap_or(RunState::Running);
        let label = match state {
            RunState::Running => "Working…",
            RunState::Waiting => "Waiting",
            RunState::WaitingInput => "Waiting for input",
            _ => continue,
        };
        visible.push(TranscriptLine::ProcessEnd {
            key: format!("status:{run_id}"),
            label: label.into(),
        });
    }
    visible
}
