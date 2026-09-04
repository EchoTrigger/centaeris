import { mkdirSync, writeFileSync } from "node:fs";
import { spawnSync } from "node:child_process";
import { createRequire } from "node:module";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const scriptDir = dirname(fileURLToPath(import.meta.url));
const projectRoot = resolve(scriptDir, "..");
const require = createRequire(import.meta.url);
const cliOptions = parseCliOptions(process.argv.slice(2));
const runMode = resolveRunMode(cliOptions.mode);
const viteMaxAttempts = resolvePositiveInt(
  cliOptions.viteMaxAttempts || process.env.CENTAERIS_UI_BUILD_MAX_ATTEMPTS,
  3,
);
const normalizedEnv = {
  ...process.env,
  ComSpec: process.env.ComSpec || "C:\\Windows\\System32\\cmd.exe",
};
const reportPath = process.env.CENTAERIS_UI_BUILD_REPORT_PATH || "";
const tscBinPath = resolve(dirname(require.resolve("typescript/package.json")), "bin", "tsc");
const viteBinPath = resolve(dirname(require.resolve("vite/package.json")), "bin", "vite.js");

const stepsByMode = {
  typecheck: [
    {
      name: "TypeScript app typecheck",
      command: process.execPath,
      args: [tscBinPath, "-p", "tsconfig.app.json", "--noEmit", "--pretty", "false"],
      maxAttempts: 1,
      retryOnExitCode: false,
    },
    {
      name: "TypeScript node typecheck",
      command: process.execPath,
      args: [tscBinPath, "-p", "tsconfig.node.json", "--noEmit", "--pretty", "false"],
      maxAttempts: 1,
      retryOnExitCode: false,
    },
    {
      name: "TypeScript test typecheck",
      command: process.execPath,
      args: [tscBinPath, "-p", "tsconfig.test.json", "--noEmit", "--pretty", "false"],
      maxAttempts: 1,
      retryOnExitCode: false,
    },
  ],
  build: [
    {
      name: "TypeScript project build",
      command: process.execPath,
      args: [tscBinPath, "-b", "--pretty", "false"],
      maxAttempts: 1,
      retryOnExitCode: false,
    },
    {
      name: "TypeScript test typecheck",
      command: process.execPath,
      args: [tscBinPath, "-p", "tsconfig.test.json", "--noEmit", "--pretty", "false"],
      maxAttempts: 1,
      retryOnExitCode: false,
    },
    {
      name: "Vite build",
      command: process.execPath,
      args: [viteBinPath, "build"],
      maxAttempts: viteMaxAttempts,
      retryOnExitCode: true,
    },
  ],
  gate: [
    {
      name: "TypeScript app typecheck",
      command: process.execPath,
      args: [tscBinPath, "-p", "tsconfig.app.json", "--noEmit", "--pretty", "false"],
      maxAttempts: 1,
      retryOnExitCode: false,
    },
    {
      name: "TypeScript node typecheck",
      command: process.execPath,
      args: [tscBinPath, "-p", "tsconfig.node.json", "--noEmit", "--pretty", "false"],
      maxAttempts: 1,
      retryOnExitCode: false,
    },
    {
      name: "TypeScript test typecheck",
      command: process.execPath,
      args: [tscBinPath, "-p", "tsconfig.test.json", "--noEmit", "--pretty", "false"],
      maxAttempts: 1,
      retryOnExitCode: false,
    },
    {
      name: "Vite build",
      command: process.execPath,
      args: [viteBinPath, "build"],
      maxAttempts: viteMaxAttempts,
      retryOnExitCode: true,
    },
  ],
};

const agentRunStartedAtMs = Date.now();
const stepReports = [];
let overallStatus = "pass";
let failureReason = "";

try {
  const selectedSteps = stepsByMode[runMode];
  for (const step of selectedSteps) {
    const stepReport = runStepWithRetries(step);
    stepReports.push(stepReport);
  }
  console.log(`\n[build] completed (mode=${runMode})`);
} catch (error) {
  overallStatus = "fail";
  failureReason =
    error instanceof Error ? error.message : String(error || "unknown build error");
  console.error(`\n[build] failed (mode=${runMode}): ${failureReason}`);
  process.exitCode = 1;
} finally {
  if (reportPath.trim().length > 0) {
    writeReportFile(reportPath, {
      schema: "ui_build_gate_v1",
      mode: runMode,
      status: overallStatus,
      startedAt: new Date(agentRunStartedAtMs).toISOString(),
      endedAt: new Date().toISOString(),
      durationMs: Date.now() - agentRunStartedAtMs,
      failureReason,
      viteMaxAttempts,
      environment: {
        nodeVersion: process.version,
        platform: process.platform,
        arch: process.arch,
        comSpec: normalizedEnv.ComSpec || "",
        cwd: projectRoot,
        tscBinPath,
        viteBinPath,
      },
      steps: stepReports,
    });
  }
}

