import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { execFileSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import test from "node:test";

const root = fileURLToPath(new URL("../", import.meta.url));
const config = readFileSync(new URL("../rust-toolchain.toml", import.meta.url), "utf8");
const channel = /^channel\s*=\s*"([^"]+)"/m.exec(config)?.[1];
assert.ok(channel, "toolchain channel must be pinned");

for (const name of ["ci", "performance"]) {
  test(`${name} honors the repository Rust toolchain`, () => {
    const workflow = readFileSync(new URL(`../.github/workflows/${name}.yml`, import.meta.url), "utf8");
    assert.doesNotMatch(workflow, /rustup\s+(?:override\s+set|toolchain\s+install)\s+\d/,
      "workflow must not override rust-toolchain.toml with a second version");
    assert.match(workflow, /rustup show active-toolchain/);
  });
}

test("the CI setup resolves the pinned toolchain in this checkout", () => {
  const selected = execFileSync("rustup", ["show", "active-toolchain"], { cwd: root, encoding: "utf8" });
  assert.ok(selected.startsWith(`${channel}-`), `expected ${channel}, got ${selected}`);
});
