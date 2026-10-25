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
  now?: () => number;
  random?: () => number;
  isOnline?: () => boolean;
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
  signal: AbortSignal, silenceTimeoutMs: number, expire: () => void, onActivity: () => void) {
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
      if (done) {
        // An incomplete frame is never delivered or acknowledged on EOF.
        break;
      }
      if (value.byteLength > 0) onActivity();
      buffer += decoder.decode(value, { stream: true });
      let boundary: RegExpExecArray | null;
      while ((boundary = /\r?\n\r?\n/.exec(buffer)) !== null) {
        const block = buffer.slice(0, boundary.index);
        buffer = buffer.slice(boundary.index + boundary[0].length);
        if (encoder.encode(block).byteLength > PRESENTATION_MAX_FRAME_BYTES) throw new PresentationProtocolError('presentation frame is too large');
        const entry = readPresentationSseBlock(block, identity);
        if (!entry) continue;
        await controller.acceptWithBackpressure(entry);
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

const STABLE_CONNECTION_MS = 30_000;
const RETRY_WINDOW_MS = 60_000;
const RETRY_WINDOW_LIMIT = 5;
const MAX_SERVER_DELAY_MS = 60_000;

function browserOnline() { return typeof navigator === 'undefined' || navigator.onLine !== false; }
function retryAfterDelay(value: string | null, now: number) {
  if (!value) return 0;
  const seconds = /^\d+$/.test(value.trim()) ? Number(value) : null;
  if (seconds !== null) return seconds * 1000;
  const deadline = Date.parse(value);
  return Number.isFinite(deadline) ? Math.max(0, deadline - now) : 0;
}
function exhausted(cause?: unknown) {
  return new Error('presentation stream repeatedly ended or failed; automatic reconnect budget exhausted', { cause });
}

export async function streamWorkspacePresentation({ workspaceId, agentId, sessionId, controller, signal, onConnection,
  request = apiResponse, wait = abortableDelay, headersTimeoutMs = 15_000, silenceTimeoutMs = 30_000, retryLimit = 5,
  subscribeWake = browserWake, now = Date.now, random = Math.random, isOnline = browserOnline }: Options) {
  if (![headersTimeoutMs, silenceTimeoutMs].every(value => Number.isFinite(value) && value > 0)) throw new Error('presentation deadlines must be positive');
  if (!Number.isSafeInteger(retryLimit) || retryLimit < 0) throw new Error('presentation retry limit must be a nonnegative integer');
  const identity = { workspaceId, agentId, sessionId };
  const query = new URLSearchParams({ agentId });
  if (sessionId !== null) query.set('sessionId', sessionId);
  const path = `/api/workspaces/${encodeURIComponent(workspaceId)}/presentation/events?${query}`;
  let failures = 0, lastActivity = 0, attempted = false;
  let retryStarts: number[] = [];
  let activeAttempt: AbortController | null = null;
  let backoff: AbortController | null = null;
  let resumeOffline: (() => void) | null = null;
  const unsubscribe = subscribeWake(force => {
    if (signal.aborted) return;
    resumeOffline?.();
    if (!force && activeAttempt && now() - lastActivity < Math.min(silenceTimeoutMs, 10_000)) return;
    activeAttempt?.abort();
    backoff?.abort();
  });
  const checkBudget = (cause?: unknown) => {
    retryStarts = retryStarts.filter(started => now() - started < RETRY_WINDOW_MS);
    if (failures > retryLimit || retryStarts.length >= RETRY_WINDOW_LIMIT) throw exhausted(cause);
  };
  const waitUntilOnline = async () => {
    while (!isOnline()) {
      if (signal.aborted) throw abortError();
      await new Promise<void>((resolve, reject) => {
        const cleanup = () => { resumeOffline = null; signal.removeEventListener('abort', cancel); };
        const cancel = () => { cleanup(); reject(abortError()); };
        resumeOffline = () => { cleanup(); resolve(); };
        signal.addEventListener('abort', cancel, { once: true });
      });
    }
  };
  try {
    while (!signal.aborted) {
      await waitUntilOnline();
      if (signal.aborted) throw abortError();
      if (attempted) { checkBudget(); retryStarts.push(now()); }
      attempted = true;
      const attempt = new AbortController();
      activeAttempt = attempt;
      lastActivity = now();
      const attemptSignal = AbortSignal.any([signal, attempt.signal]);
      let serverDelay = 0;
      let failure: unknown;
      try {
        const headers: Record<string, string> = { Accept: 'text/event-stream' };
        if (controller.lastCursor !== null) headers['Last-Event-ID'] = controller.lastCursor;
        const pending = request(path, { signal: attemptSignal, headers, timeoutMs: null }).then(response => {
          if (attemptSignal.aborted) { void response.body?.cancel().catch(() => {}); throw abortError(); }
          return response;
        });
        const response = await bounded(pending, headersTimeoutMs, attemptSignal, () => attempt.abort());
        if (!response.ok) throw new ApiError('presentation stream request failed', response.status, null, response.headers.get('Retry-After'));
        const connectedAt = now();
        onConnection('running');
        await readPresentationStream(response, identity, controller, attemptSignal, silenceTimeoutMs, () => attempt.abort(), () => {
          lastActivity = now();
          // Received bytes after a sustained connection prove recovery. Applying
          // the same navigation snapshot or cursor again never buys new retries.
          if (lastActivity - connectedAt >= STABLE_CONNECTION_MS) failures = 0;
        });
        // This subscription outlives an individual run. Clean EOF is a transport
        // interruption too; resume rather than inventing a terminal state.
      } catch (error) {
        if (signal.aborted) throw abortError();
        if (error instanceof ApiError && [400, 401, 403, 404].includes(error.status)) throw error;
        if (error instanceof PresentationProtocolError) throw error;
        if (error instanceof ApiError) serverDelay = retryAfterDelay(error.retryAfter, now());
        failure = error;
      } finally { attempt.abort(); activeAttempt = null; }
      failures++;
      checkBudget(failure);
      // A long server cooldown must not be shortened to fit a client timer.
      // Return to the existing manual recovery UI instead of retrying early.
      if (serverDelay > MAX_SERVER_DELAY_MS) throw exhausted(failure);
      onConnection('reconnecting');
      const serverNotBefore = now() + serverDelay;
      const jitter = 0.5 + Math.max(0, Math.min(1, random()));
      let delay = Math.max(serverDelay, Math.round(Math.min(500 * 2 ** Math.max(0, failures - 1), 8000) * jitter));
      do {
        backoff = new AbortController();
        try { await wait(delay, AbortSignal.any([signal, backoff.signal])); break; }
        catch (error) {
          if (signal.aborted || !backoff.signal.aborted) throw error;
          // Online/visible wakes may expedite our backoff, but never a server's
          // Retry-After floor or either automatic connection budget.
          delay = Math.max(0, serverNotBefore - now());
        } finally { backoff = null; }
      } while (delay > 0);
    }
    throw abortError();
  } finally { unsubscribe(); }
}
