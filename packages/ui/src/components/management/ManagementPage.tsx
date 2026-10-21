import { type ReactNode, useLayoutEffect, useRef } from "react";
import { ArrowLeft, CircleAlert, LoaderCircle, Search } from "lucide-react";
import { Button } from "../ui/button";
import { Input } from "../ui/input";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "../ui/select";
import { Switch } from "../ui/switch";
import "../../styles/desktop-management.css";

export function ManagementHeader({ title, description, actions, focusOnNavigation = false }: { title: string; description?: ReactNode; actions?: ReactNode; focusOnNavigation?: boolean }) {
  const heading = useRef<HTMLHeadingElement>(null);
  // Only focus internal navigation; mounting a resource tab must preserve its tab-list focus.
  useLayoutEffect(() => { if (focusOnNavigation && title.trim()) heading.current?.focus(); }, [title, focusOnNavigation]);
  return <header className="dmHeader"><div><h1 ref={heading} tabIndex={-1}>{title}</h1>{description ? <p>{description}</p> : null}</div><div className="dmActions">{actions}</div></header>;
}
export function ManagementSearch({ value, onChange, label }: { value: string; onChange(value: string): void; label: string }) {
  return <div className="dmSearch"><Search size={16} aria-hidden="true" /><Input aria-label={label} placeholder={label} value={value} onChange={event => onChange(event.target.value)} /></div>;
}
export function ManagementFilter({ label, value, onChange, options }: { label: string; value: string; onChange(value: string): void; options: readonly (readonly [string, string])[] }) {
  return <Select value={value} onValueChange={next => { if (next !== null) onChange(next); }}>
    <SelectTrigger aria-label={label}><SelectValue>{options.find(([key]) => key === value)?.[1]}</SelectValue></SelectTrigger>
    <SelectContent align="start" alignItemWithTrigger={false}>{options.map(([key, text]) => <SelectItem key={key} value={key}>{text}</SelectItem>)}</SelectContent>
  </Select>;
}
export function ManagementBack({ children, onClick, disabled }: { children: ReactNode; onClick(): void; disabled?: boolean }) {
  return <Button variant="ghost" size="sm" className="dmBack" onClick={onClick} disabled={disabled}><ArrowLeft aria-hidden="true" />{children}</Button>;
}
export function ManagementError({ children, onRetry, retryLabel = "Retry" }: { children: ReactNode; onRetry?: () => void; retryLabel?: string }) {
  return <div className="dmError" role="alert"><CircleAlert size={16} aria-hidden="true" /><div>{children}</div>{onRetry ? <Button variant="outline" size="sm" onClick={onRetry}>{retryLabel}</Button> : null}</div>;
}
export function ManagementLoading({ children }: { children: ReactNode }) {
  return <p className="dmLoading" role="status"><LoaderCircle size={16} className="dmSpinner" aria-hidden="true" />{children}</p>;
}
export function ManagementEmpty({ title, description, action }: { title: string; description: string; action?: ReactNode }) {
  return <div className="dmEmpty"><h2>{title}</h2><p>{description}</p>{action}</div>;
}
export function ManagementStatus({ children, tone = "neutral" }: { children: ReactNode; tone?: "neutral" | "success" | "warning" | "error" }) {
  return <span className="dmStatus" data-tone={tone}>{children}</span>;
}
export function ManagementToggle({ checked, disabled, label, onChange }: { checked: boolean; disabled?: boolean; label: string; onChange(): void }) {
  return <label className="dmToggle"><span>{checked ? "Enabled" : "Disabled"}</span><Switch checked={checked} disabled={disabled} aria-label={label} onCheckedChange={onChange} /></label>;
}
