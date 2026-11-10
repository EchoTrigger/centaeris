export function applicationMenuTemplate(menu, dispatch) {
  const action = (label, value, accelerator) => ({ label, accelerator, click: () => dispatch(value) });
  switch (menu) {
    case "File": return [action("New chat", "new-chat", "CmdOrCtrl+N"), action("Open Folder…", "open-folder", "CmdOrCtrl+O"), action("Settings", "settings", "CmdOrCtrl+,")];
    case "Edit": return [{ label: "Undo", role: "undo" }, { label: "Redo", role: "redo" }, { label: "Cut", role: "cut" }, { label: "Copy", role: "copy" }, { label: "Paste", role: "paste" }, { label: "Select All", role: "selectAll" }];
    case "View": return [action("Toggle sidebar", "toggle-sidebar", "CmdOrCtrl+B"), action("Toggle right panel", "toggle-panel"), { label: "Actual Size", role: "resetZoom" }, { label: "Zoom In", role: "zoomIn" }, { label: "Zoom Out", role: "zoomOut" }, { label: "Toggle Full Screen", role: "togglefullscreen" }];
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
  return { x: Math.round(Math.min(anchor.x * zoom, bounds.width)), y: Math.round(Math.min(anchor.y * zoom, bounds.height)) };
}
