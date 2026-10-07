import { t } from "../i18n";
import { useTranslation } from "../i18n";
import { Grid2X2, Search, Sparkles, Table2 } from "lucide-react";
import { AgentMark } from "./AgentMark";

const QUICK_ACTIONS = () => ([
  { label: t("homePlane.createSlides"), icon: Grid2X2, prompt: t("homePlane.helpMeCreateASlideDeckFirstConfirmThe") },
  { label: t("homePlane.spreadsheet"), icon: Table2, prompt: t("homePlane.helpMeOrganizeThisIntoATableFirstConfirm") },
  { label: t("homePlane.research"), icon: Search, prompt: t("homePlane.researchThisTopicFirstOutlineAPlanAndThe") },
  { label: t("homePlane.visualize"), icon: Sparkles, prompt: t("homePlane.visualizeThisInformationFirstConfirmTheMostImportantRelationships") },
]);

export function HomePlane({ agent }) {
  const { t } = useTranslation();
  return (
    <div className="shHome" aria-label={t("homePlane.home")}>
      <AgentMark className="shHomeAvatar" agent={agent} />
    </div>
  );
}

export function HomeQuickActions({ onQuickAction }) {
  const { t } = useTranslation();
  return (
    <div className="shQuickActions" aria-label={t("homePlane.quickActions")}>
        {QUICK_ACTIONS().map(({ label, icon: Icon, prompt }) => (
          <button className="shQuickAction" type="button" key={label} onClick={() => onQuickAction(prompt)}>
            <Icon aria-hidden="true" /><span>{label}</span>
          </button>
        ))}
    </div>
  );
}
