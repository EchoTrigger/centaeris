import { createJsxSubjectLoader } from "./jsxSubject.mjs";

// Node callback/effect characterization, not a React DOM or browser renderer.
// Synthetic route/API contexts let the real components drive visible props and
// request/cancel behavior without starting a server or installing a dependency.
let active;
export let context;
const equal = (left, right) => left && right && left.length === right.length && left.every((item, index) => Object.is(item, right[index]));
export function useState(initial) {
  const renderer = active; const index = renderer.cursor++;
  if (!(index in renderer.slots)) renderer.slots[index] = typeof initial === "function" ? initial() : initial;
  renderer.setters[index] ||= value => {
    renderer.slots[index] = typeof value === "function" ? value(renderer.slots[index]) : value;
  };
  return [renderer.slots[index], renderer.setters[index]];
}
export function useRef(initial) {
  const [ref] = useState(() => ({ current: initial })); return ref;
}
export function useMemo(create, dependencies) {
  const index = active.cursor++; const previous = active.slots[index];
  if (!previous || !equal(previous.dependencies, dependencies)) active.slots[index] = { value: create(), dependencies };
  return active.slots[index].value;
}
export function useCallback(callback, dependencies) { return useMemo(() => callback, dependencies); }
export function useEffect(effect, dependencies) {
  const index = active.cursor++; const previous = active.effects[index];
  if (!previous || !equal(previous.dependencies, dependencies)) active.pending.set(index, { effect, dependencies });
}
export const useLayoutEffect = useEffect;
export function renderer(component, props, environment) {
  const subject = { slots: [], setters: [], effects: [], pending: new Map(), cursor: 0, tree: null };
  return Object.assign(subject, {
    render() {
      active = subject; context = environment; subject.cursor = 0;
      try { subject.tree = component(props); return subject.tree; } finally { active = null; }
    },
    flush() {
      context = environment;
      for (const [index, pending] of subject.pending) {
        subject.effects[index]?.cleanup?.();
        subject.effects[index] = { dependencies: pending.dependencies, cleanup: pending.effect() };
      }
      subject.pending.clear();
    },
    async settle() {
      for (let attempt = 0; attempt < 20; attempt++) { subject.render(); subject.flush(); await Promise.resolve(); }
      return subject.render();
    },
    unmount() { for (const effect of subject.effects) effect?.cleanup?.(); },
  });
}
export function nodes(tree, predicate) {
  if (!tree || typeof tree !== "object") return [];
  if (Array.isArray(tree)) return tree.flatMap(child => nodes(child, predicate));
  if (!tree.props) return [];
  return [...(predicate(tree) ? [tree] : []), ...nodes(tree.props.children, predicate)];
}
const text = value => Array.isArray(value) ? value.map(text).join("") : typeof value === "string" ? value : "";
export const button = (tree, label) => nodes(tree, node => node.type === "button" && (text(node.props.children) === label || node.props["aria-label"] === label))[0];
export function environment(overrides = {}) {
  const state = {
    workspace: { id: "workspace", role: "member" },
    user: { id: "user" },
    agents: [{ id: "agent", workspaceId: "workspace", name: "Synthetic Agent", status: "active", avatarKind: "centaeris" }],
    location: { pathname: "/w/workspace/settings/agents", search: "?agentId=agent", state: null },
    navigations: [], revalidationCount: 0, modal: null,
    blocker: { state: "unblocked", resets: 0, proceeds: 0, reset() { this.state = "unblocked"; this.resets++; }, proceed() { this.state = "proceeding"; this.proceeds++; } },
    ...overrides,
  };
  state.navigate = overrides.navigate || (path => state.navigations.push(path));
  state.revalidator = { revalidate: async () => { state.revalidationCount++; } };
  return state;
}
export function deferred() {
  let resolve; let reject;
  const promise = new Promise((accept, fail) => { resolve = accept; reject = fail; });
  return { promise, resolve, reject };
}
export class ApiError extends Error { constructor(message, status) { super(message); this.status = status; } }
export function tailRefreshFixture(testContext) {
  const timers = new Map(); let sequence = 0;
  testContext.mock.method(globalThis, "setTimeout", (callback, delay) => { timers.set(++sequence, { callback, delay }); return sequence; });
  testContext.mock.method(globalThis, "clearTimeout", id => timers.delete(id));
  return async subject => {
    const entry = [...timers.values()].find(timer => timer.delay === 5000);
    if (!entry) throw new Error("Expected the existing automatic history refresh");
    entry.callback(); await subject.settle();
  };
}
const moduleUrl = import.meta.url;
const dataUrl = source => `data:text/javascript;base64,${Buffer.from(source).toString("base64")}`;
const stub = (specifier, source) => [specifier, dataUrl(`import {context,useRef} from ${JSON.stringify(moduleUrl)}; ${source}`)];
export function subjectLoader({ realModal = false, realDocumentPreview = false, realMessageList = false, extraOverrides = [] } = {}) {
  const overrides = new Map([
    ["react", dataUrl(`export * from ${JSON.stringify(import.meta.resolve("react"))}; export {useState,useRef,useMemo,useCallback,useEffect,useLayoutEffect} from ${JSON.stringify(moduleUrl)};`)],
    stub("react-router", 'export const Link=()=>null; export const useRouteLoaderData=id=>id==="authenticated"?{user:context.user}:({workspace:context.workspace,agents:context.agents}); export const useNavigate=()=>context.navigate; export const useLocation=()=>context.location; export const useRevalidator=()=>context.revalidator; export const useBlocker=value=>{context.blocker.predicate=value;context.blocker.shouldBlock=typeof value==="function"?value({currentLocation:context.location,nextLocation:context.nextLocation||{...context.location,search:"?agentId=other"}}):Boolean(value);return context.router?context.router.getBlocker("model-draft",value):context.blocker;};'),
    stub("../api", `export {ApiError} from ${JSON.stringify(moduleUrl)}; export const apiJson=(...args)=>context.request(...args); export const apiResponse=(...args)=>context.request(...args); export const clearCsrfToken=()=>{}; export const apiUrl=path=>path; export const jsonOptions=(method,value)=>({method,body:JSON.stringify(value)});`),
    stub("../components/DocumentPreview", "export const DocumentPreview=()=>null;"),
    stub("react-dom", "export const createPortal=(children)=>children;"),
    ...["../shell/ShellPage", "../shell/AgentMark", "../agent-chat/AgentMessageList", "../agent-chat/AgentSessionPreviewShell", "../agent-chat/AgentSessionActivityPreview", "../shell/AgentEditorModal"].map(specifier => stub(specifier, `export const ${specifier.split("/").at(-1)}=()=>null;`)),
  ]);
  if (realDocumentPreview) {
    overrides.delete("../components/DocumentPreview");
    overrides.set(...stub("./TextFilePreview", "export const TextFilePreview=()=>null;"));
  }
  if (realMessageList) {
    overrides.delete("../agent-chat/AgentMessageList");
    overrides.set(...stub("../chat/MarkdownContent", "export const MarkdownContent=()=>null;"));
    overrides.set(...stub("./AgentSessionReference", "export const AgentSessionReference=()=>null;"));
  }
  for (const [specifier, target] of extraOverrides) overrides.set(specifier, target);
  if (!realModal) overrides.set(...stub("../components/useModalDialog", "export const useModalDialog=value=>{context.modal=value;return useRef(null);};"));
  return createJsxSubjectLoader(overrides);
}
