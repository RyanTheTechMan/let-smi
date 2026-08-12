import { readFile, stat } from "node:fs/promises";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const suffixes = new Map([
  ["aarch64-apple-darwin", ["darwin-arm64", "macho"]],
  ["x86_64-apple-darwin", ["darwin-x64", "macho"]],
  ["aarch64-pc-windows-msvc", ["win32-arm64-msvc", "pe"]],
  ["x86_64-pc-windows-msvc", ["win32-x64-msvc", "pe"]],
  ["aarch64-unknown-linux-gnu", ["linux-arm64-gnu", "elf"]],
  ["x86_64-unknown-linux-gnu", ["linux-x64-gnu", "elf"]],
  ["x86_64-unknown-linux-musl", ["linux-x64-musl", "elf"]],
]);

const target = process.argv[2];
if (!target || !suffixes.has(target)) {
  throw new Error(
    `Usage: node scripts/validate-native-artifact.mjs <target>; received ${target ?? "nothing"}`,
  );
}

const repositoryRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const [suffix, format] = suffixes.get(target);
const fileName = `let-smi.${suffix}.node`;
const candidates = [
  resolve(repositoryRoot, "packages/gpu", fileName),
  resolve(repositoryRoot, "packages/gpu/npm", suffix, fileName),
];
let artifact;
for (const candidate of candidates) {
  try {
    if ((await stat(candidate)).isFile()) {
      artifact = candidate;
      break;
    }
  } catch {
    // Try the next valid build/assembled-package location.
  }
}
if (artifact === undefined) {
  throw new Error(
    `Native artifact is missing; checked: ${candidates.join(", ")}`,
  );
}
const metadata = await stat(artifact);
if (!metadata.isFile() || metadata.size < 1_024) {
  throw new Error(
    `Native artifact is missing or implausibly small: ${artifact}`,
  );
}

const header = (await readFile(artifact)).subarray(0, 4);
const valid =
  (format === "elf" && header.equals(Buffer.from([0x7f, 0x45, 0x4c, 0x46]))) ||
  (format === "pe" &&
    header.subarray(0, 2).equals(Buffer.from([0x4d, 0x5a]))) ||
  (format === "macho" &&
    ["cffaedfe", "feedfacf", "cafebabe", "bebafeca"].includes(
      header.toString("hex"),
    ));
if (!valid) throw new Error(`Unexpected ${format} header in ${artifact}`);

console.log(
  `Validated ${target} artifact (${metadata.size} bytes): ${artifact}`,
);
