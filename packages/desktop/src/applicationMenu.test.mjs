import { test } from "node:test";
import assert from "node:assert/strict";
import { applicationMenuTemplate, applicationMenuPosition, applyDesktopTheme } from "./applicationMenu.mjs";
test("File opens a folder through an application action and Edit uses native roles", () => {
  const actions = [];
  const menu = applicationMenuTemplate("File", action => actions.push(action));
  const open = menu.find(item => item.label === "Open Folder…");
  assert.equal(open.accelerator, "CmdOrCtrl+O"); open.click();
  assert.deepEqual(actions, ["open-folder"]);
  assert.ok(applicationMenuTemplate("Edit", () => {}).some(item => item.role === "paste"));
  assert.throws(() => applicationMenuTemplate("Unknown", () => {}));
});

test("application menus anchor below their button in screen coordinates, including zoom", () => {
  const owner = { getContentBounds: () => ({ x: 100, y: 50, width: 1200, height: 800 }), webContents: { getZoomFactor: () => 1.25 } };
  assert.deepEqual(applicationMenuPosition(owner, { x: 80, y: 36 }), { x: 200, y: 95 });
  assert.throws(() => applicationMenuPosition(owner, { x: NaN, y: 36 }));
});

test("native menus follow the chosen theme and retain System tracking", () => {
 const theme = {};
 for (const preference of ["dark", "light", "system"]) { applyDesktopTheme(theme, {preference}); assert.equal(theme.themeSource, preference); }
 assert.throws(() => applyDesktopTheme(theme, {preference:"dark", extra:true}));
});
