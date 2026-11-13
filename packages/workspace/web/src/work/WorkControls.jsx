import { useTranslation } from "../i18n";
import { Menu } from "@centaeris/ui/primitives/menu";
import { Check, CheckCircle2, ChevronDown, Circle, CircleDashed, Clock3, XCircle } from "lucide-react";

export function StatusIcon({ status }) {
  const { t } = useTranslation();
  const Icon = status === "done" ? CheckCircle2 : status === "cancelled" ? XCircle : status === "in_progress" ? Clock3 : status === "todo" ? CircleDashed : Circle;
  return <Icon className={`workStatusIcon ${status === "done" ? "isDone" : status === "in_progress" ? "isStarted" : ""}`} aria-label={t(`work.status.${status}`)} />;
}

export function ChoiceMenu({ label, value, options, onChange, icon, className = "", disabled = false }) {
  const { t } = useTranslation();
  const selected = options.find(option => option.value === value);
  return <Menu.Root modal={false}>
    <Menu.Trigger className={`workChoice ${className}`} disabled={disabled} aria-label={t("work.propertyLabel", { label, value: selected?.label || value })}>
      {selected?.icon || icon}<span>{selected?.label || value}</span><ChevronDown className="workChoiceChevron" aria-hidden="true" />
    </Menu.Trigger>
    <Menu.Portal><Menu.Positioner className="workMenuPositioner" align="start" sideOffset={5}>
      <Menu.Popup className="workMenu" aria-label={label}>
        <div className="workMenuHeading">{label}</div>
        <Menu.RadioGroup value={value}>
          {options.map(option => <Menu.RadioItem key={option.value} value={option.value} disabled={disabled} className="workMenuItem" nativeButton closeOnClick render={<button type="button" />} onClick={() => onChange(option.value)}>
            {option.icon}<span>{option.label}</span>{option.value === value ? <Check aria-hidden="true" /> : null}
          </Menu.RadioItem>)}
        </Menu.RadioGroup>
      </Menu.Popup>
    </Menu.Positioner></Menu.Portal>
  </Menu.Root>;
}

export function Property({ label, ...props }) {
  return <div className="workProperty"><span>{label}</span><ChoiceMenu label={label} {...props} /></div>;
}
