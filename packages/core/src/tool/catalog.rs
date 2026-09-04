use serde_json::{json, Value};

use super::types::{ToolContract, ToolTurnBehavior};

pub const BUILTIN_TOOL_PROVIDER_ID: &str = "centaeris.builtin";

pub(crate) const MODEL_TOOL_CATALOG_ORDER: &[&str] =
    &["read", "bash", "edit", "write", "task_output", "agent"];

pub(crate) const READ_MAX_LINES: usize = 2_000;
pub(crate) const READ_MAX_BYTES: usize = 50 * 1024;
pub(crate) const EDIT_MAX_ITEMS: usize = 64;
pub(crate) const EDIT_MAX_OLD_TEXT_BYTES: usize = 8 * 1024;
pub(crate) const EDIT_MAX_NEW_TEXT_BYTES: usize = 32 * 1024;
pub(crate) const EDIT_MAX_ARGS_BYTES: usize = 64 * 1024;
pub(crate) const WORKSPACE_MUTATION_MAX_BYTES: usize = 8 * 1024 * 1024;

pub fn canonicalize_tool_name(raw: &str) -> Option<&'static str> {
    MODEL_TOOL_CATALOG_ORDER
        .iter()
        .copied()
        .find(|candidate| *candidate == raw)
}

pub fn list_tool_contracts() -> Vec<ToolContract> {
    MODEL_TOOL_CATALOG_ORDER
        .iter()
        .map(|name| build_tool_contract(name))
        .collect()
}

pub(crate) fn select_tool_contracts_by_names(names: &[String], limit: usize) -> Vec<ToolContract> {
    list_tool_contracts()
        .into_iter()
        .filter(|contract| names.iter().any(|name| name == &contract.name))
        .take(limit.max(1))
        .collect()
}

fn build_tool_contract(name: &str) -> ToolContract {
    let (category, summary, input_schema, concurrency_safe) = tool_contract(name);
    let mut contract = ToolContract {
        name: name.to_string(),
        category: category.to_string(),
        summary: summary.to_string(),
        input_schema,
        concurrency_safe,
        turn_behavior: ToolTurnBehavior::ContinueTurn,
        provider_id: Some(BUILTIN_TOOL_PROVIDER_ID.to_string()),
        schema_hash: None,
        scopes: vec![],
        dynamic: false,
    };
    contract.schema_hash = Some(
        contract
            .contract_digest()
            .expect("built-in tool contract must serialize"),
    );
    contract
}

