export function formatWorkDuration(
  elapsedMs: number,
  translate: (key: string, options: { count: number }) => string,
): string {
  const seconds = Math.max(0, Math.floor(elapsedMs / 1000));
  return [
    ["workProgress.hours", Math.floor(seconds / 3600)],
    ["workProgress.minutes", Math.floor(seconds / 60) % 60],
    ["workProgress.seconds", seconds % 60],
  ].flatMap(([key, count]) => Number(count) > 0
    ? [translate(String(key), { count: Number(count) })] : []).join(" ");
}
