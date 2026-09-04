import {
  useCallback,
  useEffect,
  useRef,
  useState,
  type KeyboardEvent,
  type RefObject,
} from "react";
import { ArrowLeft, LockKeyhole } from "lucide-react";
import {
  configureNativeMcp,
  getNativeMcpCatalog,
  type NativeMcpCatalog,
  type NativeMcpServer,
} from "../../lib/chatBridge";

export type McpComposerMode = "mcp" | "mcp-configure" | null;

type UseMcpComposerControllerOptions = {
  mode: McpComposerMode;
  panelResetKey: string;
  onModeChange: (mode: McpComposerMode) => void;
  focusTextarea: () => void;
};

export type McpComposerController = {
  mode: McpComposerMode;
  catalog: NativeMcpCatalog | null;
  loading: boolean;
  error: string;
  notice: string;
  selectedIndex: number;
  configuringServer: NativeMcpServer | null;
  bearerToken: string;
  tokenInputRef: RefObject<HTMLInputElement | null>;
  openCatalog: () => void;
  openConfiguration: (server: NativeMcpServer) => void;
  backToCatalog: () => void;
  saveConfiguration: () => Promise<void>;
  setBearerToken: (value: string) => void;
  setSelectedIndex: (index: number) => void;
  handleKeyDown: (event: KeyboardEvent<HTMLTextAreaElement>) => boolean;
};

export const useMcpComposerController = ({
  mode,
  panelResetKey,
  onModeChange,
  focusTextarea,
}: UseMcpComposerControllerOptions): McpComposerController => {
  const tokenRef = useRef<HTMLInputElement>(null);
  const previousPanelResetKeyRef = useRef(panelResetKey);
  const panelOwnerRevisionRef = useRef(0);
  const catalogRequestRef = useRef(0);
  const [catalog, setCatalog] = useState<NativeMcpCatalog | null>(null);
  const [catalogLoading, setCatalogLoading] = useState(false);
  const [configurationLoading, setConfigurationLoading] = useState(false);
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");
  const [selectedIndex, setSelectedIndex] = useState(0);
  const [configuringServer, setConfiguringServer] = useState<NativeMcpServer | null>(null);
  const [bearerToken, setBearerToken] = useState("");
  if (previousPanelResetKeyRef.current !== panelResetKey) {
    previousPanelResetKeyRef.current = panelResetKey;
    panelOwnerRevisionRef.current += 1;
  }

  const loadCatalog = useCallback(async () => {
    const requestId = catalogRequestRef.current + 1;
    catalogRequestRef.current = requestId;
    setCatalogLoading(true);
    setError("");
    try {
      const nextCatalog = await getNativeMcpCatalog();
      if (catalogRequestRef.current !== requestId) {
        return;
      }
      setCatalog(nextCatalog);
      setSelectedIndex(0);
    } catch (loadError) {
      if (catalogRequestRef.current !== requestId) {
        return;
      }
      setError(loadError instanceof Error ? loadError.message : String(loadError));
    } finally {
      if (catalogRequestRef.current === requestId) {
        setCatalogLoading(false);
      }
    }
  }, []);

  const openCatalog = useCallback(() => {
    setNotice("");
    onModeChange("mcp");
    void loadCatalog();
    focusTextarea();
  }, [focusTextarea, loadCatalog, onModeChange]);

  const openConfiguration = useCallback((server: NativeMcpServer) => {
    if (!server.configurable || server.status === "disabled" || server.status === "unsupported") {
      return;
    }
    setConfiguringServer(server);
    setBearerToken("");
    setError("");
    onModeChange("mcp-configure");
    requestAnimationFrame(() => tokenRef.current?.focus());
  }, [onModeChange]);

  const backToCatalog = useCallback(() => {
    setBearerToken("");
    setError("");
    onModeChange("mcp");
  }, [onModeChange]);

  const saveConfiguration = useCallback(async () => {
    if (!configuringServer || !bearerToken) {
      return;
    }
    const requestOwner = panelOwnerRevisionRef.current;
    setConfigurationLoading(true);
    setError("");
    try {
      const nextCatalog = await configureNativeMcp({
        pluginName: configuringServer.pluginName,
        serverId: configuringServer.serverId,
        bearerToken,
      });
      if (panelOwnerRevisionRef.current !== requestOwner) {
        return;
      }
      setCatalog(nextCatalog);
      setBearerToken("");
      setNotice("Saved · applies to next run");
      onModeChange("mcp");
      focusTextarea();
    } catch (saveError) {
      if (panelOwnerRevisionRef.current !== requestOwner) {
        return;
      }
      setError(saveError instanceof Error ? saveError.message : String(saveError));
    } finally {
      if (panelOwnerRevisionRef.current === requestOwner) {
        setConfigurationLoading(false);
      }
    }
  }, [bearerToken, configuringServer, focusTextarea, onModeChange]);

  const handleKeyDown = useCallback((event: KeyboardEvent<HTMLTextAreaElement>): boolean => {
    if (mode !== "mcp") {
      return false;
    }
    const count = catalog?.servers.length ?? 0;
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      if (count) {
        const delta = event.key === "ArrowDown" ? 1 : -1;
        setSelectedIndex((current) => (current + delta + count) % count);
      }
      return true;
    }
    if (event.key === "Enter" && !event.shiftKey) {
      event.preventDefault();
      const server = catalog?.servers[selectedIndex];
      if (server) {
        openConfiguration(server);
      }
      return true;
    }
    return false;
  }, [catalog?.servers, mode, openConfiguration, selectedIndex]);

  useEffect(() => {
    previousPanelResetKeyRef.current = panelResetKey;
    catalogRequestRef.current += 1;
    setBearerToken("");
    setConfiguringServer(null);
    setError("");
    setCatalogLoading(false);
    setConfigurationLoading(false);
  }, [panelResetKey]);

  useEffect(() => {
    if (mode !== "mcp-configure") {
      setBearerToken("");
    }
  }, [mode]);

  return {
    mode,
    catalog,
    loading: catalogLoading || configurationLoading,
    error,
    notice,
    selectedIndex,
    configuringServer,
    bearerToken,
    tokenInputRef: tokenRef,
    openCatalog,
    openConfiguration,
    backToCatalog,
    saveConfiguration,
    setBearerToken,
    setSelectedIndex,
    handleKeyDown,
  };
};

