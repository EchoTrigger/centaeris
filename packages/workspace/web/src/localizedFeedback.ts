import { useState, type Dispatch, type SetStateAction } from "react";
import { useTranslation } from "./i18n";

type LocalizedFeedback = { key: string; values?: Record<string, unknown> };
type Feedback = string | LocalizedFeedback;
export const feedback = (key: string, values?: Record<string, unknown>): LocalizedFeedback => ({ key, values });

// Store message identity, not a translated snapshot, so an open error follows the locale.
export function useLocalizedFeedback(initial: Feedback = ""): [string, Dispatch<SetStateAction<Feedback>>] {
  const [value, setValue] = useState<Feedback>(initial);
  const { t } = useTranslation();
  return [typeof value === "string" ? value : t(value.key, value.values), setValue];
}
