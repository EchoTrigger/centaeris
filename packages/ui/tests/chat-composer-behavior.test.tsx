import {
  useState,
  type ChangeEvent,
  type ComponentProps,
  type FormEvent,
  type KeyboardEvent as ReactKeyboardEvent,
} from "react";
import {
  act,
  create,
  type ReactTestInstance,
  type ReactTestRenderer,
} from "react-test-renderer";
import { afterAll, beforeEach, expect, test, vi } from "vitest";
import type {
  AgentContextUsageSummary,
  NativeMcpCatalog,
  NativeMcpServer,
  SelectableModel,
} from "../src/lib/chatBridge";

const renderHarness = vi.hoisted(() => ({ buttonRenders: 0 }));
const mcpHarness = vi.hoisted(() => ({
  configureNativeMcp: vi.fn<
    (input: {
      pluginName: string;
      serverId: string;
      bearerToken: string;
    }) => Promise<NativeMcpCatalog>
  >(),
  getNativeMcpCatalog: vi.fn<() => Promise<NativeMcpCatalog>>(),
}));

vi.mock("../src/components/ui/button", () => ({
  Button: ({
    children,
    type,
    disabled,
    className,
    "aria-label": ariaLabel,
    onClick,
  }: ComponentProps<"button">) => {
    renderHarness.buttonRenders += 1;
    return (
      <button
        type={type}
        disabled={disabled}
        className={className}
        aria-label={ariaLabel}
        onClick={onClick}
      >
        {children}
      </button>
    );
  },
}));

vi.mock("../src/lib/chatBridge", () => ({
  configureNativeMcp: mcpHarness.configureNativeMcp,
  getNativeMcpCatalog: mcpHarness.getNativeMcpCatalog,
}));

import { ChatComposer } from "../src/components/chat/ChatComposer";

globalThis.IS_REACT_ACT_ENVIRONMENT = true;

const originalDocumentDescriptor = Object.getOwnPropertyDescriptor(
  globalThis,
  "document",
);
const originalRequestAnimationFrameDescriptor = Object.getOwnPropertyDescriptor(
  globalThis,
  "requestAnimationFrame",
);

type Deferred<T> = {
  promise: Promise<T>;
  resolve: (value: T) => void;
};

const deferred = <T,>(): Deferred<T> => {
  let resolvePromise: ((value: T) => void) | null = null;
  const promise = new Promise<T>((resolve) => {
    resolvePromise = resolve;
  });
  return {
    promise,
    resolve: (value) => {
      if (!resolvePromise) {
        throw new Error("Deferred promise did not initialize");
      }
      resolvePromise(value);
    },
  };
};

const configurableServer: NativeMcpServer = {
  pluginName: "plugin-one",
  pluginDisplayName: "Plugin One",
  serverId: "server-one",
  pluginEnabled: true,
  status: "needsConfiguration",
  configurable: true,
  configured: false,
  transport: "streamableHttp",
  endpoint: "https://example.test/mcp",
  toolNames: ["search"],
};

const catalog = (servers: NativeMcpServer[]): NativeMcpCatalog => ({
  schema: "native.mcp.catalog.v1",
  servers,
});

const findButtonByStrongText = (
  renderer: ReactTestRenderer,
  text: string,
): ReactTestInstance => {
  const readText = (instance: ReactTestInstance): string =>
    instance.children
      .map((child) => typeof child === "string" ? child : readText(child))
      .join("");
  const button = renderer.root
    .findAllByType("button")
    .find((candidate) => readText(candidate).includes(text));
  if (!button) {
    throw new Error(`Missing button labelled ${text}`);
  }
  return button;
};

const click = async (button: ReactTestInstance): Promise<void> => {
  const props = button.props as unknown as { onClick?: () => void };
  if (!props.onClick) {
    throw new Error("Button does not have an onClick handler");
  }
  await act(async () => {
    props.onClick?.();
    await Promise.resolve();
  });
};

const changeTextarea = async (
  renderer: ReactTestRenderer,
  value: string,
): Promise<void> => {
  const textarea = renderer.root.findByType("textarea");
  const textareaProps = textarea.props as unknown as {
    onChange: (event: ChangeEvent<HTMLTextAreaElement>) => void;
  };
  await act(async () => {
    textareaProps.onChange({
      target: { value },
    } as unknown as ChangeEvent<HTMLTextAreaElement>);
  });
};

