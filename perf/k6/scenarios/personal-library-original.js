// Two synthetic Runs against an externally bounded real-model relay. No provider keys here.
import { check } from 'k6';
import { Rate, Counter } from 'k6/metrics';
import crypto from 'k6/crypto';
import { configuredTarget, login, startRun, waitForTerminalObserved, verifyPublishedArtifact } from './common.js';

const expected = {
  displayName: __ENV.PERF_LIBRARY_DISPLAY_NAME,
  inputRef: __ENV.PERF_LIBRARY_INPUT_REF,
  sourceVersion: __ENV.PERF_LIBRARY_SOURCE_VERSION,
  sha256: __ENV.PERF_LIBRARY_SHA256,
  sizeBytes: Number(__ENV.PERF_LIBRARY_SIZE_BYTES),
};
const csvEntry = __ENV.PERF_LIBRARY_CSV_ENTRY || 'numbers.csv';
const csvColumn = __ENV.PERF_LIBRARY_CSV_COLUMN || 'value';
if (typeof expected.displayName !== 'string' || !expected.displayName.trim() || expected.displayName.length > 255
    || typeof expected.inputRef !== 'string' || !expected.inputRef.startsWith('library:') || expected.inputRef.length > 1024
    || typeof expected.sourceVersion !== 'string' || !/^(0|[1-9][0-9]*)$/.test(expected.sourceVersion)
    || typeof expected.sha256 !== 'string' || !/^sha256:[a-f0-9]{64}$/.test(expected.sha256)
    || !Number.isSafeInteger(expected.sizeBytes) || expected.sizeBytes < 1 || expected.sizeBytes > 64 * 1024 * 1024
    || !/^[A-Za-z0-9][A-Za-z0-9_./-]{0,127}\.csv$/.test(csvEntry) || csvEntry.split('/').includes('..')
    || !/^[A-Za-z_][A-Za-z0-9_]{0,63}$/.test(csvColumn)) {
  throw new Error('explicit synthetic Library expectation required');
}
const firstMarker = 'LIBRARY_ORIGINAL_RUN1_OK';
const reuseMarker = 'LIBRARY_ORIGINAL_REUSE_OK';
const failures = new Rate('personal_library_original_failed');
const verified = new Counter('personal_library_original_runs_verified');
export const options = {
  scenarios: { acceptance: { executor: 'per-vu-iterations', vus: 1, iterations: 1, maxDuration: '8m', gracefulStop: '5s' } },
  thresholds: { personal_library_original_failed: ['rate<0.01'], personal_library_original_runs_verified: ['count==2'], checks: ['rate==1'] },
};

