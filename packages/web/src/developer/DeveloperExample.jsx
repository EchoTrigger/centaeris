import { useState } from "react";
import { apiUrl } from "../api";
import { useTranslation } from "../i18n";
import reference from "./oneShotReference.json";

export const runPath = reference.operations.find(operation => operation.id === "createRun").path;

function shellQuote(value) {
  return "'" + value.replaceAll("'", "'\"'\"'") + "'";
}

const credentialFingerprint = "python3 -c 'import hashlib, os; print(hashlib.sha256(os.environ[\"CENTAERIS_RUN_KEY\"].encode()).hexdigest())'";

function minimalRequestExample(endpoint) {
  return [
    'export CENTAERIS_RUN_KEY="your-api-key"',
    "operation_id=$(python3 -c 'import time, uuid; print(f\"{int(time.time())}.{uuid.uuid4()}\")')",
    "",
    "curl -N --fail-with-body " + shellQuote(endpoint) + " \\",
    '  -H "Authorization: Bearer $CENTAERIS_RUN_KEY" \\',
    '  -H "Idempotency-Key: $operation_id" \\',
    '  -H "Content-Type: application/json" \\',
    "  -d '{\"prompt\":\"Reply with one sentence describing Centaeris.\"}'",
  ].join("\n");
}

export function firstCallExample(endpoint) {
  return [
    "(",
    "  set -euo pipefail",
    "  umask 077",
    '  : "${CENTAERIS_RUN_KEY:?Set CENTAERIS_RUN_KEY to your API credential secret}"',
    "  operation_dir=./centaeris-first-call",
    '  mkdir "$operation_dir"',
    "  python3 -c 'import time, uuid; print(f\"{int(time.time())}.{uuid.uuid4()}\")' > \"$operation_dir/operation-id\"",
    '  printf \'%s\\n\' \'{"prompt":"Reply with one sentence describing Centaeris."}\' > "$operation_dir/request.json"',
    "  printf '%s\\n' " + shellQuote(endpoint) + ' > "$operation_dir/endpoint"',
    "  " + credentialFingerprint + ' > "$operation_dir/credential-fingerprint"',
    '  operation_id=$(cat "$operation_dir/operation-id")',
    '  printf "operationId=%s; saved request: %s\\n" "$operation_id" "$operation_dir" >&2',
    "  curl --disable --no-buffer --silent --show-error --fail-with-body \\",
    "    --connect-timeout 15 --max-time 700 \\",
    '    --header "Authorization: Bearer $CENTAERIS_RUN_KEY" \\',
    '    --header "Idempotency-Key: $operation_id" \\',
    '    --header "Content-Type: application/json" \\',
    '    --header "Accept: text/event-stream, application/json" \\',
    '    --dump-header "$operation_dir/headers.txt" \\',
    '    --data-binary "@$operation_dir/request.json" \\',
    '    "$(cat "$operation_dir/endpoint")" | tee "$operation_dir/response.txt"',
    ")",
  ].join("\n");
}

export function statusExample() {
  return [
    "(",
    "  set -eu",
    '  : "${CENTAERIS_RUN_KEY:?Use the same API credential as the original request}"',
    "  operation_dir=./centaeris-first-call",
    '  operation_id=$(cat "$operation_dir/operation-id")',
    '  endpoint=$(cat "$operation_dir/endpoint")',
    "  credential_fingerprint=$(" + credentialFingerprint + ")",
    '  if [ "$credential_fingerprint" != "$(cat "$operation_dir/credential-fingerprint")" ]; then',
    '    printf "%s\\n" "Use the original credential; a replacement key cannot read this operation." >&2',
    "    exit 1",
    "  fi",
    "  curl --disable --silent --show-error --fail-with-body \\",
    "    --connect-timeout 15 --max-time 30 \\",
    '    --header "Authorization: Bearer $CENTAERIS_RUN_KEY" \\',
    '    --header "Accept: application/json" \\',
    '    "$endpoint/operations/$operation_id"',
    ")",
  ].join("\n");
}

export function CopyButton({ value, label, accessibleLabel }) {
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
    <button type="button" className="secondary" aria-label={accessibleLabel} onClick={() => void copy()}>{label}</button>
    <span role="status">{status ? t(status) : ""}</span>
  </span>;
}

export function DeveloperExample({ saveRequest = false }) {
  const { t } = useTranslation();
  const endpoint = apiUrl(runPath);
  const command = saveRequest ? firstCallExample(endpoint) : minimalRequestExample(endpoint);

  return <section className="settingsPanel developerSection" aria-labelledby="developer-example-heading">
    <div className="developerSectionHead">
      <div><h2 id="developer-example-heading">{t(saveRequest ? "developer.savedRequest" : "developer.minimumRequest")}</h2>
        <p>{t(saveRequest ? "developer.savedExampleHint" : "developer.exampleHint")}</p></div>
      <CopyButton value={command} label={t("developer.copyExample")} />
    </div>
    <pre><code>{command}</code></pre>
  </section>;
}

export function DeveloperStatusExample() {
  const { t } = useTranslation();
  const command = statusExample();
  return <section className="settingsPanel developerSection" aria-labelledby="developer-status-heading">
    <div className="developerSectionHead">
      <div><h2 id="developer-status-heading">{t("developer.checkOriginalOperation")}</h2>
        <p>{t("developer.statusExampleHint")}</p></div>
      <CopyButton value={command} label={t("developer.copyStatusExample")} />
    </div>
    <pre><code>{command}</code></pre>
  </section>;
}
