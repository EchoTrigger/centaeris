import { useEffect, useRef, useState } from "react";
import { useSearchParams } from "react-router";
import { Button } from "@centaeris/ui/button";
import { Textarea } from "@centaeris/ui/textarea";
import { apiJson, jsonOptions } from "../api";
import { MarkdownContent } from "../chat/MarkdownContent";
import { FileLinkCard } from "../chat/FileLinkCard";
import { useTranslation } from "../i18n";
import "./IssueDeliverables.css";

function deliverablesPath(workspaceId, issueId) {
  return `/api/workspaces/${encodeURIComponent(workspaceId)}/issues/${encodeURIComponent(issueId)}/deliverables`;
}

function failureMessage(failure, t) {
  if (failure.status === 409) return t("issues.conflict");
  if (failure.status === 403 || failure.status === 404) return t("deliverables.unavailable");
  return failure.message;
}

export function DeliverableSubmit({ workspaceId, userId, issue, executionId, run, onChanged, onCancel }) {
  const { t } = useTranslation();
  const endpoint = deliverablesPath(workspaceId, issue.id);
  const [summary, setSummary] = useState(run.summary);
  const [artifactIds, setArtifactIds] = useState([]);
  const [recipientId, setRecipientId] = useState(() => [issue.creatorId, issue.assigneeId].find(id => id && id !== userId) || "");
  const [replacesId, setReplacesId] = useState("");
  const [recipients, setRecipients] = useState([]);
  const [previous, setPrevious] = useState([]);
  const [loading, setLoading] = useState(true);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState(null);
  const submission = useRef(null);
  useEffect(() => {
    const controller = new AbortController();
    setLoading(true);
    Promise.all([
      apiJson(`/api/workspaces/${encodeURIComponent(workspaceId)}/issues/assignees`, { signal: controller.signal }),
      apiJson(endpoint, { signal: controller.signal }),
    ]).then(([people, result]) => {
      if (controller.signal.aborted) return;
      setRecipients(people.assignees); setPrevious(result.deliverables);
    }).catch(failure => { if (!controller.signal.aborted) setError(failure); })
      .finally(() => { if (!controller.signal.aborted) setLoading(false); });
    return () => { controller.abort(); submission.current?.abort(); };
  }, [endpoint, workspaceId]);
  const allowedRecipients = issue.visibility === "participants"
    ? recipients.filter(person => person.id === issue.creatorId || person.id === issue.assigneeId) : recipients;
  const revisions = previous.filter(item => item.isCurrent && item.author.id === userId && item.recipient.id === recipientId);
  const selectedRecipient = allowedRecipients.some(person => person.id === recipientId) ? recipientId : "";
  async function submit(event) {
    event.preventDefault();
    if (pending || loading || !selectedRecipient || (!summary.trim() && !artifactIds.length)) return;
    const controller = new AbortController(); submission.current = controller;
    setPending(true); setError(null);
    try {
      await apiJson(endpoint, { ...jsonOptions("POST", {
        version: issue.version, executionId, agentRunId: run.id, summary: summary.trim(),
        artifactIds, recipientId: selectedRecipient, replacesId: replacesId || null,
      }), signal: controller.signal });
      if (!controller.signal.aborted) { onChanged(); onCancel(); }
    } catch (failure) { if (!controller.signal.aborted) setError(failure); }
    finally { if (!controller.signal.aborted) setPending(false); }
  }
  return <form className="issueDeliverableForm" aria-label={t("deliverables.submit")} aria-busy={pending || loading} onSubmit={event => void submit(event)}>
    <label>{t("deliverables.summary")}<Textarea rows={5} maxLength={32768} value={summary} disabled={pending} onChange={event => setSummary(event.target.value)} /></label>
    {run.artifacts.length ? <fieldset><legend>{t("deliverables.files")}</legend>{run.artifacts.map(artifact => <label className="issueDeliverableFile" key={artifact.id}>
      <input type="checkbox" checked={artifactIds.includes(artifact.id)} disabled={pending} onChange={event => setArtifactIds(ids => event.target.checked ? [...ids, artifact.id] : ids.filter(id => id !== artifact.id))} />
      <span>{artifact.displayName}</span>
    </label>)}</fieldset> : null}
    <label>{t("deliverables.recipient")}<select value={selectedRecipient} disabled={pending || loading} onChange={event => { setRecipientId(event.target.value); setReplacesId(""); }}>
      <option value="">{t("appRoute.selectAnOption")}</option>{allowedRecipients.map(person => <option key={person.id} value={person.id}>{person.name}</option>)}
    </select></label>
    {revisions.length ? <label>{t("deliverables.replaces")}<select value={replacesId} disabled={pending || loading} onChange={event => setReplacesId(event.target.value)}>
      <option value="">{t("deliverables.newSubmission")}</option>{revisions.map(item => <option key={item.id} value={item.id}>{t("deliverables.revision", { revision: item.revision })}</option>)}
    </select></label> : null}
    {error ? <div role="alert" className="issueExecutionError"><span>{failureMessage(error, t)}</span><Button type="button" variant="ghost" disabled={pending} onClick={onChanged}>{t("issues.reload")}</Button></div> : null}
    <footer><Button type="button" variant="ghost" disabled={pending} onClick={onCancel}>{t("issues.cancel")}</Button><Button type="submit" variant="ghost" disabled={pending || loading || !selectedRecipient || (!summary.trim() && !artifactIds.length)}>{t("deliverables.submit")}</Button></footer>
  </form>;
}

