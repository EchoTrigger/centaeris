import path from "node:path";
import { writeThirdPartyLicenses } from "./third-party-licenses.mjs";

const hostRoot = path.resolve(import.meta.dirname, "..");
const repoRoot = path.resolve(hostRoot, "..", "..");
const outputRoot = path.resolve(process.argv[2] ?? "THIRD_PARTY_LICENSES");
const arguments_ = process.argv.slice(3);
const rustPackageNames = arguments_.filter((value) => value !== "--rust-only");

await writeThirdPartyLicenses(
  repoRoot,
  hostRoot,
  outputRoot,
  rustPackageNames.length > 0 ? rustPackageNames : undefined,
  !arguments_.includes("--rust-only"),
);
