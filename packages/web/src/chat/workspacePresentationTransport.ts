import { ApiError, apiResponse } from '../api.ts';
import { abortableDelay } from './abortableDelay.ts';
import { PRESENTATION_MAX_FRAME_BYTES, PresentationProtocolError, readPresentationSseBlock, type PresentationEntry, type PresentationIdentity } from './workspacePresentationProtocol.ts';

type PresentationController = Readonly<{
  lastCursor: string | null;
  acceptWithBackpressure(entry: PresentationEntry): Promise<void>;
}>;
type Options = PresentationIdentity & Readonly<{
  controller: PresentationController;
  signal: AbortSignal;
  onConnection(connection: 'running' | 'reconnecting'): void;
  request?: typeof apiResponse;
  wait?: typeof abortableDelay;
  headersTimeoutMs?: number;
  silenceTimeoutMs?: number;
  retryLimit?: number;
  subscribeWake?: (wake: (force?: boolean) => void) => () => void;
}>;
export class PresentationTimeoutError extends Error {}
function abortError() { return new DOMException('Aborted', 'AbortError'); }

async function bounded<T>(read: Promise<T>, milliseconds: number, signal: AbortSignal, expire: () => void): Promise<T> {
  if (signal.aborted) { void read.catch(() => {}); throw abortError(); }
  let timer: ReturnType<typeof setTimeout> | undefined;
  let onAbort: (() => void) | undefined;
  const timeout = new Promise<T>((_resolve, reject) => {
    timer = setTimeout(() => {
      // Reject before aborting so this attempt retains its actual failure cause.
      reject(new PresentationTimeoutError('presentation stream became inactive'));
      expire();
    }, milliseconds);
    onAbort = () => reject(abortError());
    signal.addEventListener('abort', onAbort, { once: true });
  });
  try { return await Promise.race([read, timeout]); }
  finally {
    clearTimeout(timer);
    if (onAbort) signal.removeEventListener('abort', onAbort);
  }
}

async function readPresentationStream(response: Response, identity: PresentationIdentity, controller: PresentationController,
  signal: AbortSignal, silenceTimeoutMs: number, expire: () => void, onApplied: () => void, onActivity: () => void) {
  if (!response.headers.get('content-type')?.toLowerCase().startsWith('text/event-stream')) {
    throw new PresentationProtocolError('presentation response is not an event stream');
  }
  const reader = response.body?.getReader();
  if (!reader) throw new PresentationProtocolError('presentation stream body is unavailable');
  const cancel = () => { void reader.cancel().catch(() => {}); };
  signal.addEventListener('abort', cancel, { once: true });
  const decoder = new TextDecoder('utf-8', { fatal: true });
  const encoder = new TextEncoder();
  let buffer = '';
  try {
    while (!signal.aborted) {
      // Any received bytes, including comment heartbeats, prove connection
      // liveness. Business progress and durable acknowledgement remain separate.
      const { value, done } = await bounded(reader.read(), silenceTimeoutMs, signal, expire);
      if (signal.aborted) throw abortError();
      onActivity();
      if (done) {
        // An incomplete frame is never delivered or acknowledged on EOF.
        break;
      }
      buffer += decoder.decode(value, { stream: true });
      let boundary: RegExpExecArray | null;
      while ((boundary = /\r?\n\r?\n/.exec(buffer)) !== null) {
        const block = buffer.slice(0, boundary.index);
        buffer = buffer.slice(boundary.index + boundary[0].length);
        if (encoder.encode(block).byteLength > PRESENTATION_MAX_FRAME_BYTES) throw new PresentationProtocolError('presentation frame is too large');
        const entry = readPresentationSseBlock(block, identity);
        if (!entry) continue;
        await controller.acceptWithBackpressure(entry);
        if (entry.cursor !== null || (entry.item.kind === "navigation" && !entry.item.hasMore)) onApplied();
      }
      if (encoder.encode(buffer).byteLength > PRESENTATION_MAX_FRAME_BYTES) throw new PresentationProtocolError('presentation frame is too large');
    }
    if (signal.aborted) throw abortError();
  } finally {
    signal.removeEventListener('abort', cancel);
    // A broken underlying source must not hold up recovery while cancel settles.
    void reader.cancel().catch(() => {});
    try { reader.releaseLock(); } catch { /* cancellation can still own a pending read */ }
  }
}

