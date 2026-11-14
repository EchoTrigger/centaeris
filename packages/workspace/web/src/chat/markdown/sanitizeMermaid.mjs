import DOMPurify from "dompurify";
import { generate, parse, walk } from "css-tree";

const localReference = /^#[\w:.-]+$/;
const resourceAttributes = new Set(["fill", "stroke", "filter", "clip-path", "mask", "marker-start", "marker-mid", "marker-end"]);
const properties = new Set(("color fill stroke fill-opacity stroke-opacity opacity stroke-width stroke-dasharray stroke-dashoffset stroke-linecap stroke-linejoin stroke-miterlimit font-family font-size font-weight font-style text-decoration text-anchor dominant-baseline alignment-baseline paint-order white-space line-height letter-spacing word-spacing display visibility overflow shape-rendering transform transform-origin filter clip-path mask marker-start marker-mid marker-end background-color border-color").split(" "));
const functions = new Set(("rgb rgba hsl hsla hwb lab lch oklab oklch color color-mix calc min max clamp translate translatex translatey scale rotate matrix").split(" "));

function safeValue(value) {
  let safe = true;
  walk(value, node => {
    if (node.type === "Raw" || (node.type === "Url" && !localReference.test(node.value))
      || (node.type === "Function" && !functions.has(node.name.toLowerCase()))) safe = false;
  });
  return safe;
}

function cleanCss(source, context, id) {
  try {
    const ast = parse(source, { context });
    walk(ast, {
      enter(node, item, list) {
        if (node.type === "Atrule") { list?.remove(item); return walk.skip; }
        if (node.type === "Rule") {
          const selectors = node.prelude?.type === "SelectorList" ? node.prelude.children.toArray() : [];
          if (!selectors.length || selectors.some(selector => {
            const first = selector.children.first;
            return first?.type !== "IdSelector" || first.name !== id || selector.children.toArray().some(part => part.type === "Combinator" && part.name !== " " && part.name !== ">");
          })) { list?.remove(item); return walk.skip; }
        }
        if (node.type === "Declaration" && (!properties.has(node.property.toLowerCase()) || !safeValue(node.value))) {
          list?.remove(item);
          return walk.skip;
        }
      },
    });
    return generate(ast);
  } catch { return ""; }
}

export function sanitizeMermaidSvg(source) {
  const fragment = DOMPurify.sanitize(source, {
    USE_PROFILES: { svg: true, svgFilters: true },
    FORBID_TAGS: ["foreignObject", "script", "image", "a", "animate", "animateMotion", "animateTransform", "set", "mpath"],
    RETURN_DOM_FRAGMENT: true,
  });
  const svg = fragment.querySelector("svg");
  if (!svg) throw new Error("Mermaid did not return an SVG diagram");
  for (const element of [svg, ...svg.querySelectorAll("*")]) {
    for (const attribute of [...element.attributes]) {
      const name = attribute.name.toLowerCase();
      if ((name === "href" || name === "xlink:href") && !localReference.test(attribute.value)) element.removeAttribute(attribute.name);
      else if (name === "style") element.setAttribute("style", cleanCss(attribute.value, "declarationList", svg.id));
      else if (resourceAttributes.has(name)) {
        try { if (!safeValue(parse(attribute.value, { context: "value" }))) element.removeAttribute(attribute.name); }
        catch { element.removeAttribute(attribute.name); }
      }
    }
    if (element.tagName.toLowerCase() === "style") element.textContent = cleanCss(element.textContent, "stylesheet", svg.id);
  }
  return svg.outerHTML;
}
