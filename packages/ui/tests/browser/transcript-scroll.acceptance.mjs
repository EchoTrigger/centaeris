import test from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, realpath, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, dirname, basename } from "node:path";
import { fileURLToPath } from "node:url";
import { createServer } from "vite";
import { runBrowser, waitFor } from "../../../web/tests/browser/browserHarness.mjs";

// Real wheel events, ResizeObserver and TanStack measurements are required here:
// a mocked virtualizer cannot expose overlapping prepend/measurement corrections.
test("deferred transcript content preserves manual reading and older-page anchors", async () => {
  const profile = await mkdtemp(join(tmpdir(), "centaeris-transcript-scroll-"));
  const server = await createServer({
    root: fileURLToPath(new URL("../..", import.meta.url)),
    logLevel: "error",
    server: { port: 0, host: "127.0.0.1", hmr: false },
  });
  try {
    await server.listen();
    await runBrowser(
      server.resolvedUrls.local[0] + "tests/browser/transcript-scroll.html",
      { width: 1200, height: 800 },
      profile,
      async ({ send, evaluate, frame }) => {
        await waitFor(
          () =>
            evaluate(
              'Boolean(window.scrollFixture && document.querySelector("[data-history-row]"))',
            ),
          "transcript mount",
        );
        await evaluate("document.fonts.ready");
        const settle = async () => {
          await frame();
          await waitFor(
            () => evaluate("window.scrollFixture.geometry().pendingReads === 0"),
            "referenced text reads",
          );
          await frame();
        };
        await settle();
        const initial = await evaluate("window.scrollFixture.geometry()");
        assert.ok(
          Math.abs(initial.height - initial.viewport - initial.top) < 3,
          "cold entry settles at latest",
        );
        const wheel = async (deltaY) => {
          await send("Input.dispatchMouseEvent", {
            type: "mouseWheel",
            x: 600,
            y: 300,
            deltaX: 0,
            deltaY,
          });
          await frame();
        };
        const anchor = () =>
          evaluate(() => {
            const root = document.querySelector(".messages-container");
            const rect = root.getBoundingClientRect();
            const paragraph = [...root.querySelectorAll("p")].find(
              (node) =>
                node.textContent.startsWith("Paragraph ") &&
                node.getBoundingClientRect().bottom > rect.top + 80 &&
                node.getBoundingClientRect().top < rect.bottom,
            );
            return paragraph
              ? { text: paragraph.textContent, top: paragraph.getBoundingClientRect().top }
              : null;
          });
        const keepsAnchor = async (before, label) => {
          assert.ok(before, label + " has readable text");
          const after = await evaluate(
            '(() => { const node = [...document.querySelectorAll("p")].find(node => node.textContent === ' +
              JSON.stringify(before.text) +
              "); return node ? node.getBoundingClientRect().top : null; })()",
          );
          assert.notEqual(after, null, label + " retains the visible paragraph");
          assert.ok(Math.abs(after - before.top) < 3, label + " preserves the paragraph position");
        };
        await wheel(-450);
        await settle();
        assert.equal(
          (await evaluate("window.scrollFixture.geometry()")).following,
          false,
          "wheel takes control",
        );
        let before = await anchor();
        await settle();
        await keepsAnchor(before, "detached hydration");
        for (const direction of [-1, 1])
          for (let n = 0; n < 8; n++) {
            await wheel(direction * 480);
            await settle();
          }
        before = await anchor();
        await settle();
        await keepsAnchor(before, "returning to cached virtual rows");
        await evaluate(
          'window.scrollFixture.follow.pauseFollowing(); document.querySelector(".messages-container").scrollTop = 0',
        );
        await settle();
        await wheel(-100);
        await wheel(350);
        await settle();
        before = await anchor();
        await evaluate("window.scrollFixture.releaseOlder()");
        await settle();
        await keepsAnchor(before, "older page arriving after manual movement");
        await evaluate("window.scrollFixture.follow.handleJumpToLatest()");
        await settle();
        const final = await evaluate("window.scrollFixture.geometry()");
        assert.equal(final.following, true, "Jump to latest resumes following");
        assert.ok(
          Math.abs(final.height - final.viewport - final.top) < 3,
          "return settles at latest",
        );
        const retainedHeight = await evaluate(
          "document.querySelector('[data-history-row=\"a-59\"]').getBoundingClientRect().height",
        );
        await evaluate(
          'window.scrollFixture.follow.pauseFollowing(); document.querySelector(".messages-container").scrollTop = 0',
        );
        await settle();
        assert.equal(
          await evaluate("Boolean(document.querySelector('[data-history-row=\"a-59\"]'))"),
          false,
          "target row was virtualized away",
        );
        await evaluate(
          "window.scrollFixture.evictAndHoldReads(); window.scrollFixture.follow.handleJumpToLatest()",
        );
        await waitFor(
          () => evaluate("Boolean(document.querySelector('[data-history-row=\"a-59\"]'))"),
          "evicted row remount",
        );
        const waitingHeight = await evaluate(
          "document.querySelector('[data-history-row=\"a-59\"]').getBoundingClientRect().height",
        );
        assert.ok(
          Math.abs(waitingHeight - retainedHeight) < 3,
          "a cache miss retains the known row height until text arrives",
        );
        await evaluate("window.scrollFixture.resumeReads()");
        await settle();
      },
    );
  } finally {
    await server.close();
    const resolved = await realpath(profile);
    assert.equal(dirname(resolved).toLowerCase(), (await realpath(tmpdir())).toLowerCase());
    assert.ok(basename(resolved).startsWith("centaeris-transcript-scroll-"));
    await rm(resolved, { recursive: true, force: true, maxRetries: 5, retryDelay: 100 });
  }
});

