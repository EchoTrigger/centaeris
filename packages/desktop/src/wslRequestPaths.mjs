import { readFile } from "node:fs/promises";
const pathFields = JSON.parse(await readFile(new URL("../../runtime/host/wsl-request-paths.json", import.meta.url), "utf8"));

export const mapWslRequestPaths = (command, payload, map) => {
  if (!payload?.request || typeof payload.request !== "object") return payload;
  const entry = pathFields[command];
  const fields = entry?.fields;
  if (!fields && command !== "session/prompt") return payload;
  const request = { ...payload.request };
  for (const field of fields ?? []) {
    if (typeof request[field] === "string") {
      request[field] = map(request[field], { importSource: entry.importSource ?? false });
    }
  }
  if (command === "session/prompt" && Array.isArray(request.attachments)) {
    request.attachments = request.attachments.map((attachment) => ({
      ...attachment, localPath: map(attachment.localPath, { importSource: true }),
    }));
  }
  return { ...payload, request };
};
