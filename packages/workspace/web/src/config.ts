export type RuntimeConfig = {
  apiBaseUrl: string;
};

export async function loadRuntimeConfig(): Promise<RuntimeConfig> {
  const response = await fetch("/config.json", { cache: "no-store" });
  if (!response.ok) throw new Error(`runtime config failed: ${response.status}`);
  const value: unknown = await response.json();
  if (!value || typeof value !== "object" || !("apiBaseUrl" in value)) {
    throw new Error("runtime config missing apiBaseUrl");
  }
  const apiBaseUrl = (value as { apiBaseUrl: unknown }).apiBaseUrl;
  if (typeof apiBaseUrl !== "string" || !apiBaseUrl.trim()) {
    throw new Error("runtime config apiBaseUrl must be a non-empty string");
  }
  const resolvedBaseUrl = apiBaseUrl === "/" ? window.location.origin : apiBaseUrl;
  const url = new URL(resolvedBaseUrl);
  if (url.protocol !== "http:" && url.protocol !== "https:") {
    throw new Error("runtime config apiBaseUrl must use http or https");
  }
  return { apiBaseUrl: resolvedBaseUrl.replace(/\/$/, "") };
}
