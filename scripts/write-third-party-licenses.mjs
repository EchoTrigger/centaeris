import path from "node:path";
import { writeThirdPartyLicenses } from "./third-party-licenses.mjs";

const repoRoot = path.resolve(import.meta.dirname, "..");
const outputRoot = path.resolve(process.argv[2] ?? "THIRD_PARTY_LICENSES");
const arguments_ = process.argv.slice(3);
const rustPackageNames = arguments_.filter((value) => value !== "--rust-only");
await writeThirdPartyLicenses(
  repoRoot,
  undefined,
  outputRoot,
  rustPackageNames.length > 0 ? rustPackageNames : undefined,
  false,
);
