import { useState } from "react";
import { apiUrl } from "../api";
import { useTranslation } from "../i18n";
import reference from "./oneShotReference.json";

export const runPath = reference.operations.find(operation => operation.id === "createRun").path;

export function CopyButton({ value, label }) {
  const { t } = useTranslation();
  const [status, setStatus] = useState("");

  async function copy() {
    try {
      await navigator.clipboard.writeText(value);
      setStatus("developer.copied");
    } catch {
      setStatus("developer.copyFailed");
    }
  }

  return <span className="developerCopy">
    <button type="button" className="secondary" onClick={() => void copy()}>{label}</button>
    <span role="status">{status ? t(status) : ""}</span>
  </span>;
}

export function DeveloperExample() {
  const { t } = useTranslation();
  const endpoint = apiUrl(runPath);
  const quotedEndpoint = "'" + endpoint.replaceAll("'", "'\"'\"'") + "'";
  const command = [
    "operation_id=$(python3 -c 'import time, uuid; print(f\"{int(time.time())}.{uuid.uuid4()}\")')",
    'printf "%s\\n" "$operation_id" >&2',
    "curl --no-buffer --silent --show-error --fail-with-body \\",
    '  --header "Authorization: Bearer $CENTAERIS_RUN_KEY" \\',
    '  --header "Idempotency-Key: $operation_id" \\',
    '  --header "Content-Type: application/json" \\',
    '  --data \'{"prompt":"Reply with one sentence describing Centaeris."}\' \\',
    '  ' + quotedEndpoint,
  ].join("\n");

  return <section className="developerSection" aria-labelledby="developer-example-heading">
    <div className="developerSectionHead">
      <div><h2 id="developer-example-heading">{t("developer.minimumRequest")}</h2>
        <p>{t("developer.exampleHint")}</p></div>
      <CopyButton value={command} label={t("developer.copyExample")} />
    </div>
    <pre><code>{command}</code></pre>
  </section>;
}
