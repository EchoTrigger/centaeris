import { useContext, useEffect, useMemo, useRef, useState } from "react";
import { Link, useSearchParams } from "react-router";
import { Button } from "@centaeris/ui/button";
import { ChevronDown, MessagesSquare, Plus, RefreshCw } from "lucide-react";
import { Textarea } from "@centaeris/ui/textarea";
import { apiJson } from "../api";
import { OperationClient } from "../chat/operationReceipts";
import { MarkdownContent } from "../chat/MarkdownContent";
import { readModelThinkingMode, readPreferredModelIdentity } from "../preferences";
import { sessionChatPath } from "../shell/sessionNavigation.mjs";
import { ConversationDockContext } from "../shell/ConversationDock";
import { useTranslation } from "../i18n";
import { DeliverableSubmit } from "./IssueDeliverables";

export function IssueExecutions({ workspaceId, userId, issue, onChanged }) {
  const { t } = useTranslation();
  const conversationDock = useContext(ConversationDockContext);
  const [params, setParams] = useSearchParams();
  const endpoint = `/api/workspaces/${encodeURIComponent(workspaceId)}/issues/${encodeURIComponent(issue.id)}/executions`;
  const operations = useMemo(() => new OperationClient({ userId, workspaceId, command: "submitMessage" }), [userId, workspaceId]);
  const [executions, setExecutions] = useState([]);
  const [total, setTotal] = useState(0);
  const [expanded, setExpanded] = useState(false);
  const activeExecution = params.get("execution") || "";
  const [instructions, setInstructions] = useState("");
  const [models, setModels] = useState([]);
  const [modelId, setModelId] = useState("");
  const [thinkingMode, setThinkingMode] = useState("");
  const [pending, setPending] = useState(false);
  const [loadingModels, setLoadingModels] = useState(false);
  const [error, setError] = useState(null);
  const [revision, setRevision] = useState(0);
  const mounted = useRef(true);
  useEffect(() => { mounted.current = true; return () => { mounted.current = false; }; }, []);
  // biome-ignore lint/correctness/useExhaustiveDependencies: Revision reloads execution records on explicit retry; Issue edits refresh accepted context.
  useEffect(() => {
    const controller = new AbortController();
    apiJson(`${endpoint}?limit=20&offset=0`, { signal: controller.signal }).then(result => {
      if (!controller.signal.aborted) { setExecutions(result.executions); setTotal(result.total); }
    }).catch(failure => { if (!controller.signal.aborted) { setExecutions([]); setTotal(0); setError(["issue_material_unavailable", "issue_material_version_changed"].includes(failure.message) ? t("issues.materialsChanged") : failure.message); } });
    return () => controller.abort();
  }, [endpoint, issue.version, revision]);
  useEffect(() => {
    if (!expanded) return;
    const controller = new AbortController();
    setLoadingModels(true);
    apiJson(`/api/workspaces/${encodeURIComponent(workspaceId)}/models`, { signal: controller.signal }).then(result => {
      if (controller.signal.aborted) return;
      const available = result.models.filter(model => model.enabled);
      setModels(available);
      const identity = readPreferredModelIdentity(userId, workspaceId);
      const preferred = available.find(model => JSON.stringify([model.providerId, model.modelName]) === identity);
      setModelId(preferred?.id || "");
      setThinkingMode(preferred ? readModelThinkingMode(userId, workspaceId, preferred) : "");
    }).catch(failure => { if (!controller.signal.aborted) setError(["issue_material_unavailable", "issue_material_version_changed"].includes(failure.message) ? t("issues.materialsChanged") : failure.message); })
      .finally(() => { if (!controller.signal.aborted) setLoadingModels(false); });
    return () => controller.abort();
  }, [expanded, workspaceId, userId, t]);
  const selectedModel = models.find(model => model.id === modelId);
  async function openReceipt(receipt) {
    const result = await apiJson(`/api/sessions/${encodeURIComponent(receipt.sessionId)}`);
    if (!mounted.current) return;
    if (result.session.workspaceId !== workspaceId || result.session.id !== receipt.sessionId) throw new Error("operation_receipt_invalid");
    operations.complete(receipt.operationId);
    conversationDock.openSession(result.session);
    setExpanded(false);
    setInstructions("");
    setRevision(value => value + 1);
  }
  async function launch() {
    if (pending || !modelId) return;
    setPending(true); setError(null);
    try {
      const receipt = await operations.submit(endpoint, { version: issue.version, modelConfigRef: modelId, ...(thinkingMode ? { thinkingMode } : {}), ...(instructions.trim() ? { instructions: instructions.trim() } : {}) });
      await openReceipt(receipt);
    } catch (failure) { if (mounted.current) setError(["issue_material_unavailable", "issue_material_version_changed"].includes(failure.message) ? t("issues.materialsChanged") : failure.message); }
    finally { if (mounted.current) setPending(false); }
  }
  return <section className="issueExecutions" aria-busy={pending}>
    <header className="issueExecutionsHeader"><h2>{t("issues.conversations")}</h2><div className="issueExecutionActions">
      {executions.length ? <Button variant="ghost" size="icon" aria-label={t("issues.refreshExecutions")} disabled={pending} onClick={() => { setError(null); setRevision(value => value + 1); }}><RefreshCw aria-hidden="true" /></Button> : null}
      {!issue.archivedAt ? <Button variant="ghost" className="issueExecutionNew" aria-expanded={expanded} disabled={pending} onClick={() => setExpanded(value => !value)}><Plus aria-hidden="true" />{t(executions.length ? "issues.newExecution" : "issues.runWithAgent")}</Button> : null}
    </div></header>
    {expanded && !issue.archivedAt ? <form className="issueExecutionForm" onSubmit={event => { event.preventDefault(); void launch(); }}>
      <label className="issueExecutionInstructions">{t("issues.executionInstructions")}<Textarea rows={3} maxLength={8192} value={instructions} disabled={pending} onChange={event => setInstructions(event.target.value)} /></label>
      <label>{t("issues.executionModel")}<select value={modelId} disabled={pending || loadingModels} onChange={event => {
        const model = models.find(item => item.id === event.target.value);
        setModelId(event.target.value); setThinkingMode(model ? readModelThinkingMode(userId, workspaceId, model) : "");
      }}><option value="">{t("appRoute.selectAnOption")}</option>{models.map(model => <option key={model.id} value={model.id}>{model.displayName || model.modelName}</option>)}</select></label>
      {selectedModel?.thinkingModes?.length ? <label>{t("issues.executionThinking")}<select value={thinkingMode} disabled={pending} onChange={event => setThinkingMode(event.target.value)}>{selectedModel.thinkingModes.map(mode => <option key={mode} value={mode}>{mode}</option>)}</select></label> : null}
      <Button type="submit" variant="ghost" disabled={!modelId || pending || loadingModels}>{t("issues.startExecution")}</Button>
    </form> : null}
    {error ? <div className="issueExecutionError" role="alert"><span>{error}</span>{error === t("issues.materialsChanged") ? <Button variant="ghost" disabled={pending} onClick={onChanged}>{t("issues.reload")}</Button> : null}<Button variant="ghost" disabled={pending} onClick={async () => {
      setPending(true); setError(null);
      try { const receipt = await operations.recover(); if (receipt) await openReceipt(receipt); else setRevision(value => value + 1); }
      catch (failure) { if (mounted.current) setError(["issue_material_unavailable", "issue_material_version_changed"].includes(failure.message) ? t("issues.materialsChanged") : failure.message); }
      finally { if (mounted.current) setPending(false); }
    }}>{t("agentChat.retry")}</Button></div> : null}
    {executions.length ? <><ul className="issueExecutionList">{executions.map(execution => <li key={execution.id}><div className="issueExecutionRow"><Button variant="ghost" className="issueExecutionTitle" aria-expanded={activeExecution === execution.id} onClick={() => setParams(previous => {
      const next = new URLSearchParams(previous);
      if (activeExecution === execution.id) next.delete("execution"); else next.set("execution", execution.id);
      return next;
    }, { replace: true })}><MessagesSquare aria-hidden="true" /><span>{execution.title}</span><ChevronDown aria-hidden="true" /></Button><Link to={sessionChatPath(workspaceId, execution.sessionId)}><span>{t(execution.waitingForInput ? "issues.answerAgent" : "issues.continueConversation")}</span>{execution.waitingForInput || execution.status !== "completed" ? <small>{t(execution.waitingForInput ? "issues.execution.waitingForInput" : `issues.execution.${execution.status}`)}</small> : null}</Link></div>{activeExecution === execution.id ? <ExecutionResults key={`${execution.id}:${revision}`} endpoint={`${endpoint}/${encodeURIComponent(execution.id)}`} execution={execution} workspaceId={workspaceId} userId={userId} issue={issue} onChanged={onChanged} /> : null}</li>)}</ul>
      {executions.length < total ? <Button variant="ghost" disabled={pending} onClick={async () => {
        setPending(true); setError(null);
        try { const result = await apiJson(`${endpoint}?limit=20&offset=${executions.length}`); if (mounted.current) { setExecutions(items => [...items, ...result.executions]); setTotal(result.total); } }
        catch (failure) { if (mounted.current) setError(["issue_material_unavailable", "issue_material_version_changed"].includes(failure.message) ? t("issues.materialsChanged") : failure.message); }
        finally { if (mounted.current) setPending(false); }
      }}>{t("issues.moreConversations")}</Button> : null}</> : null}
  </section>;
}