fn tool_contract(tool_name: &str) -> (&'static str, &'static str, Value, bool) {
    match tool_name {
        "read" => (
            "filesystem.read",
            "Read a file, list a bounded directory, or read an authorized AgentRun input. File content returns at most 2000 complete lines or 50 KiB, whichever is reached first; continue large files with the returned offset until complete. PNG, JPEG, and WebP files become model-visible image observations in the next request. Office and PDF routing is deterministic and does not require selecting an extractor.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "File or directory path resolved from the current working directory. Prefer relative paths; absolute paths and parent traversal are accepted only when the current sandbox policy permits them. On Windows, use native drive or UNC paths. Use . for the current directory. Provide exactly one of path, input_ref, or input_refs." },
                    "input_ref": { "type": "string", "description": "Opaque input ref declared by the current AgentRun authorization. Provide exactly one of path or input_ref." },
                    "input_refs": { "type": "array", "minItems": 1, "maxItems": 4, "uniqueItems": true, "items": { "type": "string", "minLength": 1 }, "description": "Read up to four authorized AgentRun inputs as one bounded batch. Cannot be combined with offset or limit." },
                    "operation": { "type": "string", "enum": ["read", "list"], "description": "Use list with a path to enumerate a bounded directory. Omit or use read for file content." },
                    "recursive": { "type": "boolean", "description": "Only valid for operation=list." },
                    "offset": { "type": "integer", "minimum": 0, "description": "Zero-based line offset for file reads, or zero-based entry offset for directory listings. Use the continuation offset returned by the previous result." },
                    "limit": { "type": "integer", "minimum": 1, "maximum": READ_MAX_LINES, "default": READ_MAX_LINES, "description": "For file reads, maximum complete lines to return (default and maximum 2000); every result is also capped at 50 KiB. For directory listings, default 100 and maximum 200." }
                },
                "oneOf": [
                    { "required": ["path"] },
                    { "required": ["input_ref"] },
                    { "required": ["input_refs"] }
                ],
                "additionalProperties": false
            }),
            true,
        ),
        "write" => (
            "filesystem.write",
            "Create a file or replace an existing file that was read during this AgentRun. Paths resolve from the current working directory, and file-version checks stay inside the executor.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "File path resolved from the current working directory. Prefer a relative path; absolute paths and parent traversal are allowed." },
                    "content": { "type": "string" }
                },
                "required": ["path", "content"],
                "additionalProperties": false
            }),
            false,
        ),
        "edit" => (
            "filesystem.write",
            "Atomically apply one to 64 exact replacements to one existing file that was read during this AgentRun. Paths resolve from the current working directory. Every edits[].old_text is matched against the same original file and must be exact, unique, and non-overlapping. Keep old_text as small as possible while still unique; do not include large unchanged regions merely to connect distant changes. Put disjoint changes in separate edits[] entries and merge overlapping or nearby changes.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "minLength": 1, "description": "Existing file path resolved from the current working directory. Prefer a relative path; absolute paths and parent traversal are allowed." },
                    "edits": {
                        "type": "array",
                        "minItems": 1,
                        "maxItems": EDIT_MAX_ITEMS,
                        "description": "Targeted replacements matched against the same original file. Use separate entries for distant disjoint changes. Never pad old_text with large unchanged regions to connect them; merge only overlapping or nearby changes.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "old_text": { "type": "string", "minLength": 1, "description": "Exact text for one targeted replacement. The executor enforces an 8192-byte UTF-8 limit. Keep it as small as possible while still unique; do not copy large unchanged regions." },
                                "new_text": { "type": "string", "description": "Replacement text. The executor enforces a 32768-byte UTF-8 limit. May be empty to remove the matched block." }
                            },
                            "required": ["old_text", "new_text"],
                            "additionalProperties": false
                        }
                    }
                },
                "required": ["path", "edits"],
                "additionalProperties": false
            }),
            false,
        ),
        "bash" => (
            "process.exec",
            "Run one Bash command in the current working directory. Default to foreground commands; the result returns after the foreground shell exits and inherited stdout/stderr become idle. To keep a local development server available after this tool returns, start it explicitly with `&`, redirect stdout/stderr to a workspace log, print its PID and log path, then run a bounded foreground health check. Do not background builds, tests, installs, git commands, or one-shot scripts. Background children remain OS/shell-owned: they have no Runtime task, recovery, stop handle, terminal projection, or output reference. The ExecutionHost always enforces its configured sandbox policy. The default timeout is 60000 ms; set timeout_ms explicitly for longer foreground commands, up to 3600000 ms.",
            json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string" },
                    "description": { "type": "string", "minLength": 1, "maxLength": 160, "description": "Short action title for this command. It is shown in tool activity and recorded with large temporary results for later search." },
                    "timeout_ms": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 3600000,
                        "default": 60000
                    }
                },
                "required": ["command"],
                "additionalProperties": false
            }),
            false,
        ),
        "task_output" => (
            "runtime.output",
            "Wait for and read the bounded result of an Agent task reference.",
            json!({
                "type": "object",
                "properties": {
                    "output_ref": {
                        "type": "object",
                        "properties": {
                            "schema": { "type": "string", "const": "task_output_ref_v1" },
                            "kind": { "type": "string", "const": "agent" },
                            "runtime_job_id": { "type": "string", "minLength": 1 },
                            "child_session_id": { "type": "string", "minLength": 1 },
                            "result_ref": { "type": "string", "minLength": 1 }
                        },
                        "required": ["schema", "kind", "runtime_job_id", "child_session_id", "result_ref"],
                        "additionalProperties": false
                    }
                },
                "required": ["output_ref"],
                "additionalProperties": false
            }),
            false,
        ),
        "agent" => (
            "runtime.agent",
            "Start a bounded independent task in a child Agent. The task always runs in the background; use task_output with the returned output_ref when its result is needed.",
            json!({
                "type": "object",
                "properties": {
                    "prompt": { "type": "string" },
                    "description": { "type": "string" },
                    "budget": {
                        "type": "object",
                        "properties": {
                            "max_summary_chars": { "type": "integer", "minimum": 1, "maximum": 16000 }
                        },
                        "additionalProperties": false
                    }
                },
                "required": ["prompt", "description"],
                "additionalProperties": false
            }),
            false,
        ),
        _ => unreachable!("fixed tool catalog contains unknown tool: {tool_name}"),
    }
}