test("continuous wheel through mixed-length turn boundaries does not expose loading rows", async () => {
  const profile = await mkdtemp(join(tmpdir(), "centaeris-transcript-boundary-"));
  const server = await createServer({
    root: fileURLToPath(new URL("../..", import.meta.url)),
    logLevel: "error",
    server: { port: 0, host: "127.0.0.1", hmr: false },
  });
  try {
    await server.listen();
    await runBrowser(
      server.resolvedUrls.local[0] + "tests/browser/transcript-scroll.html?mixed",
      { width: 1200, height: 800 },
      profile,
      async ({ send, evaluate, frame }) => {
        await waitFor(() => evaluate("Boolean(window.scrollFixture)"), "mixed transcript mount");
        await evaluate("document.fonts.ready");
        await waitFor(
          () => evaluate("window.scrollFixture.geometry().pendingReads === 0"),
          "initial referenced text",
        );
        await frame();
        const frames = [];
        for (const direction of [-1, 1])
          for (let n = 0; n < 24; n++) {
            await send("Input.dispatchMouseEvent", {
              type: "mouseWheel",
              x: 600,
              y: 300,
              deltaX: 0,
              deltaY: direction * 180,
            });
            await frame();
            frames.push(
              await evaluate(() => {
                const viewport = document
                  .querySelector(".messages-container")
                  .getBoundingClientRect();
                const visibleLoads = [
                  ...document.querySelectorAll('[data-history-row] [aria-busy="true"]'),
                ]
                  .map((node) => ({
                    top: node.getBoundingClientRect().top,
                    bottom: node.getBoundingClientRect().bottom,
                  }))
                  .filter(
                    (rect) => rect.top < viewport.bottom - 40 && rect.bottom > viewport.top + 40,
                  );
                return { ...window.scrollFixture.geometry(), visibleLoads };
              }),
            );
          }
        const unfinished = frames.filter((frame) => frame.visibleLoads.length > 0);
        assert.equal(
          unfinished.length,
          0,
          "offscreen text must be ready before crossing into view: " +
            JSON.stringify(unfinished.slice(0, 4)),
        );
        await waitFor(
          () => evaluate("window.scrollFixture.geometry().pendingReads === 0"),
          "boundary text reads",
        );
        await frame();
        await evaluate(() => {
          const viewport = document.querySelector(".messages-container");
          const boundary = [...viewport.querySelectorAll('[data-message-role="user"]')].at(-2);
          window.boundaryUser = boundary;
          viewport.scrollTop +=
            boundary.getBoundingClientRect().top -
            viewport.getBoundingClientRect().top -
            viewport.clientHeight / 2;
        });
        await frame();
        const samples = [];
        for (let n = 0; n < 16; n++) {
          await send("Input.dispatchMouseEvent", {
            type: "mouseWheel",
            x: 600,
            y: 300,
            deltaX: 0,
            deltaY: n % 2 ? 18 : -18,
          });
          await frame();
          samples.push(
            await evaluate(() => {
              const original = window.boundaryUser;
              const rect = original.getBoundingClientRect();
              return {
                retained: original.isConnected,
                top: rect.top,
                opacity: getComputedStyle(original).opacity,
              };
            }),
          );
        }
        assert.ok(
          samples.every((sample) => sample.retained && sample.opacity === "1"),
          "crossing a turn boundary retains the painted user message",
        );
        const tops = samples.map((sample) => sample.top);
        assert.ok(
          Math.max(...tops) - Math.min(...tops) < 22,
          "small reverse wheel gestures cannot cause a boundary height jump",
        );
      },
    );
  } finally {
    await server.close();
    const resolved = await realpath(profile);
    assert.equal(dirname(resolved).toLowerCase(), (await realpath(tmpdir())).toLowerCase());
    assert.ok(basename(resolved).startsWith("centaeris-transcript-boundary-"));
    await rm(resolved, { recursive: true, force: true, maxRetries: 5, retryDelay: 100 });
  }
});

