import { MarkdownContent } from "../chat/MarkdownContent";
import { useTranslation } from "../i18n";
import { DeveloperExample, DeveloperStatusExample } from "./DeveloperExample";
import { developerGuides } from "./guides";
import reference from "./oneShotReference.json";

function describeType(schema) {
  if (schema.const !== undefined) return JSON.stringify(schema.const);
  if (schema.enum) return schema.enum.map(value => JSON.stringify(value)).join(" | ");
  if (schema.anyOf) return schema.anyOf.map(describeType).join(" | ");
  if (schema.type === "array") return describeType(schema.items) + "[]";
  return schema.type + (schema.format || schema.contentEncoding ? " (" + (schema.format || schema.contentEncoding) + ")" : "");
}

function FieldTable({ fields }) {
  const { t } = useTranslation();
  return <div className="developerTable"><table>
    <thead><tr><th scope="col">{t("developer.field")}</th><th scope="col">{t("developer.fieldType")}</th>
      <th scope="col">{t("developer.required")}</th><th scope="col">{t("developer.constraints")}</th></tr></thead>
    <tbody>{fields.map(({ name, schema, required }) => {
      const constraints = ["minLength", "maxLength", "maxItems", "minimum", "maximum", "pattern"]
        .filter(key => schema[key] !== undefined).map(key => key + " " + schema[key]);
      return <tr key={name}><th scope="row"><code>{name}</code></th><td><code>{describeType(schema)}</code></td>
        <td>{t(required ? "developer.yes" : "developer.no")}</td>
        <td>{constraints.length ? <code>{constraints.join(" · ")}</code> : "–"}</td></tr>;
    })}</tbody>
  </table></div>;
}

function SchemaFields({ name }) {
  const schema = reference.schemas[name];
  return <FieldTable fields={Object.entries(schema.properties).map(([name, property]) => ({
    name, schema: property, required: schema.required?.includes(name),
  }))} />;
}

function ReferenceTables() {
  const { t } = useTranslation();
  const jsonResponses = new Set();
  for (const operation of reference.operations) {
    for (const response of operation.responses) if (response.schema) jsonResponses.add(response.schema);
  }

  return <section className="developerReference" aria-labelledby="developer-reference-heading">
    <h2 id="developer-reference-heading">{t("developer.httpEndpoints")}</h2>
    {reference.operations.map(operation => <section key={operation.id} className="developerReferenceOperation">
      <h3>{t("developer.operation." + operation.id)}</h3>
      <p><code className="developerMethod">{operation.method}</code>{" "}<code>{operation.path}</code></p>
      {operation.headers?.length ? <><h4>{t("developer.requestHeaders")}</h4><FieldTable fields={operation.headers} /></> : null}
      {operation.requestSchema ? <><h4>{t("developer.requestBody")}</h4><SchemaFields name={operation.requestSchema} /></> : null}
      {operation.emptyBody ? <p>{t("developer.emptyRequestBody")}</p> : null}
      <ul>{operation.responses.map(response => <li key={response.status + ":" + response.contentType}>
        <code>{response.status} {response.contentType}</code>
        {response.schema ? <> · <code>{response.schema}</code></> : null}
        {response.condition ? <> — {t("developer.response." + response.condition)}</> : null}
      </li>)}</ul>
    </section>)}
    <h2>{t("developer.jsonResponses")}</h2>
    {Array.from(jsonResponses, name => <section key={name} className="developerReferenceOperation">
      <h3><code>{name}</code></h3><SchemaFields name={name} />
    </section>)}
    <h2>{t("developer.sseEvents")}</h2>
    {reference.events.map(event => <details key={event.name} className="developerEvent">
      <summary><code>{event.name}</code></summary><SchemaFields name={event.schema} />
    </details>)}
  </section>;
}

export function DeveloperDocumentation({ topic, exampleLanguage, onExampleLanguageChange }) {
  const { t, i18n } = useTranslation();
  const guides = developerGuides[i18n.resolvedLanguage] || developerGuides.en;
  return <article className="developerArticle">
    <header className="settingsPageHeading"><h1>{t("developer.doc." + topic + ".title")}</h1></header>
    <div className="developerProse"><MarkdownContent text={guides[topic]} /></div>
    {topic === "quickstart" ? <>
      <DeveloperExample saveRequest language={exampleLanguage} onLanguageChange={onExampleLanguageChange} />
      <div className="developerProse"><MarkdownContent text={guides.firstResult} /></div>
      <DeveloperStatusExample language={exampleLanguage} />
      <div className="developerProse"><MarkdownContent text={guides.recovery} /></div>
    </> : null}
    {topic === "events" ? <ReferenceTables /> : null}
  </article>;
}
