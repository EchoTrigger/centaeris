import { useEffect, useRef, useState } from "react";
import { useLocation } from "react-router";
import { ApiError, apiJson, clearCsrfToken, jsonOptions } from "../api";
import { useTranslation } from "../i18n";

const redirectBrowser = url => window.location.assign(url);
const ERROR_KEYS = {
  account_current_password_invalid: "sso.passwordInvalid",
  sso_cancelled: "sso.cancelled",
  sso_identity_conflict: "sso.identityConflict",
  sso_identity_unlinked: "sso.identityUnlinked",
  sso_state_invalid: "sso.expired",
  sso_provider_changed: "sso.providerChanged",
  sso_provider_unavailable: "sso.providerUnavailable",
  sso_account_unavailable: "sso.accountUnavailable",
  sso_too_many_attempts: "sso.tooManyAttempts",
};

export function SsoFeedback({ search = "" }) {
  const { t } = useTranslation();
  const params = new URLSearchParams(search);
  if (params.has("ssoError")) return <p className="error" role="alert">{t(ERROR_KEYS[params.get("ssoError")] || "sso.failed")}</p>;
  const notice = params.get("ssoNotice");
  return notice === "linked" || notice === "unlinked"
    ? <p className="success" role="status">{t(notice === "linked" ? "sso.linkedNotice" : "sso.unlinkedNotice")}</p> : null;
}

export function SsoLoginOptions({ returnTo = "/", redirect = redirectBrowser }) {
  const { t } = useTranslation();
  const [providers, setProviders] = useState([]);
  const [error, setError] = useState("");
  const [loading, setLoading] = useState(false);
  const busy = useRef(false);
  useEffect(() => {
    const controller = new AbortController();
    apiJson("/api/sso/providers", { signal: controller.signal }).then(result => {
      if (!controller.signal.aborted) setProviders(result.providers);
    }).catch(() => {
      if (!controller.signal.aborted) setError("sso.loadFailed");
    });
    return () => controller.abort();
  }, []);

  async function signIn(provider) {
    if (busy.current) return;
    busy.current = true; setLoading(true); setError("");
    try {
      const result = await apiJson(`/api/sso/${encodeURIComponent(provider.id)}/login`, jsonOptions("POST", { returnTo }));
      redirect(result.authorizationUrl);
    } catch (requestError) {
      setError(ERROR_KEYS[requestError instanceof ApiError ? requestError.message : ""] || "sso.failed");
      busy.current = false; setLoading(false);
    }
  }

  if (!providers.length) return error ? <p className="error" role="alert">{t(error)}</p> : null;
  return <div className="ssoLoginOptions">
    <span className="ssoDivider">{t("sso.or")}</span>
    {providers.map(provider => <button type="button" key={provider.id} disabled={loading} onClick={() => signIn(provider)}>{t("sso.signInWith", { provider: provider.displayName })}</button>)}
    {error ? <p className="error" role="alert">{t(error)}</p> : null}
  </div>;
}

