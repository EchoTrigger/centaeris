import { useState } from "react";
import { apiUrl } from "../api";
import { useTranslation } from "../i18n";
import { agentSettingsPath } from "../agent-chat/agentSettingsNavigation";
import { buildDelegationGuide } from "./appDelegationGuide";

export default function AppDelegationDetails({ delegation, tab, onTabChange, onBack, canConfigure, active, busy, onRotate, onRevoke, businessUsers }) {
  const { t, i18n } = useTranslation();
  const [language, setLanguage] = useState("curl");
  const [copied, setCopied] = useState(false);
  const [copyError, setCopyError] = useState("");
  const native = Boolean(delegation.agentId);
  const tabs = native ? ["overview", "api", "users", "credentials"] : ["overview", "api", "credentials"];
  const apiAddress = apiUrl("/api");
  const guide = buildDelegationGuide(apiAddress.slice(0, -4), delegation);
  const targetName = t(native ? "appDelegations.nativeAgentName" : "appDelegations.publishedAssistantName", { name: native ? delegation.agentName : delegation.definitionName });
  const status = delegation.revokedAt ? t("appDelegations.status.revoked") : active ? t("appDelegations.status.active") : t("appDelegations.expired");
  const expiry = delegation.expiresAt === null ? t("appDelegations.permanent") : new Date(delegation.expiresAt).toLocaleString(i18n.language);

  function keyDown(event) {
    const index = tabs.indexOf(tab);
    const next = ["ArrowRight", "ArrowDown"].includes(event.key) ? tabs[(index + 1) % tabs.length] : ["ArrowLeft", "ArrowUp"].includes(event.key) ? tabs[(index + tabs.length - 1) % tabs.length] : event.key === "Home" ? tabs[0] : event.key === "End" ? tabs.at(-1) : null;
    if (!next) return;
    event.preventDefault(); onTabChange(next);
    event.currentTarget.parentElement.querySelector(`[data-tab="${next}"]`).focus();
  }

  async function copyExample() {
    setCopyError("");
    try { await navigator.clipboard.writeText(guide.examples[language]); setCopied(true); }
    catch { setCopied(false); setCopyError(t("appDelegations.exampleCopyFailed")); }
  }

  return <section className="appDelegationDetails" data-selected-delegation-id={delegation.id}>
    <button type="button" className="appDelegationBack" data-action="back" onClick={onBack}>{t("appDelegations.backToApplications")}</button>
    <header className="appDelegationDetailHeader"><h2>{delegation.appName}</h2><p>{targetName}</p><small>{delegation.workspaceName}</small></header>
    <div className="appDelegationDetailLayout">
      <div role="tablist" aria-orientation="horizontal" aria-label={t("appDelegations.detailNavigation")} className="appDelegationDetailNav">
        {tabs.map(key => <button key={key} type="button" role="tab" id={`delegation-tab-${key}`} data-tab={key} aria-controls={`delegation-panel-${key}`} aria-selected={tab === key} tabIndex={tab === key ? 0 : -1} onKeyDown={keyDown} onClick={() => onTabChange(key)}>{t(`appDelegations.tab.${key}`)}</button>)}
      </div>
      <div role="tabpanel" id={`delegation-panel-${tab}`} aria-labelledby={`delegation-tab-${tab}`} tabIndex={0} className="appDelegationDetailPanel">
        {tab === "overview" ? <>
          <h3>{t("appDelegations.tab.overview")}</h3><p>{t(native ? "appDelegations.nativeOverview" : "appDelegations.publishedOverview")}</p>
          <dl className="appDelegationFacts"><dt>{t("appDelegations.assistant")}</dt><dd>{targetName}<code translate="no">{delegation.agentId || delegation.definitionId}</code></dd><dt>{t("appDelegations.apiAddress")}</dt><dd><code translate="no">{apiAddress}</code></dd><dt>{t("appDelegations.authorizationId")}</dt><dd><code translate="no">{delegation.id}</code></dd><dt>{t("appDelegations.authorizationStatus")}</dt><dd>{status}</dd><dt>{t("appDelegations.duration")}</dt><dd>{expiry}</dd></dl>
          <p>{t("appDelegations.currentAccessHint")}</p>
          {native && canConfigure ? <a className="appDelegationInlineLink" href={agentSettingsPath(delegation.workspaceId, delegation.agentId)}>{t("appDelegations.rootConfiguration")}</a> : null}
          <div className="appDelegationActions"><button type="button" onClick={() => onTabChange("api")}>{t("appDelegations.quickStart")}</button><button type="button" onClick={() => onTabChange("credentials")}>{t("appDelegations.manageCredentials")}</button></div>
        </> : null}
        {tab === "api" ? <div className="appDelegationGuide">
          <h3>{t("appDelegations.quickStart")}</h3><p>{t("appDelegations.tokenBackendHint")}</p><p>{t(native ? "appDelegations.nativeGuideHint" : "appDelegations.publishedGuideHint")}</p>
          <ol className="appDelegationGuideSteps">{guide.steps.map(step => <li key={step.key}>
            <h4>{t(`appDelegations.guideStep.${step.key}`, { defaultValue: step.key })}</h4><code className="appDelegationEndpoint" translate="no">{step.method} {step.path}</code>
            <details><summary>{t("appDelegations.requestDetails")}</summary><p>{t("appDelegations.requiredPermissions")}: <code translate="no">{step.requiredScopes.join(", ")}</code></p>{step.request ? <><h5>{t("appDelegations.requestBody")}</h5><pre translate="no">{step.request}</pre></> : null}<h5>{t("appDelegations.responseBody")}</h5><pre translate="no">{step.response}</pre></details>
          </li>)}</ol>
          <div className="appDelegationExampleHeader"><h4>{t("appDelegations.codeExamples")}</h4><div className="appDelegationActions" aria-label={t("appDelegations.exampleLanguage")}>{["curl", "python", "javascript"].map(key => <button type="button" key={key} aria-pressed={language === key} onClick={() => { setLanguage(key); setCopied(false); setCopyError(""); }}>{key === "curl" ? "cURL" : key === "python" ? "Python" : "JavaScript"}</button>)}</div></div>
          <pre className="appDelegationExample" translate="no">{guide.examples[language]}</pre><button type="button" onClick={() => void copyExample()}>{t(copied ? "appDelegations.exampleCopied" : "appDelegations.copyExample")}</button>{copyError ? <p role="alert" className="accountSecurityError">{copyError}</p> : null}
          {native ? <><h4>{t("appDelegations.deliveryMeaning")}</h4><p>{t("appDelegations.deliveryHint")}</p><p>{t("appDelegations.cursorHint")}</p><p>{t("appDelegations.readHint")}</p><h4>{t("appDelegations.difyTitle")}</h4><ol><li>{t("appDelegations.difyIdentity")}</li><li>{t("appDelegations.difyResolve")}<pre translate="no">{'request_body = {"businessUserId": stable_id}'}</pre><code translate="no">{'{{#encode.request_body#}}'}</code></li><li>{t("appDelegations.difyConsume")}</li></ol><p>{t("appDelegations.difyBoundary")}</p></> : null}
          <details className="appDelegationDeveloperDetails"><summary>{t("appDelegations.errorsAndRetry")}</summary><p>{t(native ? "appDelegations.nativeRetryHint" : "appDelegations.publishedRetryHint")}</p><p>{t("appDelegations.errorHint")}</p></details>
          {!native ? <p>{t("appDelegations.sseHint")}</p> : null}
        </div> : null}
        {tab === "users" && native ? businessUsers : null}
        {tab === "credentials" ? <>
          <h3>{t("appDelegations.tab.credentials")}</h3><p>{t("appDelegations.rotateHint")}</p><p>{t(native ? "appDelegations.acceptedWorkHint" : "appDelegations.publishedWithdrawalHint")}</p>
          <dl className="appDelegationFacts"><dt>{t("appDelegations.authorizationId")}</dt><dd><code translate="no">{delegation.id}</code></dd><dt>{t("appDelegations.authorizationStatus")}</dt><dd>{status}</dd><dt>{t("appDelegations.duration")}</dt><dd>{expiry}</dd><dt>{t("appDelegations.credentialVersion")}</dt><dd>{delegation.credentialVersion}</dd><dt>{t("appDelegations.createdAt")}</dt><dd>{new Date(delegation.createdAt).toLocaleString(i18n.language)}</dd><dt>{t("appDelegations.scopes")}</dt><dd>{delegation.scopes.map(scope => <span key={scope}>{t(`appDelegations.scope.${scope.replace(":", ".")}`)}</span>)}</dd></dl>
          <details className="appDelegationDeveloperDetails"><summary>{t("appDelegations.exactPermissions")}</summary><code translate="no">{delegation.scopes.join(", ")}</code></details>
          <div className="appDelegationActions">{active ? <button type="button" disabled={busy} onClick={() => void onRotate(delegation)}>{t("appDelegations.rotateToken")}</button> : null}{!delegation.revokedAt ? <button type="button" disabled={busy} onClick={() => void onRevoke(delegation)}>{t("appDelegations.revokeGrant")}</button> : null}</div>
        </> : null}
      </div>
    </div>
  </section>;
}
