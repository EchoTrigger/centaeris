import { spawnSync } from "node:child_process";
import { requireDistribution, toLinuxPath } from "../src/wslPaths.mjs";

const BUILD = `set -eu
repo=$1
profile=$2
cd "$repo"
export CARGO_BUILD_JOBS=2
export CARGO_TARGET_DIR="$HOME/.cache/centaeris-target"
if [ "$profile" = release ]; then
  "$HOME/.cargo/bin/cargo" build --locked --release -p centaeris-runtime --bin centaeris-runtime
else
  "$HOME/.cargo/bin/cargo" build --locked -p centaeris-runtime --bin centaeris-runtime
fi
mkdir -p "$repo/target/wsl/$profile"
cp -- "$CARGO_TARGET_DIR/$profile/centaeris-runtime" "$repo/target/wsl/$profile/centaeris-runtime"
`;

export const buildWslRuntime = (repoRoot, profile) => {
  if (!["debug", "release"].includes(profile)) throw new Error("Invalid Runtime build profile");
  const distribution = requireDistribution(process.env.CENTAERIS_WSL_DISTRIBUTION || "Ubuntu-24.04");
  const repo = toLinuxPath(repoRoot, distribution, { importSource: true });
  const result = spawnSync("wsl.exe", ["--distribution", distribution, "--exec", "/bin/sh", "-s", "--", repo, profile], {
    input: BUILD, stdio: ["pipe", "inherit", "inherit"], windowsHide: true,
  });
  if (result.error) throw result.error;
  if (result.status !== 0) throw new Error(`WSL Runtime build failed (${result.status}); install the repository's Rust toolchain in ${distribution}`);
};