const pressTextareaKey = async (
  renderer: ReactTestRenderer,
  options: {
    key: string;
    shiftKey?: boolean;
    isComposing?: boolean;
    keyCode?: number;
  },
): Promise<ReturnType<typeof vi.fn>> => {
  const textarea = renderer.root.findByType("textarea");
  const textareaProps = textarea.props as unknown as {
    onKeyDown: (event: ReactKeyboardEvent<HTMLTextAreaElement>) => void;
  };
  const preventDefault = vi.fn();
  await act(async () => {
    textareaProps.onKeyDown({
      key: options.key,
      shiftKey: options.shiftKey ?? false,
      preventDefault,
      nativeEvent: {
        isComposing: options.isComposing ?? false,
        keyCode: options.keyCode ?? 0,
      },
    } as unknown as ReactKeyboardEvent<HTMLTextAreaElement>);
  });
  return preventDefault;
};

beforeEach(() => {
  mcpHarness.configureNativeMcp.mockReset();
  mcpHarness.getNativeMcpCatalog.mockReset();
  Object.defineProperty(globalThis, "document", {
    configurable: true,
    value: {
      addEventListener: vi.fn(),
      removeEventListener: vi.fn(),
    },
  });
  Object.defineProperty(globalThis, "requestAnimationFrame", {
    configurable: true,
    value: (callback: FrameRequestCallback) => {
      callback(0);
      return 1;
    },
  });
});

afterAll(() => {
  if (originalDocumentDescriptor) {
    Object.defineProperty(globalThis, "document", originalDocumentDescriptor);
  } else {
    Reflect.deleteProperty(globalThis, "document");
  }
  if (originalRequestAnimationFrameDescriptor) {
    Object.defineProperty(
      globalThis,
      "requestAnimationFrame",
      originalRequestAnimationFrameDescriptor,
    );
  } else {
    Reflect.deleteProperty(globalThis, "requestAnimationFrame");
  }
});

const createComposerProps = (): ComponentProps<typeof ChatComposer> => ({
  panelResetKey: "session-one",
  inputValue: "",
  isStreaming: false,
  hasInput: false,
  modelRuntimeSummary: "No model",
  selectableModels: [],
  activeModelIndex: -1,
  runtimeConfigError: "",
  reasoningEffort: null,
  reasoningEfforts: [],
  contextUsage: null,
  compactInteractive: false,
  isCompacting: false,
  onInputChange: vi.fn(),
  onSubmit: vi.fn(),
  onComposerAction: vi.fn(),
  onModelSelect: vi.fn(),
  onReasoningEffortSelect: vi.fn(),
  onCompact: vi.fn(),
});

test("stable props isolate the composer from an unrelated parent render", async () => {
  const composerProps = createComposerProps();
  const rerender = { current: null as (() => void) | null };
  const rendered = { current: null as ReactTestRenderer | null };
  renderHarness.buttonRenders = 0;

  function Harness() {
    const [, setRevision] = useState(0);
    rerender.current = () => setRevision((current) => current + 1);
    return <ChatComposer {...composerProps} />;
  }

  await act(async () => {
    rendered.current = create(<Harness />);
  });
  expect(renderHarness.buttonRenders).toBe(1);

  const triggerParentRender = rerender.current;
  if (!triggerParentRender) {
    throw new Error("Harness did not expose its rerender callback");
  }
  await act(async () => {
    triggerParentRender();
  });
  expect(renderHarness.buttonRenders).toBe(1);

  const renderer = rendered.current;
  if (!renderer) {
    throw new Error("Harness did not render");
  }
  await act(async () => renderer.unmount());
});

