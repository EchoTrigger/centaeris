import assert from "node:assert/strict";
import fs from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { readDesktopFilePreview } from "./desktopFilePreview.mjs";

const fixtureRoot = async (context) => {
  const root = await fs.mkdtemp(path.join(os.tmpdir(), "centaeris-desktop-preview-"));
  context.after(() => fs.rm(root, { recursive: true, force: true }));
  return root;
};

test("desktop preview reads relative text with its canonical workspace root", async (context) => {
  const root = await fixtureRoot(context);
  const file = path.join(root, "sample.txt");
  const content = "desktop preview smoke\n";
  await fs.writeFile(file, content);
  const response = await readDesktopFilePreview({
    request: { path: "sample.txt", workspaceRoot: root },
  });
  assert.deepEqual(response, {
    root: await fs.realpath(root),
    path: await fs.realpath(file),
    name: "sample.txt",
    content,
    byteLen: Buffer.byteLength(content),
    encoding: "utf-8",
    contentKind: "text",
    mimeType: "text/plain; charset=utf-8",
    dataUrl: null,
  });
});

test("absolute previews resolve independently of workspace and strip verbatim path prefixes", async (context) => {
  const root = await fixtureRoot(context);
  const file = path.join(root, "outside.txt");
  await fs.writeFile(file, "absolute preview");
  const response = await readDesktopFilePreview({
    request: { path: path.toNamespacedPath(file), workspaceRoot: path.join(root, "missing") },
  });
  assert.equal(response.path, await fs.realpath(file));
  assert.equal(response.root, await fs.realpath(root));
  assert.equal(response.content, "absolute preview");
});

test("basePath takes precedence and a base file resolves relative to its parent", async (context) => {
  const root = await fixtureRoot(context);
  await fs.mkdir(path.join(root, "base"));
  await fs.writeFile(path.join(root, "base", "anchor.txt"), "anchor");
  await fs.writeFile(path.join(root, "base", "sample.txt"), "base selection");
  const response = await readDesktopFilePreview({
    request: {
      path: "sample.txt",
      basePath: "base/anchor.txt",
      workspaceRoot: path.join(root, "missing"),
    },
  }, root);
  assert.equal(response.root, await fs.realpath(path.join(root, "base")));
  assert.equal(response.content, "base selection");
});

test("relative previews preserve parent traversal and canonical link targets", async (context) => {
  const root = await fixtureRoot(context);
  const base = path.join(root, "base");
  const outside = path.join(root, "outside");
  await fs.mkdir(base);
  await fs.mkdir(outside);
  await fs.writeFile(path.join(outside, "sample.txt"), "target");
  await fs.symlink(outside, path.join(base, "linked"), process.platform === "win32" ? "junction" : "dir");
  for (const relative of ["../outside/sample.txt", "linked/sample.txt"]) {
    const response = await readDesktopFilePreview({ request: { path: relative, basePath: base } });
    assert.equal(response.root, await fs.realpath(base));
    assert.equal(response.path, await fs.realpath(path.join(outside, "sample.txt")));
    assert.equal(response.content, "target");
  }
});

test("path trimming follows Unicode whitespace and retains BOM filename characters", async (context) => {
  const root = await fixtureRoot(context);
  await fs.writeFile(path.join(root, "\uFEFFsample.txt"), "named file");
  const response = await readDesktopFilePreview({
    request: { path: " \u0085\uFEFFsample.txt\u0085 ", basePath: ` \u0085${root}\u0085 ` },
  });
  assert.equal(response.name, "\uFEFFsample.txt");
  assert.equal(response.content, "named file");
});

test("text previews remove leading UTF-8 BOMs and preserve byte length", async (context) => {
  const root = await fixtureRoot(context);
  const bytes = Buffer.from("\uFEFF\uFEFF中文\n");
  const file = path.join(root, "sample.constructor");
  await fs.writeFile(file, bytes);
  const response = await readDesktopFilePreview({ request: { path: file } });
  assert.equal(response.contentKind, "text");
  assert.equal(response.content, "中文\n");
  assert.equal(response.byteLen, bytes.length);
  await fs.writeFile(file, Buffer.from([0xC3, 0x28]));
  await assert.rejects(readDesktopFilePreview({ request: { path: file } }), {
    code: "desktop_file_preview_failed",
    message: "desktop file preview supports UTF-8 text, images, and PDF only",
  });
});

