import { Button } from "@centaeris/ui/button";
import { Input } from "@centaeris/ui/input";
import { Textarea } from "@centaeris/ui/textarea";
import { useTranslation } from "../i18n";

export function IssueTemplateFields({ fields, values, onChange, disabled }) {
  const { t } = useTranslation();
  return <div className="issueTemplateFields">{fields.map(field => {
    if (field.type === "items") {
      const rows = values[field.key] || [];
      return <fieldset key={field.key}><legend>{field.label}</legend>{rows.map((row, index) => <div className="issueTemplateItem" key={index}>
        <IssueTemplateFields fields={field.fields} values={row} disabled={disabled} onChange={next => onChange({ ...values, [field.key]: rows.map((old, i) => i === index ? next : old) })} />
        <Button type="button" variant="ghost" disabled={disabled} aria-label={t("issueTemplate.removeItem", { number: index + 1 })} onClick={() => onChange({ ...values, [field.key]: rows.filter((_, i) => i !== index) })}>{t("issues.remove")}</Button>
      </div>)}<Button type="button" variant="ghost" disabled={disabled || rows.length >= 100} onClick={() => onChange({ ...values, [field.key]: [...rows, {}] })}>{t("issueTemplate.addItem")}</Button></fieldset>;
    }
    const props = { required: field.required, disabled, value: values[field.key] || "", onChange: event => onChange({ ...values, [field.key]: event.target.value }) };
    return <label key={field.key}>{field.label}{field.type === "textarea" ? <Textarea {...props} rows={3} maxLength={8192} /> : <Input {...props} type={field.type === "date" ? "date" : field.type === "money" ? "number" : "text"} min={field.minValue ?? undefined} step={field.type === "money" ? "0.01" : undefined} maxLength={512} />}</label>;
  })}</div>;
}

export function IssueTemplateValues({ definition, values }) {
  return <dl className="issueTemplateValues">{definition.fields.map(field => {
    const value = values[field.key];
    if (value === undefined || value === "") return null;
    return <div key={field.key}><dt>{field.label}</dt><dd>{field.type === "items" ? <ol>{value.map((row, index) => <li key={index}><IssueTemplateValues definition={{ fields: field.fields }} values={row} /></li>)}</ol> : value}</dd></div>;
  })}</dl>;
}
