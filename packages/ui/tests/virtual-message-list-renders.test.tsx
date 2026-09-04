import { createRef, type ReactNode } from "react";
import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { expect, test, vi } from "vitest";

const harness = vi.hoisted(() => ({
  assistantRenders: 0,
  totalSize: 440,
  messageReads: new Map(),
  roleReads: new Map(),
  turnReads: new Map(),
}));

const count = (reads: Map<string, number>, messageId: string) => {
  reads.set(messageId, (reads.get(messageId) ?? 0) + 1);
};

vi.mock("@tanstack/react-virtual", () => ({
  useVirtualizer: () => ({
    getTotalSize: () => harness.totalSize,
    getVirtualItems: () => [
      { index: 0, start: 0 },
      { index: 1, start: 220 },
    ],
    measureElement: () => {},
  }),
}));

vi.mock("../src/components/chat/chatViewStore", () => {
  const messages = {
    user: { id: "user", role: "user", text: "hello", timestamp: Date.now() },
  };
  return {
    selectChatMessageIds: () => ["user", "assistant"],
    selectChatMessageById: (messageId: string) => () => {
      count(harness.messageReads, messageId);
      return messages[messageId as keyof typeof messages] ?? null;
    },
    selectChatMessageRoleById: (messageId: string) => () => {
      count(harness.roleReads, messageId);
      return messageId === "user" ? "user" : "assistant";
    },
    selectChatTurnByMessageId: (messageId: string) => () => {
      count(harness.turnReads, messageId);
      return { id: "turn", messages: [] };
    },
    useChatViewStore: (selector: (state: unknown) => unknown) => selector({}),
  };
});

vi.mock("../src/components/chat/AgentResultStream", () => ({
  AgentResultStream: () => {
    harness.assistantRenders += 1;
    return <div>assistant</div>;
  },
}));

vi.mock("../src/components/ui/button", () => ({
  Button: ({ children, ...props }: { children: ReactNode }) => (
    <button {...props}>{children}</button>
  ),
}));

vi.mock("../src/components/ui/tooltip", () => ({
  Tooltip: ({ children }: { children: ReactNode }) => children,
}));

import { VirtualMessageList } from "../src/components/chat/VirtualMessageList";

globalThis.IS_REACT_ACT_ENVIRONMENT = true;

test("edit-only parent updates do not rerender the expensive assistant subtree", async () => {
  harness.assistantRenders = 0;
  harness.messageReads.clear();
  harness.roleReads.clear();
  harness.turnReads.clear();
  const stable = () => {};
  const baseProps = {
    containerRef: createRef<HTMLDivElement>(),
    editingUserMessageId: null,
    copiedUserMessageId: null,
    latestUserMessageId: "user",
    editableUserMessageId: "user",
    onScroll: stable,
    onContentSizeChange: stable,
    onOpenWorkspacePath: stable,
    onOpenAgentSession: stable,
  };
  let renderer: ReactTestRenderer | null = null;

  await act(async () => {
    renderer = create(
      <VirtualMessageList
        {...baseProps}
        editingPrompt=""
        onEditingPromptChange={() => {}}
        onCancelEditingUserMessage={() => {}}
        onSubmitEditedUserMessage={() => {}}
        onCopyUserMessage={() => {}}
        onStartEditingUserMessage={() => {}}
      />,
    );
  });

  expect(harness.assistantRenders).toBe(1);
  expect(harness.turnReads.get("assistant")).toBe(1);

  await act(async () => {
    renderer!.update(
      <VirtualMessageList
        {...baseProps}
        editingPrompt="unrelated draft"
        onEditingPromptChange={() => {}}
        onCancelEditingUserMessage={() => {}}
        onSubmitEditedUserMessage={() => {}}
        onCopyUserMessage={() => {}}
        onStartEditingUserMessage={() => {}}
      />,
    );
  });

  expect(harness.roleReads.get("assistant")).toBe(2);
  expect(harness.messageReads.get("user")).toBe(2);
  expect(harness.turnReads.get("assistant")).toBe(1);
  expect(harness.assistantRenders).toBe(1);

  await act(async () => renderer!.unmount());
});

test("content-size notifications carry each measured total", async () => {
  harness.totalSize = 440;
  const onContentSizeChange = vi.fn();
  const stable = () => {};
  const props = {
    containerRef: createRef<HTMLDivElement>(),
    editingUserMessageId: null,
    editingPrompt: "",
    copiedUserMessageId: null,
    latestUserMessageId: "user",
    editableUserMessageId: "user",
    onScroll: stable,
    onContentSizeChange,
    onEditingPromptChange: stable,
    onCancelEditingUserMessage: stable,
    onSubmitEditedUserMessage: stable,
    onCopyUserMessage: stable,
    onStartEditingUserMessage: stable,
    onOpenWorkspacePath: stable,
    onOpenAgentSession: stable,
  };
  let renderer: ReactTestRenderer | null = null;

  await act(async () => {
    renderer = create(<VirtualMessageList {...props} />);
  });
  expect(onContentSizeChange).toHaveBeenLastCalledWith(440);

  harness.totalSize = 880;
  await act(async () => {
    renderer!.update(<VirtualMessageList {...props} />);
  });
  expect(onContentSizeChange).toHaveBeenLastCalledWith(880);

  await act(async () => renderer!.unmount());
});
