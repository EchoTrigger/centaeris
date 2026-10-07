import assert from 'node:assert/strict';
import { resolve } from 'node:path';
import test from 'node:test';
import { coreSourceFromMetadata, CORE_PACKAGES, CORE_URL } from './core-source.mjs';

const revision = 'a'.repeat(40);
const directory = resolve('unrelated directory/core checkout');
function metadata(source = `git+${CORE_URL}?rev=${revision}#${revision}`, root = directory) {
  return { packages: CORE_PACKAGES.map(([name, folder]) => ({ name, source,
    manifest_path: resolve(root, 'packages', folder, 'Cargo.toml') })) };
}
test('finds pinned Core without a sibling directory, including paths with spaces', () => {
  assert.equal(coreSourceFromMetadata(metadata(), revision), directory);
});
test('rejects missing, mixed, floating and incorrectly pinned Core dependencies', () => {
  for (const source of [null, `git+${CORE_URL}#${revision}`, `git+${CORE_URL}?rev=${'b'.repeat(40)}#${revision}`]) {
    assert.throws(() => coreSourceFromMetadata(metadata(source), revision));
  }
  const missing = metadata(); missing.packages.pop();
  assert.throws(() => coreSourceFromMetadata(missing, revision));
  const mixed = metadata(); mixed.packages[1].manifest_path = resolve('another source/packages/model-catalog/Cargo.toml');
  assert.throws(() => coreSourceFromMetadata(mixed, revision));
});
test('local overrides require an explicit matching path and all four patched packages', () => {
  assert.equal(coreSourceFromMetadata(metadata(null), revision, directory), directory);
  assert.throws(() => coreSourceFromMetadata(metadata(null), revision, resolve('wrong')));
  const mixed = metadata(null); mixed.packages[0].source = `git+${CORE_URL}?rev=${revision}#${revision}`;
  assert.throws(() => coreSourceFromMetadata(mixed, revision, directory));
});
