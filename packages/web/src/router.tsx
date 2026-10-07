
import { useTranslation } from "./i18n";
import { useEffect, useState } from "react";
import { Navigate, Outlet, createBrowserRouter, redirect, useLoaderData, useLocation, useRevalidator, useRouteLoaderData } from "react-router";
import { ApiError, apiJson, hasAuthenticationRequiredHandler, isAuthenticationRequired, requireWorkspaces, setAuthenticationRequiredHandler, type WorkspaceSummary } from "./api";
import { SessionExpiredDialog } from "./components/SessionExpiredDialog";
import LibraryObjectRoute from "./routes/LibraryObjectRoute";
import LibraryRoute from "./routes/LibraryRoute";
import LoginRoute from "./routes/LoginRoute";
import { ForgotPasswordRoute, ResetPasswordRoute } from "./routes/PasswordResetRoute";
import InvitationActivationRoute from "./routes/InvitationActivationRoute";
import SettingsRoute from "./routes/SettingsRoute";
import WorkspaceChooserRoute from "./routes/WorkspaceChooserRoute";
import WorkspaceHomeRoute from "./routes/WorkspaceHomeRoute";
import TrashSessionRoute from "./routes/TrashSessionRoute";

export type AuthenticatedUser = {
  id: string;
  email: string;
  isStaff: boolean;
  isSuperuser: boolean;
};

export type AuthLoaderData = { user: AuthenticatedUser };
export type AgentSummary = {
  id: string;
  workspaceId: string;
  name: string;
  description: string;
  instructions: string;
  avatarKind: "centaeris" | "banana";
  status: "active" | "deleted";
  deletedAt: string | null;
  createdAt: string;
  updatedAt: string;
};
export type WorkspaceLoaderData = {
  workspace: WorkspaceSummary;
  workspaces: WorkspaceSummary[];
  agents: AgentSummary[];
};
export type AccountSettingsLoaderData = {
  workspace: WorkspaceSummary | null;
  workspaces: WorkspaceSummary[];
};

let lastAuthLoaderData: AuthLoaderData | undefined;

function workspacePreferenceKey(userId: string) {
  return `centaeris:last-workspace:${userId}`;
}

function readWorkspacePreference(userId: string) {
  try {
    return window.localStorage.getItem(workspacePreferenceKey(userId));
  } catch {
    return null;
  }
}

function WorkspaceEntry() {
  useTranslation();
  const { workspaces } = useLoaderData() as { workspaces: WorkspaceSummary[] };
  const { user } = useRouteLoaderData("authenticated") as AuthLoaderData;
  const location = useLocation();
  const rememberedId = readWorkspacePreference(user.id);
  const destination = workspaces.length === 1
    ? workspaces[0]
    : workspaces.find((workspace) => workspace.id === rememberedId);

  if (destination) {
    return <Navigate
      replace
      state={location.state}
      to={`/w/${encodeURIComponent(destination.id)}/app`}
    />;
  }
  return <WorkspaceChooserRoute />;
}

function WorkspaceLayout() {
  useTranslation();
  const { user } = useRouteLoaderData("authenticated") as AuthLoaderData;
  const { workspace } = useLoaderData() as WorkspaceLoaderData;

  useEffect(() => {
    try {
      window.localStorage.setItem(workspacePreferenceKey(user.id), workspace.id);
    } catch {
      // Navigation preferences are optional; membership remains the authority.
    }
  }, [user.id, workspace.id]);

  return <Outlet key={`${user.id}:${workspace.id}`} />;
}

function RootLayout() {
  useTranslation();
  const [sessionExpired, setSessionExpired] = useState(false);
  const auth = useRouteLoaderData("authenticated") as AuthLoaderData | undefined;
  const revalidator = useRevalidator();

  useEffect(() => {
    if (!auth?.user) return undefined;
    return setAuthenticationRequiredHandler(() => setSessionExpired(true));
  }, [auth?.user]);

  useEffect(() => {
    if (!auth?.user) setSessionExpired(false);
  }, [auth?.user]);

  return <>
    <Outlet />
    <SessionExpiredDialog
      open={sessionExpired}
      user={auth?.user}
      onReauthenticated={async () => {
        await apiJson<AuthLoaderData>("/api/me");
        await revalidator.revalidate();
      }}
      onContinue={() => setSessionExpired(false)}
    />
  </>;
}

function RouteLoading() {
  const { t } = useTranslation();
  return <main className="routeLoading" aria-live="polite">{t("router.loadingWorkspace")}</main>;
}