function parseCliOptions(rawArgs) {
  const options = {
    mode: "build",
    viteMaxAttempts: "",
  };
  for (let i = 0; i < rawArgs.length; i += 1) {
    const token = rawArgs[i];
    if (token.startsWith("--mode=")) {
      options.mode = token.slice("--mode=".length).trim();
      continue;
    }
    if (token === "--mode" && i + 1 < rawArgs.length) {
      options.mode = rawArgs[i + 1].trim();
      i += 1;
      continue;
    }
    if (token.startsWith("--vite-max-attempts=")) {
      options.viteMaxAttempts = token.slice("--vite-max-attempts=".length).trim();
      continue;
    }
    if (token === "--vite-max-attempts" && i + 1 < rawArgs.length) {
      options.viteMaxAttempts = rawArgs[i + 1].trim();
      i += 1;
    }
  }
  return options;
}

function resolveRunMode(rawMode) {
  const normalized = String(rawMode || "build")
    .trim()
    .toLowerCase();
  if (normalized === "typecheck" || normalized === "build" || normalized === "gate") {
    return normalized;
  }
  throw new Error(`unsupported mode: ${rawMode}`);
}

function resolvePositiveInt(rawValue, fallbackValue) {
  const parsed = Number.parseInt(String(rawValue || ""), 10);
  if (Number.isFinite(parsed) && parsed > 0) {
    return parsed;
  }
  return fallbackValue;
}

function runStepWithRetries(step) {
  const maxAttempts = Math.max(1, step.maxAttempts || 1);
  const startedAtMs = Date.now();
  const attempts = [];
  for (let attempt = 1; attempt <= maxAttempts; attempt += 1) {
    console.log(`\n[build] ${step.name} (attempt ${attempt}/${maxAttempts})`);
    const result = spawnSync(step.command, step.args, {
      stdio: "inherit",
      cwd: projectRoot,
      env: normalizedEnv,
    });
    const normalizedResult = normalizeSpawnResult(result);
    attempts.push({
      attempt,
      ...normalizedResult,
    });
    if (normalizedResult.ok) {
      return {
        name: step.name,
        status: "pass",
        durationMs: Date.now() - startedAtMs,
        attempts,
      };
    }

    const shouldRetry = attempt < maxAttempts && isRetryableFailure(normalizedResult, step);
    const reason = normalizedResult.summary;
    if (!shouldRetry) {
      throw new Error(`${step.name} failed: ${reason}`);
    }
    console.error(`[build] retry ${step.name} because: ${reason}`);
  }

  throw new Error(`${step.name} failed unexpectedly`);
}

function normalizeSpawnResult(result) {
  if (result.error) {
    const errorCode = String(result.error.code || "").toUpperCase();
    return {
      ok: false,
      kind: "spawn_error",
      code: errorCode || "UNKNOWN",
      statusCode: -1,
      signal: "",
      summary: `${errorCode || "spawn_error"}: ${result.error.message || "failed to start process"}`,
    };
  }
  if (typeof result.status === "number" && result.status !== 0) {
    return {
      ok: false,
      kind: "exit_code",
      code: "",
      statusCode: result.status,
      signal: "",
      summary: `exit code ${result.status}`,
    };
  }
  if (result.signal) {
    return {
      ok: false,
      kind: "signal",
      code: "",
      statusCode: -1,
      signal: String(result.signal),
      summary: `terminated by signal ${result.signal}`,
    };
  }
  return {
    ok: true,
    kind: "ok",
    code: "",
    statusCode: 0,
    signal: "",
    summary: "ok",
  };
}

function isRetryableFailure(result, step) {
  if (result.kind === "spawn_error") {
    return ["EPERM", "EACCES", "ETXTBSY", "EBUSY"].includes(result.code);
  }
  if (result.kind === "exit_code" && step.retryOnExitCode) {
    return true;
  }
  return false;
}

function writeReportFile(pathRaw, report) {
  const normalizedPath = resolve(projectRoot, pathRaw);
  const outputDir = dirname(normalizedPath);
  mkdirSync(outputDir, { recursive: true });
  writeFileSync(normalizedPath, `${JSON.stringify(report, null, 2)}\n`, "utf8");
  console.log(`[build] report => ${normalizedPath}`);
}
