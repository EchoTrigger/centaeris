import { createRoot } from "react-dom/client";
import { useState } from "react";
import { configureApi } from "../../src/api";
import { AgentDocumentIdentity } from "../../src/shell/useAgentDocumentIdentity";

configureApi({ apiBaseUrl: location.origin });
if (new URLSearchParams(location.search).has("withoutIcon")) document.querySelector('link[rel="icon"]')!.remove();
const first = { name: "Researcher", avatarKind: "centaeris" as const, avatarUrl: null as string | null };
const second = { name: "Builder", avatarKind: "banana" as const, avatarUrl: null };
function Fixture() {
  const [agent, setAgent] = useState<typeof first | null>(null);
  const [overlay, setOverlay] = useState<typeof first | null>(null);
  return <><AgentDocumentIdentity agent={agent} />{overlay ? <AgentDocumentIdentity agent={overlay} /> : null}
    <button type="button" onClick={() => setAgent(first)}>First Agent</button>
    <button type="button" onClick={() => setAgent(second)}>Second Agent</button>
    <button type="button" onClick={() => setAgent(value => value && { ...value, name: "Renamed researcher" })}>Rename Agent</button>
    <button type="button" onClick={() => setAgent({ ...first, avatarUrl: "/api/agents/fixture/avatar?v=valid" })}>Custom avatar</button>
    <button type="button" onClick={() => setAgent({ ...first, avatarUrl: "/api/agents/fixture/avatar?v=slow" })}>Slow avatar</button>
    <button type="button" onClick={() => setAgent({ ...first, avatarUrl: "/api/agents/fixture/avatar?v=missing" })}>Broken avatar</button>
    <button type="button" onClick={() => setAgent(null)}>Leave Agent</button>
    <button type="button" onClick={() => setOverlay(second)}>New page mounts</button>
    <button type="button" onClick={() => setOverlay(null)}>New page unmounts</button>
  </>;
}
createRoot(document.getElementById("fixture")!).render(<Fixture />);
