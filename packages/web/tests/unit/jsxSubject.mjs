import { readFileSync, existsSync } from "node:fs";
import { resolve, dirname } from "node:path";
import { transformWithOxc } from "vite";

// Lightweight component characterization through the project's existing JSX
// transformer. No browser, dev server or build is started. Translation and
// unrelated composer children are isolated; the actual picker callbacks run.
const dataUrl = code => `data:text/javascript;base64,${Buffer.from(code).toString("base64")}`;
export function createJsxSubjectLoader(overrides = new Map()) {
  const cache = new Map();
  async function load(path) {
    const file = resolve(path);
    if (cache.has(file)) return cache.get(file);
    const source = readFileSync(file, "utf8");
    if (file.endsWith(".json")) {
      const url = `data:application/json;base64,${Buffer.from(JSON.stringify(JSON.parse(source))).toString("base64")}`;
      cache.set(file, url); return url;
    }
    const result = await transformWithOxc(source, file, { jsx: { runtime: "automatic" } });
    const replacements = new Map();
    for (const match of result.code.matchAll(/(?:from\s*|import\s*)["']([^"']+)["']/g)) {
      const specifier = match[1];
      if (replacements.has(specifier)) continue;
      let target;
      if (overrides.has(specifier)) target = overrides.get(specifier);
      else if (specifier.endsWith(".css")) target = dataUrl("");
      else if (specifier.endsWith("/i18n")) target = dataUrl('export const t=key=>key; export const useTranslation=()=>({t});');
      else if (specifier.endsWith("/ContextUsagePicker")) target = dataUrl('export const ContextUsagePicker=()=>null;');
      else if (specifier.endsWith("/AttachmentCard")) target = dataUrl('export const AttachmentCard=()=>null; export const LocalAttachmentCard=()=>null; export const localAttachmentKey=file=>file.name;');
      else if (specifier.startsWith(".")) {
        const base = resolve(dirname(file), specifier);
        const child = [base, ...[".jsx", ".js", ".tsx", ".ts"].map(extension => base + extension)].find(existsSync);
        if (!child) throw new Error(`Unresolved characterization import: ${specifier}`);
        target = await load(child);
      } else target = import.meta.resolve(specifier);
      replacements.set(specifier, target);
    }
    let code = result.code;
    for (const [specifier, target] of replacements) code = code.replaceAll(`"${specifier}"`, JSON.stringify(target)).replaceAll(`'${specifier}'`, JSON.stringify(target));
    const url = dataUrl(code);
    cache.set(file, url);
    return url;
  }
  return load;
}
export const loadJsxSubject = createJsxSubjectLoader();

export function elements(node, predicate) {
  if (!node || typeof node !== "object") return [];
  if (Array.isArray(node)) return node.flatMap(child => elements(child, predicate));
  if (!node.props) return [];
  const component = typeof node.type === "function" ? node.type : node.type?.type;
  if (typeof component === "function" && typeof node.type !== "string") return elements(component(node.props), predicate);
  return [...(predicate(node) ? [node] : []), ...elements(node.props.children, predicate)];
}