function RouteError() {
  const { t } = useTranslation();
  return <main className="routeError" role="alert">
    <h1>{t("router.thisPageCouldNotBeLoaded")}</h1>
    <p>{t("router.ifReloadingDoesNotHelpReturnToYourWorkspaces")}</p>
    <div className="routeErrorActions">
      <button type="button" onClick={() => window.location.reload()}>{t("router.reloadPage")}</button>
      <a href="/workspaces">{t("router.backToWorkspaces")}</a>
    </div>
  </main>;
}

async function authLoader({ request }: { request: Request }): Promise<AuthLoaderData> {
  try {
    lastAuthLoaderData = await apiJson<AuthLoaderData>("/api/me", { signal: request.signal });
    return lastAuthLoaderData;
  } catch (error) {
    if (isAuthenticationRequired(error)) {
      if (lastAuthLoaderData && hasAuthenticationRequiredHandler()) return lastAuthLoaderData;
      const url = new URL(request.url);
      const next = `${url.pathname}${url.search}${url.hash}`;
      throw redirect(`/login?${new URLSearchParams({ next })}`);
    }
    throw error;
  }
}

async function workspacesLoader({ request }: { request: Request }) {
  const result = await apiJson<{ workspaces: unknown }>("/api/workspaces", { signal: request.signal });
  return { workspaces: requireWorkspaces(result.workspaces) };
}

async function accountSettingsLoader({ request }: { request: Request }): Promise<AccountSettingsLoaderData> {
  const result = await apiJson<{ workspaces: unknown }>("/api/workspaces", { signal: request.signal });
  const workspaces = requireWorkspaces(result.workspaces);
  const workspaceIds = new URL(request.url).searchParams.getAll("workspaceId");
  if (workspaceIds.length > 1 || (workspaceIds.length === 1 && !workspaceIds[0])) {
    throw new Error("workspace_context_invalid");
  }
  if (!workspaceIds.length) return { workspace: null, workspaces };
  const workspace = workspaces.find((item) => item.id === workspaceIds[0]);
  if (!workspace) throw new Error("workspace_context_invalid");
  return { workspace, workspaces };
}

async function workspaceLoader({ params, request }: { params: { workspaceId?: string }; request: Request }): Promise<WorkspaceLoaderData> {
  const workspaceId = params.workspaceId || "";
  const result = await apiJson<{ workspaces: unknown }>("/api/workspaces", { signal: request.signal });
  const workspaces = requireWorkspaces(result.workspaces);
  const workspace = workspaces.find((item) => item.id === workspaceId);
  if (!workspace) throw redirect("/workspaces");
  try {
    const agentResult = await apiJson<{ agents: AgentSummary[] }>(
      `/api/workspaces/${encodeURIComponent(workspaceId)}/agents`,
      { signal: request.signal },
    );
    if (!Array.isArray(agentResult.agents)) throw new Error("agent_directory_invalid");
    return { workspace, workspaces, agents: agentResult.agents };
  } catch (error) {
    if (error instanceof ApiError && error.status === 404) throw redirect("/workspaces");
    throw error;
  }
}

export function createRouter() {
  return createBrowserRouter([
    {
      Component: RootLayout,
      ErrorBoundary: RouteError,
      HydrateFallback: RouteLoading,
      children: [
        { path: "login", Component: LoginRoute },
        { path: "forgot-password", Component: ForgotPasswordRoute },
        { path: "reset-password", Component: ResetPasswordRoute },
        { path: "activate", Component: InvitationActivationRoute },
        {
          id: "authenticated",
          loader: authLoader,
          children: [
            {
              index: true,
              loader: workspacesLoader,
              Component: WorkspaceEntry,
            },
            { path: "workspaces", loader: workspacesLoader, Component: WorkspaceEntry },
            ...["general", "preferences", "security", "applications"].map((section) => ({ path: `settings/${section}`, loader: accountSettingsLoader, Component: SettingsRoute })),
            {
              id: "workspace",
              path: "w/:workspaceId",
              loader: workspaceLoader,
              Component: WorkspaceLayout,
              children: [
                { index: true, loader: ({ params }) => redirect(`/w/${encodeURIComponent(params.workspaceId || "")}/app`) },
                {
                  Component: WorkspaceHomeRoute,
                  children: [
                    { path: "app", element: <></> },
                    { path: "agents/:agentId", element: <></> },
                    ...["preferences", "security", "applications", "general", "members", "groups", "plugins", "models", "global-plugins", "agents"].map((section) => ({ path: `settings/${section}`, Component: SettingsRoute })),
                  ],
                },
                { path: "trash/sessions/:sessionId", Component: TrashSessionRoute },
                { path: "library", Component: LibraryRoute },
                { path: "library/:libraryObjectId", Component: LibraryObjectRoute },
                { path: "settings", loader: ({ params }) => redirect(`/w/${encodeURIComponent(params.workspaceId || "")}/settings/general`) },
              ],
            },
          ],
        },
      ],
    },
  ]);
}
