import { Navigate, useLocation, useRouteLoaderData } from "react-router";
import { useTranslation } from "../i18n";
import { WorkBreadcrumb } from "./WorkBreadcrumb";
import { DemoInbox } from "./DemoInbox";
import { WorkHome } from "./WorkHome";
import { Issues } from "./Issues";
import "./work-prototype.css";
import "../catalog.css";

export default function WorkPrototypeRoute() {
  const { t } = useTranslation();
  const { workspace } = useRouteLoaderData("workspace");
  const { user } = useRouteLoaderData("authenticated");
  const location = useLocation();
  const base = `/w/${encodeURIComponent(workspace.id)}/work`;
  const [page = "mine", detail] = location.pathname.slice(base.length).split("/").filter(Boolean);
  const labels = { mine: "work.myIssues", tasks: "work.issues", projects: "work.projects", reviews: "work.reviews", views: "work.views" };
  if (detail && page === "projects") return <Navigate to={`${base}/${page}`} replace />;
  return <div className="workPrototype">
    {page === "inbox" ? <DemoInbox key={`${user.id}:${workspace.id}`} userId={user.id} workspaceId={workspace.id} />
      : page === "home" ? <WorkHome workspace={workspace} />
      : ["tasks", "mine"].includes(page) ? <Issues key={`${workspace.id}:${page}:${detail || "list"}`} workspaceId={workspace.id} userId={user.id} mine={page === "mine"} issueId={detail} />
      : <header className="workHeader"><WorkBreadcrumb current={t(labels[page] || "work.myIssues")} /></header>}
  </div>;
}
