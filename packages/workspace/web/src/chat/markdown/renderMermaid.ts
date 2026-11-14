import { sanitizeMermaidSvg } from "./sanitizeMermaid.mjs";

let loading: Promise<typeof import("mermaid").default> | undefined;
let queue = Promise.resolve();
let nextId = 0;

export function renderMermaid(code: string, dark: boolean, cancelled: () => boolean): Promise<string | null> {
  const drawing = queue.then(async () => {
    if (cancelled()) return null;
    loading ??= import("mermaid").then(module => module.default);
    const mermaid = await loading;
    if (cancelled()) return null;
    const style = getComputedStyle(document.documentElement);
    const surface = style.getPropertyValue("--surface-color").trim();
    const raised = style.getPropertyValue("--surface-raised").trim();
    const text = style.getPropertyValue("--on-surface").trim();
    const outline = style.getPropertyValue("--outline").trim();
    mermaid.initialize({
      theme: "base", startOnLoad: false, securityLevel: "strict",
      htmlLabels: false, flowchart: { htmlLabels: false }, suppressErrorRendering: true,
      fontFamily: style.fontFamily,
      themeVariables: {
        darkMode: dark, background: surface, primaryColor: raised, primaryTextColor: text,
        primaryBorderColor: outline, secondaryColor: surface, secondaryTextColor: text,
        secondaryBorderColor: outline, tertiaryColor: raised, tertiaryTextColor: text,
        tertiaryBorderColor: outline, lineColor: outline, textColor: text,
        mainBkg: raised, nodeBorder: outline, edgeLabelBackground: surface,
        actorBkg: raised, actorBorder: outline, actorTextColor: text,
        noteBkgColor: surface, noteTextColor: text, noteBorderColor: outline,
      },
      secure: ["secure", "securityLevel", "startOnLoad", "maxTextSize", "maxEdges", "suppressErrorRendering", "htmlLabels", "flowchart", "themeCSS", "theme", "themeVariables", "fontFamily"],
    });
    const { svg } = await mermaid.render(`centaeris-mermaid-${nextId++}`, code);
    return cancelled() ? null : sanitizeMermaidSvg(svg);
  });
  queue = drawing.then(() => {}, () => {});
  return drawing;
}