for (const short of [false, true])
  test(
    "structured transcript prepends preserve the content being read" +
      (short ? " with a short tail" : " across five pages"),
    async () => {
      const profile = await mkdtemp(join(tmpdir(), "centaeris-structured-scroll-"));
      const server = await createServer({
        root: fileURLToPath(new URL("../..", import.meta.url)),
        logLevel: "error",
        server: { port: 0, host: "127.0.0.1", hmr: false },
      });
      try {
        await server.listen();
        await runBrowser(
          server.resolvedUrls.local[0] +
            "tests/browser/transcript-structure.html" +
            (short ? "?short" : ""),
          { width: 1200, height: 850 },
          profile,
          async ({ evaluate, frame }) => {
            await waitFor(
              () =>
                evaluate(
                  "Boolean(window.structureFixture && document.querySelector('[data-history-row]'))",
                ),
              "structured transcript mount",
            );
            await evaluate("document.fonts.ready");
            for (let n = 0; n < 8; n++) await frame();
            assert.equal(await evaluate("window.structureFixture.count"), 302);
            for (let page = 1; page < 5; page++) {
              await evaluate(() => {
                window.structureFixture.follow.pauseFollowing();
                document.querySelector(".messages-container").scrollTop = 0;
              });
              for (let n = 0; n < 4; n++) await frame();
              const before = await evaluate(() => {
                const viewport = document
                  .querySelector(".messages-container")
                  .getBoundingClientRect();
                const node = [
                  ...document.querySelectorAll(
                    ".agent-operation-summary,.agentProcessSectionHeading,.agentAssistantAnswer p,.user-message p",
                  ),
                ].find(
                  (node) =>
                    node.textContent.trim() &&
                    node.getBoundingClientRect().top >= viewport.top &&
                    node.getBoundingClientRect().top < viewport.bottom,
                );
                window.readingNode = node;
                return node
                  ? {
                      text: node.textContent,
                      id: node.closest("[data-transcript-anchor]")?.dataset.transcriptAnchor,
                      anchorTop: node.closest("[data-transcript-anchor]")?.getBoundingClientRect()
                        .top,
                      top: node.getBoundingClientRect().top,
                    }
                  : null;
              });
              assert.ok(before, "page " + page + " has readable content");
              await evaluate(
                "(()=>{window.prependFrames=[];const id=" +
                  JSON.stringify(before.id) +
                  ';let count=0;function sample(){const node=[...document.querySelectorAll("[data-transcript-anchor]")].find(node=>node.dataset.transcriptAnchor===id);window.prependFrames.push(node?node.getBoundingClientRect().top:null);if(++count<16)requestAnimationFrame(sample);}requestAnimationFrame(sample);window.structureFixture.loadOlder();})()',
              );
              for (let n = 0; n < 10; n++) await frame();
              const after = await evaluate(
                '(()=>{const node=[...document.querySelectorAll(".agent-operation-summary,.agentProcessSectionHeading,.agentAssistantAnswer p,.user-message p")].find(node=>node.textContent===' +
                  JSON.stringify(before.text) +
                  '&&node.closest("[data-transcript-anchor]")?.dataset.transcriptAnchor===' +
                  JSON.stringify(before.id) +
                  ");return node?{top:node.getBoundingClientRect().top}:null;})()",
              );
              assert.ok(after, "page " + page + " retains readable content");
              assert.ok(
                Math.abs(after.top - before.top) < 3,
                "page " +
                  page +
                  " preserves text position: " +
                  JSON.stringify({ before: before.top, after: after.top }),
              );
              const painted = await evaluate("window.prependFrames");
              assert.ok(
                painted.every((top) => top !== null && Math.abs(top - before.anchorTop) < 3),
                "page " +
                  page +
                  " keeps the content in place throughout prepend: " +
                  JSON.stringify(painted),
              );
            }
            assert.equal(
              await evaluate("document.querySelectorAll('.runStatusSpinner').length"),
              0,
              "history does not start Thinking again",
            );
          },
        );
      } finally {
        await server.close();
        const resolved = await realpath(profile);
        assert.equal(dirname(resolved).toLowerCase(), (await realpath(tmpdir())).toLowerCase());
        assert.ok(basename(resolved).startsWith("centaeris-structured-scroll-"));
        await rm(resolved, { recursive: true, force: true, maxRetries: 5, retryDelay: 100 });
      }
    },
  );

