import assert from 'node:assert/strict';
import { once } from 'node:events';
import { readFileSync } from 'node:fs';
import { createServer } from 'node:http';
import test from 'node:test';
import { observePresentation } from '../harness/presentation_delivery.mjs';

const fixture = JSON.parse(readFileSync(new URL('../../tests/fixtures/workspace-presentation.json', import.meta.url)));
const range = fixture.frames.find(item => item.kind === 'content');
const { refId, revision, byteLength } = range.range;
const reference = { refId, revision, byteLength };
const initial = structuredClone(fixture.initialFrames);
initial.find(item => item.kind === 'snapshot').page.blocks[0].body.content = {
  inlineContent: null, sourceRef: reference,
};
const checkpoint = initial.pop();
const encode = item => 'data: ' + JSON.stringify(item) + '\n\n';
const applied = 'id: ' + fixture.initialCursor + '\n' + encode(checkpoint);

test('presentation measurement uses the browser transport and resumes the applied checkpoint', async t => {
  const requests = [];
  const server = createServer((request, response) => {
    requests.push({ path: request.url, cookie: request.headers.cookie, cursor: request.headers['last-event-id'] });
    response.setHeader('Content-Type', 'text/event-stream');
    response.end(requests.length === 1 ? initial.map(encode).join('') + encode(range) + applied : applied);
  });
  server.listen(0, '127.0.0.1');
  t.after(() => { server.closeAllConnections(); server.close(); });
  await once(server, 'listening');
  const sample = await observePresentation({
    baseUrl: 'http://127.0.0.1:' + server.address().port, identity: fixture.scope,
    cookie: 'sessionid=fixture', reference, slowReadMs: 1,
  });
  const path = '/api/workspaces/workspace/presentation/events?sessionId=session';
  assert.deepEqual(requests, [
    { path, cookie: 'sessionid=fixture', cursor: undefined },
    { path, cookie: 'sessionid=fixture', cursor: fixture.initialCursor },
  ]);
  assert.equal(sample.ranges, 1);
  assert.ok(sample.firstRangeMs >= 0 && sample.checkpointMs >= sample.firstRangeMs && sample.resumeMs >= 0);
});
