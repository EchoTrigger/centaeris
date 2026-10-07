import { t } from "../i18n";
import { useTranslation } from "../i18n";
import { useEffect, useState } from "react";
import { Link, useLoaderData, useLocation, useNavigate } from "react-router";
import { Building2 } from "lucide-react";
import { OnboardingUtilities } from "../components/OnboardingUtilities";

const WORKSPACE_ROLE_LABELS = () => ({ owner: t("workspaceChooserRoute.owner"), admin: t("invitationActivationRoute.administrator"), member: t("invitationActivationRoute.member") });

export default function WorkspaceChooserRoute() {
  const { t } = useTranslation();
  const { workspaces } = useLoaderData();
  const location = useLocation();
  const navigate = useNavigate();
  const [notice, setNotice] = useState(() => location.state?.workspaceNotice || "");

  useEffect(() => {
    if (!notice) return undefined;
    if (location.state?.workspaceNotice) {
      navigate(`${location.pathname}${location.search}`, { replace: true, state: null });
    }
    const timeout = window.setTimeout(() => setNotice(""), 4200);
    return () => window.clearTimeout(timeout);
  }, [location.pathname, location.search, location.state, navigate, notice]);

  return (
    <main className="shWorkspaceChooser">
      {notice ? <div className="shWorkspaceToast" role="status">{notice}</div> : null}
      <section>
        <h1>{t("workspaceChooserRoute.chooseWorkspace")}</h1>
        {!workspaces.length ? <p>{t("workspaceChooserRoute.yourAccountDoesNotHaveAccessToAnyWorkspaces")}</p> : null}
        <nav aria-label={t("workspaceChooserRoute.workspace")}>
          {workspaces.map((workspace) => (
            <Link to={`/w/${encodeURIComponent(workspace.id)}/app`} key={workspace.id}>
              <span><Building2 aria-hidden="true" /></span>
              <strong>{workspace.name}</strong>
              <small>{WORKSPACE_ROLE_LABELS()[workspace.role]}</small>
            </Link>
          ))}
        </nav>
      </section>
      <OnboardingUtilities />
    </main>
  );
}