for (const [mode, width, height] of [
  ["paint", 940, 620],
  ["paint", 1200, 760],
  ["reading", 940, 620],
  ["reading", 1200, 760],
  ["earlier", 940, 620],
  ["disclosure", 940, 620],
  ["latest", 940, 620],
])
  test(
    "first older-page visual finishes while wheel reading continues: " + mode + " " + width,
    async () => {
      const jump = mode === "latest";
      const profile = await mkdtemp(join(tmpdir(), "centaeris-visual-scroll-"));
      let releaseVisual;
      const visualReady = new Promise((resolve) => {
        releaseVisual = resolve;
      });
      const server = await createServer({
        root: fileURLToPath(new URL("../..", import.meta.url)),
        logLevel: "error",
        server: { port: 0, host: "127.0.0.1", hmr: false },
        plugins: [
          {
            name: "hold-first-visual",
            configureServer(vite) {
              vite.middlewares.use(async (req, _res, next) => {
                if (req.url.split("?")[0] === "/src/components/chat/DesktopVisualRenderer.tsx")
                  await visualReady;
                next();
              });
            },
          },
        ],
      });
      try {
        await server.listen();
        await runBrowser(
          server.resolvedUrls.local[0] + "tests/browser/transcript-structure.html?delayedVisual",
          { width, height },
          profile,
          async ({ send, evaluate, frame }) => {
            await waitFor(
              () =>
                evaluate(
                  "Boolean(window.structureFixture&&document.querySelector('[data-history-row]'))",
                ),
              "visual history mount",
            );
            await evaluate("document.fonts.ready");
            for (let n = 0; n < 8; n++) await frame();
            if (mode === "paint")
              await evaluate(() => {
                window.overlappingText = [];
                window.textLayoutSamples = 0;
                const sample = () => {
                  window.textLayoutSamples++;
                  const viewport = document
                    .querySelector(".messages-container")
                    .getBoundingClientRect();
                  const text = [];
                  for (const row of document.querySelectorAll("[data-history-row]")) {
                    const walker = document.createTreeWalker(row, NodeFilter.SHOW_TEXT);
                    while (walker.nextNode()) {
                      if (
                        !walker.currentNode.textContent.trim() ||
                        !walker.currentNode.parentElement.checkVisibility({
                          checkOpacity: true,
                          checkVisibilityCSS: true,
                        })
                      )
                        continue;
                      const range = document.createRange();
                      range.selectNodeContents(walker.currentNode);
                      for (const rect of range.getClientRects()) {
                        if (
                          rect.width &&
                          rect.height &&
                          rect.bottom > viewport.top &&
                          rect.top < viewport.bottom
                        )
                          text.push({
                            row: row.dataset.historyRow,
                            top: rect.top,
                            bottom: rect.bottom,
                            left: rect.left,
                            right: rect.right,
                          });
                      }
                    }
                  }
                  for (let i = 0; i < text.length; i++)
                    for (let j = i + 1; j < text.length; j++) {
                      const a = text[i],
                        b = text[j];
                      if (
                        a.row !== b.row &&
                        Math.min(a.bottom, b.bottom) - Math.max(a.top, b.top) > 1 &&
                        Math.min(a.right, b.right) - Math.max(a.left, b.left) > 1 &&
                        window.overlappingText.length < 10
                      )
                        window.overlappingText.push({ a, b });
                    }
                };
                // Sample after the virtualizer's ResizeObserver delivery, when
                // actual text geometry is known and the browser is about to paint.
                const observer = new ResizeObserver(sample);
                const observeRows = () => {
                  for (const row of document.querySelectorAll("[data-history-row]"))
                    observer.observe(row);
                };
                const mutations = new MutationObserver(observeRows);
                mutations.observe(document.querySelector(".messages-container"), {
                  childList: true,
                  subtree: true,
                });
                observeRows();
                window.stopTextObservation = () => {
                  observer.disconnect();
                  mutations.disconnect();
                };
              });
            const wheel = async (deltaY) => {
              await send("Input.dispatchMouseEvent", {
                type: "mouseWheel",
                x: width - 140,
                y: 230,
                deltaX: 0,
                deltaY,
              });
              await frame();
            };
            for (let n = 0; n < 30; n++) {
              await wheel(-300);
              if (
                await evaluate(
                  "window.structureFixture.loading()||Boolean(document.querySelector('.cv-pending'))",
                )
              )
                break;
            }
            await waitFor(
              () => evaluate("Boolean(document.querySelector('.cv-pending'))"),
              "first visual is pending",
            );
            for (let n = 0; n < 4; n++) await frame();
            const markerTop = () =>
              evaluate(
                () =>
                  [...document.querySelectorAll(".user-message p")]
                    .find((node) => node.textContent === "Question 5")
                    ?.getBoundingClientRect().top,
              );
            const start = await markerTop();
            for (let n = 0; n < 6; n++) await wheel(-30);
            for (let n = 0; n < 2; n++) await wheel(30);
            const before = await markerTop();
            assert.ok(
              Math.abs(before - start - 120) < 3,
              "pending visual never cancels or reverses native wheel movement",
            );
            let earlier;
            if (mode === "earlier") {
              for (let n = 0; n < 8; n++) await wheel(-100);
              earlier = await evaluate(() => {
                const viewport = document
                  .querySelector(".messages-container")
                  .getBoundingClientRect();
                const node = [...document.querySelectorAll(".agent-operation-summary")].find(
                  (node) => {
                    const rect = node.getBoundingClientRect();
                    return rect.top >= viewport.top && rect.bottom <= viewport.bottom;
                  },
                );
                return node
                  ? {
                      id: node.closest("[data-transcript-anchor]").dataset.transcriptAnchor,
                      top: node.getBoundingClientRect().top,
                    }
                  : null;
              });
              assert.ok(earlier, "wheel moves into earlier process text before the pending visual");
            }
            if (mode === "disclosure") {
              earlier = await evaluate(() => {
                const question = [...document.querySelectorAll(".user-message p")]
                  .find((node) => node.textContent === "Question 5")
                  .getBoundingClientRect();
                const viewport = document
                  .querySelector(".messages-container")
                  .getBoundingClientRect();
                const button = [...document.querySelectorAll('button[aria-expanded="false"]')].find(
                  (node) => {
                    const rect = node.getBoundingClientRect();
                    return rect.top > question.bottom && rect.bottom < viewport.bottom;
                  },
                );
                if (!button) return null;
                const result = {
                  id: button.closest("[data-transcript-anchor]").dataset.transcriptAnchor,
                  top: button.getBoundingClientRect().top,
                  disclosure: true,
                };
                button.click();
                return result;
              });
              assert.ok(
                earlier,
                "a visible disclosure can be opened while the earlier visual is pending",
              );
              for (let n = 0; n < 4; n++) await frame();
            }
            if (jump) await evaluate("window.structureFixture.follow.handleJumpToLatest()");
            if (!jump)
              await evaluate(
                "(()=>{window.visualFinishFrames=[];const id=" +
                  JSON.stringify(earlier?.id ?? null) +
                  ';let count=0;function sample(){const node=id?[...document.querySelectorAll("[data-transcript-anchor]")].find(node=>node.dataset.transcriptAnchor===id)?.querySelector(".agent-operation-summary,button[aria-expanded]"):[...document.querySelectorAll(".user-message p")].find(node=>node.textContent==="Question 5");window.visualFinishFrames.push(node?.getBoundingClientRect().top??null);if(++count<16)requestAnimationFrame(sample);}requestAnimationFrame(sample);})()',
              );
            releaseVisual();
            await waitFor(
              () =>
                evaluate(
                  "Boolean(document.querySelector('.desktop-visual'))&&!document.querySelector('.cv-pending')",
                ),
              "real visual rendered",
            );
            for (let n = 0; n < 8; n++) await frame();
            if (!jump) {
              const frames = await evaluate("window.visualFinishFrames");
              const expected = earlier?.top ?? before;
              assert.ok(
                frames.every((top) => top !== null && Math.abs(top - expected) < 3),
                "lazy visual commits without a transient reading-position jump: " +
                  JSON.stringify(frames),
              );
            }
            if (jump) {
              const end = await evaluate(() => {
                const viewport = document.querySelector(".messages-container");
                return {
                  following: window.structureFixture.follow.isFollowingLatest,
                  gap: viewport.scrollHeight - viewport.clientHeight - viewport.scrollTop,
                };
              });
              assert.equal(end.following, true);
              assert.ok(
                Math.abs(end.gap) < 3,
                "jump to latest releases the pending history anchor",
              );
            } else if (earlier) {
              const after = await evaluate(
                '(()=>{const node=[...document.querySelectorAll("[data-transcript-anchor]")].find(node=>node.dataset.transcriptAnchor===' +
                  JSON.stringify(earlier.id) +
                  ');return node?.querySelector(".agent-operation-summary,button[aria-expanded]")?.getBoundingClientRect().top;})()',
              );
              assert.ok(
                Math.abs(after - earlier.top) < 3,
                "the selected reading point stays in place while the visual finishes: " +
                  JSON.stringify({ before: earlier.top, after }),
              );
            } else {
              const after = await markerTop();
              assert.ok(
                Math.abs(after - before) < 3,
                "first visual preserves the paragraph position after wheel input: " +
                  JSON.stringify({ before, after }),
              );
            }
            if (mode === "paint") {
              const overlaps = await evaluate(
                "window.stopTextObservation();window.overlappingText",
              );
              assert.ok(
                await evaluate("window.textLayoutSamples > 2"),
                "observer samples insertion and deferred size changes",
              );
              assert.deepEqual(
                overlaps,
                [],
                "new history text never paints over another row while its height settles",
              );
            }
          },
        );
      } finally {
        releaseVisual();
        await server.close();
        const resolved = await realpath(profile);
        assert.equal(dirname(resolved).toLowerCase(), (await realpath(tmpdir())).toLowerCase());
        assert.ok(basename(resolved).startsWith("centaeris-visual-scroll-"));
        await rm(resolved, { recursive: true, force: true, maxRetries: 5, retryDelay: 100 });
      }
    },
  );