test("a configuration result cannot reopen an MCP panel after its session reset", async () => {
  mcpHarness.getNativeMcpCatalog.mockResolvedValue(catalog([configurableServer]));
  const save = deferred<NativeMcpCatalog>();
  const newOwnerSave = deferred<NativeMcpCatalog>();
  mcpHarness.configureNativeMcp
    .mockReturnValueOnce(save.promise)
    .mockReturnValueOnce(newOwnerSave.promise);
  const baseProps = createComposerProps();

  function Harness({ panelResetKey }: { panelResetKey: string }) {
    const [inputValue, setInputValue] = useState("");
    return (
      <ChatComposer
        {...baseProps}
        panelResetKey={panelResetKey}
        inputValue={inputValue}
        hasInput={Boolean(inputValue.trim())}
        onInputChange={setInputValue}
      />
    );
  }

  const rendered = { current: null as ReactTestRenderer | null };
  await act(async () => {
    rendered.current = create(<Harness panelResetKey="session-one" />);
  });
  const renderer = rendered.current;
  if (!renderer) {
    throw new Error("Composer did not render");
  }

  await changeTextarea(renderer, "/mcp");
  await click(findButtonByStrongText(renderer, "/mcp"));
  await click(findButtonByStrongText(renderer, configurableServer.serverId));

  const tokenInput = renderer.root.findByType("input");
  const tokenProps = tokenInput.props as unknown as {
    onChange: (event: ChangeEvent<HTMLInputElement>) => void;
  };
  await act(async () => {
    tokenProps.onChange({
      target: { value: "secret-token" },
    } as unknown as ChangeEvent<HTMLInputElement>);
  });

  const form = renderer.root.findByType("form");
  const formProps = form.props as unknown as {
    onSubmit: (event: FormEvent<HTMLFormElement>) => void;
  };
  await act(async () => {
    formProps.onSubmit({
      preventDefault: vi.fn(),
    } as unknown as FormEvent<HTMLFormElement>);
    await Promise.resolve();
  });
  expect(mcpHarness.configureNativeMcp).toHaveBeenCalledWith({
    pluginName: "plugin-one",
    serverId: "server-one",
    bearerToken: "secret-token",
  });

  await act(async () => {
    renderer.update(<Harness panelResetKey="session-two" />);
  });
  expect(renderer.root.findAllByProps({ "aria-label": "MCP servers" })).toHaveLength(0);

  const newSessionCatalog = deferred<NativeMcpCatalog>();
  mcpHarness.getNativeMcpCatalog.mockReturnValue(newSessionCatalog.promise);
  await changeTextarea(renderer, "/mcp");
  await click(findButtonByStrongText(renderer, "/mcp"));
  const newOwnerServer = {
    ...configurableServer,
    pluginName: "new-owner-plugin",
    serverId: "new-owner-server",
  } satisfies NativeMcpServer;
  await act(async () => {
    newSessionCatalog.resolve(catalog([newOwnerServer]));
    await Promise.resolve();
  });
  await click(findButtonByStrongText(renderer, "new-owner-server"));
  const newOwnerTokenInput = renderer.root.findByType("input");
  const newOwnerTokenProps = newOwnerTokenInput.props as unknown as {
    onChange: (event: ChangeEvent<HTMLInputElement>) => void;
  };
  await act(async () => {
    newOwnerTokenProps.onChange({
      target: { value: "new-owner-token" },
    } as unknown as ChangeEvent<HTMLInputElement>);
  });
  const newOwnerForm = renderer.root.findByType("form");
  const newOwnerFormProps = newOwnerForm.props as unknown as {
    onSubmit: (event: FormEvent<HTMLFormElement>) => void;
  };
  await act(async () => {
    newOwnerFormProps.onSubmit({
      preventDefault: vi.fn(),
    } as unknown as FormEvent<HTMLFormElement>);
    await Promise.resolve();
  });

  await act(async () => {
    save.resolve(catalog([{
      ...configurableServer,
      serverId: "stale-saved-server",
      status: "ready",
      configured: true,
    }]));
    await Promise.resolve();
  });
  expect(renderer.root.findAllByType("strong").map((node) => node.children.join("")))
    .not.toContain("stale-saved-server");
  expect(findButtonByStrongText(renderer, "Testing…")).toBeDefined();

  await act(async () => {
    newOwnerSave.resolve(catalog([{ ...newOwnerServer, status: "ready", configured: true }]));
    await Promise.resolve();
  });

  await act(async () => renderer.unmount());
});

