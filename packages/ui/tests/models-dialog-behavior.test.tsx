import type { ReactNode } from "react";
import type { ReactTestInstance } from "react-test-renderer";
import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { expect, test, vi } from "vitest";
import type { AgentRuntimeConfig } from "../src/lib/chatBridge";

const runtime = vi.hoisted(() => ({
  getAgentRuntimeConfig: vi.fn(),
  testAgentRuntimeModel: vi.fn(),
  setAgentRuntimeConfig: vi.fn(),
}));

vi.mock("../src/lib/chatBridge", () => ({
  getAgentRuntimeConfig: runtime.getAgentRuntimeConfig,
  resetAgentRuntimeConfig: vi.fn(),
  setAgentRuntimeConfig: runtime.setAgentRuntimeConfig,
  testAgentRuntimeModel: runtime.testAgentRuntimeModel,
}));

import { ModelsDialog } from "../src/components/ModelsDialog";

globalThis.IS_REACT_ACT_ENVIRONMENT = true;

const config = {
  executionHost: "localUser",
  autoContinueAfterResumeWait: false,
  modelProviders: [{
    providerId: "custom.test",
    name: "Test provider",
    builtIn: false,
    accessKind: "custom",
    configured: true,
    models: [
      { providerId: "custom.test", providerName: "Test provider", model: "model-one", modelThinkingModes: [], supportsVision: false, builtIn: false },
      { providerId: "custom.test", providerName: "Test provider", model: "model-two", modelThinkingModes: [], supportsVision: false, builtIn: false },
    ],
  }],
  selectableModels: [],
  customModelProviders: [{
    providerId: "custom.test",
    name: "Test provider",
    baseUrl: "https://example.test/v1",
    api: "openai-responses",
    models: [
      { model: "model-one", contextTokens: "128k", maxOutputTokens: "32k", supportsVision: false },
      { model: "model-two", contextTokens: "128k", maxOutputTokens: "32k", supportsVision: false },
    ],
  }],
  updatedAt: 1,
} satisfies AgentRuntimeConfig;

const text = (children: ReactNode): string => Array.isArray(children)
  ? children.map(text).join("")
  : typeof children === "string" || typeof children === "number"
    ? String(children)
    : "";

const button = (renderer: ReactTestRenderer, label: string): ReactTestInstance =>
  renderer.root.findAllByType("button").find((candidate) => text(candidate.props.children) === label && !candidate.props["aria-label"])
  ?? (() => { throw new Error(`Missing button ${label}`); })();

test("a model test result is cleared when the selected model changes", async () => {
  runtime.getAgentRuntimeConfig.mockResolvedValue(config);
  runtime.testAgentRuntimeModel.mockResolvedValue({
    httpStatus: 200,
    latencyMs: 12,
    outputPreview: "OK",
  });
  const rendered = { current: null as ReactTestRenderer | null };

  await act(async () => {
    rendered.current = create(
      <ModelsDialog
        onClose={() => {}}
        confirmAction={async () => true}
      />,
    );
    await Promise.resolve();
  });
  const renderer = rendered.current;
  if (!renderer) throw new Error("Models dialog did not render");

  await act(async () => renderer.root.findByProps({"aria-label":"Open Test provider"}).props.onClick());
  await act(async () => button(renderer!, "model-one").props.onClick());
  await act(async () => {
    button(renderer!, "Test").props.onClick();
    await Promise.resolve();
  });
  expect(renderer.root.findAllByProps({ className: "modelsTestSummary is-success" })).toHaveLength(1);

  await act(async () => renderer.root.findByProps({"aria-label":"Close detail"}).props.onClick());
  await act(async () => button(renderer!, "model-two").props.onClick());
  expect(renderer.root.findAllByProps({ className: "modelsTestSummary is-success" })).toHaveLength(0);

  await act(async () => renderer!.unmount());
});

