
import { useCallback, useEffect, useRef, useState } from "react";
import { Outlet, matchPath, useLocation, useNavigate, useRouteLoaderData } from "react-router";
import { useTranslation } from "../i18n";
import { AppPageContent } from "./AppRoute";
import { isWorkspaceCatalogPath } from "../shell/sessionNavigation.mjs";

export default function WorkspaceChatLayout() {
  const { t } = useTranslation();
  const [dockContainer, setDockContainer] = useState(null);
  const { workspace, selectedSession } = useRouteLoaderData("workspace");
  const location = useLocation();
  const locationRef = useRef(location);
  locationRef.current = location;
  const navigate = useNavigate();
  const [notice, setNotice] = useState("");
  const [floating, setFloating] = useState(false);
  const lastCatalog = useRef("");
  const pendingFloatingSession = useRef(undefined);
  const base = `/w/${encodeURIComponent(workspace.id)}`;
  const isChatRoute = Boolean(matchPath(`${base}/app`, location.pathname) || matchPath(`${base}/sessions/:sessionId`, location.pathname));
  const isCatalogRoute = isWorkspaceCatalogPath(base, location.pathname);
  if (isCatalogRoute) lastCatalog.current = location.pathname + location.search;
  const [chatLocation, setChatLocation] = useState(() => isChatRoute ? location : null);
  const [chatSession, setChatSession] = useState(() => isChatRoute ? selectedSession : null);
  const [modelsVersion, setModelsVersion] = useState(0);
  const onModelsChanged = useCallback(() => setModelsVersion((version) => version + 1), []);

  // Keep the conversation and its navigation alive while a catalog or settings is open.
  if (isChatRoute && chatLocation !== location) {
    setChatLocation(location);
    if (chatSession?.id !== selectedSession?.id) setChatSession(selectedSession);
  }
  if (isCatalogRoute && pendingFloatingSession.current !== undefined) {
    const session = pendingFloatingSession.current;
    pendingFloatingSession.current = undefined;
    setChatSession(session);
    setChatLocation({ pathname: session?.id ? `${base}/sessions/${encodeURIComponent(session.id)}` : `${base}/app`, search: "", state: null });
  }
  const sessionRoute = Boolean(chatLocation && matchPath(`${base}/sessions/:sessionId`, chatLocation.pathname));

  function acceptSession(path, session) {
    const currentLocation = locationRef.current;
    if (matchPath(`${base}/app`, currentLocation.pathname) || matchPath(`${base}/sessions/:sessionId`, currentLocation.pathname)) {
      navigate(path, { replace: true });
      return;
    }
    const url = new URL(path, window.location.origin);
    setChatLocation({ pathname: url.pathname, search: url.search, state: null });
    setChatSession(session);
    if (currentLocation.pathname.startsWith(`${base}/settings/`)) {
      const returnTo = currentLocation.state?.returnTo;
      if (!returnTo || returnTo === `${chatLocation?.pathname}${chatLocation?.search || ""}${chatLocation?.hash || ""}`) {
        navigate(`${currentLocation.pathname}${currentLocation.search}`, { replace: true, state: { ...currentLocation.state, returnTo: path } });
      }
    }
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

  return <div className="workspaceChatLayout">
    {chatLocation || isCatalogRoute ? <div hidden={!isChatRoute && !isCatalogRoute}>
      <AppPageContent
        dockContainer={dockContainer}
        initialSession={chatSession}
        workspaceDraft={!sessionRoute}
        visible={isChatRoute || (isCatalogRoute && floating)}
        chatFloating={isCatalogRoute && floating}
        onShowChat={() => {
          if (isChatRoute) return;
          setFloating(true);
        }}
        onMinimizeChat={() => setFloating(false)}
        onDockChat={() => { setFloating(true); navigate(lastCatalog.current || `${base}/work`); }}
        onExpandChat={() => { setFloating(false); navigate(chatLocation ? chatLocation.pathname + chatLocation.search : `${base}/app`); }}
        onOpenFloatingSession={session => {
          if (isCatalogRoute) {
            setChatSession(session);
            setChatLocation({ pathname: session?.id ? `${base}/sessions/${encodeURIComponent(session.id)}` : `${base}/app`, search: "", state: null });
          } else {
            pendingFloatingSession.current = session;
            navigate(lastCatalog.current || `${base}/work`);
          }
          setFloating(true);
        }}
        location={chatLocation ?? { pathname: `${base}/app`, search: "", state: null }}
        navigationKey={location.key}
        catalogContent={isCatalogRoute ? <Outlet /> : null}
        modelsVersion={modelsVersion}
        onSessionAccepted={acceptSession}
      />
    </div> : null}
    {notice ? <div className="shWorkspaceToast" role="status">{notice}</div> : null}
    {!isCatalogRoute ? <Outlet context={{ returnTo: chatLocation ? `${chatLocation.pathname}${chatLocation.search}${chatLocation.hash || ""}` : null, onModelsChanged }} /> : null}
    <nav className="conversationDock" ref={setDockContainer} aria-label={t("work.conversation.toolbar")} />
  </div>;
}