test("the newest MCP catalog request owns the visible server list", async () => {
  const firstRequest = deferred<NativeMcpCatalog>();
  const secondRequest = deferred<NativeMcpCatalog>();
  mcpHarness.getNativeMcpCatalog
    .mockReturnValueOnce(firstRequest.promise)
    .mockReturnValueOnce(secondRequest.promise);
  const serverTwo = {
    ...configurableServer,
    pluginName: "plugin-two",
    pluginDisplayName: "Plugin Two",
    serverId: "server-two",
  } satisfies NativeMcpServer;
  const baseProps = createComposerProps();

  function Harness() {
    const [inputValue, setInputValue] = useState("");
    return (
      <ChatComposer
        {...baseProps}
        inputValue={inputValue}
        hasInput={Boolean(inputValue.trim())}
        onInputChange={setInputValue}
      />
    );
  }

  const rendered = { current: null as ReactTestRenderer | null };
  await act(async () => {
    rendered.current = create(<Harness />);
  });
  const renderer = rendered.current;
  if (!renderer) {
    throw new Error("Composer did not render");
  }

  await changeTextarea(renderer, "/mcp");
  await click(findButtonByStrongText(renderer, "/mcp"));
  await changeTextarea(renderer, "/mcp");
  await click(findButtonByStrongText(renderer, "/mcp"));

  await act(async () => {
    secondRequest.resolve(catalog([serverTwo]));
    await Promise.resolve();
  });
  expect(findButtonByStrongText(renderer, "server-two")).toBeDefined();

  await act(async () => {
    firstRequest.resolve(catalog([configurableServer]));
    await Promise.resolve();
  });
  expect(renderer.root.findAllByType("strong").map((node) => node.children.join("")))
    .not.toContain("server-one");
  expect(findButtonByStrongText(renderer, "server-two")).toBeDefined();

  await act(async () => renderer.unmount());
});

test("a locked MCP server cannot enter configuration", async () => {
  const lockedServer = {
    ...configurableServer,
    status: "disabled",
    configurable: false,
  } satisfies NativeMcpServer;
  mcpHarness.getNativeMcpCatalog.mockResolvedValue(catalog([lockedServer]));
  const baseProps = createComposerProps();

  function Harness() {
    const [inputValue, setInputValue] = useState("");
    return (
      <ChatComposer
        {...baseProps}
        inputValue={inputValue}
        hasInput={Boolean(inputValue.trim())}
        onInputChange={setInputValue}
      />
    );
  }

  const rendered = { current: null as ReactTestRenderer | null };
  await act(async () => {
    rendered.current = create(<Harness />);
  });
  const renderer = rendered.current;
  if (!renderer) {
    throw new Error("Composer did not render");
  }

  await changeTextarea(renderer, "/mcp");
  await click(findButtonByStrongText(renderer, "/mcp"));
  const lockedButton = findButtonByStrongText(renderer, "server-one");
  expect(lockedButton.props.disabled).toBe(true);
  await click(lockedButton);
  expect(renderer.root.findAllByProps({ "aria-label": "Configure MCP server" }))
    .toHaveLength(0);
  expect(mcpHarness.configureNativeMcp).not.toHaveBeenCalled();

  await act(async () => renderer.unmount());
});

test("IME composition and its trailing Enter cannot submit a prompt", async () => {
  const now = vi.spyOn(Date, "now").mockReturnValue(1_000);
  const props = {
    ...createComposerProps(),
    inputValue: "hello",
    hasInput: true,
    onSubmit: vi.fn(),
  };
  const rendered = { current: null as ReactTestRenderer | null };
  await act(async () => {
    rendered.current = create(<ChatComposer {...props} />);
  });
  const renderer = rendered.current;
  if (!renderer) {
    throw new Error("Composer did not render");
  }

  await pressTextareaKey(renderer, { key: "Enter", shiftKey: true });
  expect(props.onSubmit).not.toHaveBeenCalled();

  const regularEnter = await pressTextareaKey(renderer, { key: "Enter" });
  expect(regularEnter).toHaveBeenCalledTimes(1);
  expect(props.onSubmit).toHaveBeenCalledTimes(1);

  const textareaProps = renderer.root.findByType("textarea").props as unknown as {
    onCompositionStart: () => void;
    onCompositionEnd: () => void;
  };
  await act(async () => textareaProps.onCompositionStart());
  await pressTextareaKey(renderer, { key: "Enter" });
  expect(props.onSubmit).toHaveBeenCalledTimes(1);

  await act(async () => textareaProps.onCompositionEnd());
  const trailingEnter = await pressTextareaKey(renderer, { key: "Enter" });
  expect(trailingEnter).toHaveBeenCalledTimes(1);
  expect(props.onSubmit).toHaveBeenCalledTimes(1);

  now.mockReturnValue(1_101);
  await pressTextareaKey(renderer, { key: "Enter" });
  expect(props.onSubmit).toHaveBeenCalledTimes(2);

  await pressTextareaKey(renderer, { key: "Enter", isComposing: true });
  await pressTextareaKey(renderer, { key: "Enter", keyCode: 229 });
  expect(props.onSubmit).toHaveBeenCalledTimes(2);

  now.mockRestore();
  await act(async () => renderer.unmount());
});

