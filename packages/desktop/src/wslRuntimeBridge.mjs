import { spawn } from "node:child_process";
import { createHash } from "node:crypto";
import { createReadStream } from "node:fs";
import { readFile } from "node:fs/promises";
import { Duplex } from "node:stream";
import { requireDistribution, toLinuxPath, toWindowsPath } from "./wslPaths.mjs";

export const connectRelayProcess = (child) => new Promise((resolve, reject) => {
  let header = Buffer.alloc(0);
  let stderr = "";
  let socket;
  let settled = false;
  const timeout = setTimeout(() => fail(new Error("WSL relay handshake timed out")), 10_000);
  const fail = (error) => {
    if (socket) { if (!socket.destroyed) socket.destroy(error); return; }
    if (settled) return;
    settled = true;
    clearTimeout(timeout);
    child.kill();
    reject(error);
  };
  child.stderr.on("data", (chunk) => { stderr = `${stderr}${chunk}`.slice(-8192); });
  child.on("error", fail);
  child.stdin.on("error", fail);
  child.stdout.on("error", fail);
  child.once("close", (code) => {
    if (socket) socket.destroy();
    else fail(new Error(`WSL relay exited (${code}): ${stderr.trim()}`));
  });
  const onHeader = (chunk) => {
    header = Buffer.concat([header, chunk]);
    const newline = header.indexOf(10);
    if (newline < 0 && header.length <= 1024) return;
    if (newline < 0 || newline > 1024
      || header.subarray(0, newline).toString() !== '{"connected":true}') {
      fail(new Error("Invalid WSL relay handshake"));
      return;
    }
    settled = true;
    clearTimeout(timeout);
    child.stdout.pause();
    child.stdout.off("data", onHeader);
    if (header.length > newline + 1) child.stdout.unshift(header.subarray(newline + 1));
    socket = new Duplex({
      read() { child.stdout.resume(); },
      write(chunk, encoding, callback) { child.stdin.write(chunk, encoding, callback); },
      final(callback) { child.stdin.end(callback); },
      destroy(error, callback) {
        child.kill();
        child.stdin.destroy();
        child.stdout.destroy();
        callback(error);
      },
    });
    child.stdout.on("data", (data) => { if (!socket.push(data)) child.stdout.pause(); });
    child.stdout.once("end", () => socket.push(null));
    resolve(socket);
  };
  child.stdout.on("data", onHeader);
});

const hashFile = async (file) => {
  const hash = createHash("sha256");
  for await (const chunk of createReadStream(file)) hash.update(chunk);
  return hash.digest("hex");
};

const BOOTSTRAP = await readFile(new URL("../../runtime/host/wsl-bootstrap.sh", import.meta.url), "utf8");
const bootstrapArgs = (operation, ...args) => ["/bin/sh", "-c", BOOTSTRAP, "centaeris-wsl", operation, ...args];

export const createWslRuntimeBridge = ({ executablePath, distribution, environment = process.env }) => {
  distribution = requireDistribution(distribution);
  const launch = (args) => spawn("wsl.exe", ["--distribution", distribution, "--exec", ...args], {
    env: environment, windowsHide: true, stdio: ["pipe", "pipe", "pipe"],
  });
  const capture = (args, input = "") => new Promise((resolve, reject) => {
    const child = launch(args);
    let stdout = "";
    let stderr = "";
    const timeout = setTimeout(() => { child.kill(); reject(new Error("WSL Runtime setup timed out")); }, 30_000);
    child.stdout.on("data", (data) => { stdout += data; if (stdout.length > 65536) child.kill(); });
    child.stderr.on("data", (data) => { stderr = `${stderr}${data}`.slice(-8192); });
    child.on("error", (error) => { clearTimeout(timeout); reject(error); });
    child.stdin.on("error", () => {});
    child.on("close", (code) => {
      clearTimeout(timeout);
      if (code !== 0) reject(new Error(`WSL Runtime setup failed (${code}): ${stderr.trim()}`));
      else resolve(stdout.trim());
    });
    child.stdin.end(input);
  });
  let prepared;
  const prepare = () => prepared ??= (async () => {
    const digest = await hashFile(executablePath);
    const source = toLinuxPath(executablePath, distribution, { importSource: true });
    const paths = (await capture(bootstrapArgs("install", source, digest))).split("\n");
    if (paths.length !== 3) throw new Error("Invalid WSL Runtime installation result");
    const [binary, workspace, defaultData] = paths.map((value) => toLinuxPath(value, distribution));
    const data = environment.CENTAERIS_WSL_DATA_DIR
      ? toLinuxPath(environment.CENTAERIS_WSL_DATA_DIR, distribution) : defaultData;
    return { binary, workspace, data };
  })().catch((error) => { prepared = null; throw error; });
  return {
    toRuntimePath: (value, options) => toLinuxPath(value, distribution, options),
    toDesktopPath: (value) => toWindowsPath(value, distribution),
    defaultWorkspace: async () => (await prepare()).workspace,
    endpoint: async () => {
      const { binary, workspace, data } = await prepare();
      const descriptor = JSON.parse(await capture(bootstrapArgs("endpoint", binary, workspace, data)));
      if (typeof descriptor.endpoint !== "string" || !descriptor.endpoint.startsWith("/")) {
        throw new Error("Invalid Linux Runtime endpoint");
      }
      return descriptor.endpoint;
    },
    connect: async () => {
      const { binary, workspace, data } = await prepare();
      return connectRelayProcess(launch(bootstrapArgs("connect", binary, workspace, data)));
    },
    start: async () => {
      const { binary, workspace, data } = await prepare();
      await capture(bootstrapArgs("start", binary, workspace, data));
    },
  };
};
