import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { createRequire } from "node:module";
import { dirname, join } from "node:path";

const packageRoot = fileURLToPath(new URL("..", import.meta.url));
const vitest = join(dirname(createRequire(import.meta.url).resolve("vitest/package.json")), "vitest.mjs");
const result = spawnSync(
  process.execPath,
  [vitest, "run", "tests/transcript-baseline.test.ts", "--reporter=verbose"],
  {
    cwd: packageRoot,
    env: { ...process.env, CENTAERIS_RENDERING_BASELINE: "1" },
    stdio: "inherit",
  },
);

process.exitCode = result.status ?? 1;
