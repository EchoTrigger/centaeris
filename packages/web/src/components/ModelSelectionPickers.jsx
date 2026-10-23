import { memo } from "react";
import { ChartNoAxesColumnIncreasing, Check, ChevronDown } from "lucide-react";
import { Menu } from "@centaeris/frontend-foundations/primitives/menu";
import { useTranslation } from "../i18n";

export const thinkingModeLabels = (t) => Object.freeze({
  none: t("workspaceContextPanel.close"), low: t("appRoute.low"), medium: t("appRoute.medium"),
  high: t("appRoute.high"), xhigh: t("appRoute.veryHigh"), max: t("appRoute.maximum"),
});

function PickerPanel({ className, label, children }) {
  return <Menu.Portal>
    <Menu.Positioner side="top" align="end" sideOffset={7} className="workspaceComposerPickerPositioner">
      <Menu.Popup className={`workspaceComposerPickerPanel ${className}`} aria-label={label}>{children}</Menu.Popup>
    </Menu.Positioner>
  </Menu.Portal>;
}

function Choice({ value, selected, onChange, children }) {
  return <Menu.RadioItem value={value} nativeButton closeOnClick render={<button type="button" aria-pressed={selected} />}
    onClick={() => onChange(value)}>
    <span>{children}</span>{selected ? <Check aria-hidden="true" /> : null}
  </Menu.RadioItem>;
}

export const ThinkingPicker = memo(function ThinkingPicker({ model, value, onChange, disabled }) {
  const { t } = useTranslation();
  const labels = thinkingModeLabels(t), modes = model?.thinkingModes || [];
  return <div className="workspaceComposerPicker workspaceComposerThinking">
    <Menu.Root modal={false}>
      <Menu.Trigger className="workspaceComposerPickerTrigger" disabled={disabled} aria-label={t("appRoute.reasoningEffort")}>
        <ChartNoAxesColumnIncreasing aria-hidden="true" />
      </Menu.Trigger>
      <PickerPanel className="is-thinking" label={t("appRoute.selectReasoningEffort")}>
        <small>{t("appRoute.reasoningEffort")}</small>
        <Menu.RadioGroup value={value}>
          {modes.map(mode => <Choice value={mode} selected={value === mode} onChange={onChange} key={mode}>{labels[mode] || mode}</Choice>)}
        </Menu.RadioGroup>
      </PickerPanel>
    </Menu.Root>
  </div>;
});

export const ModelPicker = memo(function ModelPicker({ groups, model, value, onChange, disabled }) {
  const { t } = useTranslation();
  return <div className="workspaceComposerPicker workspaceComposerModel">
    <Menu.Root modal={false}>
      <Menu.Trigger className="workspaceComposerPickerTrigger" disabled={disabled} aria-label={t("appRoute.aiModel")}>
        <span>{model?.displayName || (groups.length ? t("appRoute.selectModel") : t("appRoute.notConfigured"))}</span><ChevronDown aria-hidden="true" />
      </Menu.Trigger>
      <PickerPanel className="is-model" label={t("appRoute.selectAiModel")}>
        <Menu.RadioGroup value={value}>
          {groups.map(group => <section key={group.provider}>
            <small>{group.label}</small>
            {group.models.map(option => <Choice value={option.id} selected={value === option.id} onChange={onChange} key={option.id}>{option.displayName}</Choice>)}
          </section>)}
        </Menu.RadioGroup>
      </PickerPanel>
    </Menu.Root>
  </div>;
});
