import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

const packageRoot = fileURLToPath(new URL("..", import.meta.url));
const vitest = fileURLToPath(
  new URL("../../../node_modules/vitest/vitest.mjs", import.meta.url),
);
const result = spawnSync(
  process.execPath,
  [vitest, "run", "tests/transcript-baseline.test.ts", "--reporter=verbose"],
  {
    cwd: packageRoot,
    env: { ...process.env, CENTAERIS_P0_BASELINE: "1" },
    stdio: "inherit",
  },
);

process.exitCode = result.status ?? 1;
