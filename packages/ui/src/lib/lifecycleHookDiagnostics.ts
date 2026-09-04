export type LifecycleHookRunStatus =
  | "Succeeded"
  | "Blocked"
  | "Failed"
  | "SkippedUntrusted";

export type LifecycleHookSourceKind = "User" | "Project" | "Plugin" | "Admin";

export type LifecycleHookEventName =
  | "SessionStart"
  | "UserPromptSubmit"
  | "SubagentStart"
  | "SubagentStop"
  | "PreToolUse"
  | "PermissionRequest"
  | "PostToolUse"
  | "PreCompact"
  | "PostCompact"
  | "Stop";

export type LifecycleHookSource = {
  kind: LifecycleHookSourceKind;
  name: string;
};

export type LifecycleHookHandlerDiagnostics = {
  id: string;
  event: LifecycleHookEventName;
  matcher?: string;
  source: LifecycleHookSource;
  trusted: boolean;
  program: string;
  args: string[];
  timeoutMs: number;
};

export type LifecycleHookRun = {
  taskId: string;
  handlerId: string;
  event: LifecycleHookEventName;
  source: LifecycleHookSource;
  status: LifecycleHookRunStatus;
  startedAtMs: number;
  completedAtMs: number;
  exitCode?: number;
  diagnostic?: string;
};

export type LifecycleHookDiagnosticsProjection = {
  handlers: LifecycleHookHandlerDiagnostics[];
  recentRuns: LifecycleHookRun[];
};

export type LifecycleHookDiagnosticsSummary = {
  handlerCount: number;
  trustedHandlerCount: number;
  failedRunCount: number;
  blockedRunCount: number;
  untrustedRunCount: number;
  latestRun?: LifecycleHookRun;
};

export const summarizeLifecycleHookDiagnostics = (
  projection: LifecycleHookDiagnosticsProjection,
): LifecycleHookDiagnosticsSummary => {
  const latestRun = projection.recentRuns
    .slice()
    .sort((left, right) => right.completedAtMs - left.completedAtMs)[0];

  return {
    handlerCount: projection.handlers.length,
    trustedHandlerCount: projection.handlers.filter((handler) => handler.trusted)
      .length,
    failedRunCount: projection.recentRuns.filter((run) => run.status === "Failed")
      .length,
    blockedRunCount: projection.recentRuns.filter(
      (run) => run.status === "Blocked",
    ).length,
    untrustedRunCount: projection.recentRuns.filter(
      (run) => run.status === "SkippedUntrusted",
    ).length,
    latestRun,
  };
};
