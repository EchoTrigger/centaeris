
import { useTranslation } from "../i18n";
import { useEffect, useRef, useState } from "react";
import { apiJson } from "../api";
import { dismissDetailsOnOutsideInteraction } from "./detailsDismissal";

function formatTokens(value) {
  if (!Number.isFinite(value)) return "0";
  if (value >= 1000) return `${(value / 1000).toFixed(value >= 100000 ? 0 : 1).replace(/\.0$/, "")}k`;
  return String(value);
}

export function ContextUsagePicker({ sessionId, isRunning }) {
  const { t } = useTranslation();
  const [contextUsage, setContextUsage] = useState(null);
  const pickerRef = useRef(null);

  useEffect(() => {
    if (!sessionId || !pickerRef.current) return undefined;
    pickerRef.current.open = false;
    return dismissDetailsOnOutsideInteraction(pickerRef.current);
  }, [sessionId]);

  useEffect(() => {
    let active = true;
    let timer;
    setContextUsage(null);
    if (!sessionId) return () => { active = false; };
    async function refresh() {
      try {
        const result = await apiJson(`/api/sessions/${sessionId}/context-usage`);
        if (result.schema !== "session.context_usage.v1" || result.sessionId !== sessionId) {
          throw new Error("session_context_usage_identity_mismatch");
        }
        if (active) setContextUsage(result.contextUsage);
      } catch {
        // Keep the latest committed request boundary during transient reads.
      }
    }
    void refresh();
    if (isRunning) timer = window.setInterval(refresh, 1000);
    return () => {
      active = false;
      if (timer) window.clearInterval(timer);
    };
  }, [sessionId, isRunning]);

  if (!sessionId) return null;
  const usedTokens = contextUsage?.usedTokens || 0;
  const maxContextTokens = contextUsage?.maxContextTokens || 0;
  const usedPercentage = contextUsage?.usedPercentage || 0;
  const breakdown = contextUsage?.breakdown;
  const segments = breakdown ? [
    [t("context.Messages"), breakdown.messageTokens, "messages"],
    [t("context.System tools"), breakdown.systemToolTokens, "system-tools"],
    [t("context.MCP tools"), breakdown.mcpToolTokens, "mcp-tools"],
    [t("context.System prompt"), breakdown.systemPromptTokens, "system-prompt"],
    [t("context.Skills"), breakdown.skillsTokens, "skills"],
    ["buffer", breakdown.autoCompactBufferTokens, "buffer"],
    [t("context.Free space"), breakdown.freeSpaceTokens, "free"],
  ] : [];
  const rows = segments.filter(([, , kind]) => kind !== "buffer");

  return (
    <details className="workspaceContextUsage" ref={pickerRef}>
      <summary aria-label={t("context.Context window")} title={t("context.Context window")}>
        <span style={{ "--context-used": `${usedPercentage * 3.6}deg` }} />
      </summary>
      <div className="workspaceContextUsagePanel">
        <header>
          <span>{t("context.Context window")}</span>
          <strong>{contextUsage ? `${formatTokens(usedTokens)} / ${formatTokens(maxContextTokens)} (${usedPercentage}%)` : t("contextUsagePicker.waitingForTheFirstRequest")}</strong>
        </header>
        {breakdown ? (
          <>
            <div className="workspaceContextUsageBar">
              {segments.filter(([, tokens]) => tokens > 0).map(([label, tokens, kind]) => (
                <i key={label} className={`is-${kind}`} style={{ width: `${maxContextTokens ? (tokens / maxContextTokens) * 100 : 0}%` }} />
              ))}
            </div>
            <div className="workspaceContextUsageRows">
              {rows.map(([label, tokens, kind]) => (
                <div key={label}>
                  <i className={`is-${kind}`} />
                  <span>{label}</span>
                  <strong>{formatTokens(tokens)}</strong>
                  <small>{maxContextTokens ? ((tokens / maxContextTokens) * 100).toFixed(1) : "0.0"}%</small>
                </div>
              ))}
            </div>
            {breakdown.mcpTools.length ? (
              <details className="workspaceContextMcpTools">
                <summary>{t("context.MCP tools")}{" "}<span>{formatTokens(breakdown.mcpToolTokens)} · {breakdown.mcpTools.length}</span></summary>
                <div>
                  {breakdown.mcpTools.map((tool) => (
                    <p key={`${tool.providerId}:${tool.name}`}>
                      <span title={`${tool.providerId} · ${tool.name}`}>{tool.providerId} · {tool.name}</span>
                      <strong>{formatTokens(tool.tokens)}</strong>
                    </p>
                  ))}
                </div>
              </details>
            ) : null}
          </>
        ) : null}
      </div>
    </details>
  );
}
