import { readdir, readFile, writeFile } from "node:fs/promises";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const repositoryRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const packageRoot = resolve(repositoryRoot, "packages/gpu/npm");
const license = await readFile(resolve(repositoryRoot, "LICENSE"), "utf8");
const publicManifest = JSON.parse(
  await readFile(resolve(repositoryRoot, "packages/gpu/package.json"), "utf8"),
);
const expectedPackages = new Map([
  ["darwin-arm64", { cpu: "arm64", os: "darwin" }],
  ["darwin-x64", { cpu: "x64", os: "darwin" }],
  ["linux-arm64-gnu", { cpu: "arm64", libc: "glibc", os: "linux" }],
  ["linux-x64-gnu", { cpu: "x64", libc: "glibc", os: "linux" }],
  ["linux-x64-musl", { cpu: "x64", libc: "musl", os: "linux" }],
  ["win32-x64-msvc", { cpu: "x64", os: "win32" }],
]);
const entries = await readdir(packageRoot, { withFileTypes: true });
const directories = entries.filter((entry) => entry.isDirectory());

if (
  directories.length !== expectedPackages.size ||
  directories.some((directory) => !expectedPackages.has(directory.name))
) {
  throw new Error(
    `Generated native package directories do not match the expected target set: ${directories
      .map((directory) => directory.name)
      .sort()
      .join(", ")}`,
  );
}

for (const directory of directories) {
  const path = join(packageRoot, directory.name);
  const manifestPath = join(path, "package.json");
  const manifest = JSON.parse(await readFile(manifestPath, "utf8"));
  const expected = expectedPackages.get(directory.name);
  const binaryName = `let-smi.${directory.name}.node`;
  if (
    manifest.name !== `let-smi-${directory.name}` ||
    manifest.version !== publicManifest.version ||
    manifest.main !== binaryName ||
    !sameStringArray(manifest.files, [binaryName]) ||
    !sameStringArray(manifest.cpu, [expected.cpu]) ||
    !sameStringArray(manifest.os, [expected.os]) ||
    (expected.libc === undefined
      ? manifest.libc !== undefined
      : !sameStringArray(manifest.libc, [expected.libc])) ||
    manifest.scripts !== undefined ||
    manifest.bin !== undefined ||
    manifest.dependencies !== undefined ||
    manifest.optionalDependencies !== undefined ||
    manifest.bundledDependencies !== undefined ||
    manifest.gypfile !== undefined
  ) {
    throw new Error(`Unexpected native package manifest: ${manifestPath}`);
  }
  manifest.publishConfig = {
    ...manifest.publishConfig,
    access: "public",
    provenance: true,
    registry: "https://registry.npmjs.org/",
  };
  manifest.repository = {
    type: "git",
    url: publicManifest.repository.url,
  };
  await writeFile(
    manifestPath,
    `${JSON.stringify(manifest, null, 2)}\n`,
    "utf8",
  );
  await writeFile(join(path, "LICENSE"), license, "utf8");
}

console.log(
  `Prepared ${String(directories.length)} native packages with license and provenance metadata.`,
);

function sameStringArray(value, expected) {
  return (
    Array.isArray(value) &&
    value.length === expected.length &&
    value.every((entry, index) => entry === expected[index])
  );
}
