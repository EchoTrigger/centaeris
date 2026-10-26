import { execFileSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
export function committedItems(events) {
  const producer = fileURLToPath(new URL('./session_stream_fixtures.py', import.meta.url));
  return JSON.parse(execFileSync(process.env.CENTAERIS_TEST_PYTHON || 'python', [producer], {
    input: JSON.stringify(events), encoding: 'utf8', timeout: 10000,
  }));
}
