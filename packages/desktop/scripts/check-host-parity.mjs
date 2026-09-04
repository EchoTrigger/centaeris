import fs from "node:fs";
import path from "node:path";

const hostRoot = path.resolve(import.meta.dirname, "..");
const repoRoot = path.resolve(hostRoot, "..", "..");

const readText = (relativePath) =>
  fs.readFileSync(path.join(repoRoot, relativePath), "utf8");

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
  const text = readText("packages/runtime/src/commands.rs");
  const start = text.indexOf("impl RuntimeHostCommand");
  if (start < 0) {
    throw new Error("could not locate RuntimeHostCommand parser");
  }
  const parser = text.slice(start);
  return new Set(
    [...parser.matchAll(/"([a-zA-Z0-9_/-]+)"\s*=>\s*Ok\(Self::/g)].map(
      (match) => match[1],
    ),
  );
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
const uiCommandsMissingRuntimeHost = diff(uiCommandsRequiringRust, runtimeHostCommands);
const electronCommandsMissingRuntimeHost = diff(
  electronCommandsRequiringRust,
  runtimeHostCommands,
);
const uiEventsMissingElectronContract = diff(ui.events, electron.events);

const failures = [
  ["UI commands missing Electron contract", uiCommandsMissingElectronContract],
  ["UI commands missing Runtime Host", uiCommandsMissingRuntimeHost],
  ["Electron commands missing Runtime Host", electronCommandsMissingRuntimeHost],
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
  extraElectronCommands: diff(electron.commands, ui.commands),
  extraElectronEvents: diff(electron.events, ui.events),
};

if (failures.length > 0) {
  console.error(JSON.stringify({ ...report, failures }, null, 2));
  process.exit(1);
}

console.log(JSON.stringify(report, null, 2));
