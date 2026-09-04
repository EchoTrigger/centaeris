use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolFailureKind {
    CommandFailed,
    TimedOut,
    SandboxUnavailable,
    HostUnavailable,
    ProviderError,
    PermissionDenied,
    InvalidInput,
    Cancelled,
    Unknown,
}

impl ToolFailureKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::CommandFailed => "command_failed",
            Self::TimedOut => "timed_out",
            Self::SandboxUnavailable => "sandbox_unavailable",
            Self::HostUnavailable => "host_unavailable",
            Self::ProviderError => "provider_error",
            Self::PermissionDenied => "permission_denied",
            Self::InvalidInput => "invalid_input",
            Self::Cancelled => "cancelled",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolErrorInfo {
    pub kind: ToolFailureKind,
    pub model_message: String,
    pub user_message: String,
    pub diagnostic_id: Option<String>,
    pub retryable: bool,
}

impl ToolErrorInfo {
    pub fn new(
        kind: ToolFailureKind,
        model_message: impl Into<String>,
        user_message: impl Into<String>,
    ) -> Self {
        Self {
            kind,
            model_message: model_message.into(),
            user_message: user_message.into(),
            diagnostic_id: None,
            retryable: false,
        }
    }

    pub fn with_diagnostic(mut self, diagnostic_id: impl Into<String>) -> Self {
        self.diagnostic_id = Some(diagnostic_id.into());
        self
    }

    pub fn with_retryable(mut self, retryable: bool) -> Self {
        self.retryable = retryable;
        self
    }

    pub fn from_unstructured_error(message: impl Into<String>) -> Self {
        let raw_message = message.into();
        let kind = classify_unstructured_tool_failure(raw_message.as_str());
        let retryable = matches!(
            kind,
            ToolFailureKind::TimedOut
                | ToolFailureKind::SandboxUnavailable
                | ToolFailureKind::HostUnavailable
        );
        let sanitized = sanitize_unstructured_tool_error(raw_message.as_str());
        let (model_message, user_message) = match &kind {
            ToolFailureKind::TimedOut => (
                "command timed out before completing".to_string(),
                "Command timed out".to_string(),
            ),
            ToolFailureKind::SandboxUnavailable => (
                "sandbox unavailable; refusing to degrade to an unsandboxed process".to_string(),
                "Sandbox unavailable".to_string(),
            ),
            ToolFailureKind::HostUnavailable => (
                "execution host is unavailable; the sandbox runtime may need restarting"
                    .to_string(),
                "Execution host unavailable".to_string(),
            ),
            ToolFailureKind::ProviderError => (
                if sanitized.is_empty() {
                    "dynamic tool provider returned an error".to_string()
                } else {
                    format!("dynamic tool provider returned an error: {sanitized}")
                },
                "Dynamic tool execution failed".to_string(),
            ),
            ToolFailureKind::PermissionDenied => (
                "tool execution was denied by policy or permission requirements".to_string(),
                "Tool execution denied".to_string(),
            ),
            ToolFailureKind::InvalidInput => invalid_tool_input_messages(raw_message.as_str()),
            ToolFailureKind::Cancelled => (
                "tool execution was cancelled before completion".to_string(),
                "Tool execution cancelled".to_string(),
            ),
            ToolFailureKind::CommandFailed => (
                if sanitized.is_empty() {
                    "command did not execute successfully".to_string()
                } else {
                    format!("command did not execute successfully: {sanitized}")
                },
                "Command execution failed".to_string(),
            ),
            ToolFailureKind::Unknown => (
                if sanitized.is_empty() {
                    "tool execution encountered an unexpected error".to_string()
                } else {
                    format!("tool execution encountered an unexpected error: {sanitized}")
                },
                "Tool execution error".to_string(),
            ),
        };
        Self {
            kind,
            model_message,
            user_message,
            diagnostic_id: Some(tool_error_diagnostic_id(raw_message.as_str())),
            retryable,
        }
    }

    pub fn from_execution_host_failure(
        failure_kind: &str,
        exit_code: Option<i32>,
        _timed_out: bool,
    ) -> Self {
        let kind = match failure_kind {
            "timed_out" | "timedOut" => ToolFailureKind::TimedOut,
            "sandbox_unavailable" | "sandboxUnavailable" => ToolFailureKind::SandboxUnavailable,
            "host_unavailable" | "hostUnavailable" => ToolFailureKind::HostUnavailable,
            "invalid_input" | "invalidInput" => ToolFailureKind::InvalidInput,
            "command_failed" | "commandFailed" if exit_code != Some(0) => {
                ToolFailureKind::CommandFailed
            }
            _ => ToolFailureKind::Unknown,
        };
        let retryable = matches!(
            kind,
            ToolFailureKind::TimedOut
                | ToolFailureKind::SandboxUnavailable
                | ToolFailureKind::HostUnavailable
        );
        let (model_message, user_message) = match &kind {
            ToolFailureKind::TimedOut => (
                "command timed out before completing".to_string(),
                "Command timed out".to_string(),
            ),
            ToolFailureKind::HostUnavailable => (
                "execution host is unavailable; the sandbox runtime may need restarting"
                    .to_string(),
                "Execution host unavailable".to_string(),
            ),
            ToolFailureKind::CommandFailed => (
                format!("command failed with exit code {}", exit_code.unwrap_or(-1)),
                "Command execution failed".to_string(),
            ),
            ToolFailureKind::SandboxUnavailable => (
                "sandbox unavailable; refusing to degrade to an unsandboxed process".to_string(),
                "Sandbox unavailable".to_string(),
            ),
            ToolFailureKind::InvalidInput => (
                "tool input is invalid; revise the tool arguments and retry".to_string(),
                "Invalid tool input".to_string(),
            ),
            _ => (
                "tool execution encountered an unexpected error".to_string(),
                "Tool execution error".to_string(),
            ),
        };
        Self {
            kind,
            model_message,
            user_message,
            diagnostic_id: None,
            retryable,
        }
    }
}