function browserWake(wake: (force?: boolean) => void) {
  if (typeof window === 'undefined') return () => {};
  const online = () => wake(true);
  const visible = () => { if (document.visibilityState === 'visible') wake(); };
  window.addEventListener('online', online);
  document.addEventListener('visibilitychange', visible);
  return () => { window.removeEventListener('online', online); document.removeEventListener('visibilitychange', visible); };
}

export async function streamWorkspacePresentation({ workspaceId, agentId, sessionId, controller, signal, onConnection,
  request = apiResponse, wait = abortableDelay, headersTimeoutMs = 15_000, silenceTimeoutMs = 30_000, retryLimit = 5,
  subscribeWake = browserWake }: Options) {
  if (![headersTimeoutMs, silenceTimeoutMs].every(value => Number.isFinite(value) && value > 0)) throw new Error('presentation deadlines must be positive');
  const identity = { workspaceId, agentId, sessionId };
  const query = new URLSearchParams({ agentId });
  if (sessionId !== null) query.set('sessionId', sessionId);
  const path = `/api/workspaces/${encodeURIComponent(workspaceId)}/presentation/events?${query}`;
  let failures = 0, wakeVersion = 0, lastActivity = 0;
  let activeAttempt: AbortController | null = null;
  let backoff: AbortController | null = null;
  const unsubscribe = subscribeWake(force => {
    if (signal.aborted) return;
    if (!force && activeAttempt && Date.now() - lastActivity < Math.min(silenceTimeoutMs, 10_000)) return;
    wakeVersion++;
    activeAttempt?.abort();
    backoff?.abort();
  });
  try {
    while (!signal.aborted) {
      const version = wakeVersion;
      const attempt = new AbortController();
      activeAttempt = attempt;
      lastActivity = Date.now();
      const attemptSignal = AbortSignal.any([signal, attempt.signal]);
      try {
        const headers: Record<string, string> = { Accept: 'text/event-stream' };
        if (controller.lastCursor !== null) headers['Last-Event-ID'] = controller.lastCursor;
        const pending = request(path, { signal: attemptSignal, headers, timeoutMs: null }).then(response => {
          if (attemptSignal.aborted) { void response.body?.cancel().catch(() => {}); throw abortError(); }
          return response;
        });
        const response = await bounded(pending, headersTimeoutMs, attemptSignal, () => attempt.abort());
        if (!response.ok) throw new ApiError('presentation stream request failed', response.status);
        onConnection('running');
        await readPresentationStream(response, identity, controller, attemptSignal, silenceTimeoutMs, () => attempt.abort(),
          () => { failures = 0; }, () => { lastActivity = Date.now(); });
        // This subscription outlives an individual run. Clean EOF is a transport
        // interruption too; resume rather than inventing a terminal state.
        failures++;
      } catch (error) {
        if (signal.aborted) throw abortError();
        if (wakeVersion !== version) continue;
        if (error instanceof ApiError && [400, 401, 403, 404].includes(error.status)) throw error;
        if (error instanceof PresentationProtocolError) throw error;
        failures++;
        if (failures > retryLimit) throw error;
      } finally { attempt.abort(); activeAttempt = null; }
      if (failures > retryLimit) throw new Error('presentation stream repeatedly ended without a checkpoint');
      onConnection('reconnecting');
      backoff = new AbortController();
      try { await wait(Math.min(500 * 2 ** Math.max(0, failures - 1), 8000), AbortSignal.any([signal, backoff.signal])); }
      catch (error) { if (signal.aborted || !backoff.signal.aborted) throw error; }
      finally { backoff = null; }
    }
    throw abortError();
  } finally { unsubscribe(); }
}
