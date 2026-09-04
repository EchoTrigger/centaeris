import { ipcMain } from "electron";
import { requireHostCommand, requireHostEventName } from "./hostContract.mjs";

let hostIpcRegistered = false;

export const registerHostIpc = ({
  invokeRustHostCommand,
  requestAppExit,
  isShuttingDown,
  localShellActions,
  validateTrustedRendererEvent,
}) => {
  if (hostIpcRegistered) {
    throw new Error("host IPC handlers already registered");
  }
  if (typeof invokeRustHostCommand !== "function") {
    throw new Error("host IPC requires invokeRustHostCommand");
  }
  if (typeof requestAppExit !== "function") {
    throw new Error("host IPC requires requestAppExit");
  }
  if (typeof isShuttingDown !== "function") {
    throw new Error("host IPC requires isShuttingDown");
  }
  if (!localShellActions) {
    throw new Error("host IPC requires localShellActions");
  }
  if (typeof validateTrustedRendererEvent !== "function") {
    throw new Error("host IPC requires validateTrustedRendererEvent");
  }
  hostIpcRegistered = true;

  ipcMain.handle("host:invoke", async (event, message) => {
    validateTrustedRendererEvent(event);
    const command =
      typeof message?.command === "string" ? message.command.trim() : "";
    if (!command) {
      throw new Error("host command is required");
    }
    const metadata = requireHostCommand(command);
    if (command === "app_exit") {
      await requestAppExit();
      return { ok: true };
    }
    if (isShuttingDown()) {
      throw new Error("Centaeris is shutting down");
    }
    if (command === "workspace_open_folder") {
      return localShellActions.openWorkspaceFolder(event, message?.payload ?? {});
    }
    if (command === "workspace_reveal_folder") {
      return localShellActions.revealWorkspaceFolder(message?.payload ?? {});
    }
    if (command === "plugin_reveal_source_ref") {
      return localShellActions.revealPluginSourceRef(message?.payload ?? {});
    }
    if (command === "plugin_select_install_path") {
      return localShellActions.selectAndInstallPlugin(event, message?.payload ?? {});
    }
    if (command === "skill_select_source_path") {
      return localShellActions.selectSkillSourcePath(event, message?.payload ?? {});
    }
    if (command === "skill_reveal_source") {
      return localShellActions.revealSkillSource(message?.payload ?? {});
    }
    return invokeRustHostCommand(command, message?.payload ?? {}, metadata);
  });

  ipcMain.handle("host:subscribe", async (event, message) => {
    validateTrustedRendererEvent(event);
    const eventName =
      typeof message?.eventName === "string" ? message.eventName.trim() : "";
    if (!eventName) {
      throw new Error("host eventName is required");
    }
    requireHostEventName(eventName);
    return { ok: true };
  });
};
