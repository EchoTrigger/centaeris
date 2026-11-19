use crate::tool_projection::ToolTranscriptLine;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum TranscriptLine {
    RunItem {
        run_id: String,
        final_answer: bool,
        line: Box<TranscriptLine>,
    },
    RunBoundary {
        run_id: String,
        state: super::runs::RunState,
        reason: String,
    },
    RunFailure {
        key: String,
        reason: String,
    },
    RunActivity {
        key: String,
        label: &'static str,
    },
    User(String),
    Summary(String),
    ReferencedAnswer {
        key: String,
    },
    LiveAssistant {
        markdown: String,
        separator: bool,
    },
    Supplement(String),
    Reasoning {
        key: String,
        text: String,
        final_started: bool,
    },
    Tool(Box<ToolTranscriptLine>),
    Subagent(SubagentTranscriptLine),
    Error(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct SubagentTranscriptLine {
    pub(super) title: String,
    pub(super) summary: String,
    pub(super) status: String,
}

impl TranscriptLine {
    pub(super) fn tool(line: ToolTranscriptLine) -> Self {
        Self::Tool(Box::new(line))
    }

    pub(super) fn for_run(self, run_id: Option<&str>, final_answer: bool) -> Self {
        match run_id {
            Some(run_id) => Self::RunItem {
                run_id: run_id.into(),
                final_answer,
                line: Box::new(self),
            },
            None => self,
        }
    }

    pub(super) fn content(&self) -> &Self {
        match self {
            Self::RunItem { line, .. } => line.content(),
            _ => self,
        }
    }

    pub(super) fn content_mut(&mut self) -> &mut Self {
        match self {
            Self::RunItem { line, .. } => line.content_mut(),
            _ => self,
        }
    }
}
