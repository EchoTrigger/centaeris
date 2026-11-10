import assert from "node:assert/strict";
import test from "node:test";
import { officeFileType } from "../../src/chat/officeFormats.mjs";
import { loadOfficePdf } from "../../src/chat/loadOfficePdf.mjs";

test("native Office preview recognizes only the supported original formats", () => {
  for (const extension of ["docx", "xlsx", "pptx"]) {
    assert.equal(officeFileType(`报告.${extension.toUpperCase()}`), extension);
  }
  for (const name of ["report.pdf", "report.docx.exe", "notes.md", "legacy.doc", "report", ""]) {
    assert.equal(officeFileType(name), null);
  }
});

test("preview polls 202, keeps credentials and returns only PDF bytes", async (context) => {
  let calls = 0;
  context.mock.method(globalThis, "fetch", async (_url, options) => {
    assert.equal(options.credentials, "include");
    assert.equal(options.cache, "no-store");
    return ++calls === 1 ? new Response("waiting", { status: 202 })
      : new Response("%PDF-test", { headers: { "Content-Type": "application/pdf" } });
  });
  assert.equal(new TextDecoder().decode(await loadOfficePdf("/preview", { pollMs: 1 })), "%PDF-test");
  assert.equal(calls, 2);
});

test("preview stops on timeout and on navigation cancellation", async (context) => {
  const fetch = context.mock.method(globalThis, "fetch", async () => new Response("waiting", { status: 202 }));
  await assert.rejects(loadOfficePdf("/preview", { timeoutMs: 10, pollMs: 1000 }), { name: "TimeoutError" });
  const controller = new AbortController();
  const pending = loadOfficePdf("/preview", { signal: controller.signal, pollMs: 1000 });
  controller.abort();
  await assert.rejects(pending, { name: "AbortError" });
  assert.equal(fetch.mock.callCount(), 2);
});

test("preview rejects failed responses and HTML login pages without polling", async (context) => {
  for (const response of [new Response("failure", { status: 503 }), new Response("login", { headers: { "Content-Type": "text/html" } })]) {
    const fetch = context.mock.method(globalThis, "fetch", async () => response);
    await assert.rejects(loadOfficePdf("/preview"), /office_preview_unavailable/);
    assert.equal(fetch.mock.callCount(), 1);
    fetch.mock.restore();
  }
});
