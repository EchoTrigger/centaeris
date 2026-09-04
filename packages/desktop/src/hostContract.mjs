export const HOST_EVENT_NAMES = new Set([
  "session/update",
  "runtime/config-changed",
  "centaeris/runtime-host-error",
  "centaeris/tray-new-chat",
]);

export const HOST_COMMANDS = new Map([
  ["app_exit", { group: "app", local: true }],
  ["session/list", { group: "session" }],
  ["_centaeris/session/diagnostics", { group: "session" }],
  ["session/load", { group: "session" }],
  ["session/new", { group: "session" }],
  ["_centaeris/session/activate", { group: "session" }],
  ["_centaeris/session/delete", { group: "session" }],
  ["_centaeris/session/project", { group: "session" }],
  ["_centaeris/session/update_metadata", { group: "session" }],
  ["_centaeris/session/reorder", { group: "session" }],

  ["agent_state_get", { group: "agent-runtime" }],
  ["agent_context_usage_get", { group: "agent-runtime" }],
  ["_centaeris/session/compact_context", { group: "agent-stream" }],
  ["agent_runtime_config_get", { group: "agent-runtime" }],
  ["agent_runtime_config_reset", { group: "agent-runtime" }],
  ["agent_runtime_config_set", { group: "agent-runtime" }],
  ["agent_runtime_model_test", { group: "agent-runtime" }],
  ["agent_runtime_garbage_collect", { group: "agent-runtime" }],
  ["mcp/catalog", { group: "mcp" }],
  ["mcp/configure", { group: "mcp" }],
  ["plugin/list", { group: "plugins" }],
  ["plugin/detail", { group: "plugins" }],
  ["plugin/install", { group: "plugins" }],
  ["plugin/remove", { group: "plugins" }],
  ["plugin/set_enabled", { group: "plugins" }],
  ["plugin/reload", { group: "plugins" }],
  ["plugin/source_ref", { group: "plugins" }],
  ["plugin_reveal_source_ref", { group: "plugins", local: true }],
  ["plugin_select_install_path", { group: "plugins", local: true }],
  ["plugin/catalog_state", { group: "plugins" }],
  ["skill/source/list", { group: "skills" }],
  ["skill/source/add", { group: "skills" }],
  ["skill/source/remove", { group: "skills" }],
  ["skill/source/set_enabled", { group: "skills" }],
  ["skill/source/ref", { group: "skills" }],
  ["skill/catalog", { group: "skills" }],
  ["skill/detail", { group: "skills" }],
  ["skill/set_enabled", { group: "skills" }],
  ["skill/reload", { group: "skills" }],
  ["skill_select_source_path", { group: "skills", local: true }],
  ["skill_reveal_source", { group: "skills", local: true }],
  ["transcript/project", { group: "agent-runtime" }],

  ["session/prompt", { group: "agent-stream" }],
  ["_centaeris/session/supplement", { group: "agent-stream" }],
  ["_centaeris/session/answer_now", { group: "agent-stream" }],
  ["_centaeris/session/answer_question", { group: "agent-stream" }],
  ["_centaeris/session/agent-runs", { group: "agent-stream" }],
  ["_centaeris/session/agent-runs/replay", { group: "agent-stream" }],
  ["_centaeris/session/agent-runs/attach", { group: "agent-stream" }],
  ["_centaeris/session/agent-runs/detach", { group: "agent-stream" }],
  ["_centaeris/session/agent-runs/detach-viewer", { group: "agent-stream" }],
  ["_centaeris/session/agent-runs/cancel", { group: "agent-stream" }],
  ["agent_runtime_job_list", { group: "agent-runtime" }],
  ["agent_runtime_job_get", { group: "agent-runtime" }],
  ["agent_dead_letter_list", { group: "agent-runtime" }],
  ["agent_dead_letter_get", { group: "agent-runtime" }],
  ["agent_dead_letter_dismiss", { group: "agent-runtime" }],
  ["agent_dead_letter_replay", { group: "agent-runtime" }],

  ["sidecar_start", { group: "sidecar" }],
  ["sidecar_stop", { group: "sidecar" }],
  ["sidecar_list", { group: "sidecar" }],

  ["workspace_get", { group: "workspace" }],
  ["workspace_activate", { group: "workspace" }],
  ["workspace_open_folder", { group: "workspace" }],
  ["workspace_reveal_folder", { group: "workspace" }],
  ["workspace_rename", { group: "workspace" }],
  ["workspace_remove", { group: "workspace" }],
  ["workspace_reset", { group: "workspace" }],
  ["workspace_file_tree", { group: "workspace" }],
  ["workspace_read_file", { group: "workspace" }],
  ["workspace_git_status_get", { group: "workspace-git" }],
  ["workspace_git_diff_get", { group: "workspace-git" }],
  ["workspace_git_file_diff_get", { group: "workspace-git" }],
  ["workspace_git_github_cli_status_get", { group: "workspace-git" }],
  ["desktop_file_preview_read", { group: "desktop-file-preview" }],
]);

export const requireHostCommand = (command) => {
  const metadata = HOST_COMMANDS.get(command);
  if (!metadata) {
    throw new Error(`host command is not registered for Electron: ${command}`);
  }
  return metadata;
};

export const requireHostEventName = (eventName) => {
  if (!HOST_EVENT_NAMES.has(eventName)) {
    throw new Error(`host event is not registered for Electron: ${eventName}`);
  }
};
