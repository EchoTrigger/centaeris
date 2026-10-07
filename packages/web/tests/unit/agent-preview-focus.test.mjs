import assert from "node:assert/strict";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { environment, renderer, subjectLoader } from "./componentHarness.mjs";

// Focus/effect collaborators for the real panel. No DOM paint,
// tab-order or browser layout is claimed by these component characterizations.
class Element {
  constructor(children = []) { this.children = children; this.isConnected = true; this.inert = false; this.tabIndex = 0; this.listeners = new Map(); this.focusCount = 0; this.replace(children); }
  replace(children) { for (const child of this.children) child.isConnected = false; this.children = children; for (const child of children) { child.parentElement = this; child.isConnected = true; } }
  contains(node) { return node === this || this.children.some(child => child.contains(node)); }
  closest() { return this.inert ? this : this.parentElement?.closest() || null; }
  getClientRects() { return [{}]; }
  getAttribute() { return null; }
  querySelector(selector) { return selector === "button" ? this.children[0] : null; }
  querySelectorAll() { return this.children; }
  focus() { this.focusCount++; if (!this.closest()) document.activeElement = this; }
  addEventListener(name, callback) { this.listeners.set(name, callback); }
  removeEventListener(name) { this.listeners.delete(name); }
}
const harnessUrl = new URL("./componentHarness.mjs", import.meta.url).href;
const dataUrl = code => `data:text/javascript;base64,${Buffer.from(code).toString("base64")}`;
const loader = subjectLoader({ realModal: true, extraOverrides: [["react", dataUrl(`export * from ${JSON.stringify(import.meta.resolve("react"))}; export {useState,useRef,useMemo,useCallback,useEffect} from ${JSON.stringify(harnessUrl)}; export const useSyncExternalStore=(_subscribe,getSnapshot)=>getSnapshot();`)]] });
const { AgentPreviewPanel } = await import(await loader(fileURLToPath(new URL("../../src/agent-chat/AgentPreviewPanel.tsx", import.meta.url))));

function dom(context, narrow) {
  const opener = new Element(); const background = new Element([opener]); const layer = new Element(); const body = new Element([background, layer]);
  const frames = new Map(); let sequence = 0; let panel;
  const request = callback => { frames.set(++sequence, callback); return sequence; };
  const cancel = id => frames.delete(id);
  const replaceGlobal = (name, value) => {
    const previous = Object.getOwnPropertyDescriptor(globalThis, name);
    Object.defineProperty(globalThis, name, { configurable: true, writable: true, value });
    context.after(() => { if (previous) Object.defineProperty(globalThis, name, previous); else delete globalThis[name]; });
  };
  replaceGlobal("HTMLElement", Element);
  replaceGlobal("document", { body, activeElement: opener, querySelector: () => panel?.isConnected ? panel : null });
  replaceGlobal("requestAnimationFrame", request);
  replaceGlobal("cancelAnimationFrame", cancel);
  replaceGlobal("window", { requestAnimationFrame: request, cancelAnimationFrame: cancel, matchMedia: () => ({ matches: narrow }) });
  return { opener, layer, body, setPanel(value) { panel = value; }, frames() { for (const [id, callback] of frames) { frames.delete(id); callback(); } } };
}

for (const narrow of [false, true]) for (const entry of ["List", "bubble"]) test(`${narrow ? "narrow" : "desktop"} pane returns to the original ${entry} entry after overview, Session and file views`, context => {
  const browser = dom(context, narrow); let closed = false;
  const props = { label: "Preview", focusKey: "overview", onClose: () => { closed = true; }, children: null };
  const subject = renderer(AgentPreviewPanel, props, environment());
  const root = new Element([new Element(), new Element()]); root.parentElement = browser.layer; browser.layer.children = [root]; browser.setPanel(root);
  const render = () => { const tree = subject.render(); tree.props.ref.current = root; subject.flush(); browser.frames(); return tree; };
  try {
    render(); assert.ok(root.contains(document.activeElement));
    for (const view of ["session", "file", "session", "overview", "session"]) {
      root.replace([new Element(), new Element()]); document.activeElement = browser.body;
      props.focusKey = view; render();
      assert.ok(root.contains(document.activeElement), "a view change focuses the new panel contents");
      assert.equal(browser.opener.focusCount, 0, "view changes must not return to the external opener");
    }
    const escape = { key: "Escape", preventDefault() {}, stopPropagation() {} };
    subject.tree.props.onKeyDown(escape);
    assert.equal(closed, true);
    root.isConnected = false; for (const child of root.children) child.isConnected = false;
    document.activeElement = browser.body; subject.unmount(); browser.frames();
    assert.equal(document.activeElement, browser.opener);
  } finally { if (root.isConnected) subject.unmount(); }
});

for (const narrow of [false, true]) test(`${narrow ? "narrow" : "desktop"} preview keeps the conversation interactive and restores the latest external entry`, context => {
  const browser = dom(context, narrow);
  const nextOpener = new Element(); browser.opener.parentElement.children.push(nextOpener); nextOpener.parentElement = browser.opener.parentElement;
  const props = { label: "Preview", focusKey: "session-a", onClose() {}, children: null };
  const subject = renderer(AgentPreviewPanel, props, environment());
  const root = new Element([new Element()]); root.parentElement = browser.layer; browser.layer.children = [root]; browser.setPanel(root);
  const render = () => { const tree = subject.render(); tree.props.ref.current = root; subject.flush(); browser.frames(); return tree; };
  try {
    const tree = render();
    assert.equal(tree.props.role, "complementary");
    assert.equal(tree.props["aria-modal"], undefined);
    assert.equal(browser.opener.parentElement.inert, false);
    nextOpener.focus(); assert.equal(document.activeElement, nextOpener, "chat remains focusable while preview is open");
    props.focusKey = "session-b"; render();
    assert.ok(root.contains(document.activeElement));
    root.isConnected = false; for (const child of root.children) child.isConnected = false;
    document.activeElement = browser.body; subject.unmount(); browser.frames();
    assert.equal(document.activeElement, nextOpener);
  } finally { if (root.isConnected) subject.unmount(); }
});

test("Agent departure never focuses the removed entry", context => {
  const browser = dom(context, false);
  const subject = renderer(AgentPreviewPanel, { label: "Preview", focusKey: "overview", onClose() {}, children: null }, environment());
  const root = new Element([new Element()]); root.parentElement = browser.layer; browser.layer.children = [root]; browser.setPanel(root);
  const tree = subject.render(); tree.props.ref.current = root; subject.flush(); browser.frames();
  browser.opener.isConnected = false; root.isConnected = false; document.activeElement = browser.body;
  subject.unmount(); browser.frames();
  assert.equal(browser.opener.focusCount, 0);
  assert.equal(document.activeElement, browser.body);
});
