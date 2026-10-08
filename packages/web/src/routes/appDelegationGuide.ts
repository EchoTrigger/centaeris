import { chatGuide } from "./chatGuide.ts";
type DelegationTarget = { agentId: string | null; definitionId: string | null; workspaceId: string };
type GuideStep = {
  key: string; method: "GET" | "POST"; path: string; requiredScopes: string[];
  request: string | null; response: string; headers?: Record<string, string>;
};
export type DelegationGuide = {
  kind: "native" | "published"; steps: GuideStep[];
  examples: { curl: string; python: string; javascript: string };
};

/** Public chat transport shared by native and published targets. */
export function buildDelegationGuide(apiBaseUrl: string, target: DelegationTarget): DelegationGuide {
  const url = new URL(apiBaseUrl);
  if (!["http:", "https:"].includes(url.protocol) || url.username || url.password || url.search || url.hash) {
    throw new Error("Invalid configured API address");
  }
  if (!target.workspaceId || Boolean(target.agentId) === Boolean(target.definitionId)) {
    throw new Error("Expected exactly one delegation target and a Workspace");
  }
  const guide = chatGuide(apiBaseUrl.replace(/\/+$/, ""), target.agentId || target.definitionId!);
  return { ...guide, kind: target.agentId ? "native" : "published" };
}
