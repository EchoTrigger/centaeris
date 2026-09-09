/* Loaded before the app stylesheet/React to prevent a mismatched first paint. */
(() => {
  const key = "centaeris:theme:v1";
  const root = document.documentElement;
  const system = matchMedia("(prefers-color-scheme: dark)");
  const normalize = (value) => value === "dark" || value === "light" ? value : "system";
  let saved;
  try { saved = localStorage.getItem(key); } catch { /* Storage may be unavailable. */ }
  root.dataset.themePreference = normalize(saved);
  function apply() {
    const preference = normalize(root.dataset.themePreference);
    const resolved = preference === "system" ? (system.matches ? "dark" : "light") : preference;
    root.dataset.themePreference = preference;
    root.dataset.theme = resolved;
    root.style.colorScheme = resolved;
    let meta = document.querySelector('meta[name="theme-color"]');
    if (!meta) { meta = document.createElement("meta"); meta.name = "theme-color"; document.head.append(meta); }
    meta.content = resolved === "dark" ? "#242424" : "#f5f5f4";
    root.style.backgroundColor = resolved === "dark" ? "#191919" : "#ffffff";
    dispatchEvent(new Event("centaeris-theme-applied"));
  }
  apply();
  system.addEventListener("change", apply);
  addEventListener("centaeris-theme-change", apply);
  addEventListener("storage", (event) => {
    if (event.key !== key && event.key !== null) return;
    root.dataset.themePreference = normalize(event.newValue);
    apply();
  });
})();
