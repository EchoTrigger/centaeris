use super::*;

/// Owns the partial result even when cancellation drops the provider future.
pub(super) struct ReasoningAttempt<'a> {
    sink: Option<&'a super::super::ToolSafePointDispatcher<'a>>,
    session_id: String,
    turn_id: String,
    pub request_id: Option<String>,
    pub text: String,
    sealed: bool,
}

impl<'a> ReasoningAttempt<'a> {
    pub fn new(
        sink: Option<&'a super::super::ToolSafePointDispatcher<'a>>,
        req: &GenerateDriverRequest,
        request_id: Option<String>,
    ) -> Self {
        Self {
            sink,
            session_id: req.session_id.clone(),
            turn_id: req.turn_id.clone(),
            request_id,
            text: String::new(),
            sealed: false,
        }
    }

    pub fn seal(&mut self, status: &str) -> Result<(), String> {
        if self.sealed {
            return Ok(());
        }
        self.sealed = true;
        if let (Some(sink), Some(request_id)) = (self.sink, &self.request_id) {
            if !self.text.trim().is_empty() {
                sink.commit(ToolSafePoint::ReasoningCompleted {
                    session_id: self.session_id.clone(),
                    turn_id: self.turn_id.clone(),
                    request_id: request_id.clone(),
                    text: self.text.clone(),
                    status: status.to_string(),
                })?;
            }
        }
        Ok(())
    }
}

impl Drop for ReasoningAttempt<'_> {
    fn drop(&mut self) {
        if let Err(error) = self.seal("interrupted") {
            eprintln!("partial reasoning persistence failed during cancellation: {error}");
        }
    }
}
