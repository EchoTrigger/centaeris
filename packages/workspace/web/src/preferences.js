import { useSyncExternalStore } from "react";

const COMPOSER_ENTER_NEW_LINE_KEY_PREFIX = "centaeris:composer-enter-new-line:v1:";
const MODEL_SELECTION_KEY_PREFIX = "centaeris:model-selection:v1:";
const MODEL_EFFORT_KEY_PREFIX = "centaeris:model-effort:v1:";
const PREFERENCE_CHANGED = "centaeris:input-preference-changed";

function key(userId) {
  return `${COMPOSER_ENTER_NEW_LINE_KEY_PREFIX}${userId}`;
}

function modelSelectionKey(userId, workspaceId) {
  return `${MODEL_SELECTION_KEY_PREFIX}${JSON.stringify([userId, workspaceId])}`;
}

function modelEffortKey(userId, workspaceId, model) {
  return `${MODEL_EFFORT_KEY_PREFIX}${JSON.stringify([userId, workspaceId, model.providerId, model.modelName])}`;
}

export function readPreferredModelIdentity(userId, workspaceId) {
  try {
    const value = JSON.parse(window.localStorage.getItem(modelSelectionKey(userId, workspaceId)) || "null");
    return Array.isArray(value) && value.length === 2 && value.every((part) => typeof part === "string" && part)
      ? JSON.stringify(value)
      : "";
  } catch {
    return "";
  }
}

export function writePreferredModelIdentity(userId, workspaceId, model) {
  try {
    window.localStorage.setItem(
      modelSelectionKey(userId, workspaceId),
      JSON.stringify([model.providerId, model.modelName]),
    );
  } catch {
    // The in-memory selection remains usable when browser storage is unavailable.
  }
}

export function readModelThinkingMode(userId, workspaceId, model) {
  if (!model) return "";
  const modes = model.thinkingModes || [];
  try {
    const saved = window.localStorage.getItem(modelEffortKey(userId, workspaceId, model));
    if (modes.includes(saved)) return saved;
  } catch {
    // Fall back to the model's initial configuration.
  }
  return modes.includes(model.thinkingMode) ? model.thinkingMode : "";
}

export function writeModelThinkingMode(userId, workspaceId, model, mode) {
  if (!model?.thinkingModes?.includes(mode)) return;
  try {
    window.localStorage.setItem(modelEffortKey(userId, workspaceId, model), mode);
  } catch {
    // The in-memory selection remains usable when browser storage is unavailable.
  }
}

export function readEnterStartsNewLine(userId) {
  try {
    return window.localStorage.getItem(key(userId)) === "1";
  } catch {
    return false;
  }
}

export function writeEnterStartsNewLine(userId, enabled) {
  try {
    if (enabled) window.localStorage.setItem(key(userId), "1");
    else window.localStorage.removeItem(key(userId));
  } catch {
    // Browser preferences are optional; the default input behavior remains available.
  }
  window.dispatchEvent(new Event(PREFERENCE_CHANGED));
}

function subscribe(listener) {
  window.addEventListener("storage", listener);
  window.addEventListener(PREFERENCE_CHANGED, listener);
  return () => {
    window.removeEventListener("storage", listener);
    window.removeEventListener(PREFERENCE_CHANGED, listener);
  };
}

export function useEnterStartsNewLine(userId) {
  return useSyncExternalStore(subscribe, () => readEnterStartsNewLine(userId), () => false);
}
