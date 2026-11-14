import { useEffect, useMemo, useRef, useState } from "react";
import { parseMarkdown } from "./parse";
import { registerMarkdownParser, type ParseResult } from "./parseWorkerClient";

const WORKER_BYTES = 64 * 1024;
const encoder = new TextEncoder();
function needsWorker(source: string) {
  return source.length > WORKER_BYTES || (source.length > WORKER_BYTES / 4 && encoder.encode(source).byteLength > WORKER_BYTES);
}

export function useMarkdownDocument(source: string, streaming: boolean) {
  const large = needsWorker(source);
  const synchronous = useMemo(() => large ? null : parseMarkdown(source), [large, source]);
  const [parsed, setParsed] = useState<ParseResult | null>(null);
  const client = useRef<ReturnType<typeof registerMarkdownParser> | null>(null);
  useEffect(() => {
    if (!large) return;
    const registered = registerMarkdownParser(setParsed);
    client.current = registered;
    return () => { registered.close(); client.current = null; };
  }, [large]);
  useEffect(() => { if (large) client.current?.update(source); }, [large, source]);
  if (!large) return { document: synchronous, pending: false, error: null };
  const current = parsed && (parsed.source === source || (streaming && source.startsWith(parsed.source)));
  return {
    document: current ? parsed.document : null,
    pending: !current || parsed.source !== source,
    error: current ? parsed.error : null,
  };
}
