export const formatDuration = (durationMs?: number): string => {
  if (durationMs === undefined) {
    return "-";
  }
  if (durationMs < 1000) {
    return `${durationMs}ms`;
  }
  return `${(durationMs / 1000).toFixed(2)}s`;
};

export const formatHmsDuration = (
  durationMs: number,
  minimumOneSecond: boolean = false,
): string => {
  const rawSeconds = minimumOneSecond
    ? Math.max(1, Math.round(durationMs / 1000))
    : Math.max(0, Math.floor(durationMs / 1000));
  const hours = Math.floor(rawSeconds / 3600);
  const minutes = Math.floor((rawSeconds % 3600) / 60);
  const seconds = rawSeconds % 60;
  if (hours > 0) {
    return `${hours}h ${minutes}m ${seconds}s`;
  }
  if (minutes > 0) {
    return `${minutes}m ${seconds}s`;
  }
  return `${seconds}s`;
};

export const formatProcessDuration = (durationMs?: number): string => {
  if (
    typeof durationMs !== "number" ||
    !Number.isFinite(durationMs) ||
    durationMs < 0
  ) {
    return "";
  }
  return formatHmsDuration(durationMs, true);
};
