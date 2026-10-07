import { i18n } from "../i18n";
import { t } from "../i18n";
import { useTranslation } from "../i18n";
import { useEffect, useState } from "react";
import { ArrowRight, Building2, LoaderCircle } from "lucide-react";
import { useNavigate } from "react-router";
import { ApiError, apiJson, apiResponse, clearCsrfToken, isAuthenticationRequired, jsonOptions } from "../api";
import { LoginForm } from "../components/LoginForm";

const ERRORS = () => ({
  invitation_not_found: t("invitationActivationRoute.thisInvitationIsInvalidOrHasAlreadyBeenUsed"),
  invitation_expired: t("invitationActivationRoute.thisInvitationHasExpiredAskAWorkspaceAdministratorFor"),
  invitation_not_pending: t("invitationActivationRoute.thisInvitationHasAlreadyBeenProcessedAndCannotBe"),
  invitation_account_mismatch: t("invitationActivationRoute.theSignedInAccountDoesNotMatchTheInvitation"),
  invitation_account_inactive: t("invitationActivationRoute.theInvitedAccountIsCurrentlyUnavailable"),
  invitation_account_created_concurrently: t("invitationActivationRoute.theAccountStatusHasJustChangedRefreshThePage"),
  invitation_account_setup_required: t("invitationActivationRoute.enterYourNameAndPassword"),
  password_invalid: t("invitationActivationRoute.thePasswordDoesNotMeetTheCurrentSecurityRequirements"),
  workspace_member_exists: t("invitationActivationRoute.thisAccountIsAlreadyAWorkspaceMember"),
  workspace_unavailable: t("invitationActivationRoute.thisWorkspaceCannotBeJoinedRightNow"),
});

function errorText(error) {
  const message = error instanceof Error ? error.message : String(error);
  return ERRORS()[message] || message;
}

function roleLabel(role) {
  return role === "admin" ? t("invitationActivationRoute.administrator") : t("invitationActivationRoute.member");
}

