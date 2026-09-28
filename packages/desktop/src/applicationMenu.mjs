export function applicationMenuTemplate(menu, dispatch) {
  const action = (label, value, accelerator) => ({ label, accelerator, click: () => dispatch(value) });
  switch (menu) {
    case "File": return [action("New chat", "new-chat", "CmdOrCtrl+N"), action("Open Folder…", "open-folder", "CmdOrCtrl+O"), { type: "separator" }, action("Settings", "settings", "CmdOrCtrl+,")];
    case "Edit": return [{ role: "undo" }, { role: "redo" }, { type: "separator" }, { role: "cut" }, { role: "copy" }, { role: "paste" }, { role: "selectAll" }];
    case "View": return [action("Toggle sidebar", "toggle-sidebar", "CmdOrCtrl+B"), action("Toggle right panel", "toggle-panel"), { type: "separator" }, { role: "resetZoom" }, { role: "zoomIn" }, { role: "zoomOut" }, { role: "togglefullscreen" }];
    case "Help": return [action("About Centaeris", "about")];
    default: throw new Error("Unknown application menu");
  }
}

export function applyDesktopTheme(nativeTheme, payload) {
  if (!payload || Array.isArray(payload) || Object.keys(payload).join(",") !== "preference" || !["system", "light", "dark"].includes(payload.preference)) throw new Error("desktop_theme requires an exact theme preference");
  nativeTheme.themeSource = payload.preference;
  return { ok: true };
}

export function applicationMenuPosition(owner, anchor) {
  if (!anchor || Object.keys(anchor).sort().join(",") !== "x,y" || !Number.isFinite(anchor.x) || !Number.isFinite(anchor.y) || anchor.x < 0 || anchor.y < 0) throw new Error("desktop_menu requires an exact finite anchor");
  const bounds = owner.getContentBounds();
  const zoom = owner.webContents.getZoomFactor();
  return { x: Math.round(bounds.x + Math.min(anchor.x * zoom, bounds.width)), y: Math.round(bounds.y + Math.min(anchor.y * zoom, bounds.height)) };
}