test("a selected slash command consumes Enter instead of submitting the prompt", async () => {
  const baseProps = {
    ...createComposerProps(),
    onSubmit: vi.fn(),
  };

  function Harness() {
    const [inputValue, setInputValue] = useState("");
    return (
      <ChatComposer
        {...baseProps}
        inputValue={inputValue}
        hasInput={Boolean(inputValue.trim())}
        onInputChange={setInputValue}
      />
    );
  }

  const rendered = { current: null as ReactTestRenderer | null };
  await act(async () => {
    rendered.current = create(<Harness />);
  });
  const renderer = rendered.current;
  if (!renderer) {
    throw new Error("Composer did not render");
  }

  await changeTextarea(renderer, "/state");
  await pressTextareaKey(renderer, { key: "Enter" });
  expect(baseProps.onSubmit).not.toHaveBeenCalled();
  expect(renderer.root.findAllByProps({ className: "contextWindowPanel" })).toHaveLength(1);

  await act(async () => renderer.unmount());
});

test("the slash palette exposes every command and routes compact once", async () => {
  const baseProps = {
    ...createComposerProps(),
    compactInteractive: true,
    onCompact: vi.fn(),
    onNewSession: vi.fn(),
    onOpenResource: vi.fn(),
  };

  function Harness() {
    const [inputValue, setInputValue] = useState("");
    return (
      <ChatComposer
        {...baseProps}
        inputValue={inputValue}
        hasInput={Boolean(inputValue.trim())}
        onInputChange={setInputValue}
      />
    );
  }

  const rendered = { current: null as ReactTestRenderer | null };
  await act(async () => {
    rendered.current = create(<Harness />);
  });
  const renderer = rendered.current;
  if (!renderer) {
    throw new Error("Composer did not render");
  }

  await changeTextarea(renderer, "/");
  const commandLabels = renderer.root
    .findByProps({ "aria-label": "Commands" })
    .findAllByType("strong")
    .map((node) => node.children.join(""));
  expect(commandLabels).toEqual([
    "/new",
    "/model",
    "/effort",
    "/state",
    "/compact",
    "/models",
    "/skills",
    "/plugins",
    "/mcp",
  ]);

  await changeTextarea(renderer, "/compact");
  await click(findButtonByStrongText(renderer, "/compact"));
  expect(baseProps.onCompact).toHaveBeenCalledTimes(1);
  expect(renderer.root.findAllByProps({ "aria-label": "Commands" })).toHaveLength(0);

  await act(async () => renderer.unmount());
});

test("the prompt input caps its height and restores focus after its action", async () => {
  const props = {
    ...createComposerProps(),
    inputValue: "a long prompt",
    hasInput: true,
    onComposerAction: vi.fn(),
  };
  const textareaNode = {
    focus: vi.fn(),
    scrollHeight: 320,
    style: { height: "" },
  };
  const rendered = { current: null as ReactTestRenderer | null };
  await act(async () => {
    rendered.current = create(<ChatComposer {...props} />, {
      createNodeMock: (element) => element.type === "textarea" ? textareaNode : null,
    });
  });
  const renderer = rendered.current;
  if (!renderer) {
    throw new Error("Composer did not render");
  }
  expect(textareaNode.style.height).toBe("200px");

  const sendButton = renderer.root
    .findAllByType("button")
    .find((candidate) => candidate.props["aria-label"] === "发送");
  if (!sendButton) {
    throw new Error("Missing send button");
  }
  await click(sendButton);
  expect(props.onComposerAction).toHaveBeenCalledTimes(1);
  expect(textareaNode.focus).toHaveBeenCalledTimes(1);

  await act(async () => {
    renderer.update(<ChatComposer {...props} inputValue="" hasInput={false} />);
  });
  expect(textareaNode.style.height).toBe("auto");

  await act(async () => renderer.unmount());
});

