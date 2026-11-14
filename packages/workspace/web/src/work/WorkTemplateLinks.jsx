import { useEffect, useState } from "react";
import { Link } from "react-router";
import { apiJson } from "../api";

export function WorkTemplateLinks({ workspaceId }) {
  const [templates, setTemplates] = useState([]);
  const [error, setError] = useState("");
  useEffect(() => {
    const controller = new AbortController();
    apiJson(`/api/workspaces/${encodeURIComponent(workspaceId)}/issue-templates`, { signal: controller.signal })
      .then(result => { if (!controller.signal.aborted) setTemplates(result.templates); })
      .catch(failure => { if (!controller.signal.aborted) setError(failure.message); });
    return () => controller.abort();
  }, [workspaceId]);
  return <>{error ? <p role="alert">{error}</p> : null}{templates?.length ? <ul className="catalogRows">{templates.map(template => <li key={template.key}><Link to={`/w/${encodeURIComponent(workspaceId)}/work/tasks/new?${new URLSearchParams({ template: template.key })}`}><strong>{template.name}</strong><span aria-hidden="true">↗</span></Link></li>)}</ul> : null}</>;
}
