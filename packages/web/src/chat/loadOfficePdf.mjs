// The endpoint may return 202 while the immutable PDF is being generated.
// Keep polling in the application so failures and slow jobs have a visible end.
export async function loadOfficePdf(src, { signal, timeoutMs = 120_000, pollMs = 1000 } = {}) {
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
      if (!response.ok || response.headers.get("Content-Type")?.split(";", 1)[0] !== "application/pdf") {
        await response.body?.cancel();
        throw Object.assign(new Error("office_preview_unavailable"), { status: response.status });
      }
      return new Uint8Array(await response.arrayBuffer());
    }
  } finally {
    clearTimeout(timeout);
    signal?.removeEventListener("abort", cancel);
  }
}
