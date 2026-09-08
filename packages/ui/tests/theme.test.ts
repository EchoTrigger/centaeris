import { readFileSync } from "node:fs";
import vm from "node:vm";
import { expect, test } from "vitest";

function boot(saved: string | null, dark: boolean, blocked = false) {
  const listeners = new Map<string, (event?: unknown) => void>();
  const dataset: Record<string, string> = {};
  const media = { matches: dark, addEventListener: (_: string, fn: () => void) => listeners.set("system", fn) };
  const scope = { document: { documentElement: { dataset, style: {} } }, matchMedia: () => media,
    localStorage: { getItem: () => { if (blocked) throw Error("blocked"); return saved; } },
    addEventListener: (name: string, fn: (event?: unknown) => void) => listeners.set(name, fn) };
  vm.runInNewContext(readFileSync(new URL("../public/theme-init.js", import.meta.url), "utf8"), scope);
  return { dataset, media, listeners, root: scope.document.documentElement };
}
test("theme follows the system by default and responds to OS changes", () => {
  const app = boot(null, true);
  expect(app.dataset).toMatchObject({ theme: "dark", themePreference: "system" });
  app.media.matches = false; app.listeners.get("system")?.();
  expect(app.dataset.theme).toBe("light");
});
test("explicit theme wins over system and changes from other windows are applied", () => {
  const app = boot("light", true);
  app.listeners.get("system")?.(); expect(app.dataset.theme).toBe("light");
  app.listeners.get("storage")?.({ key: "centaeris:theme:v1", newValue: "dark" });
  expect(app.dataset.theme).toBe("dark");
  app.listeners.get("storage")?.({ key: "centaeris:theme:v1", newValue: null });
  expect(app.dataset.themePreference).toBe("system");
});
test("unavailable storage and invalid preferences retain a working system default", () => {
  expect(boot("invalid", true).dataset.theme).toBe("dark");
  expect(boot(null, false, true).dataset.theme).toBe("light");
});
