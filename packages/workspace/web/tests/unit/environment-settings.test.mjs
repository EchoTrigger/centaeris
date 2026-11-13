import assert from "node:assert/strict";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { matchRoutes } from "react-router";
import { ApiError, button, deferred, environment, nodes, renderer, subjectLoader } from "./componentHarness.mjs";
import { createJsxSubjectLoader } from "./jsxSubject.mjs";

const load = subjectLoader();
const { default: EnvironmentSettings } = await import(await load(fileURLToPath(new URL("../../src/routes/EnvironmentSettings.jsx", import.meta.url))));
const toggle = tree => nodes(tree, node => node.props.role === "switch")[0];
const resource = (tree, name) => nodes(tree, node => node.type === "input" && node.props["aria-label"] === `environmentSettings.${name}`)[0];
const saveButton = tree => nodes(tree, node => node.type === "button" && node.props.type === "submit")[0];
const submit = subject => nodes(subject.tree, node => node.type === "form")[0].props.onSubmit({ preventDefault() {} });
const alert = tree => nodes(tree, node => node.props.role === "alert");
const status = tree => nodes(tree, node => node.props.role === "status");
const GIB = 2 ** 30;
const policy = (changes = {}) => ({ network: "disabled", cpuMilli: 1000, memoryBytes: 2 * GIB, workspaceBytes: 5 * GIB, revision: 1, ...changes });
const editResources = async (subject, values) => {
  for (const [name, value] of Object.entries(values)) resource(subject.tree, name).props.onChange({ target: { value } });
  await subject.settle();
};

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

test("resource settings read cores and GiB and save the whole draft persistently", async () => {
  const save = deferred();
  const writes = [];
  let stored = policy();
  const env = environment({ request: async (url, options) => {
    assert.equal(url, "/api/workspaces/workspace/environment-settings");
    if (options?.method === "PATCH") {
      const payload = JSON.parse(options.body);
      writes.push(payload);
      await save.promise;
      stored = { ...stored, ...payload, revision: stored.revision + 1 };
    }
    return { ...stored };
  } });
  const subject = renderer(EnvironmentSettings, { workspace: { id: "workspace" } }, env);
  await subject.settle();
  assert.equal(resource(subject.tree, "cpu").props.value, "1");
  assert.equal(resource(subject.tree, "memory").props.value, "2");
  assert.equal(resource(subject.tree, "workspaceSpace").props.value, "5");
  assert.equal(status(subject.tree).length, 0);
  assert.equal(toggle(subject.tree).props.checked, false);
  await editResources(subject, { cpu: "2", memory: "4", workspaceSpace: "10" });
  toggle(subject.tree).props.onChange({ target: { checked: true } });
  await subject.settle();
  assert.equal(resource(subject.tree, "memory").props.value, "4", "network changes keep the unsaved resource draft");
  assert.deepEqual(writes, [], "the form saves network and resources together");
  const pending = submit(subject);
  await subject.settle();
  assert.equal(toggle(subject.tree).props.disabled, true);
  assert.equal(saveButton(subject.tree).props.disabled, true);
  for (const name of ["cpu", "memory", "workspaceSpace"]) assert.equal(resource(subject.tree, name).props.disabled, true);
  assert.deepEqual(writes, [{ network: "publicInternet", cpuMilli: 2000, memoryBytes: 4294967296, workspaceBytes: 10737418240 }]);
  assert.equal(status(subject.tree).length, 0, "a pending write is not a saved setting");
  save.resolve(); await pending; await subject.settle();
  assert.equal(toggle(subject.tree).props.checked, true);
  assert.equal(toggle(subject.tree).props.disabled, false);
  assert.equal(status(subject.tree)[0].props.children, "environmentSettings.saved");
  subject.unmount();

  const remounted = renderer(EnvironmentSettings, { workspace: { id: "workspace" } }, env);
  await remounted.settle();
  assert.equal(toggle(remounted.tree).props.checked, true);
  assert.equal(resource(remounted.tree, "cpu").props.value, "2");
  assert.equal(resource(remounted.tree, "memory").props.value, "4");
  assert.equal(resource(remounted.tree, "workspaceSpace").props.value, "10");
  assert.equal(status(remounted.tree).length, 0);
  remounted.unmount();
});

test("milliCPU and byte values round-trip without rejecting decimal display precision", async () => {
  for (const stored of [
    policy({ cpuMilli: 1001, memoryBytes: 3 * GIB + 1, workspaceBytes: 1 }),
    policy({ cpuMilli: 4294967295, memoryBytes: Number.MAX_SAFE_INTEGER, workspaceBytes: Number.MAX_SAFE_INTEGER }),
  ]) {
    const writes = [];
    const env = environment({ request: async (_url, options) => {
      if (options?.method === "PATCH") writes.push(JSON.parse(options.body));
      return stored;
    } });
    const subject = renderer(EnvironmentSettings, { workspace: { id: "workspace" } }, env);
    await subject.settle();
    assert.equal(resource(subject.tree, "cpu").props.value, String(stored.cpuMilli / 1000));
    assert.equal(resource(subject.tree, "memory").props.value, String(stored.memoryBytes / GIB));
    assert.equal(resource(subject.tree, "workspaceSpace").props.value, String(stored.workspaceBytes / GIB));
    await submit(subject); await subject.settle();
    const expected = { network: stored.network, cpuMilli: stored.cpuMilli, memoryBytes: stored.memoryBytes, workspaceBytes: stored.workspaceBytes };
    assert.deepEqual(writes, [expected]);
    assert.equal(alert(subject.tree).length, 0);
    subject.unmount();
  }
});

