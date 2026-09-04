import fs from "node:fs";
import path from "node:path";

const hostRoot = path.resolve(import.meta.dirname, "..");
const repoRoot = path.resolve(hostRoot, "..", "..");

const readText = (relativePath) =>
  fs.readFileSync(path.join(repoRoot, relativePath), "utf8");

const requireExactFields = (value, fields, label) => {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new Error(`${label} must be an object`);
  }
  const actual = Object.keys(value).sort();
  const expected = [...fields].sort();
  if (actual.length !== expected.length || actual.some((field, index) => field !== expected[index])) {
    throw new Error(`${label} fields must be exactly ${expected.join(", ")}`);
  }
};

const walkFiles = (root, predicate) => {
  const found = [];
  const visit = (current) => {
    for (const entry of fs.readdirSync(current, { withFileTypes: true })) {
      const fullPath = path.join(current, entry.name);
      if (entry.isDirectory()) {
        visit(fullPath);
        continue;
      }
      if (predicate(fullPath)) {
        found.push(fullPath);
      }
    }
  };
  visit(root);
  return found;
};

const collectUiBridgeUsage = () => {
  const uiRoot = path.join(repoRoot, "packages", "ui", "src");
  const commands = new Set();
  const events = new Set();
  for (const filePath of walkFiles(uiRoot, (item) => /\.(ts|tsx)$/.test(item))) {
    const text = fs.readFileSync(filePath, "utf8");
    for (const match of text.matchAll(
      /invokeHost(?:<[^>]+>)?\(\s*["']([^"']+)["']/g,
    )) {
      commands.add(match[1]);
    }
    for (const match of text.matchAll(
      /listenHost(?:<[^>]+>)?\(\s*["']([^"']+)["']/g,
    )) {
      events.add(match[1]);
    }
  }
  return { commands, events };
};

const collectElectronContract = () => {
  const text = readText("packages/desktop/src/hostContract.mjs");
  const eventSection = text.slice(
    text.indexOf("HOST_EVENT_NAMES"),
    text.indexOf("HOST_COMMANDS"),
  );
  const commandSection = text.slice(text.indexOf("HOST_COMMANDS"));
  const commandMatches = [
    ...commandSection.matchAll(/\["([a-zA-Z0-9_/-]+)",\s*\{([^}]*)\}/g),
  ];
  return {
    events: new Set(
      [...eventSection.matchAll(/"([^"]+)"/g)].map((match) => match[1]),
    ),
    commands: new Set(commandMatches.map((match) => match[1])),
    localCommands: new Set(
      commandMatches
        .filter((match) => /\blocal:\s*true\b/.test(match[2]))
        .map((match) => match[1]),
    ),
  };
};

const collectRuntimeHostCommands = () => {
  const manifest = JSON.parse(
    readText("packages/runtime/generated/runtime-methods.json"),
  );
  requireExactFields(manifest, ["schema", "methods"], "Runtime method registry");
  if (manifest.schema !== "centaeris.runtime-method-registry.v1") {
    throw new Error(`unsupported Runtime method registry: ${manifest.schema}`);
  }
  if (!Array.isArray(manifest.methods)) {
    throw new Error("Runtime method registry methods must be an array");
  }
  const commands = new Set();
  const scopes = new Set(["sharedRuntime", "executionHost", "hostSurface"]);
  const operationKinds = new Set([
    "read",
    "desiredStateWrite",
    "identityMutation",
    "creation",
    "oneShotAction",
  ]);
  const retryPolicies = new Set([
    "safeRetry",
    "sameOperationId",
    "noAutomaticRetry",
  ]);
  for (const [index, method] of manifest.methods.entries()) {
    requireExactFields(
      method,
      [
        "name",
        "operationKind",
        "reconcileMethod",
        "retryPolicy",
        "scope",
      ],
      `Runtime method registry methods[${index}]`,
    );
    if (typeof method.name !== "string" || !/^[a-zA-Z0-9_/-]+$/.test(method.name)) {
      throw new Error(`invalid Runtime method name at methods[${index}]`);
    }
    if (!scopes.has(method.scope)) {
      throw new Error(`invalid Runtime method scope at methods[${index}]`);
    }
    if (!operationKinds.has(method.operationKind)) {
      throw new Error(`invalid Runtime operation kind at methods[${index}]`);
    }
    if (!retryPolicies.has(method.retryPolicy)) {
      throw new Error(`invalid Runtime retry policy at methods[${index}]`);
    }
    if (
      method.reconcileMethod !== null &&
      (typeof method.reconcileMethod !== "string" ||
        !/^[a-zA-Z0-9_/-]+$/.test(method.reconcileMethod))
    ) {
      throw new Error(`invalid Runtime reconcile method at methods[${index}]`);
    }
    if (commands.has(method.name)) {
      throw new Error(`duplicate Runtime method: ${method.name}`);
    }
    commands.add(method.name);
  }
  for (const [index, method] of manifest.methods.entries()) {
    if (method.reconcileMethod !== null && !commands.has(method.reconcileMethod)) {
      throw new Error(
        `unknown Runtime reconcile method at methods[${index}]: ${method.reconcileMethod}`,
      );
    }
  }
  return commands;
};

const diff = (left, right) => [...left].filter((item) => !right.has(item)).sort();

const ui = collectUiBridgeUsage();
const electron = collectElectronContract();
const runtimeHostCommands = collectRuntimeHostCommands();

const uiCommandsMissingElectronContract = diff(ui.commands, electron.commands);
const uiCommandsRequiringRust = new Set(
  [...ui.commands].filter((command) => !electron.localCommands.has(command)),
);
const electronCommandsRequiringRust = new Set(
  [...electron.commands].filter((command) => !electron.localCommands.has(command)),
);
const expectedRuntimeCommandsWithoutElectronRoute = new Set([
  "initialize",
  "process_capture",
]);
const runtimeCommandsWithoutElectronRoute = new Set(
  diff(runtimeHostCommands, electron.commands),
);
const uiCommandsMissingRuntimeHost = diff(uiCommandsRequiringRust, runtimeHostCommands);
const electronCommandsMissingRuntimeHost = diff(
  electronCommandsRequiringRust,
  runtimeHostCommands,
);
const unexpectedRuntimeCommandsWithoutElectronRoute = diff(
  runtimeCommandsWithoutElectronRoute,
  expectedRuntimeCommandsWithoutElectronRoute,
);
const staleRuntimeCommandsWithoutElectronRoute = diff(
  expectedRuntimeCommandsWithoutElectronRoute,
  runtimeCommandsWithoutElectronRoute,
);
const uiEventsMissingElectronContract = diff(ui.events, electron.events);

const failures = [
  ["UI commands missing Electron contract", uiCommandsMissingElectronContract],
  ["UI commands missing Runtime Host", uiCommandsMissingRuntimeHost],
  ["Electron commands missing Runtime Host", electronCommandsMissingRuntimeHost],
  ["Runtime commands missing an Electron route classification", unexpectedRuntimeCommandsWithoutElectronRoute],
  ["Stale Runtime-only Electron route classifications", staleRuntimeCommandsWithoutElectronRoute],
  ["UI events missing Electron contract", uiEventsMissingElectronContract],
].filter(([, items]) => items.length > 0);

const report = {
  ok: failures.length === 0,
  counts: {
    uiCommands: ui.commands.size,
    uiEvents: ui.events.size,
    electronCommands: electron.commands.size,
    electronEvents: electron.events.size,
    runtimeHostCommands: runtimeHostCommands.size,
  },
  runtimeCommandsWithoutElectronRoute: [...runtimeCommandsWithoutElectronRoute].sort(),
  extraElectronCommands: diff(electron.commands, ui.commands),
  extraElectronEvents: diff(electron.events, ui.events),
};

if (failures.length > 0) {
  console.error(JSON.stringify({ ...report, failures }, null, 2));
  process.exit(1);
}

console.log(JSON.stringify(report, null, 2));
