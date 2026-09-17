import { dialog, shell } from "electron";
import fs from "node:fs";
import { requireHostCommand } from "./hostContract.mjs";
import { ensureDefaultWorkspaceDirectory } from "./defaultWorkspace.mjs";

const requireExactObjectKeys = (value, expectedKeys, label) => {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new Error(`${label} must be an object`);
  }
  const actualKeys = Object.keys(value).sort();
  const expected = [...expectedKeys].sort();
  if (
    actualKeys.length !== expected.length
    || actualKeys.some((key, index) => key !== expected[index])
  ) {
    throw new Error(
      `${label} fields must be exactly ${expected.join(", ") || "empty"}`,
    );
  }
  return value;
};

export const createLocalShellActions = ({
  getRequestWindow,
  getMainWindow,
  getHomePath,
  invokeRustHostCommand,
}) => {
  if (typeof getRequestWindow !== "function") {
    throw new Error("local shell actions require getRequestWindow");
  }
  if (typeof getMainWindow !== "function") {
    throw new Error("local shell actions require getMainWindow");
  }
  if (typeof getHomePath !== "function") {
    throw new Error("local shell actions require getHomePath");
  }
  if (typeof invokeRustHostCommand !== "function") {
    throw new Error("local shell actions require invokeRustHostCommand");
  }

  const activateWorkspaceRoot = (root) => invokeRustHostCommand(
    "workspace_activate",
    { request: { root } },
    requireHostCommand("workspace_activate"),
  );

  const openWorkspaceFolder = async (event, payload) => {
    const envelope = requireExactObjectKeys(
      payload,
      ["request"],
      "workspace_open_folder payload",
    );
    const request = requireExactObjectKeys(
      envelope.request,
      ["mode"],
      "workspace_open_folder request",
    );
    if (request.mode === "defaultDirectory") {
      return activateWorkspaceRoot(
        await ensureDefaultWorkspaceDirectory(getHomePath()),
      );
    }
    if (request.mode !== "customPath") {
      throw new Error(
        "workspace_open_folder request.mode must be defaultDirectory or customPath",
      );
    }
    const requestWindow = getRequestWindow(event);
    const options = {
      title: "Open Folder",
      properties: ["openDirectory"],
    };
    const owner = requestWindow ?? getMainWindow();
    const result = owner
      ? await dialog.showOpenDialog(owner, options)
      : await dialog.showOpenDialog(options);
    if (result.canceled) {
      return {
        activeWorkspaceRoot: null,
        workspaces: [],
        cancelled: true,
      };
    }
    const root = result.filePaths[0];
    if (!root) {
      throw new Error("workspace_open_folder did not return a folder path");
    }
    return activateWorkspaceRoot(root);
  };

  const revealWorkspaceFolder = async (payload) => {
    const root =
      typeof payload?.request?.root === "string" ? payload.request.root.trim() : "";
    if (!root) {
      throw new Error("workspace_reveal_folder requires request.root");
    }
    const errorMessage = await shell.openPath(root);
    if (errorMessage) {
      throw new Error(`workspace_reveal_folder failed: ${errorMessage}`);
    }
    return invokeRustHostCommand(
      "workspace_get",
      {},
      requireHostCommand("workspace_get"),
    );
  };

  const revealPluginSourceRef = async (payload) => {
    const request = payload?.request ?? {};
    const sourceRef = await invokeRustHostCommand(
      "plugin/source_ref",
      { request },
      requireHostCommand("plugin/source_ref"),
    );
    if (sourceRef?.kind !== "local_path") {
      throw new Error("plugin_reveal_source_ref only supports local_path source refs");
    }
    const sourcePath =
      typeof sourceRef.path === "string" ? sourceRef.path.trim() : "";
    if (!sourcePath) {
      throw new Error("plugin_reveal_source_ref requires source_ref.path");
    }
    if (!fs.existsSync(sourcePath)) {
      throw new Error(`plugin_reveal_source_ref path does not exist: ${sourcePath}`);
    }
    shell.showItemInFolder(sourcePath);
    return {
      ...sourceRef,
      opened: true,
    };
  };

  const selectAndInstallPlugin = async (event, payload) => {
    requireExactObjectKeys(payload, ["request"], "plugin_select_install_path payload");
    requireExactObjectKeys(
      payload.request,
      [],
      "plugin_select_install_path request",
    );
    const requestWindow = getRequestWindow(event);
    const options = {
      title: "Install Plugin",
      properties: ["openDirectory"],
    };
    const owner = requestWindow ?? getMainWindow();
    const result = owner
      ? await dialog.showOpenDialog(owner, options)
      : await dialog.showOpenDialog(options);
    if (result.canceled) {
      return { cancelled: true, plugin: null };
    }
    const sourcePath = result.filePaths[0];
    if (!sourcePath) {
      throw new Error("plugin_select_install_path did not return a directory path");
    }
    const plugin = await invokeRustHostCommand(
      "plugin/install",
      { request: { sourcePath } },
      requireHostCommand("plugin/install"),
    );
    return { cancelled: false, plugin };
  };

  const selectSkillSourcePath = async (event, payload) => {
    const exactPayload = requireExactObjectKeys(
      payload,
      ["request"],
      "skill_select_source_path payload",
    );
    const request = requireExactObjectKeys(
      exactPayload.request,
      ["kind"],
      "skill_select_source_path request",
    );
    const kind =
      typeof request.kind === "string"
        ? request.kind.trim()
        : "";
    if (kind !== "catalogDirectory" && kind !== "skillFile") {
      throw new Error(
        "skill_select_source_path requires request.kind catalogDirectory or skillFile",
      );
    }
    const requestWindow = getRequestWindow(event);
    const options = kind === "catalogDirectory"
      ? {
          title: "Select Skill Catalog Directory",
          properties: ["openDirectory"],
        }
      : {
          title: "Select SKILL.md",
          properties: ["openFile"],
          filters: [{ name: "Skill manifest", extensions: ["md"] }],
        };
    const owner = requestWindow ?? getMainWindow();
    const result = owner
      ? await dialog.showOpenDialog(owner, options)
      : await dialog.showOpenDialog(options);
    if (result.canceled) {
      return { cancelled: true, path: null };
    }
    const selectedPath = result.filePaths[0];
    if (!selectedPath) {
      throw new Error("skill_select_source_path did not return a path");
    }
    return { cancelled: false, path: selectedPath };
  };

  const revealSkillSource = async (payload) => {
    const exactPayload = requireExactObjectKeys(
      payload,
      ["request"],
      "skill_reveal_source payload",
    );
    const request = requireExactObjectKeys(
      exactPayload.request,
      ["sourceId"],
      "skill_reveal_source request",
    );
    const sourceRef = await invokeRustHostCommand(
      "skill/source/ref",
      { request },
      requireHostCommand("skill/source/ref"),
    );
    if (sourceRef?.kind !== "local_path") {
      throw new Error("skill_reveal_source only supports local_path source refs");
    }
    const sourcePath =
      typeof sourceRef.path === "string" ? sourceRef.path.trim() : "";
    if (!sourcePath) {
      throw new Error("skill_reveal_source requires source_ref.path");
    }
    if (!fs.existsSync(sourcePath)) {
      throw new Error(`skill_reveal_source path does not exist: ${sourcePath}`);
    }
    shell.showItemInFolder(sourcePath);
    return { ...sourceRef, opened: true };
  };

  return {
    openWorkspaceFolder,
    revealWorkspaceFolder,
    revealPluginSourceRef,
    selectAndInstallPlugin,
    selectSkillSourcePath,
    revealSkillSource,
  };
};
