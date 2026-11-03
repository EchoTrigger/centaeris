
import { useCallback, useEffect, useRef, useState } from "react";
import { Outlet, matchPath, useLocation, useNavigate, useRouteLoaderData } from "react-router";
import { AppPageContent } from "./AppRoute";

export default function WorkspaceChatLayout() {
  const { workspace, selectedSession } = useRouteLoaderData("workspace");
  const location = useLocation();
  const locationRef = useRef(location);
  locationRef.current = location;
  const navigate = useNavigate();
  const [notice, setNotice] = useState("");
  const base = `/w/${encodeURIComponent(workspace.id)}`;
  const isChatRoute = Boolean(matchPath(`${base}/app`, location.pathname) || matchPath(`${base}/sessions/:sessionId`, location.pathname));
  const [chatLocation, setChatLocation] = useState(() => isChatRoute ? location : null);
  const [chatSession, setChatSession] = useState(() => isChatRoute ? selectedSession : null);
  const [modelsVersion, setModelsVersion] = useState(0);
  const onModelsChanged = useCallback(() => setModelsVersion((version) => version + 1), []);

  // Retain one conversation at this layout position while its settings Outlet changes.
  if (isChatRoute && chatLocation !== location) { setChatLocation(location); setChatSession(selectedSession); }
  const sessionRoute = Boolean(chatLocation && matchPath(`${base}/sessions/:sessionId`, chatLocation.pathname));

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
    {chatLocation ? <AppPageContent
      initialSession={chatSession}
      workspaceDraft={!sessionRoute}
      location={chatLocation}
      modelsVersion={modelsVersion}
      onSessionAccepted={acceptSession}
    /> : null}
    {notice ? <div className="shWorkspaceToast" role="status">{notice}</div> : null}
    <Outlet context={{ returnTo: chatLocation ? `${chatLocation.pathname}${chatLocation.search}` : null, onModelsChanged }} />
  </>;
}
