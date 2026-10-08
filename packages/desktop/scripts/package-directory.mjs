import fs from "node:fs";
import { createRequire } from "node:module";
import path from "node:path";

export function packageDirectory(name, from) {
  const require = createRequire(path.join(from, "package.json"));
  for (const directory of require.resolve.paths(name) ?? []) {
    const candidate = path.join(directory, name);
    if (fs.existsSync(path.join(candidate, "package.json"))) return fs.realpathSync(candidate);
  }
  throw new Error(`Package ${name} is not installed for ${from}`);
}