for (const reading of [false, true])
  test(
    "stage and final presentation retain their readers while " +
      (reading ? "manually reading" : "following latest"),
    async () => {
      const profile = await mkdtemp(join(tmpdir(), "centaeris-stage-stream-"));
      const server = await createServer({
        root: fileURLToPath(new URL("../..", import.meta.url)),
        logLevel: "error",
        server: { port: 0, host: "127.0.0.1", hmr: false },
      });
      try {
        await server.listen();
        await runBrowser(
          server.resolvedUrls.local[0] + "tests/browser/streaming-transcript.html",
          { width: 940, height: 620 },
          profile,
          async ({ send, evaluate, frame }) => {
            await waitFor(
              () =>
                evaluate(
                  "Boolean(window.stageFixture && document.querySelector('[data-live-tail]'))",
                ),
              "streaming transcript mount",
            );
            await evaluate("document.fonts.ready");
            for (let n = 0; n < 4; n++) await frame();
            const target = "Checking the implementation carefully. ".repeat(40).trim();
            const push = (type, payload, extra = {}) =>
              evaluate(
                "window.stageFixture.push(" +
                  JSON.stringify(type) +
                  "," +
                  JSON.stringify(payload) +
                  "," +
                  JSON.stringify(extra) +
                  ")",
              );
            await push("ModelSnapshot", { revision: 1, text: target, reasoning: null });
            await waitFor(
              () =>
                evaluate(
                  "Boolean(document.querySelector('[data-live-tail] .answer-content .markdown-content'))",
                ),
              "live stage text",
            );
            await evaluate(
              "window.stageReader=document.querySelector('[data-live-tail] .answer-content .markdown-content');true",
            );
            let anchor;
            if (reading) {
              await send("Input.dispatchMouseEvent", {
                type: "mouseWheel",
                x: 700,
                y: 230,
                deltaX: 0,
                deltaY: -240,
              });
              await frame();
              anchor = await evaluate(() => {
                const bounds = document
                  .querySelector(".messages-container")
                  .getBoundingClientRect();
                const node = [...document.querySelectorAll("p")].find(
                  (node) =>
                    node.textContent.startsWith("Earlier paragraph") &&
                    node.getBoundingClientRect().top >= bounds.top &&
                    node.getBoundingClientRect().bottom < bounds.bottom,
                );
                window.readingParagraph = node;
                return node ? node.getBoundingClientRect().top : null;
              });
              assert.notEqual(anchor, null, "manual wheel reaches earlier readable text");
            }
            const before = await evaluate("window.stageReader.textContent.length");
            assert.ok(before < target.length, "stage has an outstanding presentation buffer");
            await evaluate(
              "window.stageFixture.push('Status', {stage:'model_process_summary',message:" +
                JSON.stringify(target) +
                "});window.stageFixture.push('ToolCall',{callId:'inspect',summary:'Inspect project'}, {toolName:'exec_command'});",
            );
            const archived = await evaluate(() => ({
              same:
                document.querySelector(
                  "[data-live-tail] .agentProcessSectionHeading .markdown-content",
                ) === window.stageReader,
              length: window.stageReader.textContent.length,
              toolVisible: document.body.textContent.includes("Running an external tool"),
            }));
            assert.equal(archived.same, true, "phase boundary keeps the actual text node mounted");
            assert.equal(archived.toolVisible, true, "tool appears before the phase buffer drains");
            assert.ok(
              archived.length >= before && archived.length < target.length,
              "phase boundary neither replays nor dumps the remaining text",
            );
            await push("ModelSnapshot", { revision: 2, text: target, reasoning: null });
            await waitFor(
              () =>
                evaluate(
                  "Boolean(document.querySelector('[data-live-tail] .agentAssistantAnswer .markdown-content'))",
                ),
              "independent final text",
            );
            await evaluate(
              "window.finalReader=document.querySelector('[data-live-tail] .agentAssistantAnswer .markdown-content');true",
            );
            assert.equal(
              await evaluate("window.finalReader === window.stageReader"),
              false,
              "equal wording in a later reply has an independent reader",
            );
            await push("Final", { content: target });
            await push("AgentRunCompleted", {}, { status: "done" });
            assert.equal(
              await evaluate(
                "Boolean(document.querySelector('[data-history-row=reply]')?.contains(window.finalReader))",
              ),
              true,
              "completion transfers the same reader into measured history",
            );
            const drain = await evaluate(
              () =>
                new Promise((resolve) => {
                  const lengths = [];
                  let n = 0;
                  function sample() {
                    lengths.push(window.finalReader.textContent.length);
                    if (++n < 8) requestAnimationFrame(sample);
                    else resolve(lengths);
                  }
                  requestAnimationFrame(sample);
                }),
            );
            assert.ok(
              new Set(drain).size > 2 && Math.max(...drain) < target.length,
              "completed final text continues to advance frame by frame",
            );
            await waitFor(
              () =>
                evaluate(
                  "window.stageReader.textContent.length === " +
                    target.length +
                    " && window.finalReader.textContent.length === " +
                    target.length,
                ),
              "both presentation buffers drain",
            );
            await frame();
            assert.equal(
              await evaluate("window.finalReader.isConnected && window.stageReader.isConnected"),
              true,
              "completion retains both text nodes",
            );
            if (reading) {
              const after = await evaluate("window.readingParagraph.getBoundingClientRect().top");
              assert.ok(
                Math.abs(after - anchor) < 3,
                "presentation never moves the manually selected reading position",
              );
            } else {
              const gap = await evaluate(
                "(()=>{const e=document.querySelector('.messages-container');return e.scrollHeight-e.clientHeight-e.scrollTop})()",
              );
              assert.ok(
                Math.abs(gap) < 3,
                "latest following reaches the real end after both buffers drain",
              );
            }
          },
        );
      } finally {
        await server.close();
        const resolved = await realpath(profile);
        assert.equal(dirname(resolved).toLowerCase(), (await realpath(tmpdir())).toLowerCase());
        assert.ok(basename(resolved).startsWith("centaeris-stage-stream-"));
        await rm(resolved, { recursive: true, force: true, maxRetries: 5, retryDelay: 100 });
      }
    },
  );

