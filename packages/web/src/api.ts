import type { RuntimeConfig } from "./config";

export class ApiError extends Error {
  readonly status: number;
  readonly payload: unknown;
  readonly retryAfter: string | null;

  constructor(
    message: string,
    status: number,
    payload: unknown = null,
    retryAfter: string | null = null,
  ) {
    super(message);
    this.status = status;
    this.payload = payload;
    this.retryAfter = retryAfter;
  }
}

export function isAuthenticationRequired(error: unknown): error is ApiError {
  return error instanceof ApiError && error.status === 401 && error.message === "authentication_required";
}

let apiBaseUrl = "";
let csrfTokenPromise: Promise<string> | undefined;
let authenticationRequiredHandler: (() => void) | undefined;

export function configureApi(config: RuntimeConfig) {
  apiBaseUrl = config.apiBaseUrl;
}

export function apiUrl(path: string) {
  if (!apiBaseUrl) throw new Error("API client is not configured");
  if (!path.startsWith("/")) throw new Error(`API path must be absolute: ${path}`);
  return `${apiBaseUrl}${path}`;
}

export function clearCsrfToken() {
  csrfTokenPromise = undefined;
}

export function setAuthenticationRequiredHandler(handler: (() => void) | undefined) {
  authenticationRequiredHandler = handler;
  return () => {
    if (authenticationRequiredHandler === handler) authenticationRequiredHandler = undefined;
  };
}

export function hasAuthenticationRequiredHandler() {
  return Boolean(authenticationRequiredHandler);
}

export type ApiRequestOptions = RequestInit & { timeoutMs?: number | null };
const REQUEST_TIMEOUT_MS = 30_000;

function requestSignal({ signal, timeoutMs = REQUEST_TIMEOUT_MS }: ApiRequestOptions): AbortSignal | undefined {
  if (timeoutMs !== null && (!Number.isFinite(timeoutMs) || timeoutMs <= 0)) {
    throw new RangeError("request timeout must be positive or null");
  }
  const deadline = timeoutMs === null ? undefined : AbortSignal.timeout(timeoutMs);
  if (signal && deadline) return AbortSignal.any([signal, deadline]);
  return signal ?? deadline;
}

async function waitForSignal<T>(pending: Promise<T>, signal?: AbortSignal): Promise<T> {
  if (!signal) return pending;
  signal.throwIfAborted();
  return new Promise<T>((resolve, reject) => {
    const abort = () => reject(signal.reason);
    signal.addEventListener("abort", abort, { once: true });
    pending.then(resolve, reject).finally(() => signal.removeEventListener("abort", abort));
  });
}

export async function csrfHeaders(method = "GET", signal?: AbortSignal): Promise<Record<string, string>> {
  if (["GET", "HEAD", "OPTIONS"].includes(method.toUpperCase())) return {};
  if (!csrfTokenPromise) {
    csrfTokenPromise = fetch(apiUrl("/api/csrf"), { credentials: "include", signal: AbortSignal.timeout(REQUEST_TIMEOUT_MS) }).then(async (response) => {
      if (!response.ok) throw new ApiError(`csrf failed: ${response.status}`, response.status);
      const body: unknown = await response.json();
      if (!body || typeof body !== "object" || !("csrfToken" in body) || typeof body.csrfToken !== "string") {
        throw new Error("csrf response missing csrfToken");
      }
      return body.csrfToken;
    }).catch((error) => {
      csrfTokenPromise = undefined;
      throw error;
    });
  }
  return { "X-CSRFToken": await waitForSignal(csrfTokenPromise, signal) };
}

export async function apiResponse(path: string, options: ApiRequestOptions = {}) {
  const signal = requestSignal(options);
  signal?.throwIfAborted();
  const { timeoutMs: _timeoutMs, ...requestOptions } = options;
  const headers = new Headers(options.headers);
  const isForm = options.body instanceof FormData;
  if (options.body && !isForm && !headers.has("Content-Type")) {
    headers.set("Content-Type", "application/json");
  }
  for (const [name, value] of Object.entries(await csrfHeaders(options.method, signal))) {
    headers.set(name, value);
  }
  const response = await waitForSignal(fetch(apiUrl(path), {
    ...requestOptions,
    signal,
    credentials: "include",
    headers,
  }), signal);
  if (!response.ok) {
    const body: unknown = await waitForSignal(response.json().catch(() => null), signal);
    const message = body && typeof body === "object" && "error" in body && typeof body.error === "string"
      ? body.error
      : `request_failed:${response.status}`;
    const error = new ApiError(message, response.status, body, response.headers.get("Retry-After"));
    if (response.status === 403 && message === "csrf_failed") clearCsrfToken();
    if (isAuthenticationRequired(error)) authenticationRequiredHandler?.();
    throw error;
  }
  return response;
}

export async function apiJson<T>(path: string, options: ApiRequestOptions = {}): Promise<T> {
  const signal = requestSignal(options);
  const response = await apiResponse(path, { ...options, signal, timeoutMs: null });
  if (response.status === 204) return undefined as T;
  return waitForSignal(response.json() as Promise<T>, signal);
}

export function jsonOptions(method: string, body: unknown): RequestInit {
  return { method, body: JSON.stringify(body) };
}

export type WorkspaceRole = "owner" | "admin" | "member";
export type WorkspaceSummary = { id: string; name: string; role: WorkspaceRole; status?: string };

export function requireWorkspaces(value: unknown): WorkspaceSummary[] {
  if (!Array.isArray(value) || value.some((workspace) => (
    !workspace
    || typeof workspace !== "object"
    || typeof workspace.id !== "string"
    || !workspace.id
    || typeof workspace.name !== "string"
    || typeof workspace.role !== "string"
    || !["owner", "admin", "member"].includes(workspace.role)
  ))) throw new Error("workspaces_invalid");
  return value as WorkspaceSummary[];
}
