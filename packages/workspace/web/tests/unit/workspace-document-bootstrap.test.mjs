import { readFileSync } from "node:fs";
import assert from "node:assert/strict";
import test from "node:test";
import { JSDOM } from "jsdom";

const html = readFileSync(new URL("../../index.html", import.meta.url), "utf8");
const key = "centaeris:workspace-identity:v1";
const avatar = `/api/workspaces/workspace/avatar/${"a".repeat(64)}`;
function load(saved, path = "/w/workspace/app") {
  return new JSDOM(html, {
    url: `http://localhost${path}`, runScripts: "dangerously",
    beforeParse(window) { if (saved !== undefined) window.sessionStorage.setItem(key, JSON.stringify(saved)); },
  });
}

test("initial HTML restores the same Workspace identity before the app or network loads", () => {
  const dom = load({ schema: "workspace.identity.v1", workspaceId: "workspace", name: "FOSU DocCheck", avatarUrl: avatar });
  assert.equal(dom.window.document.title, "FOSU DocCheck");
  assert.equal(dom.window.document.querySelector('link[rel="icon"]').getAttribute("href"), avatar);
  assert.equal(dom.window.document.getElementById("root").childElementCount, 0);
  dom.window.close();
});

test("initial HTML safely restores the name initial when the avatar was removed", () => {
  const dom = load({ schema: "workspace.identity.v1", workspaceId: "workspace", name: "<&", avatarUrl: null });
  assert.equal(dom.window.document.title, "<&");
  const data = dom.window.document.querySelector('link[rel="icon"]').getAttribute("href");
  const svg = new dom.window.DOMParser().parseFromString(decodeURIComponent(data.split(",")[1]), "image/svg+xml");
  assert.equal(svg.querySelector("text").textContent, "<");
  dom.window.close();
});

test("unknown or invalid identities never show another Workspace or accept an external image", () => {
  for (const saved of [undefined,
    { schema: "workspace.identity.v1", workspaceId: "other", name: "Other", avatarUrl: null },
    { schema: "workspace.identity.v1", workspaceId: "workspace", name: "Wrong", avatarUrl: "https://example.test/avatar.png" },
  ]) {
    const dom = load(saved);
    assert.equal(dom.window.document.title, "");
    assert.ok(dom.window.document.querySelector('link[rel="icon"]').getAttribute("href").startsWith("data:"));
    dom.window.close();
  }
  const dom = load({ schema: "workspace.identity.v1", workspaceId: "workspace", name: "Private", avatarUrl: avatar }, "/login");
  assert.equal(dom.window.document.title, "Centaeris");
  assert.equal(dom.window.document.querySelector('link[rel="icon"]').getAttribute("href"), "/centaeris-mark.png");
  dom.window.close();
});
