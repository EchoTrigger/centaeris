import { createRoot } from "react-dom/client";
import { useState } from "react";
import { useWorkspaceDocumentIdentity } from "../../src/shell/useWorkspaceDocumentIdentity";

if (new URLSearchParams(location.search).has("withoutIcon")) document.querySelector('link[rel="icon"]')!.remove();
function Workspace({ id, name }: { id: string; name: string }) {
  useWorkspaceDocumentIdentity({ id, name });
  const [page, setPage] = useState("First Agent");
  return <><h1>{page}</h1>{["First Agent", "Second Agent", "Session", "Settings", "Renamed Agent with custom avatar"].map(label =>
    <button key={label} type="button" onClick={() => setPage(label)}>{label}</button>)}</>;
}
function Fixture() {
  const [workspace, setWorkspace] = useState<{ id: string; name: string } | null>(null);
  return <>{workspace ? <Workspace key={workspace.id} {...workspace} /> : null}
    <button type="button" onClick={() => setWorkspace({ id: "research", name: "Research workspace" })}>Open workspace</button>
    <button type="button" onClick={() => setWorkspace({ id: "research", name: "Renamed workspace" })}>Rename workspace</button>
    <button type="button" onClick={() => setWorkspace({ id: "build", name: "Build workspace" })}>Switch workspace</button>
    <button type="button" onClick={() => setWorkspace(null)}>Leave workspace</button>
  </>;
}
createRoot(document.getElementById("fixture")!).render(<Fixture />);
