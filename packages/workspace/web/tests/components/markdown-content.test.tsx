// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, expect, test, vi } from "vitest";
import { MarkdownContent } from "../../src/chat/MarkdownContent";
import { TranscriptSearchQuery } from "../../src/chat/TranscriptSearchHighlight";
import { highlightCode } from "../../src/chat/markdown/highlight";
import { sanitizeMermaidSvg } from "../../src/chat/markdown/sanitizeMermaid.mjs";

const diagram = vi.hoisted(() => ({ theme: "light", render: vi.fn(async (code: string, dark: boolean) => {
  if (code.includes("INVALID")) throw new Error("invalid diagram");
  return `<svg xmlns="http://www.w3.org/2000/svg"><text>${dark ? "dark" : "light"} diagram</text></svg>`;
}) }));
vi.mock("../../src/chat/markdown/renderMermaid", () => ({ renderMermaid: diagram.render }));
vi.mock("../../src/theme", () => ({ useResolvedTheme: () => diagram.theme }));
vi.mock("../../src/i18n", () => ({ useTranslation: () => ({ t: (key: string) => key }) }));
afterEach(() => { cleanup(); diagram.theme = "light"; diagram.render.mockClear(); });

test("GFM preserves nested lists, task state, escaped tables and document-wide references", () => {
  const source = "###### Deep heading\n\n- Parent\n  - **Nested** and *emphasized*\n\n- [x] Finished\n- [ ] Pending\n\n| Name | Value |\n| :--- | ---: |\n| a\\|b | ~~old~~ |\n\nSee [guide][later].\n\nA paragraph.\n\n[later]: https://example.com/guide";
  const view = render(<MarkdownContent text={source} />);
  expect(screen.getByRole("heading", { level: 6 }).textContent).toBe("Deep heading");
  expect(view.container.querySelector("ul ul")?.textContent).toContain("Nested");
  expect(screen.getByText("emphasized").tagName).toBe("EM");
  expect(screen.getAllByRole("checkbox").map(input => (input as HTMLInputElement).checked)).toEqual([true, false]);
  expect(screen.getByRole("cell", { name: "a|b" })).toBeTruthy();
  expect(screen.getByText("old").tagName).toBe("S");
  expect(screen.getByRole("link", { name: "guide" }).getAttribute("href")).toBe("https://example.com/guide");
});

test("streamed references and multiline list continuations converge to the complete document", async () => {
  const prefix = "See [guide][later].\n\n- First\n\n  Continued\n\n  - Nested\n\n";
  const complete = prefix + "[later]: /docs/guide\n\n````text\n``` embedded\n\ncode\n````";
  const view = render(<MarkdownContent text={prefix} isStreaming />);
  expect(screen.queryByRole("link", { name: "guide" })).toBeNull();
  view.rerender(<MarkdownContent text={complete} isStreaming />);
  await waitFor(() => expect(screen.getByRole("link", { name: "guide" }).getAttribute("href")).toBe("/docs/guide"));
  expect(view.container.querySelector("ul ul")?.textContent).toBe("Nested");
  view.rerender(<MarkdownContent text={complete} />);
  expect(view.container.querySelector("pre code")?.textContent).toBe("``` embedded\n\ncode");
  view.rerender(<MarkdownContent text="Replacement" isStreaming />);
  expect(screen.getByText("Replacement")).toBeTruthy();
  expect(screen.queryByRole("link", { name: "guide" })).toBeNull();
});

test("raw HTML stays text and disallowed links never become navigation or image requests", () => {
  const view = render(<MarkdownContent text={'<script>alert(1)</script>\n\n[unsafe](javascript:alert) [artifact](/api/artifacts/secret)\n\n![remote](https://example.com/a.png)'} />);
  expect(view.container.querySelector("script")).toBeNull();
  expect(view.container.textContent).toContain("<script>alert(1)</script>");
  expect(screen.queryByRole("link", { name: "unsafe" })).toBeNull();
  expect(screen.queryByRole("link", { name: "artifact" })).toBeNull();
  expect(screen.getByRole("link", { name: "remote" }).getAttribute("href")).toBe("https://example.com/a.png");
  expect(view.container.querySelector("img")).toBeNull();
});

test("named and numeric character references use the same document-independent Markdown semantics", () => {
  const view = render(<MarkdownContent text="&copy; &NotEqualTilde; &notit; &#x1f642;" />);
  expect(view.container.textContent).toBe("© ≂̸ &notit; 🙂");
});

