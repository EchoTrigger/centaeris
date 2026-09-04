import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { rmSync, writeFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { test } from "vitest";

const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), "../../..");
const biomeEntry = resolve(repoRoot, "node_modules/@biomejs/biome/bin/biome");

function runFixture(source) {
  const fixturePath = resolve(repoRoot, "packages/ui/src/.lint-contract-test.tsx");
  writeFileSync(fixturePath, source);
  try {
    return spawnSync(process.execPath, [biomeEntry, "lint", fixturePath], {
      cwd: repoRoot,
      encoding: "utf8",
    });
  } finally {
    rmSync(fixturePath, { force: true });
  }
}

test("missing React dependencies are reported without blocking the gate", () => {
  const result = runFixture(`
    import { useEffect } from "react";
    export function LintContract({ value }) {
      useEffect(() => console.log(value), []);
      return null;
    }
  `);
  const output = `${result.stdout}${result.stderr}`;
  assert.equal(result.status, 0);
  assert.match(output, /lint\/correctness\/useExhaustiveDependencies/);
});

test("conditional React hooks block the gate", () => {
  const result = runFixture(`
    import { useEffect } from "react";
    export function LintContract({ enabled }) {
      if (enabled) useEffect(() => {}, []);
      return null;
    }
  `);
  const output = `${result.stdout}${result.stderr}`;
  assert.notEqual(result.status, 0);
  assert.match(output, /lint\/correctness\/useHookAtTopLevel/);
});
