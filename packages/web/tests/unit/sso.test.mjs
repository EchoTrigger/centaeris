import assert from "node:assert/strict";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { renderToStaticMarkup } from "react-dom/server";
import { ApiError, deferred, environment, nodes, renderer, subjectLoader } from "./componentHarness.mjs";

const load = subjectLoader();
const { SsoFeedback, SsoLoginOptions, ExternalIdentitySettings } = await import(await load(fileURLToPath(new URL("../../src/components/ExternalLogin.jsx", import.meta.url))));
const provider = { id: "test-oauth", displayName: "Test OAuth", enabled: true, identity: null };
const buttons = subject => nodes(subject.tree, node => node.type === "button");
const password = subject => nodes(subject.tree, node => node.type === "input" && node.props.type === "password")[0];
const form = subject => nodes(subject.tree, node => node.type === "form")[0];
const submit = subject => form(subject).props.onSubmit({ preventDefault() {} });

test("disabled external login adds no login controls", async () => {
  const subject = renderer(SsoLoginOptions, {}, environment({ request: async () => ({ providers: [] }) }));
  await subject.settle();
  assert.equal(subject.tree, null);
  subject.unmount();
});

test("provider login posts a local return target and follows the issued authorization URL", async () => {
  const writes = [], redirects = [];
  const env = environment({ request: async (_url, options) => {
    if (!options?.method) return { providers: [provider] };
    writes.push(JSON.parse(options.body));
    return { authorizationUrl: "https://identity.example/authorize?state=opaque" };
  } });
  const subject = renderer(SsoLoginOptions, { returnTo: "/workspaces", redirect: url => redirects.push(url) }, env);
  await subject.settle();
  assert.ok(buttons(subject).some(node => String(node.props.children).includes("sso.signInWith")));
  await buttons(subject)[0].props.onClick();
  assert.deepEqual(writes, [{ returnTo: "/workspaces" }]);
  assert.deepEqual(redirects, ["https://identity.example/authorize?state=opaque"]);
  subject.unmount();
});

test("binding verifies the current password before leaving the current page", async () => {
  const writes = [], redirects = [], pending = deferred();
  const env = environment({ request: async (url, options) => {
    if (!options?.method) return { providers: [provider], canVerifyPassword: true };
    writes.push({ url, body: JSON.parse(options.body) });
    return pending.promise;
  } });
  const subject = renderer(ExternalIdentitySettings, { returnTo: "/settings/security", redirect: url => redirects.push(url) }, env);
  await subject.settle();
  buttons(subject).find(node => node.props.children === "sso.link").props.onClick();
  await subject.settle();
  password(subject).props.onChange({ target: { value: "synthetic-password" } });
  await subject.settle();
  const first = submit(subject), duplicate = submit(subject);
  await subject.settle();
  assert.equal(writes.length, 1);
  assert.deepEqual(writes[0], { url: "/api/account/external-identities/test-oauth/link", body: { currentPassword: "synthetic-password", returnTo: "/settings/security" } });
  assert.deepEqual(redirects, []);
  pending.resolve({ authorizationUrl: "https://identity.example/authorize" });
  await Promise.all([first, duplicate]);
  assert.deepEqual(redirects, ["https://identity.example/authorize"]);
  subject.unmount();
});

test("a rejected password stays on the binding form with an actionable error", async () => {
  const redirects = [];
  const env = environment({ request: async (_url, options) => {
    if (!options?.method) return { providers: [provider], canVerifyPassword: true };
    throw new ApiError("account_current_password_invalid", 403);
  } });
  const subject = renderer(ExternalIdentitySettings, { redirect: value => redirects.push(value) }, env);
  await subject.settle();
  buttons(subject).find(node => node.props.children === "sso.link").props.onClick(); await subject.settle();
  password(subject).props.onChange({ target: { value: "wrong" } }); await subject.settle();
  await submit(subject); await subject.settle();
  assert.equal(redirects.length, 0);
  assert.ok(nodes(subject.tree, node => node.props.role === "alert").length);
  assert.ok(form(subject));
  subject.unmount();
});

test("linked identities show a masked label and unlink after confirmation", async () => {
  let linked = true;
  const writes = [];
  const env = environment({ location: { pathname: "/settings/security", search: "?ssoNotice=linked" }, request: async (url, options) => {
    if (!options?.method) return { providers: [{ ...provider, identity: linked ? { id: "identity", displayName: "Test Alice", subjectHint: "••••lice", isCurrentSession: false } : null }], canVerifyPassword: true };
    writes.push(url); linked = false; return { ok: true, signedOut: false };
  } });
  const subject = renderer(ExternalIdentitySettings, {}, env); await subject.settle();
  assert.ok(nodes(subject.tree, node => node.props.children === "••••lice").length);
  buttons(subject).find(node => node.props.children === "sso.unlink").props.onClick(); await subject.settle();
  assert.deepEqual(writes, []);
  password(subject).props.onChange({ target: { value: "synthetic-password" } }); await subject.settle();
  await submit(subject); await subject.settle();
  const html = renderToStaticMarkup(subject.tree);
  assert.ok(html.includes("sso.unlinkedNotice"));
  assert.ok(!html.includes("sso.linkedNotice"));
  assert.deepEqual(writes, ["/api/account/external-identities/identity/unlink"]);
  assert.ok(buttons(subject).some(node => node.props.children === "sso.link"));
  subject.unmount();
});

test("callback feedback never renders untrusted provider text", () => {
  const subject = renderer(SsoFeedback, { search: "?ssoError=private-token-from-provider" }, environment());
  const html = renderToStaticMarkup(subject.render());
  assert.ok(html.includes("sso.failed"));
  assert.ok(!html.includes("private-token"));
});

test("accounts without password verification cannot remove their only supported login method", async () => {
  const env = environment({ request: async () => ({ providers: [{ ...provider, identity: { id: "identity", displayName: "Test Alice" } }], canVerifyPassword: false }) });
  const subject = renderer(ExternalIdentitySettings, {}, env); await subject.settle();
  assert.equal(buttons(subject).find(node => node.props.children === "sso.unlink").props.disabled, true);
  subject.unmount();
});