export function IssueDeliverables({ workspaceId, userId, issue, onChanged }) {
  const { t } = useTranslation();
  const [params] = useSearchParams();
  const endpoint = deliverablesPath(workspaceId, issue.id);
  const [data, setData] = useState(null);
  const [error, setError] = useState(null);
  const targeted = useRef("");
  // biome-ignore lint/correctness/useExhaustiveDependencies: An accepted Issue revision refreshes submitted work and its responses.
  useEffect(() => {
    const controller = new AbortController();
    setError(null);
    apiJson(endpoint, { signal: controller.signal }).then(result => { if (!controller.signal.aborted) setData({ ...result, endpoint, userId }); })
      .catch(failure => {
        if (!controller.signal.aborted) {
          setError(failure);
          if (failure.status === 403 || failure.status === 404) setData(null);
        }
      });
    return () => controller.abort();
  }, [endpoint, issue.version, userId]);
  const visible = data?.endpoint === endpoint && data.userId === userId ? data : null;
  const targetId = params.get("deliverable");
  useEffect(() => {
    const target = targetId ? `${endpoint}:${targetId}` : "";
    if (!target || targeted.current === target || !visible?.deliverables.some(item => item.id === targetId)) return;
    const element = document.getElementById(`deliverable-${targetId}`);
    if (element) { element.scrollIntoView?.({ block: "start" }); targeted.current = target; }
  }, [endpoint, visible, targetId]);
  if (!error && !visible?.deliverables.length) return null;
  return <section className="issueDeliverables">
    <h2>{t("deliverables.title")}</h2>
    {error ? <div role="alert" className="issueExecutionError"><span>{failureMessage(error, t)}</span><Button type="button" variant="ghost" onClick={onChanged}>{t("issues.reload")}</Button></div> : null}
    {visible?.deliverables.map(deliverable => <DeliverableCard key={deliverable.id} deliverable={deliverable} endpoint={endpoint} userId={userId} issue={issue} onChanged={onChanged} />)}
  </section>;
}

function DeliverableCard({ deliverable, endpoint, userId, issue, onChanged }) {
  const { t } = useTranslation();
  const [feedbackOpen, setFeedbackOpen] = useState(false);
  const [text, setText] = useState("");
  const [pending, setPending] = useState(false);
  const [error, setError] = useState(null);
  const request = useRef(null);
  useEffect(() => () => request.current?.abort(), []);
  const accepted = deliverable.responses.some(response => response.action === "accept");
  const canRespond = deliverable.isCurrent && !issue.archivedAt && [deliverable.author.id, deliverable.recipient.id].includes(userId);
  async function respond(action) {
    if (pending || (action === "feedback" && !text.trim())) return;
    const controller = new AbortController(); request.current = controller;
    setPending(true); setError(null);
    try {
      await apiJson(`${endpoint}/${encodeURIComponent(deliverable.id)}/responses`, {
        ...jsonOptions("POST", { version: issue.version, action, text: action === "feedback" ? text.trim() : "" }), signal: controller.signal,
      });
      if (!controller.signal.aborted) { setText(""); setFeedbackOpen(false); onChanged(); }
    } catch (failure) { if (!controller.signal.aborted) setError(failure); }
    finally { if (!controller.signal.aborted) setPending(false); }
  }
  return <article id={`deliverable-${deliverable.id}`} className="issueDeliverableCard" aria-busy={pending}>
    <header><strong>{t("deliverables.revision", { revision: deliverable.revision })}</strong><span>{deliverable.author.name} → {deliverable.recipient.name}</span><time dateTime={deliverable.createdAt}>{new Date(deliverable.createdAt).toLocaleString()}</time>{accepted ? <span>{t("deliverables.accepted")}</span> : null}</header>
    {deliverable.summary ? <MarkdownContent text={deliverable.summary} /> : null}
    {deliverable.artifacts.length ? <ul className="issueDeliverableFiles">{deliverable.artifacts.map(artifact => <li key={artifact.id}><FileLinkCard filename={artifact.displayName} href={artifact.downloadUrl} download /></li>)}</ul> : null}
    {deliverable.responses.length ? <ol className="issueDeliverableResponses">{deliverable.responses.map(response => <li key={response.id}>
      <div><strong>{response.actor.name}</strong><span>{t(response.action === "accept" ? "deliverables.accepted" : "deliverables.feedback")}</span><time dateTime={response.createdAt}>{new Date(response.createdAt).toLocaleString()}</time></div>
      {response.text ? <p>{response.text}</p> : null}
    </li>)}</ol> : null}
    {canRespond ? <>
      <div className="issueDeliverableActions"><Button variant="ghost" disabled={pending} onClick={() => setFeedbackOpen(value => !value)} aria-expanded={feedbackOpen}>{t("deliverables.giveFeedback")}</Button>{deliverable.recipient.id === userId && !accepted ? <Button variant="ghost" disabled={pending} onClick={() => void respond("accept")}>{t("deliverables.accept")}</Button> : null}</div>
      {feedbackOpen ? <form className="issueDeliverableForm" onSubmit={event => { event.preventDefault(); void respond("feedback"); }}><label>{t("deliverables.feedback")}<Textarea required rows={3} maxLength={32768} value={text} disabled={pending} onChange={event => setText(event.target.value)} /></label><footer><Button type="submit" variant="ghost" disabled={pending || !text.trim()}>{t("deliverables.sendFeedback")}</Button></footer></form> : null}
    </> : null}
    {error ? <div role="alert" className="issueExecutionError"><span>{failureMessage(error, t)}</span><Button variant="ghost" disabled={pending} onClick={onChanged}>{t("issues.reload")}</Button></div> : null}
  </article>;
}
