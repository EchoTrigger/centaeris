import assert from "node:assert/strict";
import { test } from "vitest";
import { isWorkspaceDirectoryLikePath } from "../src/lib/workspacePathKind.ts";

test("classifies workspace directory-like paths", () => {
  assert.equal(isWorkspaceDirectoryLikePath("core/src/"), true);
  assert.equal(isWorkspaceDirectoryLikePath("ui/src/components"), true);
  assert.equal(isWorkspaceDirectoryLikePath("core/src/lib.rs"), false);
  assert.equal(isWorkspaceDirectoryLikePath("Cargo.toml"), false);
  assert.equal(isWorkspaceDirectoryLikePath(".gitignore"), false);
  assert.equal(isWorkspaceDirectoryLikePath("README.md"), false);
});