function requireFact(value) { if (!value) throw new Error('personal Library evidence mismatch'); }
function decoded(result) {
  requireFact(typeof result?.modelContent === 'string' && result.outputComplete === true);
  try { return JSON.parse(result.modelContent); }
  catch (_) { throw new Error('material result JSON required'); }
}
function shellQuote(value) { return "'" + value.replace(/'/g, "'\\''") + "'"; }
function pythonCommand(path, reuse) {
  return `python3 - ${shellQuote(path)} <<'PY'\n`
    + 'import csv, io, sys, zipfile\nfrom pathlib import Path\n'
    + `with zipfile.ZipFile(sys.argv[1]) as archive:\n    with archive.open(${JSON.stringify(csvEntry)}) as source:\n`
    + `        total = sum(int(row[${JSON.stringify(csvColumn)}]) for row in csv.DictReader(io.TextIOWrapper(source, encoding="utf-8")))\n`
    + 'assert total == 42\n'
    + (reuse ? 'assert Path("/mnt/data/library-total.txt").read_bytes() == b"42\\n"\n'
      : 'Path("/mnt/data/library-total.txt").write_bytes(b"42\\n")\n')
    + `print(${JSON.stringify(reuse ? reuseMarker : firstMarker)})\nPY`;
}
function firstPrompt() {
  return `Synthetic personal Library acceptance. No files are attached. Discover my Library ZIP with exact displayName ${JSON.stringify(expected.displayName)} using list_materials scope=library and query matching that name. `
    + 'Use its returned input_ref with read_material delivery=execution_file exactly once. Do not read its body as content. '
    + 'Use the real returned execution path as ZIP_PATH in the following exact bash command (replace only ZIP_PATH with that path, preserving shell quoting). '
    + "Preserve the standalone '-' immediately after python3: it executes the script from stdin; the ZIP path is only its argument. "
    + 'Additional skill read calls are permitted. Do not fabricate paths or contents, create a replacement ZIP, or run other bash commands.\n'
    + pythonCommand('ZIP_PATH', false)
    + `\nAfter its successful output, publish_artifact path=/mnt/data/library-total.txt, then reply exactly ${firstMarker}.`;
}
function secondPrompt(path) {
  return 'Continue in this same Session. Reuse the already delivered original ZIP at the same path. '
    + 'Do not call list_materials, search_materials, read_material or read_material_result; do not publish again. '
    + 'Additional skill read calls are permitted. Run this exact bash command once, then reply exactly ' + reuseMarker + '.\n'
    + pythonCommand(path, true);
}

// Verify all committed calls/results, not just the terminal or assistant claim.
function paired(result, admitted) {
  requireFact(result.status === 'completed' && Array.isArray(result.events));
  const calls = new Map(), results = new Map(), ids = new Set();
  let sequence = 0, terminal = false, finalText = null, userMessages = 0;
  for (const event of result.events) {
    requireFact(event.sessionId === admitted.sessionId && event.agentRunId === admitted.agentRunId
      && Number.isInteger(event.sequence) && event.sequence > sequence && !terminal && !ids.has(event.eventId));
    ids.add(event.eventId); sequence = event.sequence;
    const p = event.payload;
    if (event.type === 'user_message') {
      requireFact(Array.isArray(p?.attachments) && p.attachments.length === 0); userMessages++;
    } else if (event.type === 'tool_call') {
      requireFact(typeof p?.callId === 'string' && !calls.has(p.callId) && typeof p.toolName === 'string');
      calls.set(p.callId, { ...p, sequence });
    } else if (event.type === 'tool_result') {
      requireFact(calls.has(p?.callId) && calls.get(p.callId).toolName === p.toolName && !results.has(p.callId)
        && ['successWithOutput', 'successNoOutput'].includes(p.resultState));
      results.set(p.callId, { ...p, sequence });
    } else if (event.type === 'assistant_message' && p?.status === 'done') finalText = p.modelMarkdown;
    else if (['agent_run_completed', 'agent_run_failed', 'agent_run_interrupted'].includes(event.type)) {
      requireFact(event.type === 'agent_run_completed'); terminal = true;
    }
  }
  requireFact(terminal && userMessages === 1 && calls.size === results.size);
  return { calls: [...calls.values()], results, finalText };
}
function verifyBash(proof, path, reuse) {
  const bash = proof.calls.filter(call => call.toolName === 'bash');
  requireFact(bash.length === 1 && bash[0].normalizedInput?.command === pythonCommand(path, reuse));
  const result = proof.results.get(bash[0].callId);
  requireFact(result.outputComplete === true && typeof result.modelContent === 'string'
    && result.modelContent.includes(reuse ? reuseMarker : firstMarker));
  return bash[0];
}
function verifyFirst(result, admitted) {
  const proof = paired(result, admitted);
  requireFact(proof.finalText?.trim() === firstMarker
    && proof.calls.every(call => ['read', 'list_materials', 'read_material', 'bash', 'publish_artifact'].includes(call.toolName)));
  const fetches = proof.calls.filter(call => call.toolName === 'read_material');
  requireFact(fetches.length === 1);
  const fetch = fetches[0], args = fetch.normalizedInput;
  requireFact(args?.delivery === 'execution_file' && args.input_ref === expected.inputRef);
  const original = decoded(proof.results.get(fetch.callId));
  requireFact(original.kind === 'execution_file' && original.delivery === 'execution_file'
    && original.inputRef === expected.inputRef && original.displayName === expected.displayName
    && original.contentType === 'application/zip' && original.sourceVersion === expected.sourceVersion
    && original.sha256 === expected.sha256 && original.sizeBytes === expected.sizeBytes
    && typeof original.path === 'string' && original.path.startsWith('/') && !original.path.includes('\0')
    && !original.path.split('/').includes('..') && !Object.prototype.hasOwnProperty.call(original, 'receiptId')
    && !Object.prototype.hasOwnProperty.call(original, 'citationIds'));
  const discovered = proof.calls.some(call => {
    if (call.toolName !== 'list_materials' || !['library', 'all'].includes(call.normalizedInput?.scope)) return false;
    const response = proof.results.get(call.callId);
    return response.sequence < fetch.sequence && decoded(response).items?.some(item =>
      item.inputRef === expected.inputRef && item.displayName === expected.displayName && item.sources?.includes('library'));
  });
  requireFact(discovered);
  const bash = verifyBash(proof, original.path, false);
  requireFact(proof.results.get(fetch.callId).sequence < bash.sequence);
  const publishes = proof.calls.filter(call => call.toolName === 'publish_artifact');
  requireFact(publishes.length === 1 && proof.results.get(bash.callId).sequence < publishes[0].sequence);
  return { path: original.path, toolCount: proof.calls.length };
}
function verifyReuse(result, admitted, path) {
  const proof = paired(result, admitted);
  requireFact(proof.finalText?.trim() === reuseMarker
    && proof.calls.every(call => ['read', 'bash'].includes(call.toolName))
    && !result.events.some(event => event.type === 'artifact_published'));
  verifyBash(proof, path, true);
  return { toolCount: proof.calls.length };
}
function report(phase, runIndex, admitted, extra = {}) {
  console.log('PERSONAL_LIBRARY_ORIGINAL ' + JSON.stringify({ schema: 'workspace.perf.personalLibraryOriginal.v1', phase, runIndex,
    sessionId: admitted?.sessionId, agentRunId: admitted?.agentRunId, ...extra }));
}

export default function () {
  let admitted, runIndex = 1, stage = 'login';
  try {
    const { workspaceId, modelId } = configuredTarget('tools');
    const { store, csrf } = login();
    stage = 'firstRun';
    admitted = startRun(store, csrf, workspaceId, modelId, firstPrompt());
    report('accepted', runIndex, admitted);
    const first = waitForTerminalObserved(store, csrf, admitted.sessionId, admitted.agentRunId, 180);
    const proof = verifyFirst(first, admitted);
    verifyPublishedArtifact(store, first, admitted.sessionId, admitted.agentRunId,
      { filename: 'library-total.txt', sizeBytes: 3, sha256: 'sha256:' + crypto.sha256('42\n', 'hex') });
    verified.add(1); report('verified', runIndex, admitted, { toolCount: proof.toolCount, passed: true, artifactVerified: true, originalVerified: true });
    const sessionId = admitted.sessionId;
    runIndex = 2; stage = 'secondRun';
    admitted = startRun(store, csrf, workspaceId, modelId, secondPrompt(proof.path), { sessionId });
    report('accepted', runIndex, admitted);
    requireFact(admitted.sessionId === sessionId);
    const second = waitForTerminalObserved(store, csrf, admitted.sessionId, admitted.agentRunId, 180);
    const reused = verifyReuse(second, admitted, proof.path);
    verified.add(1); failures.add(0);
    check(true, { 'personal Library original delivery and same-Session reuse verified': value => value });
    report('verified', runIndex, admitted, { toolCount: reused.toolCount, passed: true, sameSession: true, samePath: true, noRefetch: true });
  } catch (_) {
    failures.add(1); check(false, { 'personal Library original delivery and same-Session reuse verified': value => value });
    report('failed', runIndex, admitted, { passed: false, stage });
    throw new Error('personal Library original acceptance failed');
  }
}
