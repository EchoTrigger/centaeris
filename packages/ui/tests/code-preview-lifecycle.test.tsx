import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { beforeEach, expect, test, vi } from "vitest";

const editorHarness = vi.hoisted(() => ({
  instances: [] as Array<{
    state: { doc: { lines: number; line: (lineNumber: number) => { from: number } } };
    setState: ReturnType<typeof vi.fn>;
    dispatch: ReturnType<typeof vi.fn>;
    destroy: ReturnType<typeof vi.fn>;
  }>,
}));

const extension = vi.hoisted(() => () => ({ extension: true }));

vi.mock("@codemirror/state", () => ({
  EditorState: {
    create: ({ doc = "" }: { doc?: string }) => ({
      doc: {
        lines: Math.max(doc.split("\n").length, 1),
        line: (lineNumber: number) => ({ from: lineNumber * 10 }),
      },
    }),
    readOnly: { of: extension },
  },
  RangeSetBuilder: class {
    add() {}
    finish() { return []; }
  },
}));

vi.mock("@codemirror/view", () => {
  class EditorView {
    static decorations = { compute: extension };
    static editable = { of: extension };
    static theme = extension;
    static scrollIntoView = (position: number) => ({ position });
    state: { doc: { lines: number; line: (lineNumber: number) => { from: number } } };
    setState = vi.fn((state) => { this.state = state; });
    dispatch = vi.fn();
    destroy = vi.fn();

    constructor({ state }: { state?: EditorView["state"] }) {
      this.state = state ?? {
        doc: { lines: 1, line: () => ({ from: 0 }) },
      };
      editorHarness.instances.push(this);
    }
  }
  return {
    EditorView,
    Decoration: { line: extension },
    drawSelection: extension,
    highlightActiveLine: extension,
    highlightActiveLineGutter: extension,
    highlightSpecialChars: extension,
    keymap: { of: extension },
    lineNumbers: extension,
  };
});

vi.mock("@codemirror/commands", () => ({ defaultKeymap: [] }));
vi.mock("@codemirror/language", () => ({
  syntaxHighlighting: extension,
  defaultHighlightStyle: {},
  bracketMatching: extension,
  foldGutter: extension,
}));
vi.mock("@codemirror/lang-cpp", () => ({ cpp: extension }));
vi.mock("@codemirror/lang-css", () => ({ css: extension }));
vi.mock("@codemirror/lang-html", () => ({ html: extension }));
vi.mock("@codemirror/lang-java", () => ({ java: extension }));
vi.mock("@codemirror/lang-javascript", () => ({ javascript: extension }));
vi.mock("@codemirror/lang-json", () => ({ json: extension }));
vi.mock("@codemirror/lang-python", () => ({ python: extension }));
vi.mock("@codemirror/lang-rust", () => ({ rust: extension }));

import { CodePreview } from "../src/components/CodePreview";

globalThis.IS_REACT_ACT_ENVIRONMENT = true;

beforeEach(() => {
  editorHarness.instances.length = 0;
});

test("reconfiguring a preview preserves one editor and restores its target scroll", async () => {
  let renderer: ReactTestRenderer | null = null;
  await act(async () => {
    renderer = create(
      <CodePreview content={"one\ntwo"} path="example.ts" targetLine={2} />,
      { createNodeMock: () => ({}) },
    );
  });

  expect(editorHarness.instances).toHaveLength(1);
  const editor = editorHarness.instances[0];
  expect(editor.setState).toHaveBeenCalledOnce();
  expect(editor.dispatch).toHaveBeenCalledOnce();

  await act(async () => {
    renderer!.update(
      <CodePreview content={"one\ntwo"} path="example.rs" targetLine={2} />,
    );
  });

  expect(editorHarness.instances).toHaveLength(1);
  expect(editor.setState).toHaveBeenCalledTimes(2);
  expect(editor.dispatch).toHaveBeenCalledTimes(2);

  await act(async () => renderer!.unmount());
  expect(editor.destroy).toHaveBeenCalledOnce();
});
