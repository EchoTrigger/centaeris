import assert from "node:assert/strict";
import test from "node:test";
import { toLinuxPath, toWindowsPath } from "./wslPaths.mjs";

const distribution = "Ubuntu-24.04";

test("WSL workspace paths preserve spaces and Unicode in the selected distribution", () => {
  assert.equal(toLinuxPath("\\\\wsl.localhost\\Ubuntu-24.04\\home\\user\\项目 one", distribution), "/home/user/项目 one");
  assert.equal(toLinuxPath("\\\\wsl$\\Ubuntu-24.04\\home\\user\\repo", distribution), "/home/user/repo");
  assert.equal(toWindowsPath("/home/user/项目 one", distribution), "\\\\wsl.localhost\\Ubuntu-24.04\\home\\user\\项目 one");
});

test("workspace selection rejects Windows mounts and a different WSL distribution", () => {
  for (const value of ["D:\\Projects\\repo", "/mnt/d/Projects/repo", "\\\\wsl.localhost\\Debian\\home\\user\\repo"]) {
    assert.throws(() => toLinuxPath(value, distribution), /Linux filesystem|distribution/);
  }
});

test("Windows inputs can be imported without treating a drive as an execution workspace", () => {
  assert.equal(toLinuxPath("D:\\Images\\input '测试'.png", distribution, { importSource: true }), "/mnt/d/Images/input '测试'.png");
  assert.throws(() => toLinuxPath("relative/path", distribution), /absolute/);
  assert.throws(() => toLinuxPath("/home/user/a\0b", distribution), /invalid/);
  assert.throws(() => toWindowsPath("/home/user", "Ubuntu\\other"), /distribution/);
});

import { readFile } from "node:fs/promises";

test("WSL path mapping agrees with the shared Host contract", async () => {
  const cases = JSON.parse(await readFile(new URL("../../runtime/host/wsl-path-cases.json", import.meta.url), "utf8"));
  for (const entry of cases) {
    const map = () => toLinuxPath(entry.input, distribution, { importSource: entry.importSource ?? false });
    if (entry.error) assert.throws(map, entry.input);
    else assert.equal(map(), entry.linux, entry.input);
  }
});