test("runtime controls select exact models and reasoning modes", async () => {
  const firstModel = {
    providerId: "provider-one",
    providerName: "Provider One",
    model: "shared-model",
    displayName: "Shared model",
    modelThinkingModes: ["low", "high"],
  } satisfies SelectableModel;
  const secondModel = {
    ...firstModel,
    providerId: "provider-two",
    providerName: "Provider Two",
  } satisfies SelectableModel;
  const props = {
    ...createComposerProps(),
    modelRuntimeSummary: "shared-model · provider-one",
    selectableModels: [firstModel, secondModel],
    activeModelIndex: 0,
    reasoningEffort: "low" as const,
    reasoningEfforts: ["low", "high"] as const,
    onModelSelect: vi.fn(),
    onReasoningEffortSelect: vi.fn(),
  };
  const rendered = { current: null as ReactTestRenderer | null };
  await act(async () => {
    rendered.current = create(<ChatComposer {...props} reasoningEfforts={[...props.reasoningEfforts]} />);
  });
  const renderer = rendered.current;
  if (!renderer) {
    throw new Error("Composer did not render");
  }

  const modelTrigger = renderer.root.findByProps({ title: props.modelRuntimeSummary });
  await click(modelTrigger);
  const modelList = renderer.root.findByProps({ "aria-label": "全局模型" });
  const modelOptions = modelList.findAllByProps({ role: "option" });
  expect(modelOptions).toHaveLength(2);

  const reasoningTrigger = renderer.root.findByProps({ "aria-label": "思考强度" });
  await click(reasoningTrigger);
  expect(renderer.root.findAllByProps({ "aria-label": "全局模型" })).toHaveLength(0);
  expect(renderer.root.findAllByProps({
    className: "composerPickerPanel is-reasoning",
  })).toHaveLength(1);
  await click(reasoningTrigger);
  expect(renderer.root.findAllByProps({
    className: "composerPickerPanel is-reasoning",
  })).toHaveLength(0);

  await click(modelTrigger);
  const reopenedModelList = renderer.root.findByProps({ "aria-label": "全局模型" });
  const reopenedModelOptions = reopenedModelList.findAllByProps({ role: "option" });
  await click(reopenedModelOptions[1]);
  expect(props.onModelSelect).toHaveBeenCalledWith(secondModel);
  expect(renderer.root.findAllByProps({ "aria-label": "全局模型" })).toHaveLength(0);

  await click(reasoningTrigger);
  const reasoningList = renderer.root.findByProps({
    className: "composerPickerPanel is-reasoning",
  });
  const highOption = reasoningList
    .findAllByType("button")
    .find((candidate) => candidate.children.some(
      (child) => typeof child !== "string" && child.children.join("") === "high",
    ));
  if (!highOption) {
    throw new Error("Missing high reasoning option");
  }
  await click(highOption);
  expect(props.onReasoningEffortSelect).toHaveBeenCalledWith("high");

  await act(async () => renderer.unmount());
});

test("zero-sized context usage renders finite percentages and MCP tool details", async () => {
  const props = {
    ...createComposerProps(),
    contextUsage: {
      sessionId: "session-one",
      usedTokens: 0,
      maxContextTokens: 0,
      usedPercentage: 0,
      isCompacting: false,
      breakdown: {
        systemPromptTokens: 0,
        systemToolTokens: 0,
        mcpToolTokens: 12,
        skillsTokens: 0,
        messageTokens: 0,
        autoCompactBufferTokens: 0,
        freeSpaceTokens: 0,
        mcpTools: [{ providerId: "provider-one", name: "search", tokens: 12 }],
      },
    } satisfies AgentContextUsageSummary,
  };
  const rendered = { current: null as ReactTestRenderer | null };
  await act(async () => {
    rendered.current = create(<ChatComposer {...props} />);
  });
  const renderer = rendered.current;
  if (!renderer) {
    throw new Error("Composer did not render");
  }

  await click(renderer.root.findByProps({ "aria-label": "Context window" }));
  const serialized = JSON.stringify(renderer.toJSON());
  expect(serialized).not.toContain("NaN");
  expect(serialized).not.toContain("Infinity");
  expect(serialized).toContain("provider-one · search");
  expect(serialized).toContain("12");

  await act(async () => renderer.unmount());
});
