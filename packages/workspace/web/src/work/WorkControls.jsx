import { Menu } from "@centaeris/ui/primitives/menu";
import { Check, CheckCircle2, ChevronDown, Circle, CircleDashed, Clock3, XCircle } from "lucide-react";

export function StatusIcon({ status }) {
  const Icon = status === "已完成" ? CheckCircle2 : status === "已取消" ? XCircle : status === "进行中" ? Clock3 : status === "待开始" ? CircleDashed : Circle;
  return <Icon className={`workStatusIcon ${status === "已完成" ? "isDone" : status === "进行中" ? "isStarted" : ""}`} aria-label={status} />;
}

export function ChoiceMenu({ label, value, options, onChange, icon, className = "" }) {
  const selected = options.find(option => option.value === value);
  return <Menu.Root modal={false}>
    <Menu.Trigger className={`workChoice ${className}`} aria-label={`${label}：${selected?.label || value}`}>
      {selected?.icon || icon}<span>{selected?.label || value}</span><ChevronDown className="workChoiceChevron" aria-hidden="true" />
    </Menu.Trigger>
    <Menu.Portal><Menu.Positioner className="workMenuPositioner" align="start" sideOffset={5}>
      <Menu.Popup className="workMenu" aria-label={label}>
        <div className="workMenuHeading">{label}</div>
        <Menu.RadioGroup value={value}>
          {options.map(option => <Menu.RadioItem key={option.value} value={option.value} className="workMenuItem" nativeButton closeOnClick render={<button type="button" />} onClick={() => onChange(option.value)}>
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
