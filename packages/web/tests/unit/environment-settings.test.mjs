import assert from "node:assert/strict";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { matchRoutes } from "react-router";
import { deferred, environment, nodes, renderer, subjectLoader } from "./componentHarness.mjs";
import { createJsxSubjectLoader } from "./jsxSubject.mjs";

const load = subjectLoader();
const { default: EnvironmentSettings } = await import(await load(fileURLToPath(new URL("../../src/routes/EnvironmentSettings.jsx", import.meta.url))));
const toggle = tree => nodes(tree, node => node.props.role === "switch")[0];

test("workspace environment settings URL resolves to the settings page", async () => {
  const module = source => `data:text/javascript;base64,${Buffer.from(source).toString("base64")}`;
  const overrides = new Map([
    ["react-router", module(`export * from ${JSON.stringify(import.meta.resolve("react-router"))}; export const createBrowserRouter=routes=>routes;`)],
    ["./api", module("export class ApiError extends Error {} export const apiJson=()=>{}, hasAuthenticationRequiredHandler=()=>false, isAuthenticationRequired=()=>false, requireWorkspaces=x=>x, setAuthenticationRequiredHandler=()=>{};")],
    ["./components/SessionExpiredDialog", module("export const SessionExpiredDialog=()=>null;")],
    ["./routes/PasswordResetRoute", module("export const ForgotPasswordRoute=()=>null, ResetPasswordRoute=()=>null;")],
  ]);
  for (const name of ["LibraryObjectRoute", "LibraryRoute", "LoginRoute", "InvitationActivationRoute", "SettingsRoute", "WorkspaceChooserRoute", "WorkspaceHomeRoute", "TrashSessionRoute"]) {
    overrides.set(`./routes/${name}`, module(`export default function ${name}(){return null}`));
  }
  const { createRouter } = await import(await createJsxSubjectLoader(overrides)(fileURLToPath(new URL("../../src/router.tsx", import.meta.url))));
  const matches = matchRoutes(createRouter(), "/w/workspace/settings/environment");
  assert.ok(matches, "settings page must be reachable");
  assert.equal(matches.at(-1).route.Component.name, "SettingsRoute");
});

test("network switch waits for saved policy and preserves value after rejected update", async () => {
  const save = deferred();
  const writes = [];
  const env = environment({ request: async (url, options) => {
    assert.equal(url, "/api/workspaces/workspace/environment-settings");
    if (options?.method === "PATCH") { writes.push(JSON.parse(options.body)); return save.promise; }
    return { network: "disabled" };
  } });
  const subject = renderer(EnvironmentSettings, { workspace: { id: "workspace" } }, env);
  await subject.settle();
  assert.equal(toggle(subject.tree).props.checked, false);
  const pending = toggle(subject.tree).props.onChange({ target: { checked: true } });
  await subject.settle();
  assert.equal(toggle(subject.tree).props.disabled, true);
  assert.equal(toggle(subject.tree).props.checked, false);
  assert.deepEqual(writes, [{ network: "publicInternet" }]);
  save.reject(new Error("unavailable")); await pending; await subject.settle();
  assert.equal(toggle(subject.tree).props.checked, false);
  assert.equal(toggle(subject.tree).props.disabled, false);
  assert.equal(nodes(subject.tree, node => node.props.role === "alert").length, 1);
  subject.unmount();
});

test("successful setting change shows saved policy and scope for new environments", async () => {
  const env = environment({ request: async (_url, options) => options?.method === "PATCH" ? JSON.parse(options.body) : { network: "disabled" } });
  const subject = renderer(EnvironmentSettings, { workspace: { id: "workspace" } }, env);
  await subject.settle();
  await toggle(subject.tree).props.onChange({ target: { checked: true } }); await subject.settle();
  assert.equal(toggle(subject.tree).props.checked, true);
  assert.equal(nodes(subject.tree, node => node.type === "small" && node.props.children === "environmentSettings.newEnvironmentsOnly").length, 1);
  assert.equal(nodes(subject.tree, node => node.props.role === "status").length, 1);
  subject.unmount();
});
