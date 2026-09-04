use super::{compact_text, GenerateResult, QueryContinuation};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum StatusStage {
    ModelProcessSummary,
    QuestionWait,
}

impl StatusStage {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::ModelProcessSummary => "model_process_summary",
            Self::QuestionWait => "question_wait",
        }
    }
}

pub(super) fn continuation_event_status(continuation: &QueryContinuation) -> &'static str {
    match continuation {
        QueryContinuation::CompleteTerminalTool | QueryContinuation::Finalize => "done",
        QueryContinuation::AwaitQuestion
        | QueryContinuation::AwaitRuntimeJob
        | QueryContinuation::ExecuteTools => "running",
    }
}

pub(super) fn should_emit_model_process_summary(continuation: &QueryContinuation) -> bool {
    matches!(
        continuation,
        QueryContinuation::AwaitQuestion
            | QueryContinuation::AwaitRuntimeJob
            | QueryContinuation::ExecuteTools
    )
}

pub(super) fn model_process_summary_message(generate_result: &GenerateResult) -> Option<String> {
    let content = generate_result.content.trim();
    if content.is_empty() {
        return None;
    }
    Some(compact_text(content, 1_200))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ToolCallEnvelope;

    fn generate_result(content: &str) -> GenerateResult {
        GenerateResult {
            content: content.to_string(),
            tool_calls: vec![ToolCallEnvelope {
                id: "call_1".to_string(),
                name: "read".to_string(),
                args_json: r#"{"input_ref":"input_1"}"#.to_string(),
            }],
            reasoning_content: None,
            input_tokens: None,
            total_tokens: None,
            prompt_cache_hit_tokens: None,
            prompt_cache_miss_tokens: None,
        }
    }

    #[test]
    fn model_process_summary_is_optional_and_bounded() {
        let valid = generate_result("我先核对招标文件中的项目背景与采购目标。");
        assert_eq!(
            model_process_summary_message(&valid),
            Some("我先核对招标文件中的项目背景与采购目标。".to_string())
        );
        assert_eq!(model_process_summary_message(&generate_result("")), None);
        assert_eq!(
            model_process_summary_message(&generate_result(&"x".repeat(1_201)))
                .expect("bounded summary")
                .chars()
                .count(),
            1_200
        );
    }
}