test("service landing has no global save and cancelling a connection edit discards its draft", async () => {
 runtime.getAgentRuntimeConfig.mockResolvedValue(config);
 let view!:ReactTestRenderer;
 await act(async()=>{view=create(<ModelsDialog onClose={()=>{}} confirmAction={async()=>true}/>);});
 expect(view.root.findAllByType("button").filter(b=>text(b.props.children)==="Save")).toHaveLength(0);
 await act(async()=>view.root.findByProps({"aria-label":"Open Test provider"}).props.onClick());
 await act(async()=>button(view,"Edit connection").props.onClick());
 await act(async()=>view.root.findByProps({"aria-label":"Provider name"}).props.onChange({target:{value:"Changed"}}));
 await act(async()=>button(view,"Cancel").props.onClick());
 await act(async()=>button(view,"Edit connection").props.onClick());
 expect(view.root.findByProps({"aria-label":"Provider name"}).props.value).toBe("Test provider");
 await act(async()=>view.unmount());
});

test("connection save reports partial failure, retains the key for retry, and keeps the edit open", async () => {
 runtime.getAgentRuntimeConfig.mockResolvedValue(config);
 runtime.setAgentRuntimeConfig.mockReset();
 runtime.setAgentRuntimeConfig.mockResolvedValueOnce(config).mockRejectedValueOnce(new Error("credential store unavailable"));
 let view!:ReactTestRenderer;
 await act(async()=>{view=create(<ModelsDialog onClose={()=>{}} confirmAction={async()=>true}/>);});
 await act(async()=>view.root.findByProps({"aria-label":"Open Test provider"}).props.onClick());
 await act(async()=>button(view,"Edit connection").props.onClick());
 await act(async()=>view.root.findByProps({"aria-label":"API key"}).props.onChange({target:{value:"test-key"}}));
 await act(async()=>button(view,"Save").props.onClick());
 expect(runtime.setAgentRuntimeConfig.mock.calls[0][0]).toHaveProperty("customModelProviders");
 expect(runtime.setAgentRuntimeConfig.mock.calls[1][0]).toEqual({modelProviderId:"custom.test",modelApiKey:"test-key"});
 expect(view.root.findByProps({role:"status"}).props.children).toContain("API key was not saved");
 expect(view.root.findByProps({"aria-label":"API key"}).props.value).toBe("test-key");
 runtime.setAgentRuntimeConfig.mockResolvedValue(config);
 await act(async()=>button(view,"Save").props.onClick());
 expect(view.root.findAllByProps({"aria-label":"API key"})).toHaveLength(0);
 await act(async()=>view.unmount());
});
test("testing a model from the service list targets its provider and model", async () => {
 runtime.getAgentRuntimeConfig.mockResolvedValue(config);
 runtime.testAgentRuntimeModel.mockClear();
 runtime.testAgentRuntimeModel.mockResolvedValue({httpStatus:200,latencyMs:12,outputPreview:"OK"});
 let view!:ReactTestRenderer;
 await act(async()=>{view=create(<ModelsDialog onClose={()=>{}} confirmAction={async()=>true}/>);});
 await act(async()=>view.root.findByProps({"aria-label":"Open Test provider"}).props.onClick());
 await act(async()=>view.root.findByProps({"aria-label":"Test model-two"}).props.onClick());
 expect(runtime.testAgentRuntimeModel).toHaveBeenCalledWith({providerId:"custom.test",model:"model-two"});
 expect(view.root.findAllByType("dialog")).toHaveLength(0);
 await act(async()=>view.unmount());
});

test("built-in services expose API credentials without custom connection fields", async () => {
 runtime.getAgentRuntimeConfig.mockResolvedValue({...config, customModelProviders:[], modelProviders:[{
  ...config.modelProviders[0], providerId:"builtin.test", name:"Built-in service", builtIn:true, accessKind:"api_key",
 }]});
 let view!:ReactTestRenderer;
 await act(async()=>{view=create(<ModelsDialog onClose={()=>{}} confirmAction={async()=>true}/>);});
 await act(async()=>view.root.findByProps({"aria-label":"Open Built-in service"}).props.onClick());
 await act(async()=>button(view,"Edit connection").props.onClick());
 expect(view.root.findByProps({"aria-label":"API key"}).props.type).toBe("password");
 expect(view.root.findAllByProps({"aria-label":"Provider name"})).toHaveLength(0);
 expect(view.root.findAllByType("select")).toHaveLength(0);
 expect(view.root.findAllByType("button").filter(b=>text(b.props.children)==="Add model")).toHaveLength(0);
 await act(async()=>view.unmount());
});
