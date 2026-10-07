import { feedback, useLocalizedFeedback } from "../localizedFeedback";
import { t } from "../i18n";
import { useTranslation } from "../i18n";
import { type FormEvent, useState } from "react";
import { Link } from "react-router";
import { ApiError, apiJson, apiResponse, clearCsrfToken, jsonOptions } from "../api";

type LoginIdentity = { id: string; email: string };

type LoginFormProps = {
  initialEmail?: string;
  expectedUserId?: string;
  emailReadOnly?: boolean;
  embedded?: boolean;
  heading?: string;
  description?: string;
  submitLabel?: string;
  notice?: string;
  onAuthenticated: (user: LoginIdentity) => void | Promise<void>;
};

export function LoginBrand({ heading, description }: { heading: string; description: string }) {
  return <div className="loginBrand">
    <img src="/centaeris-mark.png" alt="" />
    <div><h1>{heading}</h1><p>{description}</p></div>
  </div>;
}

function loginError(error: unknown, reauthenticating: boolean) {
  if (error instanceof ApiError && error.message === "invalid_credentials") {
    return reauthenticating ? feedback("loginForm.incorrectPasswordPleaseTryAgain") : feedback("loginForm.incorrectEmailOrPassword");
  }
  if (error instanceof ApiError && error.message === "csrf_failed") return feedback("loginForm.securityVerificationFailedPleaseTryAgain");
  return reauthenticating ? feedback("loginForm.unableToSignInAgainPleaseTryAgain") : feedback("loginForm.unableToSignInPleaseTryAgain");
}

export function LoginForm({
  initialEmail = "",
  expectedUserId,
  emailReadOnly = false,
  embedded = false,
  heading = t("loginForm.signIn"),
  description = "Centaeris Workspace",
  submitLabel = t("loginForm.signIn"),
  notice = "",
  onAuthenticated,
}: LoginFormProps) {
  const { t } = useTranslation();
  const [email, setEmail] = useState(initialEmail);
  const [password, setPassword] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useLocalizedFeedback("");
  const reauthenticating = Boolean(expectedUserId);

  async function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (busy) return;
    setBusy(true);
    setError("");
    try {
      const result = await apiJson<{ user: LoginIdentity }>("/api/login", jsonOptions("POST", { email, password }));
      clearCsrfToken();
      if (!result.user?.id || !result.user.email) throw new Error("login_response_invalid");
      if (expectedUserId && result.user.id !== expectedUserId) {
        try {
          await apiResponse("/api/logout", { method: "POST" });
        } finally {
          clearCsrfToken();
          window.location.assign("/login");
        }
        return;
      }
      await onAuthenticated(result.user);
    } catch (requestError) {
      setError(loginError(requestError, reauthenticating));
    } finally {
      setBusy(false);
    }
  }

  return <form className={`loginForm${embedded ? " loginFormEmbedded" : ""}`} onSubmit={submit}>
    {!embedded ? <LoginBrand heading={heading} description={description} /> : null}
    {notice ? <div className="success" role="status">{notice}</div> : null}
    <label className="field">
      <span>{t("loginForm.email")}</span>
      <input type="email" autoComplete="username" required readOnly={reauthenticating || emailReadOnly} value={email} onChange={(event) => setEmail(event.target.value)} />
    </label>
    <label className="field">
      <span>{t("loginForm.password")}</span>
      <input autoFocus={reauthenticating || emailReadOnly} type="password" autoComplete="current-password" required value={password} onChange={(event) => setPassword(event.target.value)} />
    </label>
    {!reauthenticating && !embedded ? <div className="loginAuxiliary"><Link to="/forgot-password">{t("loginForm.forgotPassword")}</Link></div> : null}
    {error ? <div className="error" role="alert">{error}</div> : null}
    <button className="primary" type="submit" disabled={busy}>{busy ? t("loginForm.signingIn") : submitLabel}</button>
  </form>;
}
