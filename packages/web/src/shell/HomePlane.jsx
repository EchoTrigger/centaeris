import { useEffect, useState } from "react";
import { t, useTranslation } from "../i18n";
import { Grid2X2, Search, Sparkles, Table2 } from "lucide-react";

const QUICK_ACTIONS = () => ([
  { label: t("homePlane.createSlides"), icon: Grid2X2, prompt: t("homePlane.helpMeCreateASlideDeckFirstConfirmThe") },
  { label: t("homePlane.spreadsheet"), icon: Table2, prompt: t("homePlane.helpMeOrganizeThisIntoATableFirstConfirm") },
  { label: t("homePlane.research"), icon: Search, prompt: t("homePlane.researchThisTopicFirstOutlineAPlanAndThe") },
  { label: t("homePlane.visualize"), icon: Sparkles, prompt: t("homePlane.visualizeThisInformationFirstConfirmTheMostImportantRelationships") },
]);

const greetingHours = [5, 12, 18];

function greetingPeriod(date) {
  const hour = date.getHours();
  return hour >= 5 && hour < 12 ? "morning" : hour >= 12 && hour < 18 ? "afternoon" : "evening";
}

export function HomePlane() {
  const { t } = useTranslation();
  const [period, setPeriod] = useState(() => greetingPeriod(new Date()));

  useEffect(() => {
    let timer;
    function refresh() {
      clearTimeout(timer);
      const now = new Date();
      setPeriod(greetingPeriod(now));
      const nextHour = greetingHours.find(hour => hour > now.getHours());
      const next = new Date(now);
      if (nextHour === undefined) next.setDate(next.getDate() + 1);
      next.setHours(nextHour ?? greetingHours[0], 0, 0, 0);
      timer = window.setTimeout(refresh, next.getTime() - now.getTime());
    }
    refresh();
    window.addEventListener("focus", refresh);
    return () => { clearTimeout(timer); window.removeEventListener("focus", refresh); };
  }, []);

  return (
    <div className="shHome">
      <img className="shHomeMark" src="/centaeris-mark.png" alt="" width="48" height="48" />
      <h1>{t(`homePlane.greeting.${period}`)}</h1>
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
