import type { Readable, Writable } from "node:stream";
import { RpcProtocolError } from "./index.ts";
import type { RpcReceiver, RpcTransport } from "./types.ts";

// Matches the Runtime's maximum outbound JSON-RPC frame.
const MAX_RESPONSE_BYTES = 160 * 1024 * 1024;

/** Own binary Node streams for a JSONL socket or child-process connection. */
export function createJsonlTransport(input: Readable, output: Writable): RpcTransport {
  const decoder = new TextDecoder("utf-8", { fatal: true });
  let receiver: RpcReceiver | undefined;
  let closed = false;
  let parts: string[] = [];
  let partialBytes = 0;

  const detach = () => {
    input.off("data", read);
    input.off("end", end);
    input.off("close", closedInput);
    input.off("error", failed);
    output.off("error", failed);
  };
  const finish = (cause?: Error) => {
    if (closed) return;
    closed = true;
    detach();
    parts = [];
    partialBytes = 0;
    receiver?.close(cause);
    input.destroy();
    output.destroy();
  };
  const consume = (text: string) => {
    const lines = text.split("\n");
    for (const [index, part] of lines.entries()) {
      partialBytes += Buffer.byteLength(part, "utf8");
      if (partialBytes > MAX_RESPONSE_BYTES) {
        finish(new RpcProtocolError("Runtime JSONL response exceeds the frame limit"));
        return;
      }
      if (part) parts.push(part);
      if (index < lines.length - 1) {
        const frame = parts.join("").replace(/\r$/, "");
        parts = [];
        partialBytes = 0;
        receiver?.message(frame);
        if (closed) return;
      }
    }
  };
  const decode = (chunk?: Uint8Array) => {
    let text: string;
    try {
      text = decoder.decode(chunk, { stream: chunk !== undefined });
    } catch (cause) {
      finish(new RpcProtocolError("Runtime JSONL input is not valid UTF-8", { cause }));
      return;
    }
    consume(text);
  };
  const read = (chunk: Uint8Array) => decode(chunk);
  const end = () => {
    decode();
    if (closed) return;
    if (parts.length) receiver?.message(parts.join(""));
    finish();
  };
  const closedInput = () => finish();
  const failed = (error: Error) => finish(error);

  return {
    send(frame) {
      return new Promise<void>((resolve, reject) => {
        output.write(`${frame}\n`, (error) => error ? reject(error) : resolve());
      });
    },
    listen(next) {
      if (receiver) throw new Error("JSONL transport already has a receiver");
      receiver = next;
      if (closed) {
        next.close();
      } else {
        input.on("data", read);
        input.on("end", end);
        input.on("close", closedInput);
        input.on("error", failed);
        output.on("error", failed);
      }
      return detach;
    },
    close() {
      finish();
    },
  };
}
