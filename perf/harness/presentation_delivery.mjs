import { resolve } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';
import { fileURLToPath } from 'node:url';
import { createTranscriptViewStore } from '../../packages/web/src/chat/transcriptViewStore.ts';
import { WorkspacePresentationController } from '../../packages/web/src/chat/workspacePresentationController.ts';
import { streamWorkspacePresentation } from '../../packages/web/src/chat/workspacePresentationTransport.ts';

export async function observePresentation({ baseUrl, identity, cookie, reference, slowReadMs = 0 }) {
  const origin = new URL(baseUrl);
  if (!['http:', 'https:'].includes(origin.protocol)
      || !['127.0.0.1', '[::1]', 'localhost'].includes(origin.hostname)
      || origin.username || origin.password || origin.pathname !== '/' || origin.search || origin.hash) {
    throw new Error('presentation probe requires an isolated loopback API origin');
  }
  if (!/^sessionid=[A-Za-z0-9_-]+$/.test(cookie)
      || ![identity.workspaceId, identity.sessionId].every(value => /^[A-Za-z0-9_.:-]{1,160}$/.test(value))) {
    throw new Error('presentation probe requires an explicit fixture scope and session cookie');
  }
  const bytes = Number(reference.byteLength);
  if (!reference.refId || !reference.revision || !Number.isSafeInteger(bytes)
      || bytes < 1 || bytes > 16 * 1024 * 1024
      || !Number.isSafeInteger(slowReadMs) || slowReadMs < 0 || slowReadMs > 200) {
    throw new Error('invalid presentation fixture reference or slow-read interval');
  }
  const store = createTranscriptViewStore();
  const controller = new WorkspacePresentationController({ ...identity, store });

  async function untilCheckpoint(resumeCursor) {
    const stop = new AbortController();
    const signal = AbortSignal.any([stop.signal, AbortSignal.timeout(30_000)]);
    const start = performance.now();
    let firstRangeMs = null, checkpoint = null, checkpointMs = null, ranges = 0;
    try {
      await streamWorkspacePresentation({
        ...identity, signal, retryLimit: 0, onConnection() {},
        async request(path, { headers, signal: attemptSignal }) {
          if ((headers['Last-Event-ID'] ?? null) !== resumeCursor) {
            throw new Error('presentation viewer sent the wrong resume cursor');
          }
          return fetch(new URL(path, origin), {
            headers: { ...headers, Cookie: cookie }, signal: attemptSignal, redirect: 'error',
          });
        },
        controller: {
          get lastCursor() { return controller.lastCursor; },
          async acceptWithBackpressure(entry) {
            if (entry.item.kind === 'reset') throw new Error('presentation fixture reset during observation');
            await controller.acceptWithBackpressure(entry);
            if (entry.item.kind === 'content' && entry.item.range.refId === reference.refId) {
              firstRangeMs ??= performance.now() - start;
              ranges++;
              if (slowReadMs) await delay(slowReadMs, undefined, { signal });
            }
            if (entry.item.kind === 'checkpoint' && entry.cursor !== null && controller.lastCursor === entry.cursor) {
              checkpoint = entry.item;
              checkpointMs = performance.now() - start;
              stop.abort();
            }
          },
        },
      });
    } catch (error) {
      if (error.name !== 'AbortError' || !checkpoint || !stop.signal.aborted) throw error;
    } finally {
      stop.abort();
    }
    if (!checkpoint) throw new Error('presentation stream ended without an applied checkpoint');
    return { firstRangeMs, checkpointMs, ranges, checkpoint };
  }

  const initial = await untilCheckpoint(null);
  const content = store.getPresentationContentSnapshot(reference);
  if (!initial.ranges || !content?.complete || content.limited || Buffer.byteLength(content.content) !== bytes) {
    throw new Error('presentation fixture content was not delivered in full');
  }
  const resumed = await untilCheckpoint(controller.lastCursor);
  if (resumed.ranges || resumed.checkpoint.sourceHighWater !== initial.checkpoint.sourceHighWater
      || resumed.checkpoint.projectionGeneration !== initial.checkpoint.projectionGeneration) {
    throw new Error('presentation checkpoint did not resume the unchanged fixture');
  }
  return { firstRangeMs: initial.firstRangeMs, checkpointMs: initial.checkpointMs,
    resumeMs: resumed.checkpointMs, ranges: initial.ranges };
}

async function main() {
  const samples = Number(process.env.PERF_PRESENTATION_SAMPLES ?? 25);
  if (!Number.isSafeInteger(samples) || samples < 1 || samples > 100) {
    throw new Error('presentation sample count must be between 1 and 100');
  }
  const config = {
    baseUrl: process.env.API_BASE, cookie: process.env.PERF_PRESENTATION_COOKIE,
    identity: { workspaceId: process.env.PERF_WORKSPACE_ID, sessionId: process.env.PERF_PRESENTATION_SESSION_ID },
    reference: JSON.parse(process.env.PERF_PRESENTATION_REFERENCE ?? 'null'),
    slowReadMs: Number(process.env.PERF_PRESENTATION_SLOW_MS ?? 0),
  };
  const measurements = [];
  for (let round = -3; round < samples; round++) {
    const result = await observePresentation(config);
    if (round >= 0) measurements.push(result);
  }
  const summarize = key => {
    const values = measurements.map(sample => sample[key]).sort((a, b) => a - b);
    return { p50: values[Math.ceil(samples * 0.50) - 1], p95: values[Math.ceil(samples * 0.95) - 1] };
  };
  console.log(JSON.stringify({
    samples, warmups: 3, bytes: Number(config.reference.byteLength), slowReadMs: config.slowReadMs,
    firstRangeMs: summarize('firstRangeMs'), checkpointMs: summarize('checkpointMs'), resumeMs: summarize('resumeMs'),
  }));
}

if (process.argv[1] && fileURLToPath(import.meta.url) === resolve(process.argv[1])) await main();
