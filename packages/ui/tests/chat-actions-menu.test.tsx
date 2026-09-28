import { act, create } from "react-test-renderer";
import { expect, test, vi } from "vitest";
import { ChatActionsMenu } from "../src/components/ChatActionsMenu";

test("chat actions dismiss on outside press and Escape, but not a press inside", async () => {
  const listeners = new Map<string, (event: unknown) => void>();
  vi.stubGlobal("document", { addEventListener: (name: string, listener: (event: unknown) => void) => listeners.set(name, listener), removeEventListener: (name: string) => listeners.delete(name) });
  const inside = {};
  const focus = vi.fn();
  const node = { open: true, contains: (target: unknown) => target === inside, querySelector: () => ({ focus }) };
  let view!: ReturnType<typeof create>;
  try {
    await act(async () => { view = create(<ChatActionsMenu><button>Pin chat</button></ChatActionsMenu>, { createNodeMock: element => element.type === "details" ? node : null }); });
    listeners.get("pointerdown")!({ target: inside });
    expect(node.open).toBe(true);
    listeners.get("pointerdown")!({ target: {} });
    expect(node.open).toBe(false);
    node.open = true;
    view.root.findByType("details").props.onKeyDown({ key: "Escape" });
    expect(node.open).toBe(false);
    expect(focus).toHaveBeenCalledOnce();
    await act(async () => view.unmount());
    expect(listeners.size).toBe(0);
  } finally { vi.unstubAllGlobals(); }
});