test("code remains exact, highlights whole-text search across syntax tokens and copies only its body", async () => {
  const code = 'const result = "value";\nconsole.log(result);';
  const copy = vi.fn().mockResolvedValue(undefined);
  Object.defineProperty(navigator, "clipboard", { configurable: true, value: { writeText: copy } });
  const view = render(<TranscriptSearchQuery.Provider value="result ="><MarkdownContent text={'```javascript\n' + code + '\n```'} /></TranscriptSearchQuery.Provider>);
  expect(view.container.querySelector("pre code")?.textContent).toBe(code);
  expect([...view.container.querySelectorAll("mark")].map(mark => mark.textContent).join("")).toBe("result =");
  fireEvent.click(screen.getByRole("button", { name: "markdown.copyCode" }));
  await waitFor(() => expect(copy).toHaveBeenCalledWith(code));
  const ranges = await highlightCode(code, "javascript");
  expect(ranges.some(range => code.slice(range.from, range.to) === "const")).toBe(true);
  view.rerender(<MarkdownContent text={'```unknown-language\n' + code + '\n```'} />);
  expect(view.container.querySelector("pre code")?.textContent).toBe(code);
});

test("KaTeX renders bracket and display formulas while amounts stay text, and terminal errors discard stale output", async () => {
  const view = render(<MarkdownContent text={'Price $5 and $10. \\(x^2\\)\n\n$$\ny = 2\n$$'} isStreaming />);
  await waitFor(() => expect(view.container.querySelectorAll("math").length).toBe(2));
  expect(view.container.textContent).toContain("Price $5 and $10.");
  view.rerender(<MarkdownContent text={'\\(x^2\\)'} isStreaming />);
  await waitFor(() => expect(view.container.querySelectorAll("math").length).toBe(1));
  const previous = view.container.querySelector("math")?.textContent;
  const invalid = '\\(x^2 + \\unknowncommand\\)';
  view.rerender(<MarkdownContent text={invalid} isStreaming />);
  await act(async () => { await new Promise(resolve => setTimeout(resolve, 25)); });
  expect(view.container.querySelector("math")?.textContent).toBe(previous);
  view.rerender(<MarkdownContent text={invalid} />);
  expect(view.container.querySelector("math")).toBeNull();
  expect(view.container.textContent).toBe(invalid);
  view.rerender(<MarkdownContent text={'\\(\\href{javascript:alert(1)}{unsafe}\\)'} />);
  await waitFor(() => expect(view.container.querySelector("math")).not.toBeNull());
  expect(view.container.querySelector("a")).toBeNull();
});

test("Mermaid retains the selected source view, follows theme, and exposes current source after a terminal error", async () => {
  const code = "graph TD\nA-->B";
  const view = render(<MarkdownContent text={'```mermaid\n' + code + '\n```'} isStreaming />);
  expect(await screen.findByRole("img", { name: "markdown.diagram" })).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "markdown.source" }));
  expect(view.container.querySelector("pre code")?.textContent).toBe(code);
  view.rerender(<MarkdownContent text={'```mermaid\n' + code + '\nB-->C\n```'} isStreaming />);
  await waitFor(() => expect(view.container.querySelector("pre code")?.textContent).toContain("B-->C"));
  expect(screen.queryByRole("img")).toBeNull();
  diagram.theme = "dark";
  fireEvent.click(screen.getByRole("button", { name: "markdown.diagram" }));
  expect(await screen.findByRole("img", { name: "markdown.diagram" })).toBeTruthy();
  expect(diagram.render.mock.calls.at(-1)?.[1]).toBe(true);
  const invalid = code + "\nB-->C\nINVALID";
  view.rerender(<MarkdownContent text={'```mermaid\n' + invalid + '\n```'} />);
  expect(await screen.findByRole("status")).toBeTruthy();
  expect(screen.queryByRole("img")).toBeNull();
  expect(view.container.querySelector("pre code")?.textContent).toBe(invalid);
});

test("SVG sanitization removes scripts, external resources and unscoped CSS while retaining local markers", () => {
  const clean = sanitizeMermaidSvg('<svg xmlns="http://www.w3.org/2000/svg" id="diagram"><style>@import url(https://bad.invalid/a.css);body{display:none}#diagram .node{fill:url(https://bad.invalid/a);stroke:#123;marker-end:url(#marker)}</style><defs><marker id="marker"><path d="M0 0L1 1"/></marker></defs><rect class="node" fill="url(https://bad.invalid/a)" onclick="bad()"/><image href="https://bad.invalid/img"/><foreignObject><div>unsafe</div></foreignObject><script>bad()</script></svg>');
  const document = new DOMParser().parseFromString(clean, "image/svg+xml");
  expect(document.querySelector("script, foreignObject, image")).toBeNull();
  expect(document.querySelector("rect")?.hasAttribute("onclick")).toBe(false);
  expect(document.querySelector("rect")?.hasAttribute("fill")).toBe(false);
  expect(clean).not.toContain("bad.invalid");
  expect(clean).not.toContain("body{");
  expect(clean).toContain("url(#marker)");
  expect(clean).toContain("stroke:#123");
});
