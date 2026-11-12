import { useState } from "react";
import { Button } from "@centaeris/ui/button";
import { Input } from "@centaeris/ui/input";
import { Textarea } from "@centaeris/ui/textarea";
import { CheckCircle2, Circle, CircleDashed, Clock3, XCircle } from "lucide-react";
import { useTranslation } from "../i18n";
import { ChoiceMenu } from "./WorkControls";

export const issueStatuses = ["backlog", "todo", "inProgress", "done", "canceled"];
export function IssueStatus({ status }) {
  const { t } = useTranslation();
  const Icon = { backlog: Circle, todo: CircleDashed, inProgress: Clock3, done: CheckCircle2, canceled: XCircle }[status];
  return <Icon className="workStatusIcon" aria-label={t(`issues.status.${status}`)} />;
}

export function IssueForm({ issue, assignees, defaultAssigneeId, pending, onSave, onCancel }) {
  const { t } = useTranslation();
  const [draft, setDraft] = useState(() => ({ title: issue?.title || "", description: issue?.description || "", status: issue?.status || "todo", assigneeId: issue ? issue.assigneeId : defaultAssigneeId ?? null }));
  const change = (field, value) => setDraft(previous => ({ ...previous, [field]: value }));
  return <form className="issueForm" onSubmit={event => { event.preventDefault(); onSave(draft); }}>
    <label>{t("issues.title")}<Input required autoFocus maxLength={240} value={draft.title} onChange={event => change("title", event.target.value)} /></label>
    <label>{t("issues.description")}<Textarea rows={6} value={draft.description} onChange={event => change("description", event.target.value)} /></label>
    <div className="issueFormProperties">
      <ChoiceMenu label={t("issues.status")} value={draft.status} options={issueStatuses.map(value => ({ value, label: t(`issues.status.${value}`), icon: <IssueStatus status={value} /> }))} onChange={value => change("status", value)} />
      <ChoiceMenu label={t("issues.assignee")} value={draft.assigneeId || ""} options={[{ value: "", label: t("issues.unassigned") }, ...assignees.map(person => ({ value: person.id, label: person.name }))]} onChange={value => change("assigneeId", value || null)} />
    </div>
    <footer><Button variant="ghost" type="button" disabled={pending} onClick={onCancel}>{t("issues.cancel")}</Button><Button variant="solid" type="submit" disabled={pending || !draft.title.trim()}>{t(issue ? "issues.save" : "issues.create")}</Button></footer>
  </form>;
}
