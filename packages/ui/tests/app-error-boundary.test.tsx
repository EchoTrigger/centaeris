import type { ReactElement } from "react";
import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { expect, test, vi } from "vitest";

const harness = vi.hoisted(() => ({ rendered: null as ReactElement | null }));

vi.mock("react-dom/client", () => ({
  default: {
    createRoot: () => ({
      render: (node: ReactElement) => {
        harness.rendered = node;
      },
    }),
  },
}));

vi.mock("../src/App", () => ({
  default: () => {
    throw new Error("broken render payload");
  },
}));

globalThis.IS_REACT_ACT_ENVIRONMENT = true;

test("the application root contains a render failure and offers recovery", async () => {
  const reload = vi.fn();
  Object.defineProperty(globalThis, "document", {
    configurable: true,
    value: {
      documentElement: { dataset: {} },
      getElementById: () => ({}),
    },
  });
  Object.defineProperty(globalThis, "window", {
    configurable: true,
    value: { location: { reload } },
  });
  const consoleError = vi.spyOn(console, "error").mockImplementation(() => {});
  let renderer: ReactTestRenderer | null = null;

  try {
    await import("../src/main");
    expect(harness.rendered).not.toBeNull();
    await act(async () => {
      renderer = create(harness.rendered!);
    });
    const alert = renderer!.root.findByProps({ role: "alert" });
    expect(alert.findByType("h1").children.join(" ")).toBe("界面暂时无法显示");
    const reloadButton = alert.findByType("button");
    expect(reloadButton.children.join(" ")).toBe("重新载入");
    reloadButton.props.onClick();
    expect(reload).toHaveBeenCalledOnce();
  } finally {
    consoleError.mockRestore();
  }
});
