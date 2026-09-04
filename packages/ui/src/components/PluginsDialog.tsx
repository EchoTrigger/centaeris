import { type ReactNode, useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  FolderOpen,
  Package,
  Plus,
  Plug,
  RefreshCw,
  Search,
  Trash2,
} from "lucide-react";
import {
  getPluginDetail,
  listPlugins,
  removePlugin,
  reloadPlugins,
  revealPluginSourceRef,
  selectAndInstallPlugin,
  setPluginEnabled,
  type PluginDescriptorV1,
  type PluginDetailV1,
} from "../lib/chatBridge";

type PluginsDialogState = {
  items: PluginDescriptorV1[];
  detail: PluginDetailV1 | null;
  loading: boolean;
  detailLoading: boolean;
  error: string;
  updatingId: string;
};

const EMPTY_STATE: PluginsDialogState = {
  items: [],
  detail: null,
  loading: false,
  detailLoading: false,
  error: "",
  updatingId: "",
};

const errorMessage = (error: unknown): string =>
  error instanceof Error ? error.message : String(error || "plugin request failed");

export function PluginsDialog() {
  const [query, setQuery] = useState("");
  const [selectedId, setSelectedId] = useState("");
  const [state, setState] = useState<PluginsDialogState>(EMPTY_STATE);
  const detailSequence = useRef(0);

  const load = useCallback(async () => {
    setState((previous) => ({ ...previous, loading: true, error: "" }));
    try {
      const items = await listPlugins();
      setState((previous) => ({ ...previous, items, loading: false, error: "" }));
      setSelectedId((previous) => items.some((item) => item.id === previous) ? previous : "");
    } catch (error) {
      setState((previous) => ({
        ...previous,
        loading: false,
        error: errorMessage(error),
      }));
    }
  }, []);

  const loadDetail = useCallback(async (item: PluginDescriptorV1) => {
    const sequence = ++detailSequence.current;
    setState((previous) => ({ ...previous, detailLoading: true, error: "" }));
    try {
      const detail = await getPluginDetail({ id: item.id });
      if (sequence !== detailSequence.current) return;
      setState((previous) => ({ ...previous, detail, detailLoading: false }));
    } catch (error) {
      if (sequence !== detailSequence.current) return;
      setState((previous) => ({
        ...previous,
        detailLoading: false,
        error: errorMessage(error),
      }));
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  useEffect(() => {
    const item = state.items.find((candidate) => candidate.id === selectedId);
    if (item) {
      void loadDetail(item);
    } else {
      detailSequence.current += 1;
      setState((previous) => ({ ...previous, detail: null, detailLoading: false }));
    }
  }, [loadDetail, selectedId, state.items]);

  const visibleItems = useMemo(() => {
    const normalizedQuery = query.trim().toLowerCase();
    return state.items.filter((item) => {
      if (!normalizedQuery) return true;
      return `${item.name} ${item.description} ${item.source}`
        .toLowerCase()
        .includes(normalizedQuery);
    });
  }, [query, state.items]);

  const selectedItem = useMemo(
    () => state.items.find((item) => item.id === selectedId) ?? null,
    [selectedId, state.items],
  );
  const selectedDetail = state.detail?.descriptor.id === selectedId ? state.detail : null;

  const toggleEnabled = async (item: PluginDescriptorV1) => {
    setState((previous) => ({ ...previous, updatingId: item.id, error: "" }));
    try {
      await setPluginEnabled({ id: item.id, enabled: !item.enabled });
      await load();
      setState((previous) => ({ ...previous, updatingId: "" }));
    } catch (error) {
      setState((previous) => ({
        ...previous,
        updatingId: "",
        error: errorMessage(error),
      }));
    }
  };

  const reload = async () => {
    setState((previous) => ({ ...previous, loading: true, error: "" }));
    try {
      await reloadPlugins();
      await load();
    } catch (error) {
      setState((previous) => ({
        ...previous,
        loading: false,
        error: errorMessage(error),
      }));
    }
  };

  const install = async () => {
    setState((previous) => ({ ...previous, loading: true, error: "" }));
    try {
      const response = await selectAndInstallPlugin();
      if (response.cancelled) {
        setState((previous) => ({ ...previous, loading: false }));
        return;
      }
      await load();
      if (response.plugin) setSelectedId(response.plugin.descriptor.id);
    } catch (error) {
      setState((previous) => ({
        ...previous,
        loading: false,
        error: errorMessage(error),
      }));
    }
  };

  const remove = async (item: PluginDescriptorV1) => {
    setState((previous) => ({ ...previous, updatingId: item.id, error: "" }));
    try {
      await removePlugin({ id: item.id });
      setSelectedId("");
      await load();
      setState((previous) => ({ ...previous, updatingId: "" }));
    } catch (error) {
      setState((previous) => ({
        ...previous,
        updatingId: "",
        error: errorMessage(error),
      }));
    }
  };

  const showSourceRef = async (item: PluginDescriptorV1) => {
    setState((previous) => ({ ...previous, error: "" }));
    try {
      await revealPluginSourceRef({ id: item.id });
    } catch (error) {
      setState((previous) => ({ ...previous, error: errorMessage(error) }));
    }
  };

  return (
    <main className="pluginsMain">
      <div className="pluginsSplitLayout">
        <aside className="pluginsSidebar" aria-label="Plugins">
          <label className="pluginsSearch">
            <Search size={16} aria-hidden="true" />
            <input
              value={query}
              onChange={(event) => setQuery(event.target.value)}
              placeholder="Search plugins"
              aria-label="Search plugins"
            />
          </label>
          <div className="pluginsList" aria-busy={state.loading}>
            {state.loading && state.items.length === 0 ? <div className="pluginsSidebarEmpty">Loading…</div> : null}
            {!state.loading && state.items.length === 0 ? <div className="pluginsSidebarEmpty">No plugins</div> : null}
            {state.items.length > 0 && visibleItems.length === 0 ? <div className="pluginsSidebarEmpty">No matches</div> : null}
            {visibleItems.map((item) => (
              <button
                type="button"
                className={`pluginListItem ${item.id === selectedId ? "is-active" : ""}`}
                key={item.id}
                onClick={() => setSelectedId(item.id)}
                aria-current={item.id === selectedId ? "true" : undefined}
              >
                <div className="pluginListCopy">
                  <strong>{item.name}</strong>
                </div>
                <span
                  className={`pluginStatus ${item.errors.length > 0 ? "is-diagnostic" : item.enabled ? "is-enabled" : ""}`}
                  title={item.errors[0] || (item.enabled ? "Enabled" : "Disabled")}
                />
              </button>
            ))}
          </div>
          <div className="pluginsSidebarActions">
            <button type="button" className="modelsAddButton modelsAddProvider pluginsReload" onClick={() => void install()} disabled={state.loading}>
              <Plus aria-hidden="true" /> Install plugin
            </button>
            <button type="button" className="modelsAddButton modelsAddProvider pluginsReload" onClick={() => void reload()} disabled={state.loading}>
              <RefreshCw aria-hidden="true" /> Reload plugins
            </button>
          </div>
        </aside>
        <section className="pluginsDetailPane" aria-live="polite">
          {state.error ? <section className="pluginsMessage"><strong>{state.error}</strong></section> : null}
          {selectedItem ? (
            <PluginDetailView
              detail={selectedDetail}
              item={selectedItem}
              loading={state.detailLoading}
              updatingId={state.updatingId}
              onReveal={showSourceRef}
              onRemove={remove}
              onToggle={toggleEnabled}
            />
          ) : (
            <div className="pluginsEmptyMain"><strong>Select a plugin</strong><span>Choose a plugin to inspect its capabilities and source.</span></div>
          )}
        </section>
      </div>
    </main>
  );
}

function PluginDetailView({
  detail,
  item,
  loading,
  updatingId,
  onReveal,
  onRemove,
  onToggle,
}: {
  detail: PluginDetailV1 | null;
  item: PluginDescriptorV1;
  loading: boolean;
  updatingId: string;
  onReveal: (item: PluginDescriptorV1) => void;
  onRemove: (item: PluginDescriptorV1) => void;
  onToggle: (item: PluginDescriptorV1) => void;
}) {
  return (
    <article className="pluginDetail">
      <header className="pluginDetailHeader">
        <div className="pluginDetailTitle">
          <span className="pluginEyebrow">PLUGIN</span>
          <h1>{item.name}</h1>
          {item.description ? <p>{item.description}</p> : null}
        </div>
        <div className="pluginDetailActions">
          <button
            type="button"
            className="resourceSwitch"
            onClick={() => void onToggle(item)}
            disabled={updatingId === item.id}
            aria-label={item.enabled ? "Disable plugin" : "Enable plugin"}
            aria-pressed={item.enabled}
            title={item.enabled ? "Disable" : "Enable"}
          ><span className={item.enabled ? "is-on" : ""} /></button>
          {item.source === "managed" ? (
            <button
              type="button"
              className="pluginsRevealButton pluginsRemoveButton"
              onClick={() => void onRemove(item)}
              disabled={updatingId === item.id}
              aria-label="Remove plugin"
            >
              <Trash2 aria-hidden="true" /> Remove
            </button>
          ) : null}
          <button
            type="button"
            className="pluginsRevealButton"
            onClick={() => void onReveal(item)}
            aria-label="Show plugin in folder"
          >
            <FolderOpen aria-hidden="true" /> Show in folder
          </button>
        </div>
      </header>
      {item.errors.map((error) => <p className="pluginsDiagnostic" key={error}>{error}</p>)}
      {loading && !detail ? <div className="pluginsDetailLoading">Loading details…</div> : null}
      <PluginDetailBody item={item} capabilities={detail?.capabilities} />
    </article>
  );
}

function PluginDetailBody({
  item,
  capabilities,
}: {
  item: PluginDescriptorV1;
  capabilities?: PluginDetailV1["capabilities"];
}) {
  const apps = capabilities?.apps ?? [];
  const mcpServers = capabilities?.mcpServers ?? [];
  const skills = capabilities?.skills ?? [];
  const cli = capabilities?.cli ?? [];
  const hooks = capabilities?.hooks ?? [];
  const capabilityNames = capabilities?.capabilities ?? item.tools;
  return (
    <div className="pluginDetailBody">
      {skills.length > 0 ? <PluginCapabilitySection title="Skills" values={skills} icon={<Package size={18} />} /> : null}
      {cli.length > 0 ? <PluginCapabilitySection title="CLI" values={cli} icon={<Package size={18} />} /> : null}
      {mcpServers.length > 0 ? <PluginCapabilitySection title="MCP servers" values={mcpServers} icon={<Plug size={18} />} /> : null}
      {apps.length > 0 ? <PluginCapabilitySection title="Apps" values={apps} icon={<Plug size={18} />} /> : null}
      {hooks.length > 0 ? <PluginCapabilitySection title="Hooks" values={hooks} icon={<Package size={18} />} /> : null}
      <h2 className="pluginInfoHeading">INFORMATION</h2>
      <dl className="pluginInfoList">
        {capabilityNames.length > 0 ? <div><dt>Capabilities</dt><dd>{capabilityNames.join(", ")}</dd></div> : null}
        <div><dt>Source</dt><dd>{item.source}</dd></div>
        {item.version ? <div><dt>Version</dt><dd>{item.version}</dd></div> : null}
        <div><dt>Location</dt><dd>{item.path}</dd></div>
        {item.manifestPath ? <div><dt>Manifest</dt><dd>{item.manifestPath}</dd></div> : null}
      </dl>
    </div>
  );
}

function PluginCapabilitySection({
  title,
  values,
  icon,
}: {
  title: string;
  values: string[];
  icon: ReactNode;
}) {
  return (
    <section className="pluginCapabilitySection">
      <h2>{title} <span>{values.length}</span></h2>
      {values.map((value) => (
        <div className="pluginCapabilityRow" key={`${title}-${value}`}>
          <div className="pluginCapabilityIcon">{icon}</div>
          <div><strong>{value.split(/[\\/]/).pop() || value}</strong><span>{value}</span></div>
        </div>
      ))}
    </section>
  );
}

export default PluginsDialog;