#[cfg(test)]
mod tests {
    use super::{canonicalize_tool_name, list_tool_contracts, EDIT_MAX_ITEMS, READ_MAX_LINES};

    #[test]
    fn fixed_catalog_projects_exact_tool_definitions() {
        fn assert_snake_case_properties(value: &serde_json::Value) {
            match value {
                serde_json::Value::Object(object) => {
                    if let Some(properties) = object.get("properties").and_then(|v| v.as_object()) {
                        for key in properties.keys() {
                            assert!(
                                key.chars().all(|character| {
                                    character.is_ascii_lowercase()
                                        || character.is_ascii_digit()
                                        || character == '_'
                                }),
                                "model tool parameter is not lower_snake_case: {key}"
                            );
                        }
                    }
                    for nested in object.values() {
                        assert_snake_case_properties(nested);
                    }
                }
                serde_json::Value::Array(items) => {
                    for nested in items {
                        assert_snake_case_properties(nested);
                    }
                }
                _ => {}
            }
        }

        let contracts = list_tool_contracts();
        let names = contracts
            .iter()
            .map(|contract| contract.name.as_str())
            .collect::<Vec<_>>();

        assert_eq!(
            names,
            vec!["read", "bash", "edit", "write", "task_output", "agent"]
        );
        assert_eq!(canonicalize_tool_name("banana"), None);
        assert_eq!(canonicalize_tool_name(" read "), None);
        assert!(contracts
            .iter()
            .all(|contract| contract.input_schema.is_object()));
        for contract in &contracts {
            assert_snake_case_properties(&contract.input_schema);
        }
        let agent = contracts
            .iter()
            .find(|contract| contract.name == "agent")
            .expect("agent contract");
        assert_eq!(
            agent.input_schema["required"],
            serde_json::json!(["prompt", "description"])
        );
        assert!(!agent.input_schema["properties"]
            .as_object()
            .expect("agent properties")
            .contains_key("allowed_tools"));
    }

    #[test]
    fn bash_contract_waits_for_foreground_shell_without_runtime_background_job() {
        let contracts = list_tool_contracts();
        let bash = contracts
            .iter()
            .find(|contract| contract.name == "bash")
            .expect("bash contract");
        let properties = bash.input_schema["properties"]
            .as_object()
            .expect("bash properties");

        assert_eq!(bash.input_schema["additionalProperties"], false);
        assert_eq!(properties["timeout_ms"]["default"], 60_000);
        assert_eq!(properties["timeout_ms"]["maximum"], 3_600_000);
        assert!(!properties.contains_key("runInBackground"));
        assert!(!properties.contains_key("foregroundBudgetMs"));
        assert!(bash.summary.contains("foreground shell exits"));
        assert!(bash.summary.contains("Default to foreground commands"));
        assert!(bash
            .summary
            .contains("no Runtime task, recovery, stop handle"));
    }

