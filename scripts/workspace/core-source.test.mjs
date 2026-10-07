import assert from 'node:assert/strict';
import { resolve } from 'node:path';
import test from 'node:test';
import { coreSourceFromMetadata, CORE_PACKAGES, resolveCoreSource, workspaceRoot } from './core-source.mjs';
const root = resolve('monorepo with spaces');
function metadata() {
  return { workspace_root: root, packages: CORE_PACKAGES.map(([name, folder]) => ({ name, source: null,
    manifest_path: resolve(root, 'packages', folder, 'Cargo.toml') })) };
}
test('all four owned Core packages resolve together, including roots with spaces', () => {
  assert.equal(coreSourceFromMetadata(metadata(), root), root);
});
test('missing, duplicate, external, mixed and Git package sources fail closed', () => {
  for (const mutate of [m => m.packages.pop(), m => m.packages.push(m.packages[0]),
    m => { m.packages[0].source = 'git+https://github.com/EchoTrigger/centaeris'; },
    m => { m.packages[1].manifest_path = resolve('outside/packages/model-catalog/Cargo.toml'); },
    m => { m.workspace_root = resolve('outside'); }]) {
    const value = metadata(); mutate(value);
    assert.throws(() => coreSourceFromMetadata(value, root));
  }
});
test('source discovery invokes the locked monorepo graph', () => {
  let calls = 0;
  const value = metadata(); value.workspace_root = workspaceRoot;
  value.packages = CORE_PACKAGES.map(([name, folder]) => ({ name, source: null,
    manifest_path: resolve(workspaceRoot, 'packages', folder, 'Cargo.toml') }));
  assert.equal(resolveCoreSource({run(command, args, options) {
    calls++; assert.equal(command, 'cargo');
    assert.deepEqual(args, ['metadata', '--locked', '--format-version=1']);
    assert.equal(options.cwd, workspaceRoot); return JSON.stringify(value);
  }}), resolve(workspaceRoot));
  assert.equal(calls, 1);
});