test("image previews use the supported extension MIME types and exact base64 bytes", async (context) => {
  const root = await fixtureRoot(context);
  const bytes = Buffer.from([0, 0xFE, 0xFF]);
  const images = new Map([
    ["PNG", "image/png"], ["jpg", "image/jpeg"], ["jpeg", "image/jpeg"],
    ["gif", "image/gif"], ["webp", "image/webp"], ["bmp", "image/bmp"],
    ["ico", "image/x-icon"], ["svg", "image/svg+xml"],
  ]);
  for (const [extension, mimeType] of images) {
    const file = path.join(root, `sample.${extension}`);
    await fs.writeFile(file, bytes);
    const response = await readDesktopFilePreview({ request: { path: file } });
    assert.equal(response.contentKind, "image");
    assert.equal(response.content, "");
    assert.equal(response.mimeType, mimeType);
    assert.equal(response.encoding, "base64");
    assert.equal(response.byteLen, bytes.length);
    assert.equal(response.dataUrl, `data:${mimeType};base64,${bytes.toString("base64")}`);
  }
});

test("PDF previews return the existing PDF response fields", async (context) => {
  const root = await fixtureRoot(context);
  const file = path.join(root, "sample.PDF");
  const bytes = Buffer.from("%PDF-1.4\n1 0 obj\n<<>>\nendobj\n%%EOF\n");
  await fs.writeFile(file, bytes);
  const response = await readDesktopFilePreview({ request: { path: file } });
  assert.equal(response.contentKind, "pdf");
  assert.equal(response.mimeType, "application/pdf");
  assert.equal(response.content, "");
  assert.equal(response.encoding, "base64");
  assert.equal(response.dataUrl, `data:application/pdf;base64,${bytes.toString("base64")}`);
});

test("preview enforces the inclusive 25 MiB file limit", async (context) => {
  const root = await fixtureRoot(context);
  const file = path.join(root, "bounded.txt");
  const limit = 25 * 1024 * 1024;
  await fs.writeFile(file, "");
  await fs.truncate(file, limit);
  const response = await readDesktopFilePreview({ request: { path: file } });
  assert.equal(response.byteLen, limit);
  await fs.truncate(file, limit + 1);
  await assert.rejects(readDesktopFilePreview({ request: { path: file } }), {
    code: "desktop_file_preview_failed",
    message: `desktop file preview is too large (${limit + 1} bytes, max ${limit})`,
  });
});

test("missing bases, empty paths and directories retain preview failure codes", async (context) => {
  const root = await fixtureRoot(context);
  for (const [request, message] of [
    [{ path: "sample.txt" }, "desktop file preview requires an absolute path or basePath/workspaceRoot"],
    [{ path: " \t\n" }, "desktop file preview path is empty"],
    [{ path: root }, "desktop preview path is not a file"],
  ]) {
    await assert.rejects(readDesktopFilePreview({ request }), {
      code: "desktop_file_preview_failed", message,
    });
  }
  await assert.rejects(readDesktopFilePreview({ request: { path: path.join(root, "missing") } }),
    { code: "desktop_file_preview_failed", message: /resolve desktop preview file failed/ });
  await assert.rejects(readDesktopFilePreview({ request: { path: "sample", basePath: path.join(root, "missing") } }),
    { code: "desktop_file_preview_failed", message: /resolve desktop preview base path failed/ });
});

test("preview request rejects unknown fields and invalid input types", async () => {
  for (const payload of [
    {}, [], null, { request: {} }, { request: [] }, { request: null },
    { request: { path: 1 } }, { request: { path: "sample", basePath: false } },
    { request: { path: "sample", workspaceRoot: 1 } },
    { request: { path: "sample", extra: true } },
    { request: { path: "sample" }, authorization: {} },
  ]) {
    await assert.rejects(readDesktopFilePreview(payload), { code: "invalid_request" });
  }
});

test("Windows drive-relative and root-relative file paths are rejected", { skip: process.platform !== "win32" }, async () => {
  for (const value of ["C:sample.txt", "\\sample.txt", "/sample.txt"]) {
    await assert.rejects(readDesktopFilePreview({ request: { path: value } }), {
      code: "desktop_file_preview_failed",
      message: "desktop file preview relative path cannot include a drive or root",
    });
  }
});