test("invalid resource drafts only show inline validation after submit and never write", async () => {
  for (const [name, value] of [
    ["cpu", ""], ["cpu", "0"], ["cpu", "-1"], ["cpu", "0.0001"], ["cpu", "4294967.296"],
    ["memory", "Infinity"], ["memory", "0.0000000001"], ["memory", "8388608"],
    ["workspaceSpace", "-2"], ["workspaceSpace", "8388608"],
  ]) {
    let writes = 0;
    const env = environment({ request: async (_url, options) => {
      if (options?.method === "PATCH") writes++;
      return policy();
    } });
    const subject = renderer(EnvironmentSettings, { workspace: { id: "workspace" } }, env);
    await subject.settle();
    await editResources(subject, { [name]: value });
    assert.equal(alert(subject.tree).length, 0, "editing alone does not add validation messages");
    await submit(subject); await subject.settle();
    assert.equal(writes, 0);
    assert.equal(resource(subject.tree, name).props.value, value, "invalid submission retains the draft");
    assert.equal(resource(subject.tree, name).props["aria-invalid"], true);
    assert.equal(alert(subject.tree)[0].props.children, "environmentSettings.invalidResource");
    await editResources(subject, { [name]: "2" });
    assert.equal(alert(subject.tree).length, 0);
    subject.unmount();
  }
});

test("old, unknown and invalid response fields are rejected without editable defaults", async () => {
  for (const value of [
    { network: "disabled" }, policy({ unknown: true }), policy({ network: "private" }),
    policy({ cpuMilli: null }), policy({ cpuMilli: 4294967296 }), policy({ cpuMilli: 1.5 }),
    policy({ memoryBytes: Number.MAX_SAFE_INTEGER + 1 }), policy({ workspaceBytes: 0 }),
    policy({ revision: 0 }), policy({ revision: "1" }),
  ]) {
    const subject = renderer(EnvironmentSettings, { workspace: { id: "workspace" } }, environment({ request: async () => value }));
    await subject.settle();
    assert.equal(alert(subject.tree)[0].props.children, "environmentSettings.loadError");
    assert.equal(toggle(subject.tree).props.disabled, true);
    assert.equal(saveButton(subject.tree).props.disabled, true);
    for (const name of ["cpu", "memory", "workspaceSpace"]) assert.equal(resource(subject.tree, name).props.disabled, true);
    assert.equal(status(subject.tree).length, 0);
    subject.unmount();
  }
});

test("permission failures keep the form disabled and allow an authoritative retry", async () => {
  let reads = 0;
  const env = environment({ request: async () => {
    if (++reads === 1) throw new ApiError("forbidden", 403);
    return policy();
  } });
  const subject = renderer(EnvironmentSettings, { workspace: { id: "workspace" } }, env);
  await subject.settle();
  assert.equal(alert(subject.tree)[0].props.children, "environmentSettings.loadError");
  assert.equal(saveButton(subject.tree).props.disabled, true);
  assert.equal(status(subject.tree).length, 0);
  await button(subject.tree, "environmentSettings.retry").props.onClick(); await subject.settle();
  assert.equal(saveButton(subject.tree).props.disabled, false);
  assert.equal(alert(subject.tree).length, 0);
  subject.unmount();
});

test("a rejected update reloads stored settings and never reports a successful save", async () => {
  const env = environment({ request: async (_url, options) => {
    if (options?.method === "PATCH") throw new ApiError("forbidden", 403);
    return policy();
  } });
  const subject = renderer(EnvironmentSettings, { workspace: { id: "workspace" } }, env);
  await subject.settle();
  await editResources(subject, { cpu: "3" });
  toggle(subject.tree).props.onChange({ target: { checked: true } }); await subject.settle();
  await submit(subject); await subject.settle();
  assert.equal(toggle(subject.tree).props.checked, false);
  assert.equal(resource(subject.tree, "cpu").props.value, "1");
  assert.equal(saveButton(subject.tree).props.disabled, false);
  assert.equal(alert(subject.tree)[0].props.children, "environmentSettings.saveUnconfirmed");
  assert.equal(status(subject.tree).length, 0);
  subject.unmount();
});

