export async function loadSpreadsheetPreview(src, { signal, timeoutMs = 120_000, pollMs = 1000 } = {}) {
  const controller = new AbortController();
  const cancel = () => controller.abort(signal.reason);
  if (signal?.aborted) cancel();
  else signal?.addEventListener("abort", cancel, { once: true });
  const timeout = setTimeout(() => controller.abort(new DOMException("Preview timed out", "TimeoutError")), timeoutMs);
  try {
    while (true) {
      const response = await fetch(src, { credentials: "include", cache: "no-store", signal: controller.signal });
      if (response.status === 202) {
        await response.body?.cancel();
        await new Promise((resolve, reject) => {
          const abort = () => { clearTimeout(timer); reject(controller.signal.reason); };
          const timer = setTimeout(() => { controller.signal.removeEventListener("abort", abort); resolve(); }, pollMs);
          if (controller.signal.aborted) abort();
          else controller.signal.addEventListener("abort", abort, { once: true });
        });
        continue;
      }
      if (!response.ok || response.headers.get("Content-Type")?.split(";", 1)[0] !== "application/json") {
        await response.body?.cancel();
        throw new Error("spreadsheet_preview_unavailable");
      }
      const workbook = await response.json();
      if (workbook?.schema !== "knowledge.workbook_preview.v1" || !Array.isArray(workbook.styles) || !Array.isArray(workbook.sheets)) {
        throw new Error("spreadsheet_preview_invalid");
      }
      return workbook;
    }
  } finally {
    clearTimeout(timeout);
    signal?.removeEventListener("abort", cancel);
  }
}
