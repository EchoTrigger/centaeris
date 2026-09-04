use super::catalog::canonicalize_tool_name;

pub fn is_tool_concurrency_safe(tool_name: &str) -> bool {
    matches!(canonicalize_tool_name(tool_name), Some("read"))
}

#[cfg(test)]
mod tests {
    use crate::tool::list_tool_contracts;

    use super::is_tool_concurrency_safe;

    #[test]
    fn concurrency_follows_default_tool_contract() {
        assert!(!list_tool_contracts().is_empty());
        assert!(is_tool_concurrency_safe("read"));
        assert!(!is_tool_concurrency_safe("write"));
        assert!(!is_tool_concurrency_safe("edit"));
        assert!(!is_tool_concurrency_safe("bash"));
        assert!(!is_tool_concurrency_safe("UnknownToolForTest"));
    }
}
