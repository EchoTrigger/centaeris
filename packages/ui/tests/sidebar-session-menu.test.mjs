import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { test } from "vitest";

const sidebarUrl = new URL("../src/components/Sidebar.tsx", import.meta.url);
const confirmDialogUrl = new URL("../src/components/ConfirmDialog.tsx", import.meta.url);
const stylesUrl = new URL("../src/index.css", import.meta.url);

test("renders session deletion confirmation inside the sidebar row", async () => {
  const [sidebarSource, stylesSource] = await Promise.all([
    readFile(sidebarUrl, "utf8"),
    readFile(stylesUrl, "utf8"),
  ]);

  assert.match(sidebarSource, /className="thinSessionActions"/);
  assert.match(sidebarSource, /onClick=\{\(\) => beginRename\(session\)\}/);
  assert.match(sidebarSource, /void confirmDelete\(session\.id\)/);
  assert.doesNotMatch(sidebarSource, /disabled=\{isRunning\}|运行中的会话不能删除/);
  assert.match(sidebarSource, /className="thinSessionRow thinSessionDeleteConfirm"/);
  assert.match(sidebarSource, /role="alertdialog"/);
  assert.match(sidebarSource, /<Trash2 aria-hidden="true" \/>Delete/);
  assert.match(sidebarSource, /autoFocus/);
  assert.match(sidebarSource, /event\.key === "Escape"/);
  assert.match(sidebarSource, /!pendingSessionIdRef\.current/);
  assert.match(sidebarSource, /event\.currentTarget\.contains\(event\.relatedTarget\)/);
  assert.match(stylesSource, /\.thinSessionDeleteConfirm\s*\{/);
  assert.match(stylesSource, /border-left: 3px solid var\(--error\)/);
  assert.match(stylesSource, /\.thinSessionDeleteConfirm button \{[\s\S]*height: 29px/);
});

test("keeps other destructive actions in the lightweight React dialog", async () => {
  const [confirmDialogSource, stylesSource] = await Promise.all([
    readFile(confirmDialogUrl, "utf8"),
    readFile(stylesUrl, "utf8"),
  ]);

  assert.match(confirmDialogSource, /<dialog/);
  assert.match(confirmDialogSource, /dialog\.showModal\(\)/);
  assert.match(confirmDialogSource, /role="alertdialog"/);
  assert.match(confirmDialogSource, /autoFocus onClick=\{onCancel\}>Cancel/);
  assert.match(confirmDialogSource, /onClick=\{onConfirm\}>Confirm/);
  assert.match(confirmDialogSource, /onCancel=\{\(event\) =>/);
  assert.match(confirmDialogSource, /getBoundingClientRect\(\)/);
  assert.match(confirmDialogSource, /onClick=\{cancelFromBackdrop\}/);
  assert.match(stylesSource, /\.confirmDialog::backdrop/);
  assert.match(stylesSource, /background: color-mix\(in srgb, var\(--on-surface\) 16%, transparent\)/);
});

test("workspace selection is removed from the sidebar", async () => {
  const source = await readFile(sidebarUrl, "utf8");
  assert.doesNotMatch(source, /aria-label="Current project"/);
  assert.match(source, /aria-label="Settings"/);
});
