// Run from any directory with Node's existing TypeScript support:
// node --expose-gc --experimental-strip-types perf/harness/presentation_content_store.mjs BASELINE_REVISION
import { execFileSync } from 'node:child_process';
import { mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { performance } from 'node:perf_hooks';
import { fileURLToPath, pathToFileURL } from 'node:url';

const revision = process.argv[2];
if (!revision) throw new Error('provide the baseline Git revision');
const repository = fileURLToPath(new URL('../../', import.meta.url));
const currentUrl = new URL('../../packages/web/src/chat/presentationContentStore.ts', import.meta.url);
const contractUrl = new URL('../../packages/web/src/chat/transcriptContract.ts', import.meta.url);
const baseline = execFileSync('git', ['show', `${revision}:packages/web/src/chat/presentationContentStore.ts`], { cwd: repository, encoding: 'utf8' });
const directory = await mkdtemp(join(tmpdir(), 'centaeris-presentation-store-'));

try {
  const baselinePath = join(directory, 'baseline.mts');
  await writeFile(baselinePath, baseline.replace('./transcriptContract.ts', contractUrl.href));
  const modules = [await import(pathToFileURL(baselinePath).href), await import(currentUrl.href)];
  const identity = { sessionId: 'session', projectionGeneration: 'generation' };
  const ref = (id, bytes) => ({ refId: `session-event:${id}`, revision: '1', byteLength: String(bytes) });
  const page = (reference, start, content) => ({
    schema: 'transcript.content.range.v1', ...identity, projectionVersion: 'transcript.projection.v1', ...reference,
    startOffset: String(start), endOffset: String(start + content.length), content,
    hasMore: start + content.length < Number(reference.byteLength),
  });
  // Build the same ASCII inputs once, outside all timed sections. apply() still
  // performs the real contract validation, cloning, digest, and cache updates.
  const readyRefs = Array.from({ length: 10000 }, (_, i) => ref(i, 0)).concat(ref('unfinished', 1));
  const outputRef = ref('output', 16 * 1024 * 1024);
  const outputPages = Array.from({ length: 256 }, (_, i) => page(outputRef, i * 65536, 'x'.repeat(65536)));
  const evictionRefs = Array.from({ length: 4096 }, (_, i) => ref(i, 256));
  const evictionPages = evictionRefs.map(reference => page(reference, 0, 'y'.repeat(256)));
  const workloads = {
    ready: mod => {
      const store = mod.createPresentationContentStore();
      store.reserve(readyRefs);
      return () => {
        let complete = 0;
        for (let i = 0; i < 1000; i++) complete += store.ready() ? 1 : 0;
        if (complete) throw new Error('expected unfinished content');
      };
    },
    output: mod => () => {
      const store = mod.createPresentationContentStore();
      store.reserve([outputRef]);
      for (const range of outputPages) store.apply(range, identity);
      if (!store.ready() || store.bytes() !== 16777216) throw new Error('output mismatch');
    },
    eviction: mod => () => {
      const store = mod.createPresentationContentStore();
      store.reserve([...evictionRefs, outputRef]);
      for (const range of evictionPages) store.apply(range, identity);
      for (const range of outputPages) store.apply(range, identity);
      if (!store.ready() || store.bytes() !== 16777216 || !store.isEvicted(evictionRefs[4095])) throw new Error('eviction mismatch');
    },
  };
  const percentile = (values, p) => [...values].sort((a, b) => a - b)[Math.ceil(values.length * p) - 1];
  console.log(JSON.stringify({ baseline: revision, node: process.version, warmups: 3, samples: 25,
    memory: 'Point-in-time process heap/RSS changes; these are not peak allocation, retained cache size, or browser measurements.' }));
  for (const [workload, prepare] of Object.entries(workloads)) {
    const runs = modules.map(prepare);
    const samples = modules.map(() => ({ ms: [], heap: [], rss: [] }));
    for (let round = 0; round < 28; round++) {
      // Alternate order within each batch to reduce systematic order bias.
      for (const index of round % 2 ? [1, 0] : [0, 1]) {
        global.gc?.();
        const before = process.memoryUsage();
        const start = performance.now();
        runs[index]();
        const elapsed = performance.now() - start;
        const after = process.memoryUsage();
        if (round >= 3) {
          samples[index].ms.push(elapsed);
          samples[index].heap.push(after.heapUsed - before.heapUsed);
          samples[index].rss.push(after.rss - before.rss);
        }
      }
    }
    console.log(JSON.stringify({ workload, results: samples.map((sample, i) => ({
      version: i === 0 ? 'baseline' : 'candidate', p50ms: percentile(sample.ms, 0.5), p95ms: percentile(sample.ms, 0.95),
      heapUsedChangeP50MiB: percentile(sample.heap, 0.5) / 1048576, rssChangeP50MiB: percentile(sample.rss, 0.5) / 1048576,
    })) }));
  }
} finally {
  await rm(directory, { recursive: true });
}
