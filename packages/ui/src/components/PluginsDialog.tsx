import { type ReactNode, useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Blocks, ChevronRight, FolderOpen, Package, Plus, Plug, RefreshCw, Trash2 } from "lucide-react";
import { getPluginDetail, listPlugins, removePlugin, reloadPlugins, revealPluginSourceRef, selectAndInstallPlugin, setPluginEnabled, type PluginDescriptorV1, type PluginDetailV1 } from "../lib/chatBridge";
import { Button } from "./ui/button";
import { ConfirmDialog } from "./ConfirmDialog";
import { ManagementBack, ManagementEmpty, ManagementError, ManagementFilter, ManagementHeader, ManagementLoading, ManagementSearch, ManagementStatus, ManagementToggle } from "./management/ManagementPage";

const message = (error: unknown) => error instanceof Error ? error.message : String(error || "Plugin request failed");
function pluginStatus(item: PluginDescriptorV1) {
  return item.errors.length ? "Needs attention" : item.enabled ? "Enabled" : "Disabled";
}
export function PluginsDialog({ selectedPluginId, onSelect }: { selectedPluginId?: string; onSelect?: (id: string) => void }) {
  const [focusOnNavigation, setFocusOnNavigation] = useState(false);
  const [query, setQuery] = useState("");
  const [filter, setFilter] = useState("all");
  const [localSelection, setLocalSelection] = useState("");
  const selectedId = selectedPluginId ?? localSelection;
  const selectionRef = useRef(selectedId); selectionRef.current = selectedId;
  const navigationOwner = useRef({ selectedId });
  if (navigationOwner.current.selectedId !== selectedId) navigationOwner.current = { selectedId };
  const select = useCallback((id: string) => { navigationOwner.current = { selectedId: id }; setFocusOnNavigation(true); if (onSelect) onSelect(id); else setLocalSelection(id); }, [onSelect]);
  const [items, setItems] = useState<PluginDescriptorV1[]>([]);
  const [detail, setDetail] = useState<PluginDetailV1 | null>(null);
  const [loading, setLoading] = useState(true);
  const [detailLoading, setDetailLoading] = useState(false);
  const [catalogError, setCatalogError] = useState("");
  const [detailError, setDetailError] = useState("");
  const [actionError, setActionError] = useState("");
  const [notice, setNotice] = useState("");
  const [pending, setPending] = useState("");
  const [removing, setRemoving] = useState<PluginDescriptorV1 | null>(null);
  const owner = useRef(0), listRequest = useRef(0), detailRequest = useRef(0);
  const busy = useRef(false);
  const load = useCallback(async (reload = false) => {
    const request = ++listRequest.current, epoch = owner.current;
    setLoading(true); setCatalogError("");
    try {
      if (reload) await reloadPlugins();
      if (epoch !== owner.current || request !== listRequest.current) return;
      const result = await listPlugins();
      if (epoch === owner.current && request === listRequest.current) setItems(result);
    } catch (error) {
      if (epoch === owner.current && request === listRequest.current) setCatalogError(message(error));
    } finally { if (epoch === owner.current && request === listRequest.current) setLoading(false); }
  }, []);
  const loadDetail = useCallback(async (id: string) => {
    const request = ++detailRequest.current, epoch = owner.current;
    setDetailLoading(true); setDetailError("");
    try {
      const result = await getPluginDetail({ id });
      if (epoch === owner.current && request === detailRequest.current) setDetail(result);
    } catch (error) {
      if (epoch === owner.current && request === detailRequest.current) setDetailError(message(error));
    } finally { if (epoch === owner.current && request === detailRequest.current) setDetailLoading(false); }
  }, []);
  useEffect(() => {
    void load();
    return () => { owner.current++; listRequest.current++; detailRequest.current++; };
  }, [load]);
  useEffect(() => {
    detailRequest.current++;
    setDetailError(""); setActionError(""); setDetailLoading(false);
    if (selectedId) void loadDetail(selectedId);
  }, [selectedId, loadDetail]);
  const mutate = async (key: string, action: (isCurrent: () => boolean) => Promise<void>) => {
    if (busy.current) return;
    busy.current = true; setPending(key); setActionError(""); setNotice("");
    const epoch = owner.current;
    const navigation = navigationOwner.current;
    const isCurrent = () => epoch === owner.current;
    try { await action(isCurrent); }
    catch (error) { if (isCurrent() && navigation === navigationOwner.current) setActionError(message(error)); }
    finally { busy.current = false; if (isCurrent()) setPending(""); }
  };
  const install = () => {
    const navigation = navigationOwner.current;
    return mutate("install", async isCurrent => {
      const receipt = await selectAndInstallPlugin();
      if (!isCurrent()) return;
      if (receipt.cancelled) { setNotice("Installation cancelled."); return; }
      if (!receipt.plugin) throw new Error("The host did not return an installed plugin. Refresh the catalog before trying again.");
      const installed = receipt.plugin;
      setItems(previous => [...previous.filter(item => item.id !== installed.descriptor.id), installed.descriptor]);
      if (navigation === navigationOwner.current) { setDetail(installed); select(installed.descriptor.id); }
      setNotice(`${installed.descriptor.name} installed.`);
      await load();
    });
  };
  const selectedItem = items.find(item => item.id === selectedId) ?? (detail?.descriptor.id === selectedId ? detail.descriptor : null);
  const selectedDetail = detail?.descriptor.id === selectedId ? detail : null;
  const visible = useMemo(() => items.filter(item => {
    const matches = `${item.name} ${item.description} ${item.source}`.toLowerCase().includes(query.trim().toLowerCase());
    return matches && (filter === "all" || filter === "attention" && item.errors.length > 0 || filter === "enabled" && item.enabled || filter === "disabled" && !item.enabled);
  }).sort((a, b) => a.name.localeCompare(b.name)), [items, query, filter]);
  const toggle = (item: PluginDescriptorV1) => mutate(item.id, async isCurrent => {
    const enabled = !item.enabled;
    await setPluginEnabled({ id: item.id, enabled });
    if (!isCurrent()) return;
    setItems(previous => previous.map(candidate => candidate.id === item.id ? { ...candidate, enabled } : candidate));
    setDetail(previous => previous?.descriptor.id === item.id ? { ...previous, descriptor: { ...previous.descriptor, enabled } } : previous);
    setNotice(`${item.name} ${enabled ? "enabled" : "disabled"}.`);
    await load();
    if (isCurrent() && selectionRef.current === item.id) await loadDetail(item.id);
  });
  const reveal = (item: PluginDescriptorV1) => mutate("reveal", async () => {
    const receipt = await revealPluginSourceRef({ id: item.id });
    if (!receipt.opened) throw new Error("The folder could not be opened. Check that this plugin location is available.");
  });
  const confirmRemove = () => {
    const item = removing; if (!item) return;
    setRemoving(null);
    void mutate(item.id, async isCurrent => {
      const receipt = await removePlugin({ id: item.id });
      if (!isCurrent()) return;
      if (receipt.removedId !== item.id) throw new Error("The host returned a different removal receipt. Refresh the catalog.");
      setItems(previous => previous.filter(candidate => candidate.id !== item.id));
      if (selectionRef.current === item.id) { detailRequest.current++; setDetail(null); select(""); }
      setNotice(`${item.name} removed.`);
      await load();
    });
  };
  return <section className="desktopManagement" aria-label="Plugins"><main className="dmPage">
    {selectedId ? <>
      <ManagementBack onClick={() => select("")}>Back to plugins</ManagementBack>
      <ManagementHeader focusOnNavigation={focusOnNavigation} title={selectedItem?.name ?? "Plugin"} description={selectedItem?.description}
        actions={selectedItem ? <ManagementToggle checked={selectedItem.enabled} disabled={Boolean(pending)} label={selectedItem.enabled ? "Disable plugin" : "Enable plugin"} onChange={() => void toggle(selectedItem)} /> : null} />
      {actionError ? <ManagementError>{actionError}</ManagementError> : null}
      {notice ? <p className="dmNotice" role="status">{notice}</p> : null}
      {detailError ? <ManagementError onRetry={() => void loadDetail(selectedId)}>Unable to load plugin details. {detailError}</ManagementError> : null}
      {selectedItem ? <>
        <div className="dmMeta"><ManagementStatus tone={selectedItem.errors.length ? "warning" : selectedItem.enabled ? "success" : "neutral"}>{pluginStatus(selectedItem)}</ManagementStatus><span>{selectedItem.source}</span>{selectedItem.version ? <span>Version {selectedItem.version}</span> : null}</div>
        <div className="dmActions"><Button variant="outline" disabled={Boolean(pending)} onClick={() => void reveal(selectedItem)}><FolderOpen />Show in folder</Button>{selectedItem.source === "managed" ? <Button variant="destructive" disabled={Boolean(pending)} onClick={() => setRemoving(selectedItem)}><Trash2 />Remove plugin</Button> : null}</div>
        {selectedItem.errors.map(error => <ManagementError key={error}>{error}</ManagementError>)}
        {detailLoading ? <ManagementLoading>Loading plugin details…</ManagementLoading> : null}
        {selectedDetail ? <PluginCapabilities detail={selectedDetail} /> : null}
        <section className="dmSection"><h2>Information</h2><dl className="dmFacts"><div><dt>Source</dt><dd>{selectedItem.source}</dd></div><div><dt>Location</dt><dd>{selectedItem.path}</dd></div>{selectedItem.manifestPath ? <div><dt>Manifest</dt><dd>{selectedItem.manifestPath}</dd></div> : null}<div><dt>Activation</dt><dd>{selectedItem.activationStatus}</dd></div>{selectedItem.scopes.length ? <div><dt>Scopes</dt><dd>{selectedItem.scopes.join(", ")}</dd></div> : null}</dl></section>
      </> : detailLoading || loading ? <ManagementLoading>Loading plugin…</ManagementLoading> : !detailError ? <ManagementEmpty title="Plugin is unavailable" description="It may have been removed or its location may be disconnected." action={<Button variant="outline" onClick={() => void load(true)}>Refresh plugins</Button>} /> : null}
    </> : <>
      <ManagementHeader focusOnNavigation={focusOnNavigation} title="Plugins" description="Manage the tools and integrations available to your agent." actions={<><Button variant="outline" aria-label="Reload plugins" disabled={loading || Boolean(pending)} onClick={() => void load(true)}><RefreshCw />Refresh</Button><Button disabled={Boolean(pending)} onClick={() => void install()}><Plus />{pending === "install" ? "Installing…" : "Install plugin"}</Button></>} />
      <div className="dmToolbar"><ManagementSearch label="Search plugins" value={query} onChange={setQuery} /><ManagementFilter label="Plugin status" value={filter} onChange={setFilter} options={[["all", "All statuses"], ["enabled", "Enabled"], ["disabled", "Disabled"], ["attention", "Needs attention"]]} /><span className="dmCount">{visible.length} of {items.length} plugins</span></div>
      {catalogError ? <ManagementError onRetry={() => void load(true)}>Unable to refresh plugins. {catalogError}</ManagementError> : null}
      {actionError ? <ManagementError>{actionError}</ManagementError> : null}
      {notice ? <p className="dmNotice" role="status">{notice}</p> : null}
      {loading ? <ManagementLoading>{items.length ? "Refreshing plugins…" : "Loading plugins…"}</ManagementLoading> : null}
      {visible.length ? <div className="dmList" aria-label="Installed plugins" aria-busy={loading}>{visible.map(item => <Button variant="ghost" key={item.id} className="dmRow" aria-label={`Open ${item.name}`} onClick={() => select(item.id)}><span className="dmRowIcon"><Blocks /></span><span className="dmRowCopy"><strong>{item.name}</strong><small>{item.description || item.source}</small></span><span className="dmRowEnd"><ManagementStatus tone={item.errors.length ? "warning" : item.enabled ? "success" : "neutral"}>{pluginStatus(item)}</ManagementStatus><ChevronRight size={15} aria-hidden="true" /></span></Button>)}</div> : !loading && !catalogError ? <ManagementEmpty title={items.length ? "No matching plugins" : "No plugins installed"} description={items.length ? "Try another search or status filter." : "Install a plugin from a local package to add tools and integrations."} action={items.length ? <Button variant="outline" onClick={() => { setQuery(""); setFilter("all"); }}>Clear filters</Button> : <Button disabled={Boolean(pending)} onClick={() => void install()}><Plus />Install plugin</Button>} /> : null}
    </>}
    {selectedId && catalogError ? <ManagementError onRetry={() => void load()}>The catalog could not be refreshed. Your current view is kept. {catalogError}</ManagementError> : null}
    <ConfirmDialog open={Boolean(removing)} title="Remove this plugin?" message={`${removing?.name ?? "This plugin"} and its managed package will be removed. You can install it again later.`} onCancel={() => setRemoving(null)} onConfirm={confirmRemove} />
  </main></section>;
}
function PluginCapabilities({ detail }: { detail: PluginDetailV1 }) {
  const sections: [string, string[], ReactNode][] = [["Skills", detail.capabilities.skills, <Package size={16} />], ["Capabilities", detail.capabilities.capabilities, <Plug size={16} />], ["MCP servers", detail.capabilities.mcpServers, <Plug size={16} />], ["Apps", detail.capabilities.apps, <Blocks size={16} />], ["CLI", detail.capabilities.cli, <Package size={16} />], ["Hooks", detail.capabilities.hooks, <Package size={16} />]];
  return <>{sections.filter(([, values]) => values.length).map(([name, values, icon]) => <section className="dmSection" key={name}><h2>{name} <ManagementStatus>{values.length}</ManagementStatus></h2><ul className="dmCapabilityList">{values.map(value => <li key={value}>{icon}<span>{value}</span></li>)}</ul></section>)}{sections.every(([, values]) => !values.length) ? <section className="dmSection"><h2>Capabilities</h2><p>This plugin does not expose any capabilities.</p></section> : null}</>;
}
export default PluginsDialog;
