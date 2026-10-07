
import { useTranslation } from "../i18n";
import { useCallback, useEffect, useRef, useState } from "react";
import { Link, Outlet, matchPath, useLocation, useNavigate, useRouteLoaderData } from "react-router";
import { Bot, Plus } from "lucide-react";
import { AppPageContent } from "./AppRoute";
import { AgentChatPageContent } from "./AgentChatRoute";
import { workspaceChatKind } from "../agent-chat/agentChatNavigation";
import { ShellPage } from "../shell/ShellPage";

export default function WorkspaceChatLayout() {
  const { t } = useTranslation();
  const { workspace, agents } = useRouteLoaderData("workspace");
  const location = useLocation();
  const locationRef = useRef(location);
  locationRef.current = location;
  const navigate = useNavigate();
  const [notice, setNotice] = useState("");
  const base = `/w/${encodeURIComponent(workspace.id)}`;
  const isChatRoute = Boolean(matchPath(`${base}/app`, location.pathname) || matchPath(`${base}/agents/:agentId`, location.pathname));
  const [chatLocation, setChatLocation] = useState(() => isChatRoute ? location : null);
  const [modelsVersion, setModelsVersion] = useState(0);
  const onModelsChanged = useCallback(() => setModelsVersion((version) => version + 1), []);

  // Retain one conversation at this layout position while its settings Outlet changes.
  if (isChatRoute && chatLocation !== location) setChatLocation(location);
  const chatAgentId = chatLocation ? matchPath(`${base}/agents/:agentId`, chatLocation.pathname)?.params.agentId || "" : "";
  const chatKind = workspaceChatKind(chatAgentId, chatLocation?.search || "");

  function acceptSession(path) {
    const currentLocation = locationRef.current;
    if (!currentLocation.pathname.startsWith(`${base}/settings/`)) {
      navigate(path, { replace: true });
      return;
    }
    const url = new URL(path, window.location.origin);
    setChatLocation({ pathname: url.pathname, search: url.search, state: null });
    navigate(`${currentLocation.pathname}${currentLocation.search}`, { replace: true, state: { ...currentLocation.state, returnTo: path } });
  }

  useEffect(() => {
    if (!location.state?.workspaceNotice) return;
    const { workspaceNotice, ...state } = location.state;
    setNotice(workspaceNotice);
    navigate(`${location.pathname}${location.search}`, { replace: true, state });
  }, [location.pathname, location.search, location.state, navigate]);

  useEffect(() => {
    if (!notice) return undefined;
    const timeout = window.setTimeout(() => setNotice(""), 4200);
    return () => window.clearTimeout(timeout);
  }, [notice]);

  return <>
    {chatLocation ? agents.length ? chatKind === "agent" ? <AgentChatPageContent key={`agent:${chatAgentId}`} agentId={chatAgentId} /> : <AppPageContent
      key={chatAgentId || agents[0].id}
      agentId={chatAgentId || agents[0].id}
      workspaceDraft={!chatAgentId}
      location={chatLocation}
      modelsVersion={modelsVersion}
      onSessionAccepted={acceptSession}
    /> : <ShellPage>
      <div className="shWorkspaceHome"><div className="shWorkspaceAgentEmpty">
        <Bot aria-hidden="true" />
        <p>{t("workspaceHomeRoute.noPrivateAgentsAreAvailableInThisWorkspaceYet")}</p>
        <div><Link className="shPrimaryButton" to={`${base}/settings/agents?new=1`}><Plus aria-hidden="true" />{t("agentCreateRoute.createAgent")}</Link></div>
      </div></div>
    </ShellPage> : null}
    {notice ? <div className="shWorkspaceToast" role="status">{notice}</div> : null}
    <Outlet context={{ returnTo: chatLocation ? `${chatLocation.pathname}${chatLocation.search}` : null, onModelsChanged }} />
  </>;
}
