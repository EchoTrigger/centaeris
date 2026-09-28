import { invokeHost } from "../host/hostBridge";

export type ProcessSnapshot = {
  processSessionId: string;
  sessionId: string;
  program: string;
  args: string[];
  cwd: string;
  source: "hostCommand" | "agentTool";
  state: "running" | "stopping" | "exited";
  exitCode: number | null;
  terminationReason: "stopped" | "timedOut" | null;
  cleanupComplete: boolean;
  outputComplete: boolean;
  error: string | null;
};
export type ProcessList = {
  serviceInstanceId: string;
  processes: ProcessSnapshot[];
};
export type ProcessPage = {
  process: ProcessSnapshot;
  chunks: { cursor: string; stream: "stdout" | "stderr"; dataBase64: string }[];
  nextCursor: string;
  earliestCursor: string;
  gap: boolean;
  hasMore: boolean;
};
function object(value: unknown, keys: string[]): Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value))
    throw Error("Invalid process response");
  const record = value as Record<string, unknown>;
  if (
    Object.keys(record).length !== keys.length ||
    keys.some((k) => !(k in record))
  )
    throw Error("Unknown or missing process response fields");
  return record;
}
function text(value: unknown): string {
  if (typeof value !== "string") throw Error("Invalid process text");
  return value;
}
function flag(value: unknown): boolean {
  if (typeof value !== "boolean") throw Error("Invalid process flag");
  return value;
}
function cursor(value: unknown): string {
  const result = text(value);
  if (!/^(0|[1-9][0-9]*)$/.test(result)) throw Error("Invalid process cursor");
  return result;
}
export function parseProcess(value: unknown): ProcessSnapshot {
  const r = object(value, [
    "processSessionId",
    "sessionId",
    "program",
    "args",
    "cwd",
    "source",
    "state",
    "exitCode",
    "terminationReason",
    "cleanupComplete",
    "outputComplete",
    "error",
  ]);
  if (
    (r.source !== "hostCommand" && r.source !== "agentTool") ||
    !["running", "stopping", "exited"].includes(text(r.state)) ||
    ![null, "stopped", "timedOut"].includes(
      r.terminationReason as string | null,
    ) ||
    (r.exitCode !== null && !Number.isInteger(r.exitCode)) ||
    !Array.isArray(r.args)
  )
    throw Error("Invalid process state");
  return {
    processSessionId: text(r.processSessionId),
    sessionId: text(r.sessionId),
    program: text(r.program),
    args: r.args.map(text),
    cwd: text(r.cwd),
    source: r.source,
    state: r.state as ProcessSnapshot["state"],
    exitCode: r.exitCode as number | null,
    terminationReason:
      r.terminationReason as ProcessSnapshot["terminationReason"],
    cleanupComplete: flag(r.cleanupComplete),
    outputComplete: flag(r.outputComplete),
    error: r.error === null ? null : text(r.error),
  };
}
export async function listProcesses(sessionId: string): Promise<ProcessList> {
  const r = object(
    await invokeHost("process_session_list", { request: { sessionId } }),
    ["serviceInstanceId", "processes"],
  );
  if (!Array.isArray(r.processes)) throw Error("Invalid process list");
  const processes = r.processes.map(parseProcess);
  if (processes.some((p) => p.sessionId !== sessionId))
    throw Error("Process owner mismatch");
  return { serviceInstanceId: text(r.serviceInstanceId), processes };
}
export async function stopProcess(
  sessionId: string,
  processSessionId: string,
): Promise<ProcessSnapshot> {
  const result = parseProcess(
    await invokeHost("process_session_stop", {
      request: { sessionId, processSessionId },
    }),
  );
  if (
    result.sessionId !== sessionId ||
    result.processSessionId !== processSessionId
  )
    throw Error("Process owner mismatch");
  return result;
}
export async function readProcess(
  sessionId: string,
  processSessionId: string,
  nextCursor: string,
): Promise<ProcessPage> {
  const r = object(
    await invokeHost("process_session_read", {
      request: {
        sessionId,
        processSessionId,
        cursor: nextCursor,
        waitMs: 1000,
      },
    }),
    ["process", "chunks", "nextCursor", "earliestCursor", "gap", "hasMore"],
  );
  if (!Array.isArray(r.chunks)) throw Error("Invalid process chunks");
  const process = parseProcess(r.process);
  if (
    process.sessionId !== sessionId ||
    process.processSessionId !== processSessionId
  )
    throw Error("Process owner mismatch");
  return {
    process,
    chunks: r.chunks.map((value) => {
      const c = object(value, ["cursor", "stream", "dataBase64"]);
      if (c.stream !== "stdout" && c.stream !== "stderr")
        throw Error("Invalid output stream");
      return {
        cursor: cursor(c.cursor),
        stream: c.stream,
        dataBase64: text(c.dataBase64),
      };
    }),
    nextCursor: cursor(r.nextCursor),
    earliestCursor: cursor(r.earliestCursor),
    gap: flag(r.gap),
    hasMore: flag(r.hasMore),
  };
}

// Plain output, not a terminal emulator. Never interpret ANSI or terminal controls.
export function plainProcessText(text: string): string {
  return text
    .replace(/\x1b\][^\x07]*(?:\x07|\x1b\\)/g, "")
    .replace(/\x1b\[[0-?]*[ -/]*[@-~]/g, "")
    .replace(/[\x00-\x08\x0b-\x1f\x7f-\x9f]/g, "");
}
export class ProcessOutputBuffer {
  private decoders = { stdout: new TextDecoder(), stderr: new TextDecoder() };
  private content = "";
  truncated = false;
  append(page: ProcessPage): string {
    if (page.gap) {
      this.decoders = { stdout: new TextDecoder(), stderr: new TextDecoder() };
      this.content += "\n[Earlier output is no longer retained]\n";
    }
    for (const chunk of page.chunks) {
      const bytes = Uint8Array.from(atob(chunk.dataBase64), (c) =>
        c.charCodeAt(0),
      );
      this.content += this.decoders[chunk.stream].decode(bytes, {
        stream: true,
      });
    }
    if (page.process.outputComplete && !page.hasMore) {
      this.content +=
        this.decoders.stdout.decode() + this.decoders.stderr.decode();
    }
    if (this.content.length > 256 * 1024) {
      this.content = this.content.slice(-256 * 1024);
      this.truncated = true;
    }
    return plainProcessText(this.content);
  }
}
