import type {
  AgentContextUsageSummary,
  AgentRuntimeConfig,
  AgentTokenUsageSummary,
} from "../../lib/chatBridge";
import { createSessionViewCacheStore } from "../../lib/sessionViewCache";
import { formatTokenQuantityInput } from "../../lib/tokenQuantities";
import type {
  AssistantExecutionTurn,
  ModelRuntimeDraft,
  RuntimeActivity,
  RuntimeActivityKind,
  RuntimeProcessState,
  SessionViewSnapshot,
  TaskStatus,
} from "./types";

export const EMPTY_MODEL_RUNTIME_DRAFT: ModelRuntimeDraft = {
  modelProviderId: "",
  model: "",
  modelApiBase: "",
  modelTimeoutMs: "",
  modelMaxRetries: "",
  modelRetryBackoffMs: "",
  modelContextTokens: "",
  modelMaxOutputTokens: "",
  modelThinkingMode: "",
};


export const sessionViewCacheStore =
  createSessionViewCacheStore<SessionViewSnapshot>({
    maxEntries: 12,
  });

export const isRecord = (value: unknown): value is Record<string, unknown> => {
  return typeof value === "object" && value !== null;
};

export const makeId = (prefix: string): string => {
  return `${prefix}-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`;
};

export const formatUserMessageTimestamp = (timestamp?: number): string => {
  if (!timestamp || !Number.isFinite(timestamp)) {
    return "";
  }
  const date = new Date(timestamp);
  if (Number.isNaN(date.getTime())) {
    return "";
  }
  const hour = `${date.getHours()}`.padStart(2, "0");
  const minute = `${date.getMinutes()}`.padStart(2, "0");
  const today = new Date();
  const isSameDay =
    date.getFullYear() === today.getFullYear() &&
    date.getMonth() === today.getMonth() &&
    date.getDate() === today.getDate();
  if (isSameDay) {
    return `${hour}:${minute}`;
  }
  const weekdays = [
    "星期日",
    "星期一",
    "星期二",
    "星期三",
    "星期四",
    "星期五",
    "星期六",
  ];
  return `${weekdays[date.getDay()]}${hour}:${minute}`;
};

export const AUTO_CONTINUE_AFTER_RESUME_WAIT_KEY =
  "queryLoop.autoContinueAfterResumeWait";

export const readAutoContinueAfterResumeWaitPreference = (): boolean | undefined => {
  if (typeof window === "undefined" || !window.localStorage) {
    return undefined;
  }
  const raw = window.localStorage.getItem(AUTO_CONTINUE_AFTER_RESUME_WAIT_KEY);
  if (!raw) {
    return undefined;
  }
  const normalized = raw.trim().toLowerCase();
  if (
    normalized === "true" ||
    normalized === "1" ||
    normalized === "yes" ||
    normalized === "on"
  ) {
    return true;
  }
  if (
    normalized === "false" ||
    normalized === "0" ||
    normalized === "no" ||
    normalized === "off"
  ) {
    return false;
  }
  return undefined;
};

export const toRuntimeDraftNumber = (value: number | undefined): string => {
  if (typeof value !== "number" || !Number.isFinite(value) || value < 0) {
    return "";
  }
  return String(Math.floor(value));
};

export const formatTokenCount = (value?: number | null): string => {
  if (typeof value !== "number" || !Number.isFinite(value) || value < 0) {
    return "-";
  }
  return Math.round(value).toLocaleString();
};

export const formatUsageInputLine = (
  inputTokens?: number | null,
  cachedTokens?: number | null,
): string => {
  return `input ${formatTokenCount(inputTokens)} (+ ${formatTokenCount(cachedTokens)} cached)`;
};

export const displayedInputTokens = (
  usage?: AgentTokenUsageSummary | null,
): number | null | undefined => {
  return usage?.promptCacheMissTokens ?? usage?.inputTokens;
};

export const displayedTotalTokens = (
  usage?: AgentTokenUsageSummary | null,
): number | null | undefined => {
  const inputTokens = displayedInputTokens(usage);
  const outputTokens = usage?.outputTokens;
  if (
    typeof inputTokens === "number" &&
    Number.isFinite(inputTokens) &&
    typeof outputTokens === "number" &&
    Number.isFinite(outputTokens)
  ) {
    return inputTokens + outputTokens;
  }
  return usage?.totalTokens;
};