test("a lost save response reloads committed network and resources without claiming success", async () => {
  let stored = policy();
  let reads = 0;
  const env = environment({ request: async (_url, options) => {
    if (options?.method === "PATCH") {
      stored = { ...stored, ...JSON.parse(options.body), revision: stored.revision + 1 };
      throw new Error("response lost");
    }
    reads++;
    return { ...stored };
  } });
  const subject = renderer(EnvironmentSettings, { workspace: { id: "workspace" } }, env);
  await subject.settle();
  await editResources(subject, { cpu: "2", memory: "4", workspaceSpace: "10" });
  toggle(subject.tree).props.onChange({ target: { checked: true } }); await subject.settle();
  await submit(subject); await subject.settle();
  assert.equal(reads, 2);
  assert.equal(toggle(subject.tree).props.checked, true);
  assert.equal(resource(subject.tree, "cpu").props.value, "2");
  assert.equal(resource(subject.tree, "memory").props.value, "4");
  assert.equal(resource(subject.tree, "workspaceSpace").props.value, "10");
  assert.equal(toggle(subject.tree).props.disabled, false);
  assert.equal(alert(subject.tree)[0].props.children, "environmentSettings.saveUnconfirmed");
  assert.equal(status(subject.tree).length, 0);
  subject.unmount();
});

test("failed reconciliation keeps every control disabled until an authoritative retry succeeds", async () => {
  let reads = 0;
  let stored = policy();
  const env = environment({ request: async (_url, options) => {
    if (options?.method === "PATCH") {
      stored = { ...stored, ...JSON.parse(options.body), revision: stored.revision + 1 };
      throw new Error("response lost");
    }
    if (++reads === 2) throw new Error("offline");
    return { ...stored };
  } });
  const subject = renderer(EnvironmentSettings, { workspace: { id: "workspace" } }, env);
  await subject.settle();
  await editResources(subject, { memory: "4" });
  toggle(subject.tree).props.onChange({ target: { checked: true } }); await subject.settle();
  await submit(subject); await subject.settle();
  assert.equal(toggle(subject.tree).props.disabled, true);
  assert.equal(saveButton(subject.tree).props.disabled, true);
  for (const name of ["cpu", "memory", "workspaceSpace"]) assert.equal(resource(subject.tree, name).props.disabled, true);
  const retry = button(subject.tree, "environmentSettings.retry");
  assert.ok(retry);
  await retry.props.onClick(); await subject.settle();
  assert.equal(toggle(subject.tree).props.checked, true);
  assert.equal(resource(subject.tree, "memory").props.value, "4");
  assert.equal(toggle(subject.tree).props.disabled, false);
  subject.unmount();
});

test("a late workspace read is aborted and cannot replace the new workspace draft", async () => {
  const oldRead = deferred();
  let oldSignal;
  const props = { workspace: { id: "first" } };
  const env = environment({ request: async (url, options) => {
    if (url.includes("/first/")) { oldSignal = options.signal; return oldRead.promise; }
    assert.equal(url, "/api/workspaces/second/environment-settings");
    return policy({ cpuMilli: 2000 });
  } });
  const subject = renderer(EnvironmentSettings, props, env);
  await subject.settle();
  props.workspace = { id: "second" }; await subject.settle();
  await editResources(subject, { cpu: "7" });
  oldRead.resolve(policy({ cpuMilli: 3000 })); await subject.settle();
  assert.equal(oldSignal.aborted, true);
  assert.equal(resource(subject.tree, "cpu").props.value, "7");
  assert.equal(saveButton(subject.tree).props.disabled, false);
  assert.equal(alert(subject.tree).length, 0);
  subject.unmount();
});

test("late workspace save outcomes cannot overwrite a new draft or reconcile the old workspace", async () => {
  for (const outcome of ["resolve", "reject"]) {
    const oldSave = deferred();
    let oldSignal;
    let oldReads = 0;
    const props = { workspace: { id: "first" } };
    const env = environment({ request: async (url, options) => {
      if (url.includes("/first/")) {
        if (options?.method === "PATCH") { oldSignal = options.signal; return oldSave.promise; }
        oldReads++;
        return policy();
      }
      assert.equal(url, "/api/workspaces/second/environment-settings");
      return policy({ cpuMilli: 2000 });
    } });
    const subject = renderer(EnvironmentSettings, props, env);
    await subject.settle();
    await editResources(subject, { cpu: "3" });
    const pending = submit(subject); await subject.settle();
    props.workspace = { id: "second" }; await subject.settle();
    await editResources(subject, { cpu: "7" });
    if (outcome === "resolve") oldSave.resolve(policy({ cpuMilli: 3000 }));
    else oldSave.reject(new Error("response lost"));
    await pending; await subject.settle();
    assert.equal(oldSignal.aborted, true);
    assert.equal(oldReads, 1, "an aborted save must not start old-workspace reconciliation");
    assert.equal(resource(subject.tree, "cpu").props.value, "7");
    assert.equal(saveButton(subject.tree).props.disabled, false);
    assert.equal(status(subject.tree).length, 0);
    assert.equal(alert(subject.tree).length, 0);
    subject.unmount();
  }
});
