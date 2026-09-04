use crate::tool_projection::ToolTranscriptLine;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum TranscriptLine {
    User(String),
    Summary(String),
    LiveAssistant { markdown: String, separator: bool },
    Supplement(String),
    Tool(ToolTranscriptLine),
    Subagent(SubagentTranscriptLine),
    Error(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct SubagentTranscriptLine {
    pub(super) title: String,
    pub(super) summary: String,
    pub(super) status: String,
}
