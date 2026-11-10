import { memo, useEffect, useState } from "react";
import { SquareTerminal } from "lucide-react";
import { t } from "../../i18n";

const greetingPeriod = (date: Date) => date.getHours() >= 5 && date.getHours() < 12
  ? "morning" : date.getHours() >= 12 && date.getHours() < 18 ? "afternoon" : "evening";

export const WelcomeGreeting = memo(function WelcomeGreeting() {
  const [period, setPeriod] = useState(() => greetingPeriod(new Date()));
  useEffect(() => {
    if (typeof window === "undefined") return;
    let timer: ReturnType<typeof setTimeout>;
    const refresh = () => {
      clearTimeout(timer);
      const now = new Date();
      setPeriod(greetingPeriod(now));
      const next = new Date(now);
      const hour = [5, 12, 18].find(boundary => boundary > now.getHours());
      if (hour === undefined) next.setDate(next.getDate() + 1);
      next.setHours(hour ?? 5, 0, 0, 0);
      timer = setTimeout(refresh, Math.max(1, next.getTime() - now.getTime()));
    };
    const visibility = () => { if (document.visibilityState === "visible") refresh(); };
    refresh();
    window.addEventListener("focus", refresh);
    document.addEventListener("visibilitychange", visibility);
    return () => { clearTimeout(timer); window.removeEventListener("focus", refresh); document.removeEventListener("visibilitychange", visibility); };
  }, []);
  return <section className="desktopWelcomeIdentity" aria-label="Welcome">
    <SquareTerminal aria-hidden="true" />
    <h1>{t(`welcome.greeting.${period}`)}</h1>
  </section>;
});
