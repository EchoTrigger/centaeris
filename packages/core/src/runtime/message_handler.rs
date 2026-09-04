use crate::runtime::context_window::refresh_session_context_window;
use crate::runtime::contracts::JsonMap;
use crate::session::state::{
    ChatMessage, MessageRole, ModelMessageSemanticsV1, SessionStateSnapshot,
};

#[derive(Debug, Clone)]
pub struct MessageHandlerConfig {
    pub max_message_chars: usize,
}

impl Default for MessageHandlerConfig {
    fn default() -> Self {
        Self {
            max_message_chars: 72_000,
        }
    }
}

#[derive(Debug, Clone)]
pub struct MessageHandler {
    config: MessageHandlerConfig,
}

impl MessageHandler {
    pub fn new(config: MessageHandlerConfig) -> Self {
        Self { config }
    }

    pub fn push_user_message(
        &self,
        session: &mut SessionStateSnapshot,
        content: &str,
        metadata: JsonMap,
    ) -> String {
        self.push_message(session, MessageRole::User, content, metadata)
    }

    pub fn push_system_message(
        &self,
        session: &mut SessionStateSnapshot,
        content: &str,
        metadata: JsonMap,
    ) -> String {
        self.push_message(session, MessageRole::System, content, metadata)
    }

    pub fn push_assistant_message(
        &self,
        session: &mut SessionStateSnapshot,
        content: &str,
        metadata: JsonMap,
    ) -> String {
        self.push_message(session, MessageRole::Assistant, content, metadata)
    }

    pub fn push_model_assistant_message(
        &self,
        session: &mut SessionStateSnapshot,
        content: &str,
        metadata: JsonMap,
        semantics: ModelMessageSemanticsV1,
    ) -> String {
        self.push_message_with_semantics(
            session,
            MessageRole::Assistant,
            content,
            metadata,
            semantics,
        )
    }

    pub fn push_model_tool_message(
        &self,
        session: &mut SessionStateSnapshot,
        content: &str,
        metadata: JsonMap,
        semantics: ModelMessageSemanticsV1,
    ) -> String {
        self.push_message_with_semantics(session, MessageRole::Tool, content, metadata, semantics)
    }

    pub fn refresh_context_window(&self, session: &mut SessionStateSnapshot) {
        refresh_session_context_window(session);
    }

    fn push_message(
        &self,
        session: &mut SessionStateSnapshot,
        role: MessageRole,
        content: &str,
        metadata: JsonMap,
    ) -> String {
        self.push_message_with_semantics(
            session,
            role,
            content,
            metadata,
            ModelMessageSemanticsV1::Plain,
        )
    }

    fn push_message_with_semantics(
        &self,
        session: &mut SessionStateSnapshot,
        role: MessageRole,
        content: &str,
        metadata: JsonMap,
        semantics: ModelMessageSemanticsV1,
    ) -> String {
        let now = now_ms();
        let sanitized = sanitize_message(content, self.config.max_message_chars);
        let message_id = format!(
            "msg:{}:{}:{}",
            session.session_id,
            now,
            session.messages.len()
        );
        session.messages.push(ChatMessage {
            message_id: message_id.clone(),
            role,
            content: sanitized,
            created_at_ms: now,
            metadata,
        });
        session
            .model_semantics
            .insert(message_id.clone(), semantics);
        session.updated_at_ms = now;
        self.refresh_context_window(session);
        message_id
    }
}

fn sanitize_message(content: &str, max_chars: usize) -> String {
    let trimmed = content.trim();
    if trimmed.chars().count() <= max_chars {
        return trimmed.to_string();
    }
    trimmed.chars().take(max_chars).collect()
}

fn now_ms() -> i64 {
    crate::runtime::contracts::current_timestamp_ms()
}

#[cfg(test)]
mod tests {
    use super::{MessageHandler, MessageHandlerConfig};
    use crate::runtime::contracts::JsonMap;
    use crate::session::state::SessionStateSnapshot;

    #[test]
    fn context_window_preserves_all_appended_messages() {
        let handler = MessageHandler::new(MessageHandlerConfig {
            max_message_chars: 100,
        });
        let mut session = SessionStateSnapshot::new("chat-a".to_string(), 0);

        handler.push_user_message(&mut session, "one", JsonMap::new());
        handler.push_user_message(&mut session, "two", JsonMap::new());
        handler.push_user_message(&mut session, "three", JsonMap::new());

        assert_eq!(session.context_window.len(), 3);
        assert_eq!(session.context_window[0].content, "one");
        assert_eq!(session.context_window[1].content, "two");
        assert_eq!(session.context_window[2].content, "three");
    }

    #[test]
    fn appended_messages_have_distinct_ids() {
        let handler = MessageHandler::new(MessageHandlerConfig::default());
        let mut session = SessionStateSnapshot::new("chat-a".to_string(), 0);

        let first_id = handler.push_user_message(&mut session, "one", JsonMap::new());
        let second_id = handler.push_user_message(&mut session, "two", JsonMap::new());

        assert_ne!(first_id, second_id);
        assert!(first_id.ends_with(":0"));
        assert!(second_id.ends_with(":1"));
    }
}
