import { memo } from "react";
import { ChartNoAxesColumnIncreasing, Check, ChevronDown } from "lucide-react";
import { useTranslation } from "../i18n";

export const thinkingModeLabels = (t) => Object.freeze({
  none: t("workspaceContextPanel.close"),
  low: t("appRoute.low"),
  medium: t("appRoute.medium"),
  high: t("appRoute.high"),
  xhigh: t("appRoute.veryHigh"),
  max: t("appRoute.maximum"),
});

function closePicker(event) {
  event.currentTarget.closest("details")?.removeAttribute("open");
}

function closePickerOnBlur(event) {
  if (!event.currentTarget.contains(event.relatedTarget)) event.currentTarget.removeAttribute("open");
}

function closePickerOnEscape(event) {
  if (event.key !== "Escape") return;
  event.preventDefault();
  event.currentTarget.removeAttribute("open");
  event.currentTarget.querySelector("summary")?.focus();
}

export const ThinkingPicker = memo(function ThinkingPicker({ model, value, onChange, disabled }) {
  const { t } = useTranslation();
  const labels = thinkingModeLabels(t);
  const modes = model?.thinkingModes || [];
  return <details className="workspaceComposerPicker workspaceComposerThinking" onBlur={closePickerOnBlur} onKeyDown={closePickerOnEscape}>
    <summary
      role="button"
      aria-label={t("appRoute.reasoningEffort")}
      aria-disabled={disabled}
      aria-haspopup="menu"
      onClick={(event) => disabled && event.preventDefault()}
    ><ChartNoAxesColumnIncreasing aria-hidden="true" /></summary>
    <div className="workspaceComposerPickerPanel is-thinking" aria-label={t("appRoute.selectReasoningEffort")}>
      <small>{t("appRoute.reasoningEffort")}</small>
      {modes.map((mode) => <button type="button" aria-pressed={value === mode} key={mode} onClick={(event) => { onChange(mode); closePicker(event); }}><span>{labels[mode] || mode}</span>{value === mode ? <Check aria-hidden="true" /> : null}</button>)}
    </div>
  </details>;
});

export const ModelPicker = memo(function ModelPicker({ groups, model, value, onChange, disabled }) {
  const { t } = useTranslation();
  return <details className="workspaceComposerPicker workspaceComposerModel" onBlur={closePickerOnBlur} onKeyDown={closePickerOnEscape}>
    <summary
      role="button"
      aria-label={t("appRoute.aiModel")}
      aria-disabled={disabled}
      aria-haspopup="menu"
      onClick={(event) => disabled && event.preventDefault()}
    ><span>{model?.displayName || (groups.length ? t("appRoute.selectModel") : t("appRoute.notConfigured"))}</span><ChevronDown aria-hidden="true" /></summary>
    <div className="workspaceComposerPickerPanel is-model" aria-label={t("appRoute.selectAiModel")}>
      {groups.map((group) => <section key={group.provider}>
        <small>{group.label}</small>
        {group.models.map((option) => <button type="button" aria-pressed={value === option.id} key={option.id} onClick={(event) => { onChange(option.id); closePicker(event); }}><span>{option.displayName}</span>{value === option.id ? <Check aria-hidden="true" /> : null}</button>)}
      </section>)}
    </div>
  </details>;
});