test("sent prompt keeps following through reserved space and real history growth", async () => {
  const profile = await mkdtemp(join(tmpdir(), "centaeris-sent-follow-"));
  const server = await createServer({
    root: fileURLToPath(new URL("../..", import.meta.url)),
    logLevel: "error", server: { port: 0, host: "127.0.0.1", hmr: false },
  });
  try {
    await server.listen();
    await runBrowser(server.resolvedUrls.local[0] + "tests/browser/streaming-transcript.html?sent",
      { width: 940, height: 620 }, profile, async ({ send, evaluate, frame }) => {
        await waitFor(() => evaluate("Boolean(window.stageFixture && document.querySelector('[data-transcript-anchor=earlier-answer]'))"), "history mount");
        await evaluate("document.fonts.ready");
        for (let n = 0; n < 4; n++) await frame();
        await evaluate("window.stageFixture.send()");
        await waitFor(() => evaluate("Boolean(document.querySelector('[data-live-tail]'))"), "new pending response");
        for (let n = 0; n < 24; n++) await frame();
        assert.equal(await evaluate("window.stageFixture.follow.isFollowingLatest"), true, "sending retains following while the reply is pending");
        assert.ok(await evaluate("parseFloat(window.stageFixture.follow.spacerRef.current.style.height)"), "sent prompt reserves room for the pending response");

        await evaluate("window.stageFixture.settleComposer()");
        for (let n = 0; n < 4; n++) await frame();
        assert.equal(await evaluate("window.stageFixture.follow.isFollowingLatest"), true, "composer resize clamping does not release the sent prompt");
        await evaluate("window.stageFixture.growEarlier()");
        for (let n = 0; n < 8; n++) await frame();
        assert.equal(await evaluate("window.stageFixture.follow.isFollowingLatest"), true, "history ResizeObserver correction is not a manual reading gesture");
        assert.ok(await evaluate("parseFloat(window.stageFixture.follow.spacerRef.current.style.height)"), "history remeasurement preserves reply space");

        const response = Array.from({ length: 18 }, (_, i) => "New answer " + i + ". " + "Streamed response text. ".repeat(12)).join("\n\n");
        await evaluate("window.stageFixture.push('ModelSnapshot',{revision:1,text:" + JSON.stringify(response) + ",reasoning:null})");
        await waitFor(() => evaluate("document.querySelector('[data-live-tail] .answer-content .markdown-content')?.lastElementChild?.textContent.trim() === " + JSON.stringify(response.split("\n\n").at(-1).trim())), "stream presentation drains");
        await frame();
        assert.equal(await evaluate("window.stageFixture.follow.isFollowingLatest"), true);
        assert.ok(await evaluate("(()=>{const e=document.querySelector('.messages-container');return Math.abs(e.scrollHeight-e.clientHeight-e.scrollTop)<3})()"), "following reaches the live response after reserved space is consumed");

        await send("Input.dispatchMouseEvent", { type: "mouseWheel", x: 700, y: 230, deltaX: 0, deltaY: -240 });
        await frame();
        assert.equal(await evaluate("window.stageFixture.follow.isFollowingLatest"), false, "a real upward wheel still releases following");
      });
  } finally {
    await server.close();
    const resolved = await realpath(profile);
    assert.equal(dirname(resolved).toLowerCase(), (await realpath(tmpdir())).toLowerCase());
    assert.ok(basename(resolved).startsWith("centaeris-sent-follow-"));
    await rm(resolved, { recursive: true, force: true, maxRetries: 5, retryDelay: 100 });
  }
});