export function ExternalIdentitySettings({ returnTo, redirect = redirectBrowser }) {
  const { t } = useTranslation();
  const location = useLocation();
  const [data, setData] = useState(null);
  const [loadError, setLoadError] = useState(false);
  const [refresh, setRefresh] = useState(0);
  const [confirmation, setConfirmation] = useState(null);
  const [password, setPassword] = useState("");
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");
  const [loading, setLoading] = useState(false);
  const busy = useRef(false);
  const opener = useRef(null);
  // biome-ignore lint/correctness/useExhaustiveDependencies: Explicit retry and successful unlink invalidate the server data.
  useEffect(() => {
    const controller = new AbortController();
    apiJson("/api/account/external-identities", { signal: controller.signal }).then(result => {
      if (!controller.signal.aborted) { setData(result); setLoadError(false); }
    }).catch(() => {
      if (!controller.signal.aborted) setLoadError(true);
    });
    return () => controller.abort();
  }, [refresh]);

  function open(provider, event) {
    opener.current = event?.currentTarget;
    setConfirmation(provider); setPassword(""); setError(""); setNotice("");
  }
  function close() {
    if (busy.current) return;
    setConfirmation(null); setPassword(""); setError("");
    opener.current?.focus();
  }
  async function submit(event) {
    event.preventDefault();
    if (busy.current || !password) return;
    busy.current = true; setLoading(true); setError("");
    try {
      if (confirmation.identity) {
        const result = await apiJson(`/api/account/external-identities/${encodeURIComponent(confirmation.identity.id)}/unlink`, jsonOptions("POST", { currentPassword: password }));
        setPassword(""); setConfirmation(null);
        if (result.signedOut) {
          clearCsrfToken(); redirect("/login?ssoNotice=unlinked"); return;
        }
        setNotice("sso.unlinkedNotice"); setRefresh(value => value + 1);
      } else {
        const result = await apiJson(`/api/account/external-identities/${encodeURIComponent(confirmation.id)}/link`, jsonOptions("POST", { currentPassword: password, returnTo: returnTo || location.pathname }));
        setPassword(""); redirect(result.authorizationUrl);
      }
    } catch (requestError) {
      setError(ERROR_KEYS[requestError instanceof ApiError ? requestError.message : ""] || "sso.failed");
    } finally {
      busy.current = false; setLoading(false);
    }
  }

  if (!data && !loadError) return <p className="ssoLoading" role="status">{t("sso.loading")}</p>;
  if (loadError) return <section className="externalIdentitySettings"><p className="error" role="alert">{t("sso.loadFailed")}</p><button type="button" onClick={() => setRefresh(value => value + 1)}>{t("sso.retry")}</button></section>;
  if (!data.providers.length) return <section className="externalIdentitySettings" aria-labelledby="external-identities-heading">
    <h2 id="external-identities-heading">{t("sso.title")}</h2>
    <p className="ssoHint">{t("sso.notConfigured")}</p>
  </section>;
  return <section className="externalIdentitySettings" aria-labelledby="external-identities-heading">
    <h2 id="external-identities-heading">{t("sso.title")}</h2>
    <p className="ssoHint">{t("sso.hint")}</p>
    <SsoFeedback search={notice || confirmation ? "" : location.search} />
    {notice ? <p className="success" role="status">{t(notice)}</p> : null}
    {!data.canVerifyPassword ? <p className="ssoHint">{t("sso.passwordRequired")}</p> : null}
    {data.providers.map(provider => <div className="ssoProvider" key={provider.id}>
      <div className="ssoProviderRow">
        <div className="ssoProviderIdentity"><strong>{provider.displayName}</strong><span>{t(provider.identity ? "sso.linked" : "sso.notLinked")}</span>
          {provider.identity ? <small>{provider.identity.displayName} <span>{provider.identity.subjectHint}</span></small> : null}
          {!provider.enabled ? <small>{t("sso.disabled")}</small> : null}
        </div>
        <button type="button" disabled={loading || !data.canVerifyPassword || (!provider.identity && !provider.enabled)} onClick={event => open(provider, event)}>{t(provider.identity ? "sso.unlink" : "sso.link")}</button>
      </div>
      {confirmation?.id === provider.id ? <form className="ssoConfirmation" onSubmit={submit}>
        <p>{t(provider.identity ? "sso.unlinkHint" : "sso.linkHint", { provider: provider.displayName })}</p>
        <label className="field"><span>{t("settingsRoute.currentPassword")}</span><input autoFocus type="password" autoComplete="current-password" required value={password} onChange={event => { setPassword(event.target.value); setError(""); }} /></label>
        {error ? <p className="error" role="alert">{t(error)}</p> : null}
        <div className="ssoActions"><button type="button" disabled={loading} onClick={close}>{t("sso.cancel")}</button><button type="submit" className="primary" disabled={loading || !password}>{t(loading ? "sso.working" : provider.identity ? "sso.confirmUnlink" : "sso.continue")}</button></div>
      </form> : null}
    </div>)}
  </section>;
}
