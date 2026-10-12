import { createRoot } from "react-dom/client";
import { createMemoryRouter, RouterProvider } from "react-router";
import { configureApi } from "../../src/api";
import { i18n } from "../../src/i18n";
import LibraryObjectRoute from "../../src/routes/LibraryObjectRoute";

await i18n.changeLanguage("en");
configureApi({ apiBaseUrl: location.origin } as Parameters<typeof configureApi>[0]);
const initial = { id: "browser-note", displayName: "A long note title to exercise wrapping of save controls", objectKind: "note", contentType: "text/markdown", status: "ready", contentGeneration: 1 };
const originalFetch = window.fetch;
window.fetch = async (input, options = {}) => {
  const url = new URL(typeof input === "string" ? input : input instanceof URL ? input.href : input.url, location.origin);
  if (!url.pathname.startsWith("/api/")) return originalFetch(input, options);
  const document = JSON.parse(sessionStorage.getItem("fixture.noteServer") || JSON.stringify({ object: initial, markdown: "# Independent body heading\n\nOriginal body" }));
  const calls = JSON.parse(sessionStorage.getItem("fixture.noteCalls") || "[]");
  calls.push({ path: url.pathname, method: options.method || "GET", body: options.body ? JSON.parse(String(options.body)) : null });
  sessionStorage.setItem("fixture.noteCalls", JSON.stringify(calls));
  const json = (value: unknown) => new Response(JSON.stringify(value), { headers: { "Content-Type": "application/json" } });
  if (url.pathname === "/api/csrf") return json({ csrfToken: "fixture" });
  if (url.pathname === "/api/library") return json({ objects: [] });
  if (url.pathname.includes("/other-note")) return json({ object: { ...initial, id: "other-note", displayName: "Other note" }, markdown: "Other body" });
  if (options.method === "PUT") {
    if (sessionStorage.getItem("fixture.offline") === "1") throw new TypeError("offline");
    const body = JSON.parse(String(options.body));
    const receipts = JSON.parse(sessionStorage.getItem("fixture.receipts") || "{}");
    if (receipts[body.saveId]) return json(receipts[body.saveId]);
    if (body.baseGeneration !== document.object.contentGeneration) return new Response(JSON.stringify({ error: "note_generation_conflict" }), { status: 409, headers: { "Content-Type": "application/json" } });
    document.markdown = body.markdown;
    document.object.displayName = body.displayName;
    document.object.contentGeneration++;
    sessionStorage.setItem("fixture.noteServer", JSON.stringify(document));
    const result = { object: document.object, saveId: body.saveId, appliedGeneration: document.object.contentGeneration };
    receipts[body.saveId] = result;
    sessionStorage.setItem("fixture.receipts", JSON.stringify(receipts));
    return json(result);
  }
  return json(url.pathname.endsWith("/note") ? document : { object: document.object });
};
const router = createMemoryRouter([{ id: "authenticated", loader: () => ({ user: { id: "note-browser-owner", email: "fixture", isStaff: false, isSuperuser: false } }), children: [
  { id: "workspace", path: "/w/fixture", loader: () => ({ workspace: { id: "fixture", name: "Notes", role: "member" }, agents: [], notes: [] }), children: [
    { path: "library/:libraryObjectId", Component: LibraryObjectRoute },
  ] },
] }], { initialEntries: ["/w/fixture/library/browser-note"] });
Object.assign(window, { fixtureNavigate: (path: string) => { void router.navigate(path); } });
createRoot(document.getElementById("fixture")!).render(<RouterProvider router={router} />);