export const buildContextUsageTooltip = (
  contextUsage?: AgentContextUsageSummary | null,
): string => {
  if (!contextUsage) {
    return "";
  }
  const latest = contextUsage.latestUsage;
  const lines = ["上下文占用"];
  if (
    typeof contextUsage.usedTokens === "number" &&
    typeof contextUsage.maxContextTokens === "number"
  ) {
    lines.push(
      `当前请求 ${formatTokenCount(contextUsage.usedTokens)} / ${formatTokenCount(contextUsage.maxContextTokens)}`,
    );
  }
  if (latest) {
    lines.push(
      "",
      "最近完成的模型请求",
      `total ${formatTokenCount(displayedTotalTokens(latest))}`,
      formatUsageInputLine(
        displayedInputTokens(latest),
        latest.promptCacheHitTokens,
      ),
      `output ${formatTokenCount(latest.outputTokens)}`,
    );
  }
  return lines.join("\n");
};

export const buildModelRuntimeDraft = (
  config?: AgentRuntimeConfig | null,
): ModelRuntimeDraft => {
  if (!config) {
    return { ...EMPTY_MODEL_RUNTIME_DRAFT };
  }
  return {
    modelProviderId: String(config.modelProviderId || "").trim(),
    model: String(config.model || "").trim(),
    modelApiBase: String(config.modelApiBase || "").trim(),
    modelTimeoutMs: toRuntimeDraftNumber(config.modelTimeoutMs),
    modelMaxRetries: toRuntimeDraftNumber(config.modelMaxRetries),
    modelRetryBackoffMs: toRuntimeDraftNumber(config.modelRetryBackoffMs),
    modelContextTokens: formatTokenQuantityInput(config.modelContextTokens),
    modelMaxOutputTokens: formatTokenQuantityInput(config.modelMaxOutputTokens),
    modelThinkingMode: String(config.modelThinkingMode || "").trim(),
  };
};


export const taskStatusLabel: Record<TaskStatus, string> = {
  running: "Running",
  done: "Done",
  error: "Failed",
};

export const parseJsonLike = (raw: unknown): unknown => {
  if (typeof raw !== "string") {
    return raw;
  }
  const trimmed = raw.trim();
  if (!trimmed) {
    return raw;
  }
  try {
    return JSON.parse(trimmed) as unknown;
  } catch {
    return raw;
  }
};

export const compactText = (value: unknown, maxLength: number = 120): string => {
  const normalized = String(value ?? "")
    .trim()
    .replace(/\s+/g, " ");
  if (!normalized) {
    return "";
  }
  if (normalized.length <= maxLength) {
    return normalized;
  }
  return `${normalized.slice(0, Math.max(0, maxLength - 3)).trimEnd()}...`;
};

export const formatExecutionError = (error: unknown): string => {
  const raw = error instanceof Error ? error.message : String(error ?? "");
  const normalized = raw.trim();
  return normalized || "处理失败。";
};

export const formatRuntimeModelError = (payload: Record<string, unknown>): string => {
  const text =
    typeof payload.message === "string" ? payload.message.trim() : "";
  const processState = normalizeRuntimeProcessState(payload.processState);
  if (processState === "provider_waiting") {
    return "模型服务排队中或触发并发限制，请稍后继续。";
  }
  if (processState === "auth_failed") {
    return "模型服务鉴权失败，请检查 API Key 或登录状态。";
  }
  if (processState === "provider_unavailable") {
    return "模型服务端故障或过载，请稍后继续。";
  }
  if (processState === "provider_interrupted") {
    return "模型服务响应中断，通常是服务端排队、长连接或网关中断导致。";
  }
  return formatExecutionError(new Error(text || "处理异常"));
};

export const DEFAULT_RUNTIME_ACTIVITY: RuntimeActivity = {
  kind: "thinking",
  label: "Thinking",
};

export const buildPendingTurn = (): AssistantExecutionTurn => {
  return {
    id: makeId("assistant-turn"),
    chunks: [],
    finalAnswer: "",
    isStreaming: true,
    startedAtMs: Date.now(),
    activity: DEFAULT_RUNTIME_ACTIVITY,
  };
};

export const RUNTIME_ACTIVITY_BY_PROCESS_STATE: Record<
  Exclude<RuntimeProcessState, "unknown">,
  RuntimeActivity
> = {
  thinking: { kind: "thinking", label: "Thinking" },
  searching: { kind: "thinking", label: "Thinking" },
  reading: { kind: "thinking", label: "Thinking" },
  executing: { kind: "executing", label: "Thinking" },
  reviewing: { kind: "thinking", label: "Thinking" },
  synthesizing: { kind: "thinking", label: "Thinking" },
  compressing: { kind: "compressing", label: "Compacting" },
  recovering: { kind: "thinking", label: "Thinking" },
  retrying: { kind: "thinking", label: "Thinking" },
  waiting: { kind: "waiting", label: "Thinking" },
  provider_waiting: { kind: "thinking", label: "Thinking" },
  auth_failed: { kind: "waiting", label: "Thinking" },
  provider_unavailable: { kind: "retrying", label: "Thinking" },
  provider_interrupted: { kind: "retrying", label: "Thinking" },
};

