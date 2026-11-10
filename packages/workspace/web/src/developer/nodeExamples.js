const requireCredential = [
  "const key = process.env.CENTAERIS_RUN_KEY;",
  'if (!key) throw new Error("Set CENTAERIS_RUN_KEY to your API credential secret");',
];

export function nodeRequestExample(endpoint, saveRequest) {
  return [
    saveRequest
      ? 'import { createHash, randomUUID } from "node:crypto";'
      : 'import { randomUUID } from "node:crypto";',
    ...(saveRequest ? ['import { createWriteStream, mkdirSync, writeFileSync } from "node:fs";'] : []),
    'import { Readable } from "node:stream";',
    'import { pipeline } from "node:stream/promises";',
    "",
    ...requireCredential,
    "const endpoint = " + JSON.stringify(endpoint) + ";",
    'const operationId = Math.floor(Date.now() / 1000) + "." + randomUUID();',
    'const body = JSON.stringify({ prompt: "Reply with one sentence describing Centaeris." });',
    ...(saveRequest ? [
      "",
      'const directory = "./centaeris-first-call";',
      "mkdirSync(directory, { mode: 0o700 });",
      'writeFileSync(directory + "/operation-id", operationId);',
      'writeFileSync(directory + "/endpoint", endpoint);',
      'writeFileSync(directory + "/request.json", body);',
      'const fingerprint = createHash("sha256").update(key).digest("hex");',
      'writeFileSync(directory + "/credential-fingerprint", fingerprint);',
    ] : []),
    "",
    'console.error("operationId=" + operationId);',
    "const response = await fetch(endpoint, {",
    '  method: "POST",',
    '  redirect: "error",',
    "  signal: AbortSignal.timeout(700_000),",
    "  headers: {",
    '    Authorization: "Bearer " + key,',
    '    "Idempotency-Key": operationId,',
    '    "Content-Type": "application/json",',
    '    Accept: "text/event-stream, application/json",',
    "  },",
    "  body,",
    "});",
    'console.error("HTTP", response.status, response.headers.get("content-type"));',
    ...(saveRequest ? [
      'writeFileSync(directory + "/headers.json", JSON.stringify(Object.fromEntries(response.headers), null, 2));',
      'await pipeline(Readable.fromWeb(response.body), createWriteStream(directory + "/response.txt"));',
      'console.error("Response saved to " + directory + "/response.txt");',
    ] : ["await pipeline(Readable.fromWeb(response.body), process.stdout, { end: false });"]),
    "if (!response.ok) process.exitCode = 1;",
  ].join("\n");
}

export const nodeStatusExample = [
  'import { createHash } from "node:crypto";',
  'import { readFileSync } from "node:fs";',
  "",
  ...requireCredential,
  'const directory = "./centaeris-first-call";',
  'const operationId = readFileSync(directory + "/operation-id", "utf8").trim();',
  'const endpoint = readFileSync(directory + "/endpoint", "utf8").trim();',
  'const fingerprint = createHash("sha256").update(key).digest("hex");',
  'if (fingerprint !== readFileSync(directory + "/credential-fingerprint", "utf8").trim()) {',
  '  throw new Error("Use the original API credential for this operation");',
  "}",
  "",
  'const response = await fetch(new URL("operations/" + encodeURIComponent(operationId), endpoint), {',
  '  redirect: "error",',
  "  signal: AbortSignal.timeout(30_000),",
  '  headers: { Authorization: "Bearer " + key, Accept: "application/json" },',
  "});",
  "console.error(\"HTTP\", response.status);",
  "console.log(await response.text());",
  "if (!response.ok) process.exitCode = 1;",
].join("\n");