    #[test]
    fn file_mutation_contracts_keep_snapshot_state_out_of_model_schema() {
        let contracts = list_tool_contracts();
        let write = contracts
            .iter()
            .find(|contract| contract.name == "write")
            .expect("write contract");
        let edit = contracts
            .iter()
            .find(|contract| contract.name == "edit")
            .expect("edit contract");
        let write_properties = write.input_schema["properties"]
            .as_object()
            .expect("write properties");
        let edit_properties = edit.input_schema["properties"]
            .as_object()
            .expect("edit properties");

        assert_eq!(
            write.input_schema["required"],
            serde_json::json!(["path", "content"])
        );
        assert!(!write_properties.contains_key("expectedFileHash"));
        assert_eq!(
            edit.input_schema["required"],
            serde_json::json!(["path", "edits"])
        );
        assert_eq!(edit_properties["edits"]["minItems"], 1);
        assert_eq!(edit_properties["edits"]["maxItems"], EDIT_MAX_ITEMS);
        let edit_item_properties = edit_properties["edits"]["items"]["properties"]
            .as_object()
            .expect("edit item properties");
        assert_eq!(
            edit_properties["edits"]["items"]["required"],
            serde_json::json!(["old_text", "new_text"])
        );
        assert!(edit_item_properties.contains_key("old_text"));
        assert!(edit_item_properties.contains_key("new_text"));
        assert!(edit_item_properties["old_text"].get("maxLength").is_none());
        assert!(edit_item_properties["new_text"].get("maxLength").is_none());
        assert!(!edit_item_properties.contains_key("oldText"));
        assert!(!edit_item_properties.contains_key("newText"));
        assert!(!edit_properties.contains_key("replaceAll"));
        assert!(!edit_properties.contains_key("expectedFileHashes"));
    }

    #[test]
    fn builtin_model_contracts_own_their_model_guidance_and_omit_ui_titles() {
        let contracts = list_tool_contracts();
        for name in ["read", "bash", "edit", "write"] {
            let contract = contracts
                .iter()
                .find(|contract| contract.name == name)
                .expect("builtin model contract");
            let properties = contract.input_schema["properties"]
                .as_object()
                .expect("tool properties");
            assert!(!properties.contains_key("title"), "{name} exposed title");
        }

        let read = contracts
            .iter()
            .find(|contract| contract.name == "read")
            .expect("read contract");
        assert!(read.summary.contains("2000 complete lines or 50 KiB"));
        assert!(read.summary.contains("continue large files"));
        assert_eq!(
            read.input_schema["properties"]["limit"]["maximum"],
            READ_MAX_LINES
        );
        assert_eq!(
            read.input_schema["properties"]["limit"]["default"],
            READ_MAX_LINES
        );

        let write = contracts
            .iter()
            .find(|contract| contract.name == "write")
            .expect("write contract");
        assert!(write.summary.contains("read during this AgentRun"));
        assert!(write
            .summary
            .contains("file-version checks stay inside the executor"));

        let edit = contracts
            .iter()
            .find(|contract| contract.name == "edit")
            .expect("edit contract");
        assert!(edit.summary.contains("one to 64 exact replacements"));
        assert!(edit.summary.contains("same original file"));
        assert!(edit.summary.contains("exact, unique, and non-overlapping"));
        assert!(edit.summary.contains("Keep old_text as small as possible"));

        let bash = contracts
            .iter()
            .find(|contract| contract.name == "bash")
            .expect("bash contract");
        assert!(bash.summary.contains("current working directory"));
        assert!(bash.summary.contains("Default to foreground commands"));
        assert!(bash.summary.contains("stdout/stderr become idle"));
        assert!(bash.summary.contains("default timeout is 60000 ms"));
    }
}
