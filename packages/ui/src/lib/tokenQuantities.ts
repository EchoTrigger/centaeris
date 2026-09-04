const TOKEN_UNIT_K = 1024;
const TOKEN_UNIT_M = TOKEN_UNIT_K * TOKEN_UNIT_K;

const trimTrailingZeroes = (value: string): string => value.replace(/\.0+$/, "").replace(/(\.\d*?)0+$/, "$1");

export const formatTokenQuantityInput = (value: number | undefined): string => {
  if (typeof value !== "number" || !Number.isFinite(value) || value < 0) {
    return "";
  }
  const tokens = Math.floor(value);
  if (tokens > 0 && tokens % TOKEN_UNIT_M === 0) {
    return `${tokens / TOKEN_UNIT_M}M`;
  }
  if (tokens > 0 && tokens % TOKEN_UNIT_K === 0) {
    return `${tokens / TOKEN_UNIT_K}k`;
  }
  return String(tokens);
};

export const formatTokenQuantityCompact = (value: number | undefined | null): string => {
  if (typeof value !== "number" || !Number.isFinite(value) || value < 0) {
    return "";
  }
  const tokens = Math.round(value);
  if (tokens >= TOKEN_UNIT_M) {
    return `${trimTrailingZeroes((tokens / TOKEN_UNIT_M).toFixed(tokens % TOKEN_UNIT_M === 0 ? 0 : 1))}M`;
  }
  if (tokens >= TOKEN_UNIT_K) {
    return `${trimTrailingZeroes((tokens / TOKEN_UNIT_K).toFixed(tokens % TOKEN_UNIT_K === 0 ? 0 : 1))}k`;
  }
  return String(tokens);
};
