import { feedback, useLocalizedFeedback } from "../localizedFeedback";

import { useTranslation } from "../i18n";
import { type FormEvent, useEffect, useState } from "react";
import { Link, useNavigate } from "react-router";
import { ApiError, apiJson, jsonOptions } from "../api";
import { LoginBrand } from "../components/LoginForm";
import { OnboardingUtilities } from "../components/OnboardingUtilities";


function resetError(error: unknown) {
  if (!(error instanceof ApiError)) return feedback("passwordResetRoute.unableToResetYourPasswordPleaseTryAgain");
  if (error.message === "account_password_reset_unavailable") return feedback("passwordResetRoute.emailIsNotConfiguredContactYourAdministratorToReset");
  if (error.message === "account_password_reset_invalid") return feedback("passwordResetRoute.thisLinkIsInvalidOrExpiredRequestANew");
  if (error.message === "account_password_invalid") return feedback("passwordResetRoute.useAtLeast15CharactersAndAvoidCommonOr");
  if (error.message === "account_password_unchanged") return feedback("passwordResetRoute.theNewPasswordMustDifferFromTheOldPassword");
  if (error.message === "csrf_failed") return feedback("loginForm.securityVerificationFailedPleaseTryAgain");
  return feedback("passwordResetRoute.unableToResetYourPasswordPleaseTryAgain");
}

export function ForgotPasswordRoute() {
  const { t } = useTranslation();
  const [email, setEmail] = useState("");
  const [sent, setSent] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useLocalizedFeedback("");

  async function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (busy) return;
    setBusy(true);
    setError("");
    try {
      await apiJson("/api/account/password-reset-requests", jsonOptions("POST", { email }));
      setSent(true);
    } catch (requestError) {
      setError(resetError(requestError));
    } finally {
      setBusy(false);
    }
  }

  if (sent) return <main className="login"><section className="loginForm">
    <LoginBrand heading={t("passwordResetRoute.checkYourEmail")} description={t("passwordResetRoute.passwordReset")} />
    <p className="loginMessage" role="status">{t("passwordResetRoute.ifThisEmailBelongsToAnAvailableAccountA")}</p>
    <Link className="secondary" to="/login">{t("passwordResetRoute.backToSignIn")}</Link>
  </section><OnboardingUtilities /></main>;

  return <main className="login"><form className="loginForm" onSubmit={submit}>
    <LoginBrand heading={t("passwordResetRoute.resetPassword")} description={t("passwordResetRoute.getAOneTimeLinkByEmail")} />
    <label className="field"><span>{t("loginForm.email")}</span><input autoFocus type="email" autoComplete="email" required value={email} onChange={(event) => setEmail(event.target.value)} /></label>
    {error ? <div className="error" role="alert">{error}</div> : null}
    <button className="primary" type="submit" disabled={busy}>{busy ? t("passwordResetRoute.sending") : t("passwordResetRoute.sendResetLink")}</button>
    <div className="loginAuxiliary"><Link to="/login">{t("passwordResetRoute.backToSignIn")}</Link></div>
  </form><OnboardingUtilities /></main>;
}

export function ResetPasswordRoute() {
  const { t } = useTranslation();
  const navigate = useNavigate();
  const [credentials] = useState(() => {
    const values = new URLSearchParams(window.location.hash.slice(1));
    return { uid: values.get("uid") || "", token: values.get("token") || "" };
  });
  const [password, setPassword] = useState("");
  const [confirmation, setConfirmation] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useLocalizedFeedback("");

  useEffect(() => {
    if (window.location.hash) window.history.replaceState(null, "", "/reset-password");
  }, []);

  async function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (busy) return;
    if (password !== confirmation) {
      setError(feedback("invitationActivationRoute.thePasswordsDoNotMatch"));
      return;
    }
    setBusy(true);
    setError("");
    try {
      await apiJson("/api/account/password-resets", jsonOptions("POST", {
        uid: credentials.uid,
        token: credentials.token,
        newPassword: password,
      }));
      navigate("/login?reset=1", { replace: true });
    } catch (requestError) {
      setError(resetError(requestError));
    } finally {
      setBusy(false);
    }
  }

  if (!credentials.uid || !credentials.token) return <main className="login"><section className="loginForm">
    <LoginBrand heading={t("passwordResetRoute.linkUnavailable")} description={t("passwordResetRoute.passwordReset")} />
    <p className="loginMessage" role="alert">{t("passwordResetRoute.theLinkIsIncompleteOrHasBeenRemovedFrom")}</p>
    <Link className="primary loginCenteredAction" to="/forgot-password">{t("passwordResetRoute.requestANewLink")}</Link>
  </section><OnboardingUtilities /></main>;

  return <main className="login"><form className="loginForm" onSubmit={submit}>
    <LoginBrand heading={t("passwordResetRoute.setANewPassword")} description={t("passwordResetRoute.allPreviousSignInSessionsWillExpireWhenYou")} />
    <label className="field"><span>{t("passwordResetRoute.newPassword")}</span><input autoFocus type="password" autoComplete="new-password" minLength={15} required value={password} onChange={(event) => setPassword(event.target.value)} /></label>
    <label className="field"><span>{t("passwordResetRoute.enterNewPasswordAgain")}</span><input type="password" autoComplete="new-password" minLength={15} required value={confirmation} onChange={(event) => setConfirmation(event.target.value)} /></label>
    {error ? <div className="error" role="alert">{error}</div> : null}
    <button className="primary" type="submit" disabled={busy}>{busy ? t("passwordResetRoute.updating") : t("passwordResetRoute.updatePassword")}</button>
  </form><OnboardingUtilities /></main>;
}