export default function InvitationActivationRoute() {
  const { t } = useTranslation();
  const navigate = useNavigate();
  const [token] = useState(() => new URLSearchParams(window.location.hash.slice(1)).get("token") || "");
  const [preview, setPreview] = useState(null);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [name, setName] = useState("");
  const [password, setPassword] = useState("");
  const [passwordConfirmation, setPasswordConfirmation] = useState("");
  const [loginRequired, setLoginRequired] = useState(false);
  const [accountMismatch, setAccountMismatch] = useState(false);
  const [notice, setNotice] = useState("");

  useEffect(() => {
    if (window.location.hash) window.history.replaceState(null, "", "/activate");
  }, []);

  useEffect(() => {
    let active = true;
    async function load() {
      if (!token) {
        setError(t("invitationActivationRoute.theInvitationLinkIsMissingItsToken"));
        setLoading(false);
        return;
      }
      try {
        const result = await apiJson("/api/invitations/preview", jsonOptions("POST", { token }));
        if (active) setPreview(result);
      } catch (requestError) {
        if (active) setError(errorText(requestError));
      } finally {
        if (active) setLoading(false);
      }
    }
    void load();
    return () => { active = false; };
  }, [token, t]);

  async function accept(event) {
    event.preventDefault();
    if (!preview || busy) return;
    if (!preview.accountExists && password !== passwordConfirmation) {
      setError(t("invitationActivationRoute.thePasswordsDoNotMatch"));
      return;
    }
    setBusy(true);
    setError("");
    try {
      const body = preview.accountExists ? { token } : { token, name, password };
      const result = await apiJson("/api/invitations/accept", jsonOptions("POST", body));
      clearCsrfToken();
      navigate(`/w/${encodeURIComponent(result.workspaceId)}/app`, { replace: true });
    } catch (requestError) {
      if (requestError instanceof ApiError && requestError.message === "invitation_login_required") {
        setLoginRequired(true);
        return;
      }
      if (requestError instanceof ApiError && requestError.message === "invitation_account_mismatch") {
        setAccountMismatch(true);
        setError(errorText(requestError));
        return;
      }
      setError(errorText(requestError));
    } finally {
      setBusy(false);
    }
  }

  async function switchAccount() {
    if (!preview || busy) return;
    setBusy(true);
    setError("");
    try {
      await apiResponse("/api/logout", { method: "POST" });
    } catch (requestError) {
      if (!isAuthenticationRequired(requestError)) {
        setError(t("invitationActivationRoute.unableToSwitchAccountsPleaseTryAgain"));
        setBusy(false);
        return;
      }
    }
    clearCsrfToken();
    setAccountMismatch(false);
    setNotice("");
    setLoginRequired(preview.accountExists);
    setBusy(false);
  }

  return <main className="activationPage">
    <section className="activationCard" aria-labelledby="activation-title">
      {loading ? <div className="activationState" aria-live="polite"><LoaderCircle aria-hidden="true" />{t("invitationActivationRoute.loadingInvitation")}</div> : null}
      {!loading && !preview ? <><h1 id="activation-title">{t("invitationActivationRoute.unableToAcceptInvitation")}</h1><p className="activationError" role="alert">{error}</p></> : null}
      {!loading && preview ? <>
        <header>
          <span className="activationWorkspaceMark" aria-hidden="true"><Building2 /></span>
          <div><small>{t("invitationActivationRoute.workspaceInvitation")}</small><h1 id="activation-title">{t("invitation.join", { workspace: preview.workspaceName })}</h1></div>
        </header>
        <dl>
          <div><dt>{t("invitationActivationRoute.account")}</dt><dd>{preview.email}</dd></div>
          <div><dt>{t("invitationActivationRoute.role")}</dt><dd>{roleLabel(preview.role)}</dd></div>
          <div><dt>{t("invitationActivationRoute.expiresAt")}</dt><dd>{new Date(preview.expiresAt).toLocaleString(i18n.resolvedLanguage)}</dd></div>
        </dl>
        {loginRequired ? <div className="activationLogin">
          <div className="activationLoginHeading"><h2>{t("invitationActivationRoute.signInWithTheInvitedAccount")}</h2><span>{t("invitation.signInAs", { email: preview.email })}</span></div>
          <LoginForm
            embedded
            emailReadOnly
            initialEmail={preview.email}
            submitLabel={t("invitationActivationRoute.signInWithTheInvitedAccount")}
            onAuthenticated={() => {
              setLoginRequired(false);
              setAccountMismatch(false);
              setError("");
              setNotice(t("invitationActivationRoute.youAreSignedInWithTheInvitedAccountConfirm"));
            }}
          />
        </div> : <form onSubmit={accept}>
          {!preview.accountExists ? <>
            <label>{t("invitationActivationRoute.name")}<input autoFocus required maxLength={150} value={name} onChange={(event) => setName(event.target.value)} /></label>
            <label>{t("invitationActivationRoute.setPassword")}<input type="password" autoComplete="new-password" minLength={15} required value={password} onChange={(event) => setPassword(event.target.value)} /></label>
            <label>{t("invitationActivationRoute.confirmPassword")}<input type="password" autoComplete="new-password" minLength={15} required value={passwordConfirmation} onChange={(event) => setPasswordConfirmation(event.target.value)} /></label>
            <p>{t("invitationActivationRoute.useAtLeast15Characters")}</p>
          </> : <p>{notice || t("invitationActivationRoute.signInWithTheInvitedEmailAboveToAccept")}</p>}
          {error ? <div className="activationError" role="alert">{error}</div> : null}
          {accountMismatch ? <button className="secondary" type="button" disabled={busy} onClick={switchAccount}>{t("invitationActivationRoute.switchToTheInvitedAccount")}</button> : null}
          <button className="primary" type="submit" disabled={busy || (!preview.accountExists && (!name.trim() || !password || !passwordConfirmation))}>
            {busy ? t("invitationActivationRoute.accepting") : t("invitationActivationRoute.acceptAndOpenWorkspace")}<ArrowRight aria-hidden="true" />
          </button>
        </form>}
      </> : null}
    </section>
  </main>;
}