export function McpComposerPanels({
  controller,
}: {
  controller: McpComposerController;
}) {
  const {
    mode,
    catalog,
    loading,
    error,
    notice,
    selectedIndex,
    configuringServer,
    bearerToken,
    tokenInputRef,
    openConfiguration,
    backToCatalog,
    saveConfiguration,
    setBearerToken,
    setSelectedIndex,
  } = controller;

  if (mode === "mcp") {
    return (
      <section className="slashCommandPanel mcpComposerPanel" aria-label="MCP servers">
        <header>
          <strong>MCP</strong>
          <span>{notice || `${catalog?.servers.length ?? 0} servers`}</span>
        </header>
        <div className="mcpServerList" role="listbox">
          {loading && !catalog ? <p>Loading MCP servers…</p> : null}
          {error ? <p className="mcpPanelError" role="status">{error}</p> : null}
          {!loading && !error && !catalog?.servers.length ? <p>No MCP servers</p> : null}
          {catalog?.servers.map((server, index) => {
            const actionable = server.configurable
              && server.status !== "disabled"
              && server.status !== "unsupported";
            const status = server.status === "needsConfiguration"
              ? "Configure"
              : server.status === "disabled"
                ? "Plugin disabled"
                : server.status === "unsupported"
                  ? "Unsupported"
                  : server.configurable
                    ? "Configured"
                    : "Managed";
            return (
              <button
                type="button"
                role="option"
                aria-selected={index === selectedIndex}
                className={`mcpServerRow ${index === selectedIndex ? "is-selected" : ""} ${actionable ? "" : "is-locked"}`}
                disabled={!actionable}
                key={`${server.pluginName}:${server.serverId}`}
                onMouseEnter={() => setSelectedIndex(index)}
                onClick={() => openConfiguration(server)}
              >
                <span className="mcpServerIdentity">
                  <strong>{server.serverId}</strong>
                  <small>{server.pluginDisplayName} · {server.toolNames.length} tool{server.toolNames.length === 1 ? "" : "s"}</small>
                </span>
                <span className={`mcpServerStatus is-${server.status}`}>
                  {!actionable ? <LockKeyhole aria-hidden="true" /> : null}
                  {status}
                </span>
              </button>
            );
          })}
        </div>
      </section>
    );
  }

  if (mode !== "mcp-configure" || !configuringServer) {
    return null;
  }

  return (
    <section className="slashCommandPanel mcpComposerPanel" aria-label="Configure MCP server">
      <header>
        <button
          type="button"
          className="mcpPanelBack"
          aria-label="Back to MCP servers"
          onClick={backToCatalog}
        >
          <ArrowLeft aria-hidden="true" />
          MCP
        </button>
        <span>{configuringServer.serverId}</span>
      </header>
      <form
        className="mcpConfigForm"
        onSubmit={(event) => {
          event.preventDefault();
          void saveConfiguration();
        }}
      >
        <label>
          <span>API key</span>
          <input
            ref={tokenInputRef}
            type="password"
            autoComplete="new-password"
            value={bearerToken}
            disabled={loading}
            onChange={(event) => setBearerToken(event.target.value)}
          />
        </label>
        <p className="mcpEndpoint" title={configuringServer.endpoint ?? ""}>
          {configuringServer.endpoint}
        </p>
        {error ? <p className="mcpPanelError" role="status">{error}</p> : null}
        <div className="mcpConfigActions">
          <button type="button" onClick={backToCatalog}>
            Cancel
          </button>
          <button type="submit" className="is-primary" disabled={!bearerToken || loading}>
            {loading ? "Testing…" : "Save & test"}
          </button>
        </div>
      </form>
    </section>
  );
}
