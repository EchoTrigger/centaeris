import fs from "node:fs/promises";
import path from "node:path";

const MAX_PREVIEW_BYTES = 25 * 1024 * 1024;
const IMAGE_MIME_TYPES = new Map([
  ["png", "image/png"],
  ["jpg", "image/jpeg"],
  ["jpeg", "image/jpeg"],
  ["gif", "image/gif"],
  ["webp", "image/webp"],
  ["bmp", "image/bmp"],
  ["ico", "image/x-icon"],
  ["svg", "image/svg+xml"],
]);

const commandError = (code, message) => {
  const error = new Error(message);
  error.code = code;
  error.payload = {
    code: code === "invalid_request" ? -32600 : -32603,
    message,
    data: { code, message },
  };
  return error;
};

const invalidRequest = (message) => {
  throw commandError("invalid_request", `invalid command request: ${message}`);
};

const previewRequest = (payload) => {
  if (!payload || typeof payload !== "object" || Array.isArray(payload)
    || Object.keys(payload).length !== 1 || !Object.hasOwn(payload, "request")) {
    invalidRequest("desktop_file_preview_read payload must contain only request");
  }
  const request = payload.request;
  if (!request || typeof request !== "object" || Array.isArray(request)) {
    invalidRequest("desktop_file_preview_read request must be an object");
  }
  const fields = new Set(["path", "workspaceRoot", "basePath"]);
  for (const field of Object.keys(request)) {
    if (!fields.has(field)) invalidRequest(`unknown field \`${field}\``);
  }
  if (typeof request.path !== "string") {
    invalidRequest("desktop_file_preview_read request.path must be a string");
  }
  for (const field of ["workspaceRoot", "basePath"]) {
    if (request[field] != null && typeof request[field] !== "string") {
      invalidRequest(`desktop_file_preview_read request.${field} must be a string or null`);
    }
  }
  return request;
};

const trimPath = (value) => value.replace(/^\p{White_Space}+|\p{White_Space}+$/gu, "");

const userSuppliedPath = (value) => {
  let normalized = trimPath(value);
  while (normalized.startsWith("\\\\?\\")) normalized = normalized.slice(4);
  return normalized;
};

const isAbsolutePath = (value) => path.isAbsolute(value)
  && (process.platform !== "win32" || path.parse(value).root.length > 1);

const pathFromCwd = (value, cwd) => {
  if (process.platform === "win32") return path.resolve(cwd, value);
  return isAbsolutePath(value) ? value : `${cwd}${path.sep}${value}`;
};

const canonicalPath = async (value, cwd, label) => {
  try {
    return await fs.realpath(pathFromCwd(value, cwd));
  } catch (error) {
    throw new Error(`resolve desktop preview ${label} failed: ${error.message}`);
  }
};

const canonicalFilePath = async (value, cwd) => {
  const canonical = await canonicalPath(value, cwd, "file");
  const metadata = await fs.stat(canonical).catch(() => null);
  if (!metadata?.isFile()) throw new Error("desktop preview path is not a file");
  return canonical;
};

const canonicalBasePath = async (value, cwd) => {
  const canonical = await canonicalPath(value, cwd, "base path");
  const metadata = await fs.stat(canonical).catch(() => null);
  if (metadata?.isFile()) return path.dirname(canonical);
  if (!metadata?.isDirectory()) throw new Error("desktop preview base path is not a directory");
  return canonical;
};

const resolvePreviewFile = async (request, cwd) => {
  const rawPath = trimPath(request.path);
  if (!rawPath) throw new Error("desktop file preview path is empty");
  const requestedPath = userSuppliedPath(rawPath);
  if (isAbsolutePath(requestedPath)) {
    const file = await canonicalFilePath(requestedPath, cwd);
    return { root: path.dirname(file), file };
  }
  if (path.parse(requestedPath).root) {
    throw new Error("desktop file preview relative path cannot include a drive or root");
  }
  const base = trimPath(request.basePath ?? "") || trimPath(request.workspaceRoot ?? "");
  if (!base) {
    throw new Error("desktop file preview requires an absolute path or basePath/workspaceRoot");
  }
  const root = await canonicalBasePath(userSuppliedPath(base), cwd);
  const file = await canonicalFilePath(`${root}${path.sep}${requestedPath}`, cwd);
  return { root, file };
};

const displayPath = (value) => {
  if (value.startsWith("\\\\?\\UNC\\")) return `\\\\${value.slice(8)}`;
  return value.startsWith("\\\\?\\") ? value.slice(4) : value;
};

export const readDesktopFilePreview = async (payload, cwd = process.cwd()) => {
  const request = previewRequest(payload);
  try {
    const { root, file } = await resolvePreviewFile(request, cwd);
    const metadata = await fs.stat(file).catch((error) => {
      throw new Error(`read desktop preview metadata failed: ${error.message}`);
    });
    if (metadata.size > MAX_PREVIEW_BYTES) {
      throw new Error(`desktop file preview is too large (${metadata.size} bytes, max ${MAX_PREVIEW_BYTES})`);
    }
    const bytes = await fs.readFile(file).catch((error) => {
      throw new Error(`read desktop preview file failed: ${error.message}`);
    });
    const extension = trimPath(path.extname(file).slice(1)).toLowerCase();
    const imageMimeType = IMAGE_MIME_TYPES.get(extension);
    const mimeType = imageMimeType || (extension === "pdf" ? "application/pdf" : null);
    let content = "";
    if (!mimeType) {
      try {
        content = new TextDecoder("utf-8", { fatal: true, ignoreBOM: true })
          .decode(bytes).replace(/^\uFEFF+/u, "");
      } catch {
        throw new Error("desktop file preview supports UTF-8 text, images, and PDF only");
      }
    }
    return {
      root: displayPath(root),
      path: displayPath(file),
      name: path.basename(file),
      content,
      byteLen: bytes.length,
      encoding: mimeType ? "base64" : "utf-8",
      contentKind: imageMimeType ? "image" : mimeType ? "pdf" : "text",
      mimeType: mimeType || "text/plain; charset=utf-8",
      dataUrl: mimeType ? `data:${mimeType};base64,${bytes.toString("base64")}` : null,
    };
  } catch (error) {
    throw commandError("desktop_file_preview_failed", error.message);
  }
};
