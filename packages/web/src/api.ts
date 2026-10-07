import type { RuntimeConfig } from "./config";

export class ApiError extends Error {
  readonly status: number;
  readonly payload: unknown;

  constructor(
    message: string,
    status: number,
    payload: unknown = null,
  ) {
    super(message);
    this.status = status;
    this.payload = payload;
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

export async function csrfHeaders(method = "GET"): Promise<Record<string, string>> {
  if (["GET", "HEAD", "OPTIONS"].includes(method.toUpperCase())) return {};
  if (!csrfTokenPromise) {
    csrfTokenPromise = fetch(apiUrl("/api/csrf"), { credentials: "include" }).then(async (response) => {
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
  return { "X-CSRFToken": await csrfTokenPromise };
}

export async function apiResponse(path: string, options: RequestInit = {}) {
  const headers = new Headers(options.headers);
  const isForm = options.body instanceof FormData;
  if (options.body && !isForm && !headers.has("Content-Type")) {
    headers.set("Content-Type", "application/json");
  }
  for (const [name, value] of Object.entries(await csrfHeaders(options.method))) {
    headers.set(name, value);
  }
  const response = await fetch(apiUrl(path), {
    ...options,
    credentials: "include",
    headers,
  });
  if (!response.ok) {
    const body: unknown = await response.json().catch(() => null);
    const message = body && typeof body === "object" && "error" in body && typeof body.error === "string"
      ? body.error
      : `request_failed:${response.status}`;
    const error = new ApiError(message, response.status, body);
    if (isAuthenticationRequired(error)) authenticationRequiredHandler?.();
    throw error;
  }
  return response;
}

export async function apiJson<T>(path: string, options: RequestInit = {}): Promise<T> {
  const response = await apiResponse(path, options);
  if (response.status === 204) return undefined as T;
  return response.json() as Promise<T>;
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