impl From<String> for ToolErrorInfo {
    fn from(message: String) -> Self {
        Self::from_unstructured_error(message)
    }
}

impl From<&str> for ToolErrorInfo {
    fn from(message: &str) -> Self {
        Self::from_unstructured_error(message)
    }
}

fn classify_unstructured_tool_failure(raw_message: &str) -> ToolFailureKind {
    let lower = raw_message.to_ascii_lowercase();
    if lower.contains("timed out") || lower.contains("timeout") || lower.contains("process_timeout")
    {
        ToolFailureKind::TimedOut
    } else if lower.contains("sandbox unavailable")
        || lower.contains("sandbox_unavailable")
        || lower.contains("sandbox runtime is currently unavailable")
        || lower.contains("sandbox runtime is unavailable")
    {
        ToolFailureKind::SandboxUnavailable
    } else if lower.contains("execution host")
        || lower.contains("host unavailable")
        || lower.contains("host_unavailable")
        || lower.contains("worker process")
    {
        ToolFailureKind::HostUnavailable
    } else if lower.contains("permission denied")
        || lower.contains("permission_denied")
        || lower.contains("denied")
    {
        ToolFailureKind::PermissionDenied
    } else if lower.contains("read path is not a file")
        || lower.contains("path is required")
        || lower.contains("invalid tool input")
        || lower.contains("invalid_input")
    {
        ToolFailureKind::InvalidInput
    } else if lower.contains("cancelled") || lower.contains("canceled") {
        ToolFailureKind::Cancelled
    } else if lower.contains("provider") {
        ToolFailureKind::ProviderError
    } else if lower.contains("command_failed")
        || lower.contains("process_exit_nonzero")
        || (lower.contains("exit") && lower.contains("nonzero"))
    {
        ToolFailureKind::CommandFailed
    } else {
        ToolFailureKind::Unknown
    }
}

fn invalid_tool_input_messages(raw_message: &str) -> (String, String) {
    let lower = raw_message.to_ascii_lowercase();
    if lower.contains("read path is not a file") {
        return (
            "Read target is not a file; provide a file path instead of a directory".to_string(),
            "Read target is not a file".to_string(),
        );
    }
    if lower.contains("path is required") {
        return (
            "tool input is missing a required path argument".to_string(),
            "Missing required path".to_string(),
        );
    }
    (
        "tool input is invalid; revise the tool arguments and retry".to_string(),
        "Invalid tool input".to_string(),
    )
}

fn sanitize_unstructured_tool_error(raw_message: &str) -> String {
    let filtered = raw_message
        .lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            if trimmed.is_empty() || line_contains_host_diagnostic(trimmed) {
                None
            } else {
                Some(redact_host_path_tokens(trimmed))
            }
        })
        .collect::<Vec<_>>()
        .join(" ");
    let collapsed = filtered.split_whitespace().collect::<Vec<_>>().join(" ");
    truncate_for_tool_error(collapsed.as_str(), 240)
}

fn line_contains_host_diagnostic(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    lower.contains("sandbox io error")
        || lower.contains("sandbox unavailable")
        || lower.contains("sandbox denied")
}

fn redact_host_path_tokens(text: &str) -> String {
    text.split_whitespace()
        .map(|token| {
            if token_contains_host_path(token) {
                "[host-path]"
            } else {
                token
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn token_contains_host_path(token: &str) -> bool {
    let lower = token.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    for index in 0..bytes.len().saturating_sub(2) {
        if bytes[index].is_ascii_alphabetic()
            && bytes[index + 1] == b':'
            && (bytes[index + 2] == b'\\' || bytes[index + 2] == b'/')
        {
            let suffix = &lower[index..];
            return suffix.contains("\\appdata\\")
                || suffix.contains("/appdata/")
                || suffix.contains("\\windows\\")
                || suffix.contains("/windows/")
                || suffix.contains("\\temp\\")
                || suffix.contains("/temp/")
                || suffix.contains("\\programdata\\")
                || suffix.contains("/programdata/");
        }
    }
    false
}

fn truncate_for_tool_error(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let keep = max_chars.saturating_sub(3);
    let mut truncated = text.chars().take(keep).collect::<String>();
    truncated.push_str("...");
    truncated
}

fn tool_error_diagnostic_id(raw_message: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut hasher = DefaultHasher::new();
    raw_message.hash(&mut hasher);
    format!("tool_error:{:016x}", hasher.finish())
}