function ExecutionResults({ endpoint, execution, workspaceId, userId, issue, onChanged }) {
  const { t } = useTranslation();
  const [data, setData] = useState({ runs: [], total: 0 });
  const [submittingRun, setSubmittingRun] = useState("");
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState("");
  useEffect(() => {
    const controller = new AbortController();
    apiJson(`${endpoint}?limit=20&offset=0`, { signal: controller.signal }).then(result => { if (!controller.signal.aborted) setData(result); }).catch(failure => { if (!controller.signal.aborted) setError(["issue_material_unavailable", "issue_material_version_changed"].includes(failure.message) ? t("issues.materialsChanged") : failure.message); }).finally(() => { if (!controller.signal.aborted) setLoading(false); });
    return () => controller.abort();
  }, [endpoint, t]);
  async function more() {
    setLoading(true); setError("");
    try { const result = await apiJson(`${endpoint}?limit=20&offset=${data.runs.length}`); setData(previous => ({ ...result, runs: [...previous.runs, ...result.runs] })); }
    catch (failure) { setError(["issue_material_unavailable", "issue_material_version_changed"].includes(failure.message) ? t("issues.materialsChanged") : failure.message); }
    finally { setLoading(false); }
  }
  return <div className="issueExecutionResults" aria-busy={loading}>
    {error ? <p role="alert">{error}</p> : null}
    {loading ? <p role="status">{t("agentChat.loading")}</p> : null}
    {data.runs.map(run => <article key={run.id}>
      {run.status !== "completed" || (execution.waitingForInput && execution.latestAgentRunId === run.id) ? <small>{t(execution.waitingForInput && execution.latestAgentRunId === run.id ? "issues.execution.waitingForInput" : `issues.execution.${run.status}`, { defaultValue: run.status })}</small> : null}
      {run.summary ? <MarkdownContent text={`${run.summary}${run.summaryTruncated ? "…" : ""}`} /> : null}
      <ol>{run.activity.map(activity => <li key={activity.id}><p>{activity.text}{activity.textTruncated ? "…" : ""}</p><time dateTime={activity.createdAt}>{new Date(activity.createdAt).toLocaleString()}</time></li>)}</ol>
      {run.artifacts.length ? <ul>{run.artifacts.map(artifact => <li key={artifact.id}><a href={artifact.downloadUrl} download>{artifact.displayName}</a></li>)}</ul> : null}
      {!issue.archivedAt && (run.summary || run.artifacts.length) ? <Button variant="solid" className="issueSubmitDeliverable" aria-expanded={submittingRun === run.id} onClick={() => setSubmittingRun(value => value === run.id ? "" : run.id)}>{t("deliverables.submit")}</Button> : null}
      {submittingRun === run.id ? <DeliverableSubmit key={run.id} workspaceId={workspaceId} userId={userId} issue={issue} executionId={execution.id} run={run} onChanged={onChanged} onCancel={() => setSubmittingRun("")} /> : null}
    </article>)}
    {data.runs.length < data.total ? <Button variant="ghost" disabled={loading} onClick={() => void more()}>{t("issues.moreResults")}</Button> : null}
  </div>;
}