const NON_ACTIVITY_PROCESS_STATES = new Set<RuntimeProcessState>([
  "waiting",
  "auth_failed",
  "provider_unavailable",
  "provider_interrupted",
]);

export const normalizeRuntimeActivity = (
  label: string,
  kind: RuntimeActivityKind = "thinking",
): RuntimeActivity => {
  const normalizedLabel = label.trim();
  return {
    kind,
    label: normalizedLabel || DEFAULT_RUNTIME_ACTIVITY.label,
  };
};

export const mapPreparingToolNameToActivity = (
  toolName: unknown,
): RuntimeActivity => {
  if (toolName === "write" || toolName === "edit") {
    return normalizeRuntimeActivity("Editing", "executing");
  }
  if (toolName === "bash") {
    return normalizeRuntimeActivity("Preparing", "executing");
  }
  if (toolName === "web_search") {
    return normalizeRuntimeActivity("Searching");
  }
  return DEFAULT_RUNTIME_ACTIVITY;
};

export const sanitizeRuntimeActivityLabel = (
  label: string | undefined,
): string | undefined => {
  const normalized = label?.trim();
  return normalized || undefined;
};

export const normalizeRuntimeProcessState = (
  value: unknown,
): RuntimeProcessState | null => {
  const normalized =
    typeof value === "string" ? value.trim().toLowerCase() : "";
  if (!normalized) {
    return null;
  }
  return Object.prototype.hasOwnProperty.call(
    RUNTIME_ACTIVITY_BY_PROCESS_STATE,
    normalized,
  )
    ? (normalized as RuntimeProcessState)
    : null;
};

export const mapProcessStateToActivity = (
  processState: unknown,
  fallbackLabel?: string,
): RuntimeActivity | null => {
  const rawState = typeof processState === "string" ? processState.trim() : "";
  if (!rawState) {
    return null;
  }
  const state = normalizeRuntimeProcessState(rawState);
  if (!state) {
    throw new Error(`未知 runtime processState: ${rawState}`);
  }
  if (state === "unknown") {
    throw new Error("未知 runtime processState: unknown");
  }
  if (NON_ACTIVITY_PROCESS_STATES.has(state)) {
    return null;
  }
  const label = sanitizeRuntimeActivityLabel(fallbackLabel);
  const definition = RUNTIME_ACTIVITY_BY_PROCESS_STATE[state];
  if (label) {
    return {
      ...normalizeRuntimeActivity(label, definition.kind),
      processState: state,
    };
  }
  return { ...definition, processState: state };
};

const fnv1a32 = (value: string): number => {
  let hash = 0x811c9dc5;
  for (let index = 0; index < value.length; index += 1) {
    hash ^= value.charCodeAt(index);
    hash = Math.imul(hash, 0x01000193);
  }
  return hash >>> 0;
};

const RUNTIME_EASTER_EGGS = [
  ["thinking", "a faint signal crossed the Wired…"],
  ["synthesizing", "Do You Remember Love?"],
  ["compressing", "keeping what should not be forgotten…"],
  ["recovering", "escaping convergence, one more time…"],
] as const;

export const runtimeEasterEgg = (
  agentRunId: string | undefined,
  processState: RuntimeProcessState | undefined,
): string | null => {
  if (!agentRunId || !processState || fnv1a32(`runtime-egg:${agentRunId}`) % 32 !== 0) {
    return null;
  }
  const [selectedState, text] = RUNTIME_EASTER_EGGS[
    (fnv1a32(`runtime-egg-choice:${agentRunId}`) >>> 16) % RUNTIME_EASTER_EGGS.length
  ];
  return processState === selectedState ? text : null;
};

export const tachikomaEasterEgg = (
  agentRunId: string | undefined,
  totalChildren: number,
  liveChildren: number,
): number | null => {
  if (
    !agentRunId ||
    totalChildren < 3 ||
    liveChildren < 1 ||
    fnv1a32(`tachikoma:${agentRunId}`) % 16 !== 0
  ) {
    return null;
  }
  return liveChildren;
};

export const mapRuntimePayloadToActivity = (
  payload: Record<string, unknown>,
  fallbackState?: RuntimeProcessState,
  fallbackLabel?: string,
): RuntimeActivity | null => {
  return mapProcessStateToActivity(
    payload.processState ?? fallbackState,
    fallbackLabel,
  );
};
