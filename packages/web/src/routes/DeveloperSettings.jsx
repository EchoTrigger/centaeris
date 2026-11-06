import { useState } from "react";
import { Link } from "react-router";
import { apiUrl } from "../api";
import { DeveloperCredentials } from "../developer/DeveloperCredentials";
import { DeveloperDocumentation } from "../developer/DeveloperDocumentation";
import { CopyButton, DeveloperExample, runPath } from "../developer/DeveloperExample";
import { useTranslation } from "../i18n";

export const DEVELOPER_DOC_GROUPS = [
  { label: "developer.guides", topics: ["quickstart", "authorization", "lifecycle"] },
  { label: "developer.reference", topics: ["events", "artifacts", "errors"] },
];

const docTopics = DEVELOPER_DOC_GROUPS.flatMap(group => group.topics);
export const DEFAULT_DEVELOPER_TOPIC = "quickstart";

export function developerTopic(value) {
  return docTopics.includes(value) ? value : DEFAULT_DEVELOPER_TOPIC;
}

export function developerDocSearch(topic) {
  return `?${new URLSearchParams({ view: "docs", topic })}`;
}

export function DeveloperSettings({ workspaceId, canManage, topic, lastTopic, navigationState }) {
  const { t } = useTranslation();
  const endpoint = apiUrl(runPath);
  const [exampleLanguage, setExampleLanguage] = useState("curl");
  return <div className="preferenceSettings developerSettings">
    <nav className="extensionTabs" aria-label={t("developer.title")}>
      <Link to={{ search: "" }} state={navigationState} aria-current={!topic ? "page" : undefined}>{t("developer.console")}</Link>
      <Link to={{ search: developerDocSearch(lastTopic) }} state={navigationState} aria-current={topic ? "page" : undefined}>{t("developer.docs")}</Link>
    </nav>
    {topic ? <DeveloperDocumentation topic={topic} exampleLanguage={exampleLanguage} onExampleLanguageChange={setExampleLanguage} /> : <>
      <header className="settingsPageHeading">
        <h1>{t("developer.agentRunApi")}</h1>
        <p>{t("developer.consoleDescription")}</p>
      </header>
      <section className="settingsPanel developerSection" aria-labelledby="developer-api-heading">
        <div className="developerSectionHead">
          <h2 id="developer-api-heading">{t("developer.apiAddress")}</h2>
          <CopyButton value={endpoint} label={t("developer.copyAddress")} />
        </div>
        <div className="developerEndpoint"><code className="developerMethod">POST</code><code>{endpoint}</code></div>
      </section>
      {canManage ? <DeveloperCredentials workspaceId={workspaceId} /> : <section className="settingsPanel developerSection">
        <h2>{t("developer.credentials")}</h2>
        <p>{t("developer.memberCredentialsHint")}</p>
      </section>}
      <DeveloperExample language={exampleLanguage} onLanguageChange={setExampleLanguage} />
    </>}
  </div>;
}
